//! Resume live agents under the selected account without discarding their conversation.
//! Reference: Orca src/main/codex/codex-legacy-session-resume.ts and
//! src/renderer/src/store/terminals/terminal-restart-state.ts,
//! Copyright (c) 2026 Lovecast Inc., MIT License.

#[cfg(all(test, unix))]
mod tests;
mod transcript;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::control::{AgentLaunch, Failure};
use krill_proto::os::ffi::OsStrExt as _;
use krill_proto::usage::{AccountsSnapshot, ConnectionStatus, ProviderKind, SessionHandoff};

use super::{Peer, Registry, Scope, lock};
use crate::usage::accounts;

const FILE: &str = "account-handoffs.json";
const PREPARED_FILE: &str = "account-replacement.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct Prepared {
    from: SessionId,
    to: SessionId,
    account_id: String,
}

pub(super) fn load(dir: Option<&Path>) -> BTreeMap<SessionId, SessionHandoff> {
    dir.and_then(|d| std::fs::read(d.join(FILE)).ok())
        .and_then(|raw| serde_json::from_slice::<Vec<SessionHandoff>>(&raw).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|h| (h.from, h))
        .collect()
}

impl Registry {
    fn live_agent_for_switch(
        &self,
        session: SessionId,
    ) -> Option<crate::agent::control::StoredLaunch> {
        if let Some(stored) = self.agents.stored(session).ok().flatten() {
            if stored.account.is_none() && stored.hibernation_blocked {
                let idle_shell = {
                    let state = lock(&self.state);
                    state
                        .sessions
                        .get(&session)
                        .is_some_and(|e| e.session.tap().foreground() == Some(e.pid))
                };
                if idle_shell {
                    return None;
                }
            }
            return Some(stored);
        }
        let (root_pid, fg_pid, entry_cwd) = {
            let state = lock(&self.state);
            let entry = state.sessions.get(&session)?;
            (
                entry.pid,
                entry.session.tap().foreground(),
                entry.cwd.as_os_str().as_bytes().to_vec(),
            )
        };
        let mut agent_pid = None;
        let mut agent = None;
        if let Some(mut cur) = fg_pid.filter(|&pid| pid > 0 && pid != root_pid) {
            for _ in 0..16 {
                if let Ok(name) = crate::sys::process_name(cur)
                    && let Some(parsed) = krill_proto::agent_history::VaultAgent::parse(name.trim())
                {
                    agent_pid = Some(cur);
                    agent = Some(parsed);
                    break;
                }
                let Ok(parent) = crate::sys::parent_pid(cur) else {
                    break;
                };
                if parent <= 1 || parent == cur || parent == root_pid {
                    break;
                }
                cur = parent;
            }
        }
        let agent = agent.or_else(|| {
            let status_agent = self.status.get(session).agent;
            krill_proto::agent_history::VaultAgent::parse(&status_agent)
        })?;
        let cwd = agent_pid
            .and_then(|pid| crate::sys::current_directory(pid).ok())
            .map(|p| p.into_os_string().as_encoded_bytes().to_vec())
            .filter(|c| !c.is_empty())
            .unwrap_or(entry_cwd);
        let permission = self
            .agents
            .agents()
            .map_or(krill_proto::agent::Permission::Yolo, |a| a.permission);
        Some(crate::agent::control::StoredLaunch {
            profile: agent.profile().to_owned(),
            permission,
            session_id: None,
            transcript_path: None,
            cwd,
            hibernation_blocked: true,
            account: None,
        })
    }

    pub(super) fn queue_account_switch(&self, provider: ProviderKind, account_id: &str) {
        let mut pending = lock(&self.account_work.pending);
        self.prepare_account_switch(provider, account_id);
        pending.insert(
            provider,
            crate::usage::accounts::switch_work::SwitchTarget {
                account_id: account_id.into(),
            },
        );
        self.account_work.wake.notify_one();
    }

