//! A missing handoff in an observation is not permission to send held prompts.
use super::*;
use krill_app::hosts::{HostId, HostTarget};
use krill_proto::SessionId;
use krill_proto::usage::{AccountsSnapshot, SessionHandoff, UsageRequest};

#[cfg(all(test, unix))]
mod recovery_tests;

/// Silent reconnect/rollback follows Orca's pty-input-recovery.ts and terminal-restart-state.ts.
/// Copyright (c) 2026 Lovecast Inc., MIT License.
#[derive(Default)]
pub(super) struct Recovery {
    automatic: bool,
    attempted: HashMap<Hold, Instant>,
    blocked: std::collections::HashSet<Hold>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Hold {
    pub host: HostId,
    pub session: SessionId,
    serial: u64,
    account: String,
    error: Option<String>,
    target: Option<HostTarget>,
    observed_at: Instant,
}

impl std::hash::Hash for Hold {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.host.hash(state);
        self.session.hash(state);
        self.serial.hash(state);
    }
}

pub(super) enum DeferredMutation {
    Refresh(HostId, UsageRequest),
    Sidebar(sidebar::SwitchSessionAccount),
}

#[derive(Clone)]
pub(super) struct ReadStamp {
    host: HostId,
    target: Option<HostTarget>,
    epoch: u64,
    connected: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Cancel {
    pub hold: Hold,
    pub target: HostTarget,
    pub epoch: u64,
}

impl Shell {
    /// A successful attach may renew the transport of an existing hold. Host reconfiguration
    /// is deliberately excluded: a matching numeric session ID on another endpoint is no proof.
    pub(super) fn rebind_account_input_target(&mut self, pane: PaneId) {
        let Some(live) = self.live.get(&pane) else {
            return;
        };
        let host = self.layout.host(pane).clone();
        let Some(session) = self.layout.session(pane) else {
            return;
        };
        let key = (host.clone(), session);
        let Some(hold) = self.account_holds.get(&key) else {
            return;
        };
        let Some(target) = live.target.as_ref() else {
            return;
        };
        if !live.pane.is_linked()
            || live.pane.session() != Some(session)
            || !live.pane.account_switching()
            || live.pane.is_closed()
            || !target.is_current()
            || self.target_for_pane(pane).as_ref() != Some(target)
            || hold.target.as_ref() == Some(target)
            || !hold
                .target
                .as_ref()
                .is_some_and(|old| old.same_session_authority(target))
        {
            return;
        }
        let mut renewed = hold.clone();
        renewed.target = Some(target.clone());
        self.account_hold_serial = self.account_hold_serial.wrapping_add(1);
        renewed.serial = self.account_hold_serial;
        self.account_holds.insert(key, renewed);
        self.invalidate_account_cancel(&host);
    }

    pub(super) fn can_recover_account_input(&self, hold: &Hold, cx: &App) -> bool {
        (hold.error.is_some() || hold.observed_at.elapsed() >= Duration::from_secs(5))
            && self.account_input_recovers_silently(hold, cx)
    }

    pub(super) fn account_input_recovers_silently(&self, hold: &Hold, cx: &App) -> bool {
        !self.account_recovery.blocked.contains(hold)
            && self
                .sidebar
                .read(cx)
                .supports_account_input_recovery(&hold.host)
    }

