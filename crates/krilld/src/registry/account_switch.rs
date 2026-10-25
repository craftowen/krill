//! Resume live agents under the selected account without discarding their conversation.
//! Reference: Orca src/main/codex/codex-legacy-session-resume.ts and
//! src/renderer/src/store/terminals/terminal-restart-state.ts,
//! Copyright (c) 2026 Lovecast Inc., MIT License.

pub(super) mod identity;
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

fn is_prunable_failed_handoff(handoff: &SessionHandoff) -> bool {
    handoff.to.is_none()
        && handoff
            .error
            .as_deref()
            .is_some_and(|e| !e.contains("retained"))
}

fn foreground_vault_agent(
    root_pid: u32,
    fg_pid: Option<u32>,
) -> Option<(u32, krill_proto::agent_history::VaultAgent)> {
    let mut cur = fg_pid.filter(|&pid| pid > 0 && pid != root_pid)?;
    for _ in 0..16 {
        if let Ok(name) = crate::sys::process_name(cur)
            && let Some(parsed) = krill_proto::agent_history::VaultAgent::parse(name.trim())
        {
            return Some((cur, parsed));
        }
        let Ok(parent) = crate::sys::parent_pid(cur) else {
            break;
        };
        if parent <= 1 || parent == cur || parent == root_pid {
            break;
        }
        cur = parent;
    }
    None
}