    fn prepare_account_switch(&self, provider: ProviderKind, account_id: &str) {
        let sessions: Vec<_> = {
            let state = lock(&self.state);
            state
                .sessions
                .keys()
                .filter(|id| !state.exited.contains_key(id) && !state.closing.contains(id))
                .copied()
                .collect()
        };
        let mut found = false;
        let mut cancelled = false;
        for from in sessions {
            // A later click must not enqueue a replacement that is still being prepared.
            if lock(&self.account_replacements).contains(&from) {
                continue;
            }
            let Some(stored) = self.live_agent_for_switch(from) else {
                continue;
            };
            let Ok(agents) = self.agents.agents() else {
                continue;
            };
            let Some((_, profile)) = agents.resolve(&stored.profile) else {
                continue;
            };
            if profile
                .program()
                .and_then(|p| Path::new(p).file_name())
                .and_then(|n| n.to_str())
                .and_then(ProviderKind::parse)
                != Some(provider)
            {
                continue;
            }
            if stored.account.as_ref().is_some_and(|a| a.id == account_id) {
                let mut handoffs = lock(&self.account_handoffs);
                if handoffs.get(&from).is_some_and(|h| h.to.is_none()) {
                    handoffs.remove(&from);
                    cancelled = true;
                }
                continue;
            }
            lock(&self.account_handoffs).insert(
                from,
                SessionHandoff {
                    from,
                    to: None,
                    account_id: account_id.into(),
                    error: None,
                },
            );
            found = true;
        }
        if found || cancelled {
            self.save_account_handoffs().ok();
        }
    }

    pub(super) fn cancel_failed_account_switch(&self, session: SessionId) -> Result<(), Failure> {
        let _selection = lock(&self.account_work.pending);
        {
            let state = lock(&self.state);
            if !state.sessions.contains_key(&session)
                || state.exited.contains_key(&session)
                || state.closing.contains(&session)
            {
                return Err(Failure::NoSession);
            }
        }
        let mut handoffs = lock(&self.account_handoffs);
        let Some(handoff) = handoffs
            .get(&session)
            .filter(|h| h.to.is_none() && h.error.is_some())
            .cloned()
        else {
            return Err(Failure::BadRequest);
        };
        handoffs.remove(&session);
        if let Err(error) = self.write_account_handoffs(&handoffs) {
            handoffs.insert(session, handoff);
            return Err(super::os(&error));
        }
        Ok(())
    }

    pub(super) fn account_switch_snapshot(&self, snapshot: &mut AccountsSnapshot) {
        let handoffs = lock(&self.account_handoffs);
        let replacements = lock(&self.account_replacements);
        let state = lock(&self.state);
        snapshot.details.handoffs = handoffs
            .values()
            .cloned()
            .map(|mut handoff| {
                // The durable recovery alias precedes retirement. Keep clients on the original
                // until retirement succeeds; a failed close must not strand them on a closed replacement.
                if handoff.to.is_some_and(|to| replacements.contains(&to))
                    || handoff.to.is_some()
                        && state.sessions.contains_key(&handoff.from)
                        && !state.exited.contains_key(&handoff.from)
                {
                    handoff.to = None;
                }
                handoff
            })
            .collect();
    }

    fn save_account_handoffs(&self) -> std::io::Result<()> {
        self.write_account_handoffs(&lock(&self.account_handoffs))
    }

    fn write_account_handoffs(
        &self,
        handoffs: &BTreeMap<SessionId, SessionHandoff>,
    ) -> std::io::Result<()> {
        let Some(dir) = self.state_dir() else {
            return Ok(());
        };
        let handoffs: Vec<_> = handoffs.values().collect();
        let result = serde_json::to_vec(&handoffs)
            .map_err(std::io::Error::other)
            .and_then(|raw| accounts::write_atomic_0600(&dir.join(FILE), &raw));
        if let Err(error) = &result {
            tracing::warn!(kind = ?error.kind(), "account handoff state save failed");
        }
        result
    }

    // Readers must not observe completion until its recovery record is durable.
    fn commit_account_handoff(
        &self,
        handoff: SessionHandoff,
        persist: impl FnOnce(&BTreeMap<SessionId, SessionHandoff>) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let mut handoffs = lock(&self.account_handoffs);
        let from = handoff.from;
        let previous = handoffs.insert(from, handoff);
        if let Err(error) = persist(&handoffs) {
            if let Some(previous) = previous {
                handoffs.insert(from, previous);
            } else {
                handoffs.remove(&from);
            }
            return Err(error);
        }
        Ok(())
    }