    pub(super) fn recover_account_input(&mut self, cx: &mut Context<Self>) {
        self.account_recovery.attempted.retain(|hold, _| {
            self.account_holds.get(&(hold.host.clone(), hold.session)) == Some(hold)
        });
        self.account_recovery.blocked.retain(|hold| {
            self.account_holds.get(&(hold.host.clone(), hold.session)) == Some(hold)
        });
        if self.account_cancel.is_some() {
            return;
        }
        let now = Instant::now();
        let hold = self
            .account_holds
            .values()
            .find(|hold| {
                self.can_recover_account_input(hold, cx)
                    && self.account_read_stamp(&hold.host).is_some()
                    && self.account_hold_target_matches(&hold.host, hold.session)
                    && self.live.iter().any(|(pane, live)| {
                        self.layout.host(*pane) == &hold.host
                            && self.layout.session(*pane) == Some(hold.session)
                            && live.pane.session() == Some(hold.session)
                            && live.pane.account_switching()
                            && !live.pane.is_closed()
                    })
                    && self.account_recovery.attempted.get(*hold).is_none_or(|at| {
                        now.saturating_duration_since(*at) >= Duration::from_secs(1)
                    })
            })
            .cloned();
        let Some(hold) = hold else { return };
        let Some(target) = self.target_for_host(&hold.host) else {
            return;
        };
        let cancel = Cancel {
            epoch: self
                .account_epochs
                .get(&hold.host)
                .copied()
                .unwrap_or_default(),
            hold,
            target,
        };
        if !self.account_cancel_is_current(&cancel) {
            return;
        }
        self.account_recovery
            .attempted
            .insert(cancel.hold.clone(), now);
        self.account_recovery.automatic = true;
        self.account_cancel = Some(cancel.clone());
        if self.notice.as_ref() == cancel.hold.error.as_ref() {
            self.notice = None;
        }
        cx.spawn(async move |this, cx| {
            let io = cancel.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    krill_app::accounts::recover_account_input(
                        &io.target,
                        io.hold.session,
                        io.hold.account.clone(),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.settle_account_recovery(&cancel, result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn settle_account_recovery(
        &mut self,
        cancel: &Cancel,
        result: Result<Option<AccountsSnapshot>, String>,
        cx: &mut Context<Self>,
    ) {
        if self.account_cancel.as_ref() != Some(cancel) || !self.account_recovery.automatic {
            return;
        }
        if self.account_cancel_is_current(cancel) {
            match result {
                Ok(Some(snapshot)) => self.finish_account_cancel(cancel, snapshot, cx),
                Ok(None) => {
                    self.account_recovery.blocked.insert(cancel.hold.clone());
                }
                Err(error) => tracing::debug!(%error, "account input recovery deferred"),
            }
        }
        self.account_cancel = None;
        self.account_recovery.automatic = false;
        self.invalidate_account_cancel(&cancel.hold.host);
        self.drain_deferred_account_mutations(cx);
        cx.notify();
    }

    pub(super) fn account_read_stamp(&self, host: &HostId) -> Option<ReadStamp> {
        let stamp = self.account_operation_stamp(host);
        self.account_read_is_current(&stamp).then_some(stamp)
    }

    pub(super) fn account_read_stamp_for(
        &self,
        host: &HostId,
        target: &HostTarget,
    ) -> Option<ReadStamp> {
        self.account_read_stamp(host)
            .filter(|stamp| stamp.target.as_ref() == Some(target))
    }

    pub(super) fn account_operation_stamp(&self, host: &HostId) -> ReadStamp {
        ReadStamp {
            host: host.clone(),
            target: self.target_for_host(host),
            epoch: self.account_epochs.get(host).copied().unwrap_or_default(),
            connected: self.link == Link::Up,
        }
    }

    pub(super) fn account_read_is_current(&self, stamp: &ReadStamp) -> bool {
        self.account_operation_is_current(stamp)
            && self
                .account_mutations
                .get(&stamp.host)
                .copied()
                .unwrap_or_default()
                == 0
            && self
                .account_cancel
                .as_ref()
                .is_none_or(|c| c.hold.host != stamp.host)
    }

    pub(super) fn account_operation_is_current(&self, stamp: &ReadStamp) -> bool {
        self.account_epochs
            .get(&stamp.host)
            .copied()
            .unwrap_or_default()
            == stamp.epoch
            && stamp.target.as_ref().is_none_or(HostTarget::is_current)
            && self.target_for_host(&stamp.host) == stamp.target
            && (stamp.host.is_remote() || (self.link == Link::Up) == stamp.connected)
    }

    pub(super) fn defer_account_mutation(&self, host: &HostId) -> bool {
        self.account_mutations
            .get(host)
            .copied()
            .unwrap_or_default()
            != 0
            || self
                .account_cancel
                .as_ref()
                .is_some_and(|cancel| &cancel.hold.host == host)
    }

    pub(super) fn invalidate_account_cancel(&mut self, host: &HostId) {
        let epoch = self.account_epochs.entry(host.clone()).or_default();
        *epoch = epoch.wrapping_add(1);
    }

    pub(super) fn begin_account_mutation(&mut self, host: &HostId) {
        self.invalidate_account_cancel(host);
        *self.account_mutations.entry(host.clone()).or_default() += 1;
    }

    pub(super) fn finish_account_mutation(&mut self, host: &HostId, cx: &mut Context<Self>) {
        if let Some(count) = self.account_mutations.get_mut(host) {
            *count = count.saturating_sub(1);
        }
        self.invalidate_account_cancel(host);
        let shell = cx.entity().downgrade();
        cx.defer(move |cx| {
            shell
                .update(cx, Self::drain_deferred_account_mutations)
                .ok();
        });
    }

    pub(super) fn observe_account_hold(&mut self, host: &HostId, handoff: &SessionHandoff) {
        let key = (host.clone(), handoff.from);
        if self
            .account_holds
            .get(&key)
            .is_some_and(|hold| hold.account == handoff.account_id && hold.error == handoff.error)
        {
            return;
        }
        self.account_hold_serial = self.account_hold_serial.wrapping_add(1);
        // Failure text and selected-account changes cannot re-authorize bytes already held
        // for a different connection. Preserve their original target until that hold ends.
        let target = self.account_holds.get(&key).map_or_else(
            || {
                self.live
                    .iter()
                    .find_map(|(pane, live)| {
                        (self.layout.host(*pane) == host
                            && self.layout.session(*pane) == Some(handoff.from))
                        .then(|| live.target.clone())
                        .flatten()
                    })
                    .or_else(|| self.target_for_host(host))
            },
            |hold| hold.target.clone(),
        );
        self.account_holds.insert(
            key,
            Hold {
                host: host.clone(),
                session: handoff.from,
                serial: self.account_hold_serial,
                account: handoff.account_id.clone(),
                error: handoff.error.clone(),
                target,
                observed_at: Instant::now(),
            },
        );
    }

    pub(super) fn cancellable_account_hold(&self) -> Option<Hold> {
        let pane = self.layout.focused()?;
        let session = self.layout.session(pane)?;
        let host = self.layout.host(pane);
        let live = self.live.get(&pane)?;
        if live.pane.is_closed()
            || live.pane.session() != Some(session)
            || !live.pane.account_switching()
        {
            return None;
        }
        self.account_holds.get(&(host.clone(), session)).cloned()
    }

    #[cfg(all(test, unix))]
    pub(super) fn failed_account_hold(&self) -> Option<Hold> {
        self.cancellable_account_hold()
            .filter(|hold| hold.error.is_some())
    }

    pub(super) fn account_hold_target_matches(&self, host: &HostId, session: SessionId) -> bool {
        self.account_holds
            .get(&(host.clone(), session))
            .is_none_or(|hold| {
                hold.target.as_ref().is_some_and(HostTarget::is_current)
                    && hold.target == self.target_for_host(host)
            })
    }

    #[cfg(all(test, unix))]
    pub(super) fn failed_account_input(&self) -> Option<SessionId> {
        self.failed_account_hold().map(|hold| hold.session)
    }

    fn account_cancel_is_current(&self, cancel: &Cancel) -> bool {
        let hold = &cancel.hold;
        self.account_epochs
            .get(&hold.host)
            .copied()
            .unwrap_or_default()
            == cancel.epoch
            && self
                .account_mutations
                .get(&hold.host)
                .copied()
                .unwrap_or_default()
                == 0
            && self.account_holds.get(&(hold.host.clone(), hold.session)) == Some(hold)
            && hold.target.as_ref() == Some(&cancel.target)
            && cancel.target.is_current()
            && self.target_for_host(&hold.host).as_ref() == Some(&cancel.target)
            && (hold.host.is_remote() || self.link == Link::Up)
    }

    pub(super) fn finish_account_cancel(
        &mut self,
        cancel: &Cancel,
        snapshot: AccountsSnapshot,
        cx: &mut Context<Self>,
    ) {
        if !self.account_cancel_is_current(cancel) {
            return;
        }
        let may_resume = snapshot.details.supported
            && !snapshot
                .details
                .handoffs
                .iter()
                .any(|h| h.from == cancel.hold.session);
        self.apply_accounts_snapshot_for_host(cancel.hold.host.clone(), snapshot, cx);
        if !may_resume || !self.account_cancel_is_current(cancel) {
            return;
        }
        let mut notices = Vec::new();
        for (pane, live) in &mut self.live {
            if self.layout.host(*pane) == &cancel.hold.host
                && self.layout.session(*pane) == Some(cancel.hold.session)
                && live.pane.session() == Some(cancel.hold.session)
                && live.target.as_ref() == Some(&cancel.target)
                && !live.pane.is_closed()
            {
                notices.extend(live.pane.resume_account_input(false, Now::get()));
            }
        }
        self.account_holds
            .remove(&(cancel.hold.host.clone(), cancel.hold.session));
        self.notice = None;
        for notice in &notices {
            self.show(Some(notice), cx);
        }
        cx.notify();
    }

    pub(super) fn continue_original_account(&mut self, hold: Hold, cx: &mut Context<Self>) {
        if self.account_cancel.is_some() || self.cancellable_account_hold().as_ref() != Some(&hold)
        {
            return;
        }
        let Some(target) = self.target_for_host(&hold.host) else {
            return;
        };
        let cancel = Cancel {
            epoch: self
                .account_epochs
                .get(&hold.host)
                .copied()
                .unwrap_or_default(),
            hold,
            target,
        };
        if !self.account_cancel_is_current(&cancel) {
            return;
        }
        self.account_cancel = Some(cancel.clone());
        self.account_recovery.automatic = false;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let io = cancel.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    krill_app::accounts::request_target(
                        &io.target,
                        &UsageRequest::CancelAccountSwitch {
                            session: io.hold.session,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.settle_account_cancel(&cancel, result, cx);
            })
            .ok();
        })
        .detach();
    }
    fn settle_account_cancel(
        &mut self,
        cancel: &Cancel,
        result: Result<AccountsSnapshot, String>,
        cx: &mut Context<Self>,
    ) {
        if self.account_cancel.as_ref() != Some(cancel) {
            return;
        }
        match result {
            Ok(snapshot) => self.finish_account_cancel(cancel, snapshot, cx),
            Err(error) if self.account_cancel_is_current(cancel) => {
                self.notice = Some(error);
            }
            Err(_) => {}
        }
        self.account_cancel = None;
        self.invalidate_account_cancel(&cancel.hold.host);
        self.drain_deferred_account_mutations(cx);
        cx.notify();
    }
    fn drain_deferred_account_mutations(&mut self, cx: &mut Context<Self>) {
        let deferred = std::mem::take(&mut self.account_deferred);
        for (source, mutation) in deferred {
            let host = match &mutation {
                DeferredMutation::Refresh(host, _) => host,
                DeferredMutation::Sidebar(target) => &target.host,
            };
            if self.defer_account_mutation(host) {
                self.account_deferred.push((source, mutation));
                continue;
            }
            if source.as_ref().is_none_or(|target| !target.is_current())
                || self.target_for_host(host) != source
            {
                if let DeferredMutation::Sidebar(target) = mutation {
                    self.sidebar.update(cx, |sidebar, cx| {
                        sidebar.finish_session_account_switch(&target.host, target.session, cx);
                    });
                }
                self.notice = Some(
                    krill_app::i18n::catalog::sidebar::HOST_DISCONNECTED
                        .get()
                        .into(),
                );
                continue;
            }
            match mutation {
                DeferredMutation::Refresh(host, request) => {
                    self.refresh_accounts_for_host(host, request, cx);
                }
                DeferredMutation::Sidebar(target) => {
                    self.switch_sidebar_session_account(target, cx);
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::shell::tests::{open, state_path};
    use gpui::TestAppContext;
    use krill_proto::os::net::UnixStream;

    #[gpui::test]
    fn cancel_receipt_requires_same_host_target_hold_and_mutation_epoch(cx: &mut TestAppContext) {
        let (view, cx) = open(cx, state_path("account-cancel-authority"));
        let (local_stream, _local_peer) = UnixStream::pair().unwrap();
        let (remote_stream, _remote_peer) = UnixStream::pair().unwrap();
        view.update(cx, |s, cx| {
            let local_pane = s.layout.focused().unwrap();
            s.layout.split(Axis::Horizontal);
            let remote_pane = s.layout.focused().unwrap();
            assert_ne!(local_pane, remote_pane);
            let session = SessionId(91);
            let remote = HostId::Remote("authority-fixture".into());
            let root = state_path("account-cancel-sockets");
            let paths = Paths {
                socket: root.join("run/sock"),
                holder: root.join("holder"),
                krilld: root.join("daemon"),
            };
            let local_target = HostTarget::local(paths.clone());
            let remote_target = HostTarget::remote("authority-fixture", root.join("remote"));
            s.paths = Some(paths.clone());
            s.link = Link::Up;
            s.host_targets.insert(remote.clone(), remote_target.clone());
            for (pane, host, target, stream) in [
                (
                    local_pane,
                    HostId::Local,
                    local_target.clone(),
                    local_stream,
                ),
                (remote_pane, remote.clone(), remote_target, remote_stream),
            ] {
                s.layout.set_host(pane, host.clone());
                s.layout.set_session(pane, session);
                s.attach_test(pane, session, stream, cx);
                s.live.get_mut(&pane).unwrap().target = Some(target);
                let mut failed = AccountsSnapshot::default();
                failed.details.supported = true;
                failed.details.handoffs.push(SessionHandoff {
                    from: session,
                    to: None,
                    account_id: "selected".into(),
                    error: Some("fixture".into()),
                });
                s.apply_accounts_snapshot_for_host(host, failed, cx);
            }
            let mut empty = AccountsSnapshot::default();
            empty.details.supported = true;
            let make_cancel = |s: &Shell| Cancel {
                hold: s.account_holds[&(HostId::Local, session)].clone(),
                target: local_target.clone(),
                epoch: s
                    .account_epochs
                    .get(&HostId::Local)
                    .copied()
                    .unwrap_or_default(),
            };
            let old = make_cancel(s);
            s.begin_account_mutation(&HostId::Local);
            s.finish_account_cancel(&old, empty.clone(), cx);
            assert!(s.live[&local_pane].pane.account_switching());
            s.finish_account_mutation(&HostId::Local, cx);
            s.finish_account_cancel(&old, empty.clone(), cx);
            assert!(s.live[&local_pane].pane.account_switching());
            let valid = make_cancel(s);
            s.paths.as_mut().unwrap().socket = root.join("new-socket");
            s.finish_account_cancel(&valid, empty.clone(), cx);
            assert!(s.live[&local_pane].pane.account_switching());
            let moved_target = HostTarget::local(s.paths.clone().unwrap());
            s.live.get_mut(&local_pane).unwrap().target = Some(moved_target.clone());
            let moved_cancel = Cancel { target: moved_target, ..valid.clone() };
            s.finish_account_cancel(&moved_cancel, empty.clone(), cx);
            assert!(s.live[&local_pane].pane.account_switching(), "new target cannot authorize an old hold");
            s.observe_account_hold(&HostId::Local, &SessionHandoff { from: session, to: None, account_id: "new host account".into(), error: Some("different failure".into()) });
            let moved_cancel = Cancel { hold: s.account_holds[&(HostId::Local, session)].clone(), ..moved_cancel };
            s.finish_account_cancel(&moved_cancel, empty.clone(), cx);
            assert!(s.live[&local_pane].pane.account_switching(), "new failure must preserve original hold target");
            let mut moved_handoff = empty.clone();
            moved_handoff.details.handoffs.push(SessionHandoff { from: session, to: Some(SessionId(999)), account_id: "new host account".into(), error: None });
            s.apply_accounts_snapshot_for_host(HostId::Local, moved_handoff, cx);
            assert_eq!(s.layout.session(local_pane), Some(session));
            s.paths = Some(paths);
            s.live.get_mut(&local_pane).unwrap().target = Some(local_target.clone());
            s.finish_account_cancel(&valid, AccountsSnapshot::default(), cx);
            assert!(s.live[&local_pane].pane.account_switching());
            let mut newer = SessionHandoff {
                from: session,
                to: None,
                account_id: "new-selected".into(),
                error: Some("new failure".into()),
            };
            s.observe_account_hold(&HostId::Local, &newer);
            s.finish_account_cancel(&valid, empty.clone(), cx);
            assert!(s.live[&local_pane].pane.account_switching());
            newer.account_id = "last-selected".into();
            s.observe_account_hold(&HostId::Local, &newer);
            let valid = make_cancel(s);
            let before_cancel = s.account_read_stamp(&HostId::Local).unwrap();
            s.account_cancel = Some(valid.clone());
            assert!(s.account_read_stamp(&HostId::Local).is_none());
            s.refresh_accounts_for_host(HostId::Local, UsageRequest::SwitchAccount { provider: krill_proto::usage::ProviderKind::Codex, account_id: "queued".into() }, cx);
            assert_eq!(s.account_deferred.len(), 1);
            assert_eq!(s.account_epochs[&HostId::Local], valid.epoch, "queued mutation must not invalidate the successful cancellation");
            // Keep the fixture read-only: verify queue ownership without issuing that mutation.
            let deferred = std::mem::take(&mut s.account_deferred);
            assert!(matches!(&deferred[0], (Some(source), DeferredMutation::Refresh(HostId::Local, _)) if source == &local_target));
            s.settle_account_cancel(&valid, Ok(empty), cx);
            assert!(!s.account_read_is_current(&before_cancel), "pre-cancel pending snapshots must be fenced out");
            assert!(!s.live[&local_pane].pane.account_switching());
            assert!(
                s.live[&remote_pane].pane.account_switching(),
                "another host's same session number must remain held"
            );
            assert!(s.account_holds.contains_key(&(remote, session)));
        });
    }
}