impl Registry {
    fn provider_for_profile(&self, profile_id: &str) -> Option<ProviderKind> {
        let agents = self.agents.agents().ok()?;
        let (_, profile) = agents.resolve(profile_id)?;
        profile
            .program()
            .and_then(|p| Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .and_then(ProviderKind::parse)
    }

    fn discover_live_identity(
        &self,
        provider: ProviderKind,
        home: &Path,
        stored: Option<&crate::agent::control::StoredLaunch>,
        root_pid: u32,
        fg_pid: Option<u32>,
    ) -> std::io::Result<Option<identity::Identity>> {
        let mut observed = identity::discover(provider, root_pid, fg_pid)?;
        if observed.is_none() && provider == ProviderKind::Claude {
            let state_dir = self.state_dir();
            let file = accounts::load_file(home, state_dir.as_deref());
            let mut claude_homes: Vec<_> = file
                .accounts
                .iter()
                .filter(|a| ProviderKind::parse(&a.provider) == Some(ProviderKind::Claude))
                .map(|a| a.config_dir().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect();
            claude_homes.push(accounts::host_default_dir(home, ProviderKind::Claude));
            if let Some(account) = stored
                .and_then(|s| s.account.as_ref())
                .filter(|a| !a.dir.is_empty())
            {
                claude_homes.push(std::path::PathBuf::from(&account.dir));
            }
            claude_homes.sort();
            claude_homes.dedup();
            observed = identity::discover_claude(&claude_homes, root_pid, fg_pid)?;
        }
        Ok(observed)
    }

    fn live_agent_for_switch_in(
        &self,
        session: SessionId,
        home: &Path,
    ) -> Option<crate::agent::control::StoredLaunch> {
        if let Some(mut stored) = self.agents.stored(session).ok().flatten() {
            let mut use_stored = true;
            if stored.account.is_none() && stored.hibernation_blocked {
                let proc_state = {
                    let state = lock(&self.state);
                    state
                        .sessions
                        .get(&session)
                        .map(|e| (e.pid, e.session.tap().foreground()))
                };
                if let Some((root_pid, Some(fg))) = proc_state {
                    let root_is_agent = crate::sys::process_name(root_pid)
                        .ok()
                        .and_then(|n| krill_proto::agent_history::VaultAgent::parse(n.trim()))
                        .is_some();
                    if !root_is_agent {
                        if fg == root_pid {
                            return None;
                        }
                        match foreground_vault_agent(root_pid, Some(fg)) {
                            Some((_, live_agent)) if live_agent.profile() != stored.profile => {
                                use_stored = false;
                            }
                            Some(_) => {
                                if let Some(provider) = self.provider_for_profile(&stored.profile)
                                    && let Ok(Some(found)) = self.discover_live_identity(
                                        provider,
                                        home,
                                        Some(&stored),
                                        root_pid,
                                        Some(fg),
                                    )
                                    && stored.session_id.as_deref() != Some(found.id.as_str())
                                {
                                    stored.session_id = Some(found.id);
                                    stored.transcript_path = found.path.to_str().map(str::to_owned);
                                }
                            }
                            None if crate::sys::process_name(fg).is_ok() => {
                                return None;
                            }
                            None => {}
                        }
                    }
                }
            }
            if use_stored {
                return Some(stored);
            }
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
        let root_agent = crate::sys::process_name(root_pid)
            .ok()
            .and_then(|n| krill_proto::agent_history::VaultAgent::parse(n.trim()))
            .map(|a| (root_pid, a));
        if fg_pid == Some(root_pid) && root_agent.is_none() {
            return None;
        }
        let fg_agent = foreground_vault_agent(root_pid, fg_pid).or(root_agent);
        if let Some(fg) = fg_pid
            && fg_agent.is_none()
            && crate::sys::process_name(fg).is_ok()
        {
            return None;
        }
        let agent = fg_agent.map(|(_, a)| a).or_else(|| {
            let status_agent = self.status.get(session).agent;
            krill_proto::agent_history::VaultAgent::parse(&status_agent)
        })?;
        let provider = self.provider_for_profile(agent.profile())?;
        let found = self
            .discover_live_identity(provider, home, None, root_pid, fg_pid)
            .ok()??;
        let cwd = fg_agent
            .and_then(|(pid, _)| crate::sys::current_directory(pid).ok())
            .map(|p| p.into_os_string().as_encoded_bytes().to_vec())
            .filter(|c| !c.is_empty())
            .unwrap_or(entry_cwd);
        Some(crate::agent::control::StoredLaunch {
            profile: agent.profile().to_owned(),
            permission: krill_proto::agent::Permission::Manual,
            session_id: Some(found.id),
            transcript_path: found.path.to_str().map(str::to_owned),
            cwd,
            hibernation_blocked: true,
            account: None,
        })
    }

    pub(super) fn queue_account_switch(&self, provider: ProviderKind, account_id: &str) {
        let mut pending = lock(&self.account_work.pending);
        self.prepare_account_switch_in(provider, account_id, &accounts::default_home(), false);
        pending.insert(
            provider,
            crate::usage::accounts::switch_work::SwitchTarget {
                account_id: account_id.into(),
                prepared: true,
                not_before: None,
            },
        );
        self.account_work.wake.notify_one();
    }

    fn prepare_account_switch_in(
        &self,
        provider: ProviderKind,
        account_id: &str,
        home: &Path,
        automatic: bool,
    ) {
        let sessions: Vec<_> = {
            let state = lock(&self.state);
            state
                .sessions
                .iter()
                .filter(|(id, _)| !state.exited.contains_key(id) && !state.closing.contains(id))
                .map(|(id, entry)| (*id, entry.pid, entry.session.tap().foreground()))
                .collect()
        };
        let Ok(agents) = self.agents.agents() else {
            return;
        };
        let state_dir = self.state_dir();
        let mut found = false;
        let mut cancelled = false;
        for (from, root_pid, fg_pid) in sessions {
            // A later click must not enqueue a replacement that is still being prepared.
            if lock(&self.account_replacements).contains(&from) {
                continue;
            }
            let Some(stored) = self.live_agent_for_switch_in(from, home) else {
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
            let current =
                crate::agent::account_for_stored_in(home, profile, state_dir.as_deref(), &stored);
            if current.as_ref().is_some_and(|a| a.id == account_id) {
                let mut handoffs = lock(&self.account_handoffs);
                if handoffs.get(&from).is_some_and(|h| h.to.is_none()) {
                    handoffs.remove(&from);
                    cancelled = true;
                }
                continue;
            }
            if stored.session_id.is_some() || stored.transcript_path.is_some() {
                lock(&self.account_switch_opt_out).remove(&from);
            } else {
                let opted_out = lock(&self.account_switch_opt_out).contains(&from);
                if opted_out || automatic {
                    let has_live_identity = self
                        .discover_live_identity(provider, home, Some(&stored), root_pid, fg_pid)
                        .ok()
                        .flatten()
                        .is_some();
                    if has_live_identity {
                        lock(&self.account_switch_opt_out).remove(&from);
                    } else if opted_out {
                        continue;
                    } else {
                        let recently_launched =
                            self.agents.launched_at(from).is_some_and(|launched| {
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .is_ok_and(|d| d.as_secs().saturating_sub(launched) < 5)
                            });
                        if !recently_launched {
                            continue;
                        }
                    }
                }
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

    pub(super) fn forget_failed_account_handoff(&self, session: SessionId) {
        lock(&self.account_switch_opt_out).remove(&session);
        let removed = {
            let mut handoffs = lock(&self.account_handoffs);
            if handoffs
                .get(&session)
                .is_some_and(is_prunable_failed_handoff)
            {
                handoffs.remove(&session);
                true
            } else {
                false
            }
        };
        if removed {
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
        drop(handoffs);
        if self
            .agents
            .stored(session)
            .ok()
            .flatten()
            .is_none_or(|s| s.session_id.is_none() && s.transcript_path.is_none())
        {
            lock(&self.account_switch_opt_out).insert(session);
        }
        Ok(())
    }

    pub(super) fn account_switch_snapshot(&self, snapshot: &mut AccountsSnapshot) {
        let handoffs = lock(&self.account_handoffs);
        let replacements = lock(&self.account_replacements);
        let state = lock(&self.state);
        let has_holder = self.holder.is_some();
        snapshot.details.handoffs = handoffs
            .values()
            .filter(|handoff| {
                !has_holder
                    || !is_prunable_failed_handoff(handoff)
                    || (state.sessions.contains_key(&handoff.from)
                        && !state.exited.contains_key(&handoff.from))
            })
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
        let wait = pending
            .values()
            .map(|target| {
                target.not_before.map_or(Duration::ZERO, |at| {
                    at.saturating_duration_since(Instant::now())
                })
            })
            .min()
            .unwrap_or(timeout)
            .min(timeout);
        if !wait.is_zero() {
            drop(
                self.account_work
                    .wake
                    .wait_timeout(pending, wait)
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
            let ready: Vec<_> = pending
                .iter()
                .filter(|(_, target)| target.not_before.is_none_or(|at| Instant::now() >= at))
                .map(|(provider, _)| *provider)
                .collect();
            let mut work = BTreeMap::new();
            for provider in ready {
                if let Some(target) = pending.remove(&provider) {
                    if !target.prepared {
                        self.prepare_account_switch_in(provider, &target.account_id, home, true);
                    }
                    work.insert(provider, target);
                }
            }
            work
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
                if result == Err(Failure::Os(libc::EBUSY)) {
                    // SQLite is still writing the original conversation. Keep the handoff
                    // pending and retry after it closes; a later selection always wins.
                    lock(&self.account_work.pending)
                        .entry(provider)
                        .or_insert_with(|| crate::usage::accounts::switch_work::SwitchTarget {
                            account_id: account_id.clone(),
                            prepared: true,
                            not_before: Some(Instant::now() + Duration::from_millis(500)),
                        });
                    continue;
                }
                let superseded = lock(&self.account_work.pending)
                    .get(&provider)
                    .is_some_and(|target| target.account_id != account_id);
                let mut handoffs = lock(&self.account_handoffs);
                if superseded && result.is_err() {
                    if handoffs
                        .get(&old)
                        .is_some_and(|h| h.account_id == account_id && h.to.is_none())
                    {
                        handoffs.remove(&old);
                    }
                } else if let Some(handoff) = handoffs
                    .get_mut(&old)
                    .filter(|h| h.account_id == account_id)
                {
                    match result {
                        Ok(new) => handoff.to = Some(new),
                        Err(error) => {
                            let reason = match error {
                                Failure::Denied => "selected account login or credential scope could not be verified".into(),
                                Failure::Os(libc::EAGAIN) => "the running conversation could not be identified from its hooks or open transcript".into(),
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
                if is_prunable_failed_handoff(&handoff) {
                    lock(&self.account_handoffs).remove(&handoff.from);
                } else if handoff.to.is_none() && handoff.error.is_none() {
                    handoff.error =
                        Some("Original session has ended; automatic resume cancelled".into());
                    lock(&self.account_handoffs).insert(handoff.from, handoff);
                }
                continue;
            }
            if handoff.error.is_some() {
                if is_prunable_failed_handoff(&handoff)
                    && self.live_agent_for_switch_in(handoff.from, home).is_none()
                {
                    lock(&self.account_handoffs).remove(&handoff.from);
                }
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
                        prepared: true,
                        not_before: None,
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
        // v0.1.37 stored one journal per replacement; earlier versions used one file.
        // Recover both layouts so upgrading never leaves an unconfirmed duplicate behind.
        let mut paths = vec![dir.join(PREPARED_FILE)];
        if let Ok(entries) = std::fs::read_dir(dir.join("account-replacements")) {
            paths.extend(
                entries
                    .flatten()
                    .take(krill_proto::holder::MAX_SESSIONS)
                    .filter_map(|entry| {
                        let path = entry.path();
                        (entry.file_type().is_ok_and(|kind| kind.is_file())
                            && path
                                .extension()
                                .is_some_and(|extension| extension == "json"))
                        .then_some(path)
                    }),
            );
        }
        for path in paths {
            self.recover_prepared_record(&path);
        }
    }

    fn recover_prepared_record(&self, path: &Path) {
        let prepared = std::fs::read(path)
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
        let mut stored = self
            .live_agent_for_switch_in(old, home)
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
        let (cwd, size, old_pid, old_fg): (
            Vec<u8>,
            krill_proto::holder::WinSize,
            u32,
            Option<u32>,
        ) = {
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
                entry.pid,
                entry.session.tap().foreground(),
            )
        };
        // Initial hooks can arrive while login validation or a first prompt is running.
        // Recover from the exact file held by this pane when hook trust was missing.
        let mut claude_homes: Vec<_> = snapshot
            .accounts
            .iter()
            .filter(|a| a.provider == ProviderKind::Claude)
            .map(|a| std::path::PathBuf::from(&a.profile_dir))
            .collect();
        claude_homes.push(accounts::host_default_dir(home, ProviderKind::Claude));
        if let Some(account) = stored.account.as_ref().filter(|a| !a.dir.is_empty()) {
            claude_homes.push(std::path::PathBuf::from(&account.dir));
        }
        claude_homes.sort();
        claude_homes.dedup();
        let binding_deadline = Instant::now() + Duration::from_secs(2);
        let mut transcript_home = None;
        loop {
            if let Some(current) = self.agents.stored(old).map_err(|e| super::os(&e))? {
                stored = current;
            }
            let foreground = lock(&self.state)
                .sessions
                .get(&old)
                .and_then(|e| e.session.tap().foreground())
                .or(old_fg);
            let mut observed = identity::discover(provider, old_pid, foreground)
                .map_err(|e| prepare_failure("process_identity", &e))?;
            if observed.is_none() && provider == ProviderKind::Claude {
                observed = identity::discover_claude(&claude_homes, old_pid, foreground)
                    .map_err(|e| prepare_failure("process_identity", &e))?;
            }
            if let Some(found) = observed {
                // Old versions could persist a display-only cwd guess. A live process
                // observation must agree even when a record already contains an ID.
                if (stored.session_id.is_some() || stored.transcript_path.is_some())
                    && transcript::session_id(provider, &stored)
                        .map_err(|e| prepare_failure("process_identity", &e))?
                        != found.id
                {
                    return Err(Failure::Os(libc::EILSEQ));
                }
                stored.session_id = Some(found.id);
                stored.transcript_path = found.path.to_str().map(str::to_owned);
                transcript_home = Some(found.home);
                break;
            }
            if stored.session_id.is_some() || stored.transcript_path.is_some() {
                break;
            }
            if self.account_original_ended(old) {
                return Err(Failure::NoSession);
            }
            if Instant::now() >= binding_deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // A project path never identifies a conversation. Only launch/resume/hooks or
        // files held by this process can supply the identity used for retirement.
        let session_id = transcript::session_id(provider, &stored)
            .map_err(|error| prepare_failure("transcript_preparation", &error))?;
        stored.session_id = Some(session_id.clone());
        let bound_path = stored.transcript_path.clone();
        stored = self
            .agents
            .bind_switch_identity(old, &stored)
            .map_err(|e| prepare_failure("identity_binding", &e))?;
        let source = transcript_home
            .map(|home| crate::agent::control::LaunchAccount {
                id: String::new(),
                env_key: provider.env_var().into(),
                dir: home.to_string_lossy().into_owned(),
            })
            .or_else(|| {
                crate::agent::account_for_stored_in(home, profile, state_dir.as_deref(), &stored)
            })
            .unwrap_or_else(|| crate::agent::control::LaunchAccount {
                id: accounts::host_default_id(provider),
                env_key: provider.env_var().into(),
                dir: String::new(),
            });
        if !target.host_default && matches!(provider, ProviderKind::Claude | ProviderKind::Codex) {
            // Establish a new profile's shared history before bridging a file would
            // create a private history directory and force a later migration.
            crate::usage::provision::provision_if_enabled(
                home,
                state_dir.as_deref(),
                provider,
                &target.profile_dir,
            );
        }
        let prepared_transcript = transcript::prepare_bound(
            provider,
            home,
            &source,
            target,
            &session_id,
            &cwd,
            bound_path.as_deref(),
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
        let (tap, new_pid) = {
            let state = lock(&self.state);
            let entry = state.sessions.get(&new).ok_or(Failure::NoSession)?;
            (entry.session.tap(), entry.pid)
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut ready = false;
        let mut next_identity_probe = Instant::now();
        while Instant::now() < deadline {
            if lock(&self.state).exited.contains_key(&new) || tap.pulse().closed {
                self.close(peer, new).ok();
                return Err(Failure::BadRequest);
            }
            if !self.agents.account_resume_observed(new, &session_id)
                && Instant::now() >= next_identity_probe
            {
                next_identity_probe = Instant::now() + Duration::from_millis(200);
                let mut observed = identity::discover(provider, new_pid, tap.foreground())
                    .map_err(|e| prepare_failure("resume_identity", &e))?;
                if observed.is_none() && provider == ProviderKind::Claude {
                    observed = identity::discover_claude(&claude_homes, new_pid, tap.foreground())
                        .map_err(|e| prepare_failure("resume_identity", &e))?;
                }
                let observed = observed
                    .filter(|found| {
                        if provider != ProviderKind::Antigravity {
                            return true;
                        }
                        let expected = if target.host_default {
                            std::path::PathBuf::from(&target.profile_dir)
                        } else {
                            Path::new(&target.profile_dir).join(".gemini/antigravity-cli")
                        };
                        found
                            .home
                            .canonicalize()
                            .ok()
                            .zip(expected.canonicalize().ok())
                            .is_some_and(|(actual, expected)| actual == expected)
                    })
                    .map(|found| (found.id, Some(found.path)));
                if let Some((sid, path)) = observed
                    && sid == session_id
                {
                    self.agents
                        .bind_discovered_session(new, &sid, path.as_deref());
                    self.agents.observe_account_resume(new, &session_id);
                }
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
        let should_continue = self.agents.take_rate_limited(old) || {
            let st = self.status.get(old);
            st.state == krill_proto::control::AgentState::Working
                && st.source == Some(krill_proto::control::Source::Hook)
        };
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
        if should_continue
            && let Err(error) =
                Self::deliver_input_to_tap(&tap, b"Continue from where you left off.", true)
        {
            tracing::warn!(
                session = new.0,
                from = old.0,
                error = %error,
                "failed to send continuation prompt after account switch"
            );
        }
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
        ErrorKind::InvalidData | ErrorKind::Unsupported => libc::EILSEQ,
        ErrorKind::InvalidInput => libc::EINVAL,
        ErrorKind::AlreadyExists => libc::EEXIST,
        ErrorKind::FileTooLarge => libc::EFBIG,
        ErrorKind::PermissionDenied => libc::EACCES,
        _ => libc::EIO,
    }))
}