    /// Wait for a user switch, waking immediately instead of waiting for the quota tick.
    pub fn wait_account_work(&self, timeout: Duration) {
        if !self
            .account_recovered
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        let pending = lock(&self.account_work.pending);
        if pending.is_empty() {
            drop(
                self.account_work
                    .wake
                    .wait_timeout(pending, timeout)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
    }

    /// Called by the account worker. No registry or account-file lock is held across CLI IO.
    pub fn process_account_work(&self) {
        self.process_account_work_in(&accounts::default_home());
    }

    fn process_account_work_in(&self, home: &Path) {
        if *lock(&self.upgrading) {
            return;
        }
        self.recover_account_work(home);
        let pending = {
            let mut pending = lock(&self.account_work.pending);
            for (provider, target) in pending.iter() {
                self.prepare_account_switch(*provider, &target.account_id);
            }
            std::mem::take(&mut *pending)
        };
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        for (provider, target) in pending {
            let account_id = target.account_id;
            let from: Vec<_> = lock(&self.account_handoffs)
                .values()
                .filter(|h| h.account_id == account_id && h.to.is_none() && h.error.is_none())
                .map(|h| h.from)
                .collect();
            for old in from {
                if lock(&self.account_work.pending)
                    .get(&provider)
                    .is_some_and(|target| target.account_id != account_id)
                {
                    break;
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.handoff_account(&peer, old, provider, &account_id, home)
                }))
                .unwrap_or(Err(Failure::BadRequest));
                let mut handoffs = lock(&self.account_handoffs);
                if let Some(handoff) = handoffs
                    .get_mut(&old)
                    .filter(|h| h.account_id == account_id)
                {
                    match result {
                        Ok(new) => handoff.to = Some(new),
                        Err(error) => {
                            let reason = match error {
                                Failure::Denied => "selected account login or credential scope could not be verified".into(),
                                Failure::Os(libc::EAGAIN) => "conversation identity is not bound; wait for the original agent's session-start hook before retrying".into(),
                                Failure::Os(libc::ENOENT) => "conversation transcript is missing; original conversation was not replaced".into(),
                                Failure::Os(libc::EILSEQ) => "conversation transcript identity is mismatched or ambiguous".into(),
                                Failure::Os(libc::EEXIST) => "selected profile contains a separate or conflicting conversation copy; reconcile its history before retrying".into(),
                                Failure::Os(libc::ETIMEDOUT) => "conversation preparation or resume confirmation timed out; check that provider hooks are enabled before retrying".into(),
                                _ => error.to_string(),
                            };
                            if handoff.error.is_none() {
                                handoff.error = Some(format!(
                                    "Session {}: could not resume with selected account ({reason}); original session preserved",
                                    old.0
                                ));
                            }
                        }
                    }
                }
                if let Ok(new) = result {
                    // Keep durable aliases short across repeated A -> B -> C switches.
                    for alias in handoffs.values_mut().filter(|h| h.to == Some(old)) {
                        alias.to = Some(new);
                    }
                }
                drop(handoffs);
                self.save_account_handoffs().ok();
            }
        }
    }

    fn recover_account_work(&self, home: &Path) {
        if self
            .account_recovered
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        // A crash between Spawned and the durable handoff must not leave a second agent
        // consuming the selected account while a retry starts another replacement.
        self.recover_prepared_account();
        let saved: Vec<_> = lock(&self.account_handoffs).values().cloned().collect();
        if saved.is_empty() {
            return;
        }
        let state_dir = self.state_dir();
        let snapshot = accounts::refresh_accounts_with(home, state_dir.as_deref(), false);
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        for mut handoff in saved {
            let (old_live, new_live) = {
                let state = lock(&self.state);
                let live = |id: &SessionId| {
                    state.sessions.contains_key(id)
                        && !state.exited.contains_key(id)
                        && !state.closing.contains(id)
                };
                (live(&handoff.from), handoff.to.is_some_and(|id| live(&id)))
            };
            if !old_live {
                if handoff.to.is_none() && handoff.error.is_none() {
                    handoff.error =
                        Some("Original session has ended; automatic resume cancelled".into());
                    lock(&self.account_handoffs).insert(handoff.from, handoff);
                }
                continue;
            }
            if handoff.error.is_some() {
                continue;
            }
            if let Some(new) = handoff.to {
                if new_live {
                    lock(&self.account_replacements).insert(new);
                    if let Err(error) = self.close(&peer, handoff.from) {
                        self.close(&peer, new).ok();
                        handoff.to = None;
                        handoff.error = Some(format!(
                            "Could not retire original session ({error}); original session preserved"
                        ));
                    }
                    lock(&self.account_handoffs).insert(handoff.from, handoff.clone());
                    lock(&self.account_replacements).remove(&new);
                } else {
                    handoff.to = None;
                    handoff.error = Some(
                        "Replacement session is unavailable; original session preserved".into(),
                    );
                }
                lock(&self.account_handoffs).insert(handoff.from, handoff);
            } else if let Some(account) = snapshot
                .accounts
                .iter()
                .find(|a| a.id == handoff.account_id)
            {
                lock(&self.account_work.pending)
                    .entry(account.provider)
                    .or_insert_with(|| crate::usage::accounts::switch_work::SwitchTarget {
                        account_id: account.id.clone(),
                    });
            } else {
                handoff.error =
                    Some("Selected account is unavailable; original session preserved".into());
                lock(&self.account_handoffs).insert(handoff.from, handoff);
            }
        }
        self.save_account_handoffs().ok();
    }

