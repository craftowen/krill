//! Resume live agents under the selected account without discarding their conversation.
//! Reference: Orca src/main/codex/codex-legacy-session-resume.ts and
//! src/renderer/src/store/terminals/terminal-restart-state.ts,
//! Copyright (c) 2026 Lovecast Inc., MIT License.

mod recovery;
#[cfg(all(test, unix))]
mod tests;
mod transcript;

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::control::{AgentLaunch, Failure};
use krill_proto::os::ffi::OsStrExt as _;
use krill_proto::usage::{AccountsSnapshot, ConnectionStatus, ProviderKind, SessionHandoff};

use super::{Peer, Registry, Scope, lock};
use crate::usage::accounts;
use crate::usage::accounts::switch_work::SwitchTarget;

const FILE: &str = "account-handoffs.json";
const PREPARED_FILE: &str = "account-replacement.json";
const PREPARED_DIR: &str = "account-replacements";
// Limit simultaneous CLI initialization and spread starts instead of launching every pane at once.
const RESTART_WORKERS: usize = 2;
const RESTART_SPACING: Duration = Duration::from_millis(300);
const CONTINUE_PROMPT: &str = "The account profile was changed and this session was restarted. Continue the existing task from where you left off, using the conversation history and current workspace state. Check what is already done before acting; do not repeat completed work. If the task is complete or you were waiting for user input, remain idle and briefly report that status.";

