//! Resume the preserved original only after the failed attempt is durably rolled back.
//! Recovery policy informed by Orca's terminal-restart-state.ts and pty-input-recovery.ts.
//! Copyright (c) 2026 Lovecast Inc., MIT License.

use krill_proto::usage::HandoffOutcome;

use super::{Failure, Registry, SessionId, SwitchScope, lock, prepared_paths, read_prepared};

impl Registry {
    pub(in crate::registry) fn recover_failed_account_input(
        &self,
        session: SessionId,
        account_id: &str,
    ) -> Result<(), Failure> {
        self.cancel_account_switch(session, Some(account_id))
    }

    pub(super) fn cancel_account_switch(
        &self,
        session: SessionId,
        automatic: Option<&str>,
    ) -> Result<(), Failure> {
        let mut pending = lock(&self.account_work.pending);
        let (session_live, session_closing, session_closed_stream) = {
            let state = lock(&self.state);
            let closing = state.closing.contains(&session);
            let live = state.sessions.contains_key(&session)
                && !state.exited.contains_key(&session)
                && !closing;
            let closed_stream = state
                .sessions
                .get(&session)
                .is_some_and(|entry| entry.session.is_closed());
            (live, closing, closed_stream)
        };
        if let Some(expected) = automatic {
            if !session_live || session_closed_stream {
                return Err(Failure::NoSession);
            }
            if expected.is_empty() {
                return Err(Failure::BadRequest);
            }
            if self
                .account_processing
                .load(std::sync::atomic::Ordering::Acquire)
                || pending.keys().any(|scope| match scope {
                    SwitchScope::Provider(_) => true,
                    SwitchScope::Session(id, _) => *id == session,
                })
                || lock(&self.account_replacements).contains(&session)
                || self.agents.account_binding_retiring(session)
            {
                return Err(Failure::Os(libc::EBUSY));
            }
            let mut handoffs = lock(&self.account_handoffs);
            if let Some(handoff) = handoffs.get(&session) {
                if handoff.account_id != expected {
                    return Err(Failure::Os(libc::EBUSY));
                }
                match handoff.outcome() {
                    HandoffOutcome::Completed { .. } => return Err(Failure::Os(libc::EBUSY)),
                    HandoffOutcome::RetainedReplacement { .. } => return Err(Failure::Denied),
                    HandoffOutcome::Pending | HandoffOutcome::RetryableFailure { .. } => {}
                }
            }
            // A prepared replacement can still own the conversation. Never infer rollback
            // from an error string or the absence of a UI handoff alone.
            if let Some(dir) = self.dir.as_deref() {
                for path in prepared_paths(dir).map_err(|_| Failure::Denied)? {
                    if read_prepared(&path)
                        .map_err(|_| Failure::Denied)?
                        .is_some_and(|p| p.from == session || p.to == session)
                    {
                        return Err(Failure::Denied);
                    }
                }
            }
            if let Some(handoff) = handoffs.remove(&session)
                && let Err(error) = self.write_account_handoffs(&handoffs)
            {
                handoffs.insert(session, handoff);
                return Err(super::super::os(&error));
            }
            drop(handoffs);
            pending
                .retain(|scope, _| !matches!(scope, SwitchScope::Session(id, _) if *id == session));
            if self
                .agents
                .stored(session)
                .ok()
                .flatten()
                .is_none_or(|s| s.session_id.is_none() && s.transcript_path.is_none())
            {
                lock(&self.account_switch_opt_out).insert(session);
            }
            return Ok(());
        }

        if session_live {
            let mut handoffs = lock(&self.account_handoffs);
            if let Some(handoff) = handoffs.get(&session).filter(|h| h.to.is_none()).cloned() {
                handoffs.remove(&session);
                if let Err(error) = self.write_account_handoffs(&handoffs) {
                    handoffs.insert(session, handoff);
                    return Err(super::super::os(&error));
                }
                drop(handoffs);
                pending.retain(
                    |scope, _| !matches!(scope, SwitchScope::Session(id, _) if *id == session),
                );
                if self
                    .agents
                    .stored(session)
                    .ok()
                    .flatten()
                    .is_none_or(|s| s.session_id.is_none() && s.transcript_path.is_none())
                {
                    lock(&self.account_switch_opt_out).insert(session);
                }
                return Ok(());
            }
            let Some(from) = handoffs.retained_from(session) else {
                return Err(Failure::BadRequest);
            };
            drop(handoffs);
            if lock(&self.account_replacements).contains(&session)
                || !self.account_original_ended(from)
            {
                return Err(Failure::BadRequest);
            }
            if !lock(&self.ids).ensure_ready() {
                return Err(Failure::Os(libc::EIO));
            }
            self.ensure_no_unresolved_account_replacement(from)?;
            self.ensure_no_unresolved_account_replacement(session)?;
            let mut handoffs = lock(&self.account_handoffs);
            let Some((removed_from, previous)) =
                handoffs.acknowledge_replacement(session, |id| id == from)
            else {
                return Err(Failure::BadRequest);
            };
            if let Err(error) = self.write_account_handoffs(&handoffs) {
                handoffs.insert(removed_from, previous);
                return Err(super::super::os(&error));
            }
            return Ok(());
        }

        if session_closing || !self.account_original_ended(session) {
            return Err(Failure::NoSession);
        }
        let dismissible = lock(&self.account_handoffs).get(&session).is_some_and(|h| {
            matches!(
                h.outcome(),
                HandoffOutcome::RetryableFailure { .. }
                    | HandoffOutcome::RetainedReplacement { .. }
            )
        });
        if !dismissible {
            return Err(Failure::NoSession);
        }
        if self
            .account_processing
            .load(std::sync::atomic::Ordering::Acquire)
            || pending
                .keys()
                .any(|scope| matches!(scope, SwitchScope::Session(id, _) if *id == session))
            || lock(&self.account_replacements).contains(&session)
        {
            return Err(Failure::Os(libc::EBUSY));
        }
        if !lock(&self.ids).ensure_ready() {
            return Err(Failure::Os(libc::EIO));
        }
        self.ensure_no_unresolved_account_replacement(session)?;
        let mut handoffs = lock(&self.account_handoffs);
        let Some(handoff) = handoffs.get(&session).cloned().filter(|h| {
            matches!(
                h.outcome(),
                HandoffOutcome::RetryableFailure { .. }
                    | HandoffOutcome::RetainedReplacement { .. }
            )
        }) else {
            return Err(Failure::NoSession);
        };
        if let HandoffOutcome::RetainedReplacement {
            replacement: Some(rep),
            ..
        } = handoff.outcome()
        {
            drop(handoffs);
            if lock(&self.account_replacements).contains(&rep) {
                return Err(Failure::Os(libc::EBUSY));
            }
            self.ensure_no_unresolved_account_replacement(rep)?;
            handoffs = lock(&self.account_handoffs);
            if handoffs.get(&session) != Some(&handoff) {
                return Err(Failure::NoSession);
            }
        }
        handoffs.remove(&session);
        if let Err(error) = self.write_account_handoffs(&handoffs) {
            handoffs.insert(session, handoff);
            return Err(super::super::os(&error));
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::registry::account_switch::tests::kept_fixture;
    use crate::registry::account_switch::{FILE, Prepared, load};
    use krill_proto::usage::SessionHandoff;
    use std::fs;

    #[test]
    fn failed_switch_recovers_original_durably_and_accepts_a_lost_receipt_retry() {
        let root = std::env::temp_dir().join(format!("kas-auto-input-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let (reg, _original) = kept_fixture(&root, SessionId(220));
        let failed = SessionHandoff {
            from: SessionId(220),
            to: None,
            account_id: "selected".into(),
            error: Some("Replacement session is unavailable; original session preserved".into()),
        };
        lock(&reg.account_handoffs).insert(failed.from, failed);
        reg.save_account_handoffs().unwrap();
        reg.recover_failed_account_input(SessionId(220), "selected")
            .unwrap();
        assert!(load(Some(&root)).is_empty());
        reg.recover_failed_account_input(SessionId(220), "selected")
            .unwrap();
        assert!(lock(&reg.state).sessions.contains_key(&SessionId(220)));
        assert!(lock(&reg.state).closing.is_empty());
        assert!(lock(&reg.account_handoffs).is_empty());
        drop(reg);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn automatic_recovery_does_not_cancel_newer_pending_or_ambiguous_replacements() {
        for case in [
            "new-target",
            "pending",
            "completed",
            "retained",
            "prepared",
            "processing",
            "closing",
            "write-failed",
        ] {
            let root =
                std::env::temp_dir().join(format!("kas-auto-input-{case}-{}", std::process::id()));
            fs::remove_dir_all(&root).ok();
            let (reg, _original) = kept_fixture(&root, SessionId(220));
            let mut failed = SessionHandoff {
                from: SessionId(220),
                to: None,
                account_id: "selected".into(),
                error: Some("fixture failure".into()),
            };
            match case {
                "new-target" => failed.account_id = "newer".into(),
                "pending" => {
                    failed.error = None;
                    lock(&reg.account_work.pending).insert(
                        SwitchScope::Session(
                            SessionId(220),
                            krill_proto::usage::ProviderKind::Codex,
                        ),
                        crate::usage::accounts::switch_work::SwitchTarget {
                            account_id: "selected".into(),
                            prepared: false,
                            not_before: None,
                        },
                    );
                }
                "completed" => failed.to = Some(SessionId(221)),
                "retained" => failed.error = Some("unconfirmed replacement retained".into()),
                "prepared" => {
                    fs::write(
                        root.join("account-replacement.json"),
                        serde_json::to_vec(&Prepared {
                            from: SessionId(220),
                            to: SessionId(221),
                            account_id: "selected".into(),
                        })
                        .unwrap(),
                    )
                    .unwrap();
                }
                "processing" => reg
                    .account_processing
                    .store(true, std::sync::atomic::Ordering::Release),
                "closing" => {
                    lock(&reg.state).closing.insert(SessionId(220));
                }
                _ => {}
            }
            lock(&reg.account_handoffs).insert(failed.from, failed.clone());
            reg.save_account_handoffs().unwrap();
            if case == "write-failed" {
                fs::remove_file(root.join(FILE)).unwrap();
                fs::create_dir(root.join(FILE)).unwrap();
            }
            assert!(
                reg.recover_failed_account_input(SessionId(220), "selected")
                    .is_err(),
                "{case}"
            );
            assert_eq!(
                lock(&reg.account_handoffs).get(&failed.from),
                Some(&failed),
                "{case}"
            );
            assert!(lock(&reg.state).sessions.contains_key(&failed.from));
            if case != "write-failed" {
                assert_eq!(load(Some(&root)).get(&failed.from), Some(&failed));
            }
            drop(reg);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn orphaned_pending_switch_recovers_without_waiting_for_an_error() {
        let root = std::env::temp_dir().join(format!("kas-auto-orphan-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let (reg, _original) = kept_fixture(&root, SessionId(220));
        lock(&reg.account_handoffs).insert(
            SessionId(220),
            SessionHandoff {
                from: SessionId(220),
                to: None,
                account_id: "selected".into(),
                error: None,
            },
        );
        reg.save_account_handoffs().unwrap();
        reg.recover_failed_account_input(SessionId(220), "selected")
            .unwrap();
        assert!(load(Some(&root)).is_empty());
        assert!(lock(&reg.state).sessions.contains_key(&SessionId(220)));
        assert!(lock(&reg.state).closing.is_empty());
        drop(reg);
        fs::remove_dir_all(root).unwrap();
    }
}