    fn recover_prepared_account(&self) {
        let Some(dir) = self.state_dir() else { return };
        let path = dir.join(PREPARED_FILE);
        let prepared = std::fs::read(&path)
            .ok()
            .filter(|raw| raw.len() <= 4096)
            .and_then(|raw| serde_json::from_slice::<Prepared>(&raw).ok());
        if let Some(prepared) = prepared {
            let committed = lock(&self.account_handoffs)
                .get(&prepared.from)
                .is_some_and(|h| h.to == Some(prepared.to));
            let source = self.agents.stored(prepared.from).ok().flatten();
            let owned = self
                .agents
                .stored(prepared.to)
                .ok()
                .flatten()
                .is_some_and(|a| {
                    a.account.is_some_and(|a| a.id == prepared.account_id)
                        && source.as_ref().is_none_or(|s| {
                            s.profile == a.profile
                                && (s.session_id.is_none() || s.session_id == a.session_id)
                        })
                });
            if !committed && owned {
                let old_live = {
                    let state = lock(&self.state);
                    state.sessions.contains_key(&prepared.from)
                        && !state.exited.contains_key(&prepared.from)
                        && !state.closing.contains(&prepared.from)
                };
                if old_live {
                    self.close(
                        &Peer {
                            pid: None,
                            scope: Scope::Full,
                        },
                        prepared.to,
                    )
                    .ok();
                } else {
                    // The original ended independently while the daemon was down. Keep the
                    // replacement and make it reachable instead of closing the remaining PTY.
                    lock(&self.account_handoffs).insert(
                        prepared.from,
                        SessionHandoff {
                            from: prepared.from,
                            to: None,
                            account_id: prepared.account_id,
                            error: Some(format!("Original session ended during recovery; unconfirmed replacement {} was retained. Select it explicitly to continue; held input was not sent", prepared.to.0)),
                        },
                    );
                    self.save_account_handoffs().ok();
                }
            }
        }
        std::fs::remove_file(path).ok();
    }

    fn account_original_ended(&self, session: SessionId) -> bool {
        let state = lock(&self.state);
        !state.sessions.contains_key(&session) || state.exited.contains_key(&session)
    }

