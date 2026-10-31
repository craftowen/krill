//! Resume live agents under the selected account without discarding their conversation.
//! Reference: Orca src/main/codex/codex-legacy-session-resume.ts and
//! src/renderer/src/store/terminals/terminal-restart-state.ts,
//! Copyright (c) 2026 Lovecast Inc., MIT License.

pub(super) mod identity;
mod input_recovery;
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
use accounts::switch_work::SwitchScope;

const MAX_HANDOFF_WORKERS: usize = 4;

const FILE: &str = "account-handoffs.json";
const PREPARED_FILE: &str = "account-replacement.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct Prepared {
    from: SessionId,
    to: SessionId,
    account_id: String,
}

#[derive(Default)]
struct RecoveryFence {
    sessions: std::collections::BTreeSet<SessionId>,
    unidentified: bool,
}

impl RecoveryFence {
    fn blocks(&self, session: SessionId) -> bool {
        self.unidentified || self.sessions.contains(&session)
    }
}

fn prepared_paths(dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut paths = vec![dir.join(PREPARED_FILE)];
    let entries = match std::fs::read_dir(dir.join("account-replacements")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(paths),
        Err(error) => return Err(error),
    };
    for (index, entry) in entries.enumerate() {
        if index >= krill_proto::holder::MAX_SESSIONS {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn read_prepared(path: &Path) -> std::io::Result<Option<Prepared>> {
    use std::io::Read as _;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() > 4096 {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // Recovery holds the handoff lock; a replaced FIFO must never stall it.
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 4096 {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut raw = Vec::new();
    file.take(4097).read_to_end(&mut raw)?;
    let prepared: Prepared = serde_json::from_slice(&raw)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
    if raw.len() > 4096 || prepared.from == prepared.to {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok(Some(prepared))
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
    fn live_agent_for_switch(
        &self,
        session: SessionId,
    ) -> Option<crate::agent::control::StoredLaunch> {
        self.live_agent_for_switch_in(session, &accounts::default_home())
    }

    fn switch_provider(
        &self,
        stored: &crate::agent::control::StoredLaunch,
    ) -> Option<ProviderKind> {
        let agents = self.agents.agents().ok()?;
        let (_, profile) = agents.resolve(&stored.profile)?;
        profile
            .program()
            .and_then(|p| Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .and_then(ProviderKind::parse)
    }

    fn queue_session_account_switch_locked(
        &self,
        session: SessionId,
        provider: ProviderKind,
        account_id: &str,
        snapshot: &AccountsSnapshot,
        pending: &mut BTreeMap<SwitchScope, accounts::switch_work::SwitchTarget>,
    ) -> Result<(), Failure> {
        // A sidebar click can still name the original while retirement is being observed.
        // Follow only durable aliases; an unconfirmed replacement is never a valid target.
        let mut session = session;
        // Retirement can be marked just before close sets state.closing. Persist a
        // follow-up against the live confirmed replacement during that window too.
        if self.agents.account_binding_retiring(session) {
            let next = lock(&self.account_handoffs)
                .get(&session)
                .and_then(|h| h.to);
            if let Some(next) = next.filter(|next| {
                let state = lock(&self.state);
                state.sessions.contains_key(next)
                    && !state.exited.contains_key(next)
                    && !state.closing.contains(next)
            }) {
                session = next;
            }
        }
        for _ in 0..krill_proto::holder::MAX_SESSIONS {
            let retired = {
                let state = lock(&self.state);
                !state.sessions.contains_key(&session)
                    || state.exited.contains_key(&session)
                    || state.closing.contains(&session)
            };
            if !retired {
                break;
            }
            let next = lock(&self.account_handoffs)
                .get(&session)
                .and_then(|h| h.to);
            let Some(next) = next.filter(|next| *next != session) else {
                return Err(Failure::NoSession);
            };
            session = next;
        }
        {
            let state = lock(&self.state);
            if !state.sessions.contains_key(&session)
                || state.exited.contains_key(&session)
                || state.closing.contains(&session)
            {
                return Err(Failure::NoSession);
            }
        }
        let committed_replacement = lock(&self.account_handoffs)
            .values()
            .any(|h| h.to == Some(session));
        if lock(&self.account_replacements).contains(&session) && !committed_replacement {
            return Err(Failure::BadRequest);
        }
        let stored = self
            .live_agent_for_switch(session)
            .ok_or(Failure::UnknownAgent)?;
        if self.switch_provider(&stored) != Some(provider) {
            return Err(Failure::BadRequest);
        }
        let target = snapshot
            .accounts
            .iter()
            .find(|a| a.provider == provider && a.id == account_id)
            .ok_or(Failure::BadRequest)?;
        if !target.host_default && !Path::new(&target.profile_dir).is_dir() {
            return Err(Failure::BadRequest);
        }
        if self.agents.account_binding_retiring(session) {
            // Retirement has committed its durable alias. Preserve it while a later
            // selection waits to be applied to the confirmed replacement.
            pending.insert(
                SwitchScope::Session(session, provider),
                accounts::switch_work::SwitchTarget {
                    account_id: account_id.into(),
                    prepared: true,
                    not_before: None,
                },
            );
            self.account_work.wake.notify_one();
            return Ok(());
        }
        let mut handoffs = lock(&self.account_handoffs);
        let previous = handoffs.get(&session).cloned();
        if stored.account.as_ref().is_some_and(|a| a.id == account_id) {
            handoffs.remove(&session);
        } else {
            handoffs.insert(
                session,
                SessionHandoff {
                    from: session,
                    to: None,
                    account_id: account_id.into(),
                    error: None,
                },
            );
        }
        if let Err(error) = self.write_account_handoffs(&handoffs) {
            if let Some(previous) = previous {
                handoffs.insert(session, previous);
            } else {
                handoffs.remove(&session);
            }
            return Err(super::os(&error));
        }
        pending.insert(
            SwitchScope::Session(session, provider),
            accounts::switch_work::SwitchTarget {
                account_id: account_id.into(),
                prepared: true,
                not_before: None,
            },
        );
        drop(handoffs);
        self.account_work.wake.notify_one();
        Ok(())
    }

    pub(super) fn queue_session_account_switch(
        &self,
        session: SessionId,
        provider: ProviderKind,
        account_id: &str,
        snapshot: &AccountsSnapshot,
    ) -> Result<(), Failure> {
        let mut pending = lock(&self.account_work.pending);
        self.queue_session_account_switch_locked(
            session,
            provider,
            account_id,
            snapshot,
            &mut pending,
        )
    }

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

    /// 셸에서 직접 띄운 에이전트는 Krill이 권한을 모르므로 Manual로 기록된다.
    /// 살아 있는 프로세스 argv에 프로필의 `yolo_args`가 모두 있으면 YOLO로 본다.
    fn live_agent_has_yolo_args(&self, session: SessionId, profile_id: &str) -> bool {
        let Some(yolo) = self
            .agents
            .agents()
            .ok()
            .and_then(|a| a.resolve(profile_id).map(|(_, p)| p.yolo_args.clone()))
            .filter(|y| !y.is_empty())
        else {
            return false;
        };
        let Some((root_pid, fg_pid)) = lock(&self.state)
            .sessions
            .get(&session)
            .map(|e| (e.pid, e.session.tap().foreground()))
        else {
            return false;
        };
        let pid = foreground_vault_agent(root_pid, fg_pid).map_or(root_pid, |(pid, _)| pid);
        crate::sys::process_args(pid).is_ok_and(|args| yolo.iter().all(|y| args.contains(y)))
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
        pending.retain(
            |scope, _| !matches!(scope, SwitchScope::Session(_, selected) if *selected == provider),
        );
        self.prepare_account_switch_in(provider, account_id, &accounts::default_home(), false);
        pending.insert(
            SwitchScope::Provider(provider),
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
        self.prepare_account_switch_excluding(
            provider,
            account_id,
            home,
            automatic,
            &std::collections::BTreeSet::new(),
        );
    }

    fn prepare_account_switch_excluding(
        &self,
        provider: ProviderKind,
        account_id: &str,
        home: &Path,
        automatic: bool,
        excluded: &std::collections::BTreeSet<SessionId>,
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
            if excluded.contains(&from) {
                continue;
            }
            // A later click must not enqueue a replacement that is still being prepared.
            if lock(&self.account_replacements).contains(&from)
                || self.agents.account_binding_retiring(from)
            {
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
                let mut handoffs = lock(&self.account_handoffs);
                if handoffs
                    .get(&from)
                    .is_some_and(|handoff| handoff.to.is_none() && handoff.account_id == account_id)
                {
                    handoffs.remove(&from);
                    cancelled = true;
                }
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
        self.cancel_account_switch(session, None)
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
    pub fn process_account_work(self: &std::sync::Arc<Self>) {
        self.process_account_work_in(&accounts::default_home());
    }

    fn process_account_work_in(self: &std::sync::Arc<Self>, home: &Path) {
        use std::sync::atomic::Ordering;
        if *lock(&self.upgrading)
            || self
                .account_processing
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let _processing = Processing(&self.account_processing);
        self.recover_account_work(home);
        let pending = {
            let mut pending = lock(&self.account_work.pending);
            let ready: Vec<_> = pending
                .iter()
                .filter(|(_, target)| target.not_before.is_none_or(|at| Instant::now() >= at))
                .map(|(scope, _)| *scope)
                .collect();
            let excluded = pending
                .keys()
                .filter_map(|scope| match scope {
                    SwitchScope::Session(session, _) => Some(*session),
                    SwitchScope::Provider(_) => None,
                })
                .collect();
            let mut work = BTreeMap::new();
            for scope in ready {
                if let Some(target) = pending.remove(&scope) {
                    if !target.prepared
                        && let SwitchScope::Provider(provider) = scope
                    {
                        self.prepare_account_switch_excluding(
                            provider,
                            &target.account_id,
                            home,
                            true,
                            &excluded,
                        );
                    }
                    work.insert(scope, target);
                }
            }
            work
        };
        let handoffs: Vec<_> = lock(&self.account_handoffs)
            .values()
            .filter(|h| h.to.is_none() && h.error.is_none())
            .cloned()
            .collect();
        let jobs: Vec<_> = handoffs
            .into_iter()
            .filter_map(|h| {
                let stored = self.live_agent_for_switch_in(h.from, home)?;
                let provider = self.switch_provider(&stored)?;
                // Recovery and explicit requests are always scoped to the saved original.
                pending
                    .keys()
                    .any(|scope| match scope {
                        SwitchScope::Provider(p) => *p == provider,
                        SwitchScope::Session(id, p) => *id == h.from && *p == provider,
                    })
                    .then_some((h.from, provider, h.account_id))
            })
            .collect();
        run_bounded(&jobs, |(old, provider, account_id)| {
            self.process_account_handoff(*old, *provider, account_id, home);
        });
    }

    fn process_account_handoff(
        self: &std::sync::Arc<Self>,
        old: SessionId,
        provider: ProviderKind,
        account_id: &str,
        home: &Path,
    ) {
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.handoff_account(&peer, old, provider, account_id, home)
        }))
        .unwrap_or(Err(Failure::BadRequest));
        if result == Err(Failure::Os(libc::EBUSY)) {
            let mut pending = lock(&self.account_work.pending);
            if !self.account_selection_current(old, provider, account_id, &pending) {
                return;
            }
            pending
                .entry(SwitchScope::Session(old, provider))
                .or_insert_with(|| accounts::switch_work::SwitchTarget {
                    account_id: account_id.to_owned(),
                    prepared: true,
                    not_before: Some(Instant::now() + Duration::from_millis(500)),
                });
            return;
        }
        // A repeated selection made during CLI IO is a new retry. Do not let this
        // older attempt overwrite its durable pending record with a failure.
        let selection = lock(&self.account_work.pending);
        let retry_queued = selection
            .get(&SwitchScope::Session(old, provider))
            .or_else(|| selection.get(&SwitchScope::Provider(provider)))
            .is_some_and(|target| target.account_id == account_id);
        let mut handoffs = lock(&self.account_handoffs);
        if let Some(handoff) = handoffs.get_mut(&old).filter(|h| {
            h.account_id == account_id && h.to.is_none() && !(result.is_err() && retry_queued)
        }) {
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
        drop(selection);
    }

    fn account_selection_current(
        &self,
        old: SessionId,
        provider: ProviderKind,
        account_id: &str,
        pending: &BTreeMap<SwitchScope, accounts::switch_work::SwitchTarget>,
    ) -> bool {
        let desired = pending
            .get(&SwitchScope::Session(old, provider))
            .or_else(|| pending.get(&SwitchScope::Provider(provider)));
        desired.is_none_or(|target| target.account_id == account_id)
            && lock(&self.account_handoffs)
                .get(&old)
                .is_some_and(|h| h.account_id == account_id && h.to.is_none() && h.error.is_none())
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
        let recovery = self.recover_prepared_account();
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
            } else if recovery.blocks(handoff.from) {
                handoff.error =
                    Some("Unconfirmed replacement recovery is pending; retry withheld".into());
                lock(&self.account_handoffs).insert(handoff.from, handoff);
            } else if let Some(account) = snapshot
                .accounts
                .iter()
                .find(|a| a.id == handoff.account_id)
            {
                lock(&self.account_work.pending)
                    .entry(SwitchScope::Session(handoff.from, account.provider))
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

    fn recover_prepared_account(&self) -> RecoveryFence {
        let mut fence = RecoveryFence::default();
        let Some(dir) = self.state_dir() else {
            return fence;
        };
        let paths = match prepared_paths(&dir) {
            Ok(paths) => paths,
            Err(error) => {
                tracing::warn!(kind = ?error.kind(), "replacement inventory is incomplete; retries withheld");
                fence.unidentified = true;
                return fence;
            }
        };
        for path in paths {
            match read_prepared(&path) {
                Ok(None) => {}
                Ok(Some(prepared)) => {
                    if !self.recover_prepared_record(&path, &prepared) {
                        fence.sessions.extend([prepared.from, prepared.to]);
                    }
                }
                Err(error) => {
                    tracing::warn!(kind = ?error.kind(), "replacement journal is unreadable; retries withheld");
                    fence.unidentified = true;
                }
            }
        }
        fence
    }

    fn ensure_no_unresolved_account_replacement(&self, session: SessionId) -> Result<(), Failure> {
        let Some(dir) = self.state_dir() else {
            return Ok(());
        };
        let paths = prepared_paths(&dir).map_err(|_| Failure::BadRequest)?;
        for path in paths {
            if let Some(prepared) = read_prepared(&path).map_err(|_| Failure::BadRequest)?
                && (prepared.from == session || prepared.to == session)
            {
                return Err(Failure::BadRequest);
            }
        }
        Ok(())
    }

    fn recover_prepared_record(&self, path: &Path, prepared: &Prepared) -> bool {
        // Only a complete holder inventory makes absence authoritative on recovery.
        if self.holder.is_none() {
            return false;
        }
        let committed = lock(&self.account_handoffs)
            .get(&prepared.from)
            .is_some_and(|h| h.to == Some(prepared.to));
        let (Ok(source), Ok(target)) = (
            self.agents.stored(prepared.from),
            self.agents.stored(prepared.to),
        ) else {
            return false;
        };
        if self.account_original_ended(prepared.to) {
            return std::fs::remove_file(path).is_ok();
        }
        if committed {
            return self.account_original_ended(prepared.from)
                && std::fs::remove_file(path).is_ok();
        }
        let owned = target.is_some_and(|a| {
            a.account.is_some_and(|a| a.id == prepared.account_id)
                && source.as_ref().is_some_and(|s| {
                    s.profile == a.profile
                        && (s.session_id.is_none() || s.session_id == a.session_id)
                })
        });
        if !owned {
            return false;
        }
        if self.account_original_ended(prepared.from) {
            // Retain the only remaining PTY. Persist the decision before removing evidence.
            lock(&self.account_handoffs).insert(prepared.from, SessionHandoff {
                from: prepared.from, to: None, account_id: prepared.account_id.clone(),
                error: Some(format!("Original session ended during recovery; unconfirmed replacement {} was retained. Select it explicitly to continue; held input was not sent", prepared.to.0)),
            });
            return self.save_account_handoffs().is_ok() && std::fs::remove_file(path).is_ok();
        }
        if lock(&self.state).closing.contains(&prepared.from) {
            return false;
        }
        // Done acknowledges Terminate; it does not prove that Exited was observed.
        self.stop_account_replacement(prepared.to) && std::fs::remove_file(path).is_ok()
    }

    fn stop_account_replacement(&self, session: SessionId) -> bool {
        if self.account_original_ended(session) {
            return true;
        }
        let closing = lock(&self.state).closing.contains(&session);
        if !closing
            && self
                .close(
                    &Peer {
                        pid: None,
                        scope: Scope::Full,
                    },
                    session,
                )
                .is_err()
        {
            return false;
        }
        let deadline = Instant::now() + Duration::from_secs(7);
        while !self.account_original_ended(session) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.account_original_ended(session)
    }

    fn account_original_ended(&self, session: SessionId) -> bool {
        let state = lock(&self.state);
        state.exited.contains_key(&session)
            || (!state.sessions.contains_key(&session)
                && !state.unregistered_kept.contains(&session))
    }

    fn transfer_account_selection(
        &self,
        old: SessionId,
        new: SessionId,
        provider: ProviderKind,
        current_account: &str,
        snapshot: impl FnOnce() -> AccountsSnapshot,
    ) -> Result<(), Failure> {
        let old_scope = SwitchScope::Session(old, provider);
        let new_scope = SwitchScope::Session(new, provider);
        let provider_scope = SwitchScope::Provider(provider);
        {
            let mut pending = lock(&self.account_work.pending);
            if pending.contains_key(&new_scope) {
                pending.remove(&old_scope);
                return Ok(());
            }
            if !pending.contains_key(&old_scope) && !pending.contains_key(&provider_scope) {
                return Ok(());
            }
        }
        // Account discovery may perform CLI IO. Refresh before the atomic recheck and
        // transfer; requests arriving during discovery take precedence below.
        let snapshot = snapshot();
        let mut pending = lock(&self.account_work.pending);
        let old_target = pending.remove(&old_scope);
        if pending.contains_key(&new_scope) {
            return Ok(());
        }
        let target = old_target
            .as_ref()
            .or_else(|| pending.get(&provider_scope))
            .map(|target| target.account_id.clone());
        if let Some(target) = target.filter(|target| target != current_account) {
            let result = self.queue_session_account_switch_locked(
                new,
                provider,
                &target,
                &snapshot,
                &mut pending,
            );
            if result.is_err()
                && let Some(old_target) = old_target
            {
                pending.insert(old_scope, old_target);
            }
            return result;
        }
        Ok(())
    }

    fn handoff_account(
        self: &std::sync::Arc<Self>,
        peer: &Peer,
        old: SessionId,
        provider: ProviderKind,
        account_id: &str,
        home: &Path,
    ) -> Result<SessionId, Failure> {
        let _claim = self.claim_resume(old).ok_or(Failure::BadRequest)?;
        self.ensure_no_unresolved_account_replacement(old)?;
        if !self.account_selection_current(
            old,
            provider,
            account_id,
            &lock(&self.account_work.pending),
        ) {
            return Err(Failure::BadRequest);
        }
        if self.account_switch_busy(old) {
            return Err(Failure::Os(libc::EBUSY));
        }
        let holder = self.holder.as_ref().ok_or(Failure::NoHolder)?;
        let mut stored = self
            .live_agent_for_switch_in(old, home)
            .ok_or(Failure::UnknownAgent)?;
        if self.switch_provider(&stored) != Some(provider) {
            tracing::warn!(
                session = old.0,
                stage = "provider_recheck",
                "account handoff rejected a changed provider"
            );
            return Err(Failure::BadRequest);
        }
        if stored.permission == krill_proto::agent::Permission::Manual
            && self.live_agent_has_yolo_args(old, &stored.profile)
        {
            stored.permission = krill_proto::agent::Permission::Yolo;
        }
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
                let permission = stored.permission;
                stored = current;
                if permission == krill_proto::agent::Permission::Yolo {
                    stored.permission = permission;
                }
            }
            let foreground = lock(&self.state)
                .sessions
                .get(&old)
                .and_then(|e| e.session.tap().foreground())
                .or(old_fg);
            let mut observed = identity::discover(provider, old_pid, foreground)
                .map_err(|e| prepare_failure(old, "process_identity", &e))?;
            if observed.is_none() && provider == ProviderKind::Claude {
                observed = identity::discover_claude(&claude_homes, old_pid, foreground)
                    .map_err(|e| prepare_failure(old, "process_identity", &e))?;
            }
            if observed.is_none()
                && provider == ProviderKind::Antigravity
                && let Some((home, id, path)) =
                    crate::agent_history::vault::discover_live_agy_identity(old_pid, foreground)
                        .or_else(|| crate::agent_history::vault::cached_live_agy_identity(old))
            {
                observed = Some(identity::Identity { id, path, home });
            }
            if observed.is_none()
                && stored.session_id.is_none()
                && provider == ProviderKind::Claude
                && let Some(sid) = state_dir
                    .as_deref()
                    .and_then(|d| crate::agent::claude::session_id(d, old))
            {
                stored.session_id = Some(sid);
            }
            if let Some(found) = observed {
                // Old versions could persist a display-only cwd guess. A live process
                // observation must agree even when a record already contains an ID.
                if (stored.session_id.is_some() || stored.transcript_path.is_some())
                    && transcript::session_id(provider, &stored)
                        .map_err(|e| prepare_failure(old, "process_identity", &e))?
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
            .map_err(|error| prepare_failure(old, "transcript_preparation", &error))?;
        stored.session_id = Some(session_id.clone());
        let bound_path = stored.transcript_path.clone();
        stored = self
            .agents
            .bind_switch_identity(old, &stored)
            .map_err(|e| prepare_failure(old, "identity_binding", &e))?;
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
        .map_err(|error| prepare_failure(old, "transcript_preparation", &error))?;
        let new = self
            .reserve_session_id(Some(old))
            .map_err(|error| super::os(&error))?;
        lock(&self.account_replacements).insert(new);
        let mut replacement = Replacement {
            registry: self,
            original: old,
            session: new,
            committed: false,
        };
        self.agents.watch_account_resume(new);
        #[cfg(windows)]
        let codex_resume = (provider == ProviderKind::Codex)
            .then(|| {
                prepared_transcript
                    .path()
                    .and_then(|p| identity::CodexResume::new(p).ok())
            })
            .flatten();
        let (launch, sid, applied) = crate::agent::resume_in(
            profile,
            stored.permission,
            new,
            state_dir.as_deref(),
            Some(&session_id),
            account.as_ref(),
        )
        .map_err(|e| prepare_failure(old, "resume_launch", &e))?;
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
            .map_err(|e| prepare_failure(old, "replacement_identity", &e))?;
        if let Some(dir) = state_dir.as_deref() {
            let raw = serde_json::to_vec(&Prepared {
                from: old,
                to: new,
                account_id: account_id.into(),
            })
            .map_err(|_| Failure::BadRequest)?;
            accounts::write_atomic_0600(&replacement_journal(dir, new), &raw)
                .map_err(|e| prepare_failure(old, "replacement_journal", &e))?;
        }
        let (new, _) = self.spawn_holder(
            holder,
            new,
            launch.argv,
            launch.env,
            cwd,
            size.cols,
            size.rows,
        )?;
        // A spawned CLI may still be at login or trust. Keep the original and held
        // input until this process has reopened the exact conversation.
        let (tap, new_pid) = {
            let state = lock(&self.state);
            let entry = state.sessions.get(&new).ok_or(Failure::NoSession)?;
            (entry.session.tap(), entry.pid)
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut ready = false;
        let mut confirmed_at = None;
        let mut next_identity_probe = Instant::now();
        while Instant::now() < deadline {
            if !self.account_selection_current(
                old,
                provider,
                &target.id,
                &lock(&self.account_work.pending),
            ) {
                break;
            }
            if lock(&self.state).exited.contains_key(&new) || tap.pulse().closed {
                self.close(peer, new).ok();
                return Err(Failure::BadRequest);
            }
            if !self.agents.account_resume_observed(new, &session_id)
                && Instant::now() >= next_identity_probe
            {
                next_identity_probe = Instant::now() + Duration::from_millis(200);
                let observed = resume_identity(|| {
                    let observed = identity::discover(provider, new_pid, tap.foreground())?;
                    #[cfg(windows)]
                    let observed = if observed.is_none() {
                        match &codex_resume {
                            Some(watch) => watch.observe(new_pid, tap.foreground())?,
                            None => observed,
                        }
                    } else {
                        observed
                    };
                    if observed.is_none() && provider == ProviderKind::Claude {
                        identity::discover_claude(&claude_homes, new_pid, tap.foreground())
                    } else {
                        Ok(observed)
                    }
                })
                .map_err(|e| prepare_failure(old, "resume_identity", &e))?;
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
                        let host_expected =
                            accounts::host_default_dir(home, ProviderKind::Antigravity);
                        let actual = found.home.canonicalize().ok();
                        actual
                            .as_ref()
                            .zip(expected.canonicalize().ok().as_ref())
                            .is_some_and(|(a, b)| a == b)
                            || actual
                                .as_ref()
                                .zip(host_expected.canonicalize().ok().as_ref())
                                .is_some_and(|(a, b)| a == b)
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
            // Preserve a short preflight interval to observe immediate exits, but do not
            // restart it on every output byte: a verified resume may already be working.
            if self.agents.account_resume_observed(new, &session_id) && tap.pulse().end > 0 {
                if confirmed_at.get_or_insert_with(Instant::now).elapsed()
                    >= Duration::from_millis(150)
                {
                    ready = true;
                    break;
                }
            } else {
                confirmed_at = None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !ready {
            tracing::warn!(
                original = old.0,
                replacement = new.0,
                provider = provider.as_str(),
                identity_confirmed = self.agents.account_resume_observed(new, &session_id),
                output_observed = tap.pulse().end > 0,
                stage = "resume_confirmation",
                "account handoff timed out without a confirmed conversation"
            );
            return Err(Failure::Os(libc::ETIMEDOUT));
        }
        // Serialize the final commit with a later user selection. A -> B -> C must never
        // strand this pane on B or erase the A -> B handoff.
        // Hooks take these locks in the same order. Freeze the verified identity
        // before releasing hook_commit, then let the holder's exit observer run.
        let hook_commit = lock(&self.hook_commit);
        let pending = lock(&self.account_work.pending);
        if !self.account_selection_current(old, provider, account_id, &pending) {
            return Err(Failure::BadRequest);
        }
        if self.account_switch_busy(old) {
            return Err(Failure::Os(libc::EBUSY));
        }
        prepared_transcript
            .verify()
            .map_err(|error| prepare_failure(old, "transcript_preparation", &error))?;
        for session in [old, new] {
            let current = self
                .agents
                .stored(session)
                .map_err(|e| super::os(&e))?
                .ok_or(Failure::Os(libc::EILSEQ))?;
            if self.switch_provider(&current) != Some(provider) {
                tracing::warn!(
                    session = session.0,
                    stage = "provider_recheck",
                    "account handoff rejected a changed provider"
                );
                return Err(Failure::BadRequest);
            }
            if transcript::session_id(provider, &current)
                .map_err(|e| prepare_failure(old, "conversation_recheck", &e))?
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
            return Err(super::os(&error));
        }
        let _retiring_binding = self.agents.retire_account_binding(old);
        drop(pending);
        drop(hook_commit);
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
        // Holder Done acknowledges the teardown request, not process exit.
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
            self.agents.finish_account_resume(new);
            return Err(Failure::Os(libc::ETIMEDOUT));
        }
        // Clients now reconnect and deliver their held prompts. A synthetic continuation
        // could overtake input still in an app journal, so only actual user input is sent.
        self.agents.take_rate_limited(old);
        self.agents.finish_account_resume(new);
        drop(replacement);
        // A later selection can arrive while retirement is in progress. The replacement
        // becomes eligible only after its guard is gone, so enqueue that latest selection again.
        self.transfer_account_selection(old, new, provider, account_id, || {
            accounts::refresh_accounts_with(home, state_dir.as_deref(), false)
        })
        .ok();
        Ok(new)
    }

    fn account_switch_busy(&self, session: SessionId) -> bool {
        use krill_proto::control::AgentState;
        !self.agents.is_rate_limited(session)
            && matches!(
                self.status.get(session).state,
                AgentState::Working | AgentState::Waiting | AgentState::Blocked
            )
    }
}

fn replacement_journal(dir: &Path, session: SessionId) -> std::path::PathBuf {
    dir.join("account-replacements")
        .join(format!("{}.json", session.0))
}

struct Processing<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for Processing<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

// Scoped workers borrow the registry and finish before the next batch; no registry lock
// survives CLI IO. A claim_resume guard independently excludes other resume APIs per session.
fn run_bounded<T: Sync>(jobs: &[T], work: impl Fn(&T) + Sync) {
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..jobs.len().min(MAX_HANDOFF_WORKERS) {
            let work = &work;
            let next = &next;
            scope.spawn(move || {
                while let Some(job) =
                    jobs.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                {
                    work(job);
                }
            });
        }
    });
}

struct Replacement<'a> {
    registry: &'a Registry,
    original: SessionId,
    session: SessionId,
    committed: bool,
}

impl Drop for Replacement<'_> {
    fn drop(&mut self) {
        // A committed replacement keeps its resume watch for the continuation gate.
        if !self.committed {
            self.registry.agents.finish_account_resume(self.session);
        }
        lock(&self.registry.account_replacements).remove(&self.session);
        let only_remaining = !self.committed
            && self.registry.account_original_ended(self.original)
            && !self.registry.account_original_ended(self.session);
        let resolved = if only_remaining {
            let retained = if let Some(handoff) =
                lock(&self.registry.account_handoffs).get_mut(&self.original)
            {
                handoff.to = None;
                handoff.error = Some(format!(
                    "Original session ended; unconfirmed replacement {} was retained. Select it explicitly to continue; held input was not sent",
                    self.session.0
                ));
                true
            } else {
                false
            };
            retained && self.registry.save_account_handoffs().is_ok()
        } else if self.committed {
            self.registry.account_original_ended(self.original)
        } else {
            // An unregistered target may be an unknown Spawn outcome. Leave its journal.
            let registered = lock(&self.registry.state)
                .sessions
                .contains_key(&self.session);
            registered && self.registry.stop_account_replacement(self.session)
        };
        if resolved && let Some(dir) = self.registry.state_dir() {
            std::fs::remove_file(replacement_journal(&dir, self.session)).ok();
        }
    }
}

// Process/file discovery races with the resumed CLI creating or rotating its transcript.
// Keep polling within the existing deadline; ambiguous/invalid identities remain fatal.
fn resume_identity(
    discover: impl FnOnce() -> std::io::Result<Option<identity::Identity>>,
) -> std::io::Result<Option<identity::Identity>> {
    match discover() {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::NotFound
            ) =>
        {
            Ok(None)
        }
        result => result,
    }
}

// Validation errors constructed without an OS errno must retain their meaning on the wire.
fn prepare_failure(original: SessionId, stage: &'static str, error: &std::io::Error) -> Failure {
    use std::io::ErrorKind;
    tracing::warn!(original = original.0, stage, kind = ?error.kind(), errno = error.raw_os_error(), "account handoff preparation failed");
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