#[derive(serde::Serialize, serde::Deserialize)]
struct Prepared {
    from: SessionId,
    to: SessionId,
    account_id: String,
    #[serde(default)]
    continue_work: bool,
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
        // Remember even an A -> B -> A selection, where A owns every original process.
        // An in-progress B replacement must observe cancellation before its commit.
        pending.insert(
            provider,
            SwitchTarget {
                account_id: account_id.into(),
            },
        );
        self.account_work.wake.notify_one();
    }

    // Called with the work queue locked, after account-file evaluation has finished.
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

    pub(super) fn account_switch_snapshot(&self, snapshot: &mut AccountsSnapshot) {
        let pending = lock(&self.account_work.pending);
        for (provider, target) in pending.iter() {
            self.prepare_account_switch(*provider, &target.account_id);
        }
        drop(pending);
        snapshot.details.handoffs = lock(&self.account_handoffs).values().cloned().collect();
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
            // Account evaluation publishes only the selected profile. Discover sessions
            // after it releases the account-file lock, before consuming the queued work.
            for (provider, target) in pending.iter() {
                self.prepare_account_switch(*provider, &target.account_id);
            }
            std::mem::take(&mut *pending)
        };
        let mut jobs = VecDeque::new();
        for (provider, target) in pending {
            let from: Vec<_> = lock(&self.account_handoffs)
                .values()
                .filter(|h| {
                    h.account_id == target.account_id && h.to.is_none() && h.error.is_none()
                })
                .map(|h| h.from)
                .collect();
            for old in from {
                jobs.push_back((provider, target.account_id.clone(), old));
            }
        }
        let jobs = Mutex::new(jobs);
        let start = Mutex::new(Instant::now());
        let worker = || {
            let peer = Peer {
                pid: None,
                scope: Scope::Full,
            };
            loop {
                let Some((provider, account_id, old)) = lock(&jobs).pop_front() else {
                    break;
                };
                {
                    let mut next = lock(&start);
                    std::thread::sleep(next.saturating_duration_since(Instant::now()));
                    *next = Instant::now() + RESTART_SPACING;
                }
                if lock(&self.account_work.pending)
                    .get(&provider)
                    .is_some_and(|target| target.account_id != account_id)
                {
                    continue;
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
                            let reason = if error == Failure::Denied {
                                "selected account login or credential scope could not be verified"
                                    .to_owned()
                            } else {
                                error.to_string()
                            };
                            handoff.error = Some(format!(
                                "Session {}: could not resume with selected account ({reason}); original session preserved",
                                old.0
                            ));
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
        };
        let workers = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(RESTART_WORKERS)
            .min(lock(&jobs).len());
        std::thread::scope(|scope| {
            for _ in 1..workers {
                // If a thread cannot be created, the calling worker drains the queue.
                if let Err(error) = std::thread::Builder::new()
                    .name("account-restart".into())
                    .spawn_scoped(scope, worker)
                {
                    tracing::warn!(kind = ?error.kind(), "account restart worker unavailable");
                }
            }
            worker();
        });
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
        let resolved_sid = stored
            .session_id
            .clone()
            .filter(|id| crate::agent::control::valid_session_id(id))
            .or_else(|| {
                crate::agent_history::chat::resolve_session_transcript(
                    state_dir.as_deref(),
                    &stored,
                    &cwd,
                )
                .map(|(_, sid)| sid)
                .filter(|id| crate::agent::control::valid_session_id(id))
            });
        // Preparation can fail. Until it succeeds the old process and PTY are untouched.
        let resume_sid = if let Some(session_id) = resolved_sid.as_deref() {
            let source = crate::agent::resume_account(
                profile,
                state_dir.as_deref(),
                stored.account.as_ref(),
                Some(session_id),
            )
            .unwrap_or_else(|| crate::agent::control::LaunchAccount {
                id: accounts::host_default_id(provider),
                env_key: provider.env_var().into(),
                dir: String::new(),
            });
            match transcript::prepare(provider, home, &source, target, session_id, &cwd) {
                Ok(()) => Some(session_id),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(prepare_failure(&e)),
            }
        } else {
            None
        };
        // A profile change must never silently replace ongoing work with a fresh conversation.
        let resume_sid = resume_sid.ok_or(Failure::BadRequest)?;
        let new = self.next_id();
        lock(&self.account_replacements).insert(new);
        let mut replacement = Replacement {
            registry: self,
            session: new,
            committed: false,
            continued: false,
        };
        // New launches already default to YOLO. Re-evaluate the current preference rather
        // than carrying a stale manual permission from before that default was introduced.
        let permission = agents.permission;
        let (launch, sid, applied) = crate::agent::resume_in(
            profile,
            permission,
            new,
            state_dir.as_deref(),
            Some(resume_sid),
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
                    permission,
                },
                sid,
                cwd.clone(),
                applied,
            )
            .map_err(|e| super::os(&e))?;
        let prepared = Prepared {
            from: old,
            to: new,
            account_id: account_id.into(),
            continue_work: true,
        };
        if let Some(dir) = state_dir.as_deref() {
            let raw = serde_json::to_vec(&prepared).map_err(|_| Failure::BadRequest)?;
            accounts::write_atomic_0600(&prepared_path(dir, new), &raw)
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
            if tap.pulse().end > 0 && tap.quiet_ms() >= 150 {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !ready {
            self.close(peer, new).ok();
            return Err(Failure::BadRequest);
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
        // Publish completion and retire the old PTY only after persistence succeeds.
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
        if let Err(error) = self.close(peer, old) {
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
        drop(pending);
        replacement.committed = true;
        // Submit only after retiring the old agent, so restoring history alone does not
        // leave the replacement idle or start the same task in two live agents.
        match self.continue_account_work(&prepared) {
            Ok(()) => replacement.continued = true,
            Err(error) => {
                tracing::warn!(kind = ?error.kind(), "account continuation pending recovery");
            }
        }
        Ok(new)
    }
}

struct Replacement<'a> {
    registry: &'a Registry,
    session: SessionId,
    committed: bool,
    continued: bool,
}

impl Drop for Replacement<'_> {
    fn drop(&mut self) {
        lock(&self.registry.account_replacements).remove(&self.session);
        if !self.committed {
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
        if (!self.committed || self.continued)
            && let Some(dir) = self.registry.state_dir()
        {
            std::fs::remove_file(prepared_path(&dir, self.session)).ok();
        }
    }
}

fn prepared_path(dir: &Path, session: SessionId) -> std::path::PathBuf {
    dir.join(PREPARED_DIR).join(format!("{}.json", session.0))
}

/// Transcript bridging raises errno-less kinds. Name the cause instead of a generic EIO.
fn prepare_failure(error: &std::io::Error) -> Failure {
    use std::io::ErrorKind;
    if error.raw_os_error().is_some() {
        return super::os(error);
    }
    Failure::Os(match error.kind() {
        ErrorKind::TimedOut => libc::ETIMEDOUT,
        ErrorKind::InvalidInput => libc::EINVAL,
        ErrorKind::AlreadyExists => libc::EEXIST,
        ErrorKind::FileTooLarge => libc::EFBIG,
        ErrorKind::BrokenPipe => libc::EPIPE,
        _ => libc::EIO,
    })
}