    fn handoff_account(
        &self,
        peer: &Peer,
        old: SessionId,
        provider: ProviderKind,
        account_id: &str,
        home: &Path,
    ) -> Result<SessionId, Failure> {
        let _claim = self.claim_resume(old).ok_or(Failure::BadRequest)?;
        let holder = self.holder.as_ref().ok_or(Failure::NoHolder)?;
        let stored = self
            .live_agent_for_switch(old)
            .ok_or(Failure::UnknownAgent)?;
        let state_dir = self.state_dir();
        let snapshot = accounts::refresh_accounts_with(home, state_dir.as_deref(), false);
        let target = snapshot
            .accounts
            .iter()
            .find(|a| a.id == account_id && a.provider == provider)
            .ok_or(Failure::BadRequest)?;
        let mut connection = snapshot.connection_for(account_id);
        let login_deadline = Instant::now() + Duration::from_secs(6);
        while connection == ConnectionStatus::Checking && Instant::now() < login_deadline {
            std::thread::sleep(Duration::from_millis(50));
            connection = accounts::refresh_accounts_with(home, state_dir.as_deref(), false)
                .connection_for(account_id);
        }
        if snapshot.connections.supported
            && matches!(
                connection,
                ConnectionStatus::NeedsLogin
                    | ConnectionStatus::CheckFailed
                    | ConnectionStatus::Checking
            )
        {
            return Err(Failure::Denied);
        }
        let agents = self.agents.agents().map_err(|_| Failure::UnknownAgent)?;
        let (canonical, profile) = agents
            .resolve(&stored.profile)
            .ok_or(Failure::UnknownAgent)?;
        if !target.host_default && !Path::new(&target.profile_dir).is_dir() {
            return Err(Failure::BadRequest);
        }
        let account = Some(crate::agent::control::LaunchAccount {
            id: target.id.clone(),
            env_key: provider.env_var().into(),
            dir: if target.host_default {
                String::new()
            } else {
                target.profile_dir.clone()
            },
        });
        let (cwd, size): (Vec<u8>, krill_proto::holder::WinSize) = {
            let state = lock(&self.state);
            if state.exited.contains_key(&old) {
                return Err(Failure::NoSession);
            }
            let entry = state.sessions.get(&old).ok_or(Failure::NoSession)?;
            let cwd = if stored.cwd.is_empty() {
                entry.cwd.as_os_str().as_bytes().to_vec()
            } else {
                stored.cwd.clone()
            };
            (
                cwd,
                entry.session.tap().size().unwrap_or(super::FALLBACK_SIZE),
            )
        };
        // A project path never identifies a conversation. Only launch/resume/hook bindings do.
        let session_id = transcript::session_id(provider, &stored)
            .map_err(|error| prepare_failure("transcript_preparation", &error))?;
        let source = crate::agent::resume_account(
            profile,
            state_dir.as_deref(),
            stored.account.as_ref(),
            Some(&session_id),
        )
        .unwrap_or_else(|| crate::agent::control::LaunchAccount {
            id: accounts::host_default_id(provider),
            env_key: provider.env_var().into(),
            dir: String::new(),
        });
        let prepared_transcript = transcript::prepare_bound(
            provider,
            home,
            &source,
            target,
            &session_id,
            &cwd,
            stored.transcript_path.as_deref(),
        )
        .map_err(|error| prepare_failure("transcript_preparation", &error))?;
        let new = self.next_id();
        lock(&self.account_replacements).insert(new);
        let mut replacement = Replacement {
            registry: self,
            original: old,
            session: new,
            committed: false,
        };
        self.agents.watch_account_resume(new);
        let (launch, sid, applied) = crate::agent::resume_in(
            profile,
            stored.permission,
            new,
            state_dir.as_deref(),
            Some(&session_id),
            account.as_ref(),
        )
        .map_err(|e| super::os(&e))?;
        if applied.as_ref().is_none_or(|a| a.id != account_id) {
            self.discard_settings(new);
            return Err(Failure::Denied);
        }
        crate::agent::pre_trust_workspace(profile, &cwd, applied.as_ref());
        self.agents
            .remember_session(
                new,
                &AgentLaunch {
                    profile: canonical.into(),
                    permission: stored.permission,
                },
                sid,
                cwd.clone(),
                applied,
            )
            .map_err(|e| super::os(&e))?;
        if let Some(dir) = state_dir.as_deref() {
            let raw = serde_json::to_vec(&Prepared {
                from: old,
                to: new,
                account_id: account_id.into(),
            })
            .map_err(|_| Failure::BadRequest)?;
            accounts::write_atomic_0600(&dir.join(PREPARED_FILE), &raw)
                .map_err(|e| super::os(&e))?;
        }
        let (new, _) = self.spawn_holder(
            peer,
            holder,
            new,
            launch.argv,
            launch.env,
            cwd,
            size.cols,
            size.rows,
        )?;
        // Give the local CLI preflight a chance to report an immediate spawn/auth failure.
        let tap = lock(&self.state)
            .sessions
            .get(&new)
            .ok_or(Failure::NoSession)?
            .session
            .tap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut ready = false;
        while Instant::now() < deadline {
            if lock(&self.state).exited.contains_key(&new) || tap.pulse().closed {
                self.close(peer, new).ok();
                return Err(Failure::BadRequest);
            }
            if self.agents.account_resume_observed(new, &session_id)
                && tap.pulse().end > 0
                && tap.quiet_ms() >= 150
            {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !ready {
            tracing::warn!(
                stage = "resume_confirmation",
                "account handoff timed out without a confirmed conversation"
            );
            return Err(Failure::Os(libc::ETIMEDOUT));
        }
        // Serialize the final commit with a later user selection. A -> B -> C must never
        // strand this pane on B or erase the A -> B handoff.
        let pending = lock(&self.account_work.pending);
        if pending
            .get(&provider)
            .is_some_and(|target| target.account_id != account_id)
        {
            self.close(peer, new).ok();
            return Err(Failure::BadRequest);
        }
        prepared_transcript
            .verify()
            .map_err(|error| prepare_failure("transcript_preparation", &error))?;
        for session in [old, new] {
            let current = self
                .agents
                .stored(session)
                .map_err(|e| super::os(&e))?
                .ok_or(Failure::Os(libc::EILSEQ))?;
            if transcript::session_id(provider, &current)
                .map_err(|e| prepare_failure("conversation_recheck", &e))?
                != session_id
            {
                return Err(Failure::Os(libc::EILSEQ));
            }
        }
        // Persist the recovery alias before retirement; snapshots hide it while the guard lives.
        if let Err(error) = self.commit_account_handoff(
            SessionHandoff {
                from: old,
                to: Some(new),
                account_id: account_id.into(),
                error: None,
            },
            |handoffs| self.write_account_handoffs(handoffs),
        ) {
            self.close(peer, new).ok();
            return Err(super::os(&error));
        }
        if !self.account_original_ended(old)
            && let Err(error) = self.close(peer, old)
        {
            // Do not leave two live agents consuming different accounts after a failed termination.
            self.close(peer, new).ok();
            lock(&self.account_handoffs).insert(
                old,
                SessionHandoff {
                    from: old,
                    to: None,
                    account_id: account_id.into(),
                    error: Some(error.to_string()),
                },
            );
            self.save_account_handoffs().ok();
            return Err(error);
        }
        // Holder Done acknowledges the teardown request, not process exit. Do not hold
        // the selection mutex while waiting for the holder's Exited observation.
        drop(pending);
        let deadline = Instant::now() + Duration::from_secs(7);
        while !self.account_original_ended(old) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        replacement.committed = true;
        if !self.account_original_ended(old) {
            lock(&self.account_handoffs).insert(old, SessionHandoff {
                from: old, to: None, account_id: account_id.into(),
                error: Some(format!("Original process termination could not be confirmed; replacement {} was retained for explicit selection. Held input was not sent", new.0)),
            });
            self.save_account_handoffs().ok();
            return Err(Failure::Os(libc::ETIMEDOUT));
        }
        drop(replacement);
        // A later selection can arrive while retirement is in progress. The replacement
        // becomes eligible only after its guard is gone, so enqueue that latest selection again.
        let later = lock(&self.account_work.pending)
            .get(&provider)
            .map(|target| target.account_id.clone());
        if let Some(target) = later.filter(|target| target != account_id) {
            self.queue_account_switch(provider, &target);
        }
        Ok(new)
    }
}

struct Replacement<'a> {
    registry: &'a Registry,
    original: SessionId,
    session: SessionId,
    committed: bool,
}

impl Drop for Replacement<'_> {
    fn drop(&mut self) {
        self.registry.agents.finish_account_resume(self.session);
        lock(&self.registry.account_replacements).remove(&self.session);
        let only_remaining = !self.committed
            && self.registry.account_original_ended(self.original)
            && !self.registry.account_original_ended(self.session);
        if only_remaining {
            if let Some(handoff) = lock(&self.registry.account_handoffs).get_mut(&self.original) {
                handoff.to = None;
                handoff.error = Some(format!(
                    "Original session ended; unconfirmed replacement {} was retained. Select it explicitly to continue; held input was not sent",
                    self.session.0
                ));
            }
        } else if !self.committed {
            self.registry
                .close(
                    &Peer {
                        pid: None,
                        scope: Scope::Full,
                    },
                    self.session,
                )
                .ok();
        }
        if let Some(dir) = self.registry.state_dir() {
            std::fs::remove_file(dir.join(PREPARED_FILE)).ok();
        }
    }
}

// Validation errors constructed without an OS errno must retain their meaning on the wire.
fn prepare_failure(stage: &'static str, error: &std::io::Error) -> Failure {
    use std::io::ErrorKind;
    tracing::warn!(stage, kind = ?error.kind(), errno = error.raw_os_error(), "account handoff preparation failed");
    Failure::Os(error.raw_os_error().unwrap_or_else(|| match error.kind() {
        ErrorKind::TimedOut => libc::ETIMEDOUT,
        ErrorKind::NotFound => libc::ENOENT,
        ErrorKind::WouldBlock => libc::EAGAIN,
        ErrorKind::InvalidData => libc::EILSEQ,
        ErrorKind::InvalidInput => libc::EINVAL,
        ErrorKind::AlreadyExists => libc::EEXIST,
        ErrorKind::FileTooLarge => libc::EFBIG,
        ErrorKind::PermissionDenied => libc::EACCES,
        _ => libc::EIO,
    }))
}
