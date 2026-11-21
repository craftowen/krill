//! Classify live sessions before the user picks which ones follow an account switch.

use std::path::Path;

use krill_proto::SessionId;
use krill_proto::control::Failure;
use krill_proto::usage::{
    AccountsSnapshot, CantMoveReason, ProviderKind, SessionSwitchCandidate, SessionSwitchGroup,
    SessionSwitchPlan,
};

use super::{Registry, effective_foreground_pid, lock, transcript};
use crate::usage::accounts;

struct Live {
    session: SessionId,
    stored: crate::agent::control::StoredLaunch,
    current: crate::agent::control::LaunchAccount,
    cwd: String,
    root_pid: u32,
    raw_fg: Option<u32>,
}

impl Registry {
    pub(in crate::registry) fn inspect_session_switches_in(
        &self,
        home: &Path,
        provider: ProviderKind,
        previous_account_id: Option<String>,
        target_account_id: String,
        snapshot: &AccountsSnapshot,
    ) -> Result<SessionSwitchPlan, Failure> {
        let known = |id: &str| {
            snapshot
                .accounts
                .iter()
                .any(|a| a.provider == provider && a.id == id)
        };
        if !known(&target_account_id) {
            return Err(Failure::BadRequest);
        }
        let sessions: Vec<_> = {
            let state = lock(&self.state);
            state
                .sessions
                .iter()
                .filter(|(id, _)| !state.exited.contains_key(id) && !state.closing.contains(id))
                .map(|(id, e)| {
                    (
                        *id,
                        e.pid,
                        e.session.tap().foreground(),
                        e.cwd.to_string_lossy().into_owned(),
                    )
                })
                .collect()
        };
        let agents = self.agents.agents().ok();
        let live: Vec<Live> = sessions
            .into_iter()
            .filter_map(|(session, root_pid, raw_fg, entry_cwd)| {
                let stored = self.live_agent_for_switch_in(session, home)?;
                if self.switch_provider(&stored) != Some(provider) {
                    return None;
                }
                let profile = agents
                    .as_ref()
                    .and_then(|a| a.resolve(&stored.profile).map(|(_, p)| p));
                let current = self.running_account_for_stored(home, provider, profile, &stored);
                let cwd = if stored.cwd.is_empty() {
                    entry_cwd
                } else {
                    String::from_utf8_lossy(&stored.cwd).into_owned()
                };
                Some(Live {
                    session,
                    stored,
                    current,
                    cwd,
                    root_pid,
                    raw_fg,
                })
            })
            .collect();
        let state_dir = self.state_dir();
        let file = accounts::load_file(home, state_dir.as_deref());
        let mut dirs: Vec<_> = Self::provider_account_dirs(&file, home, provider)
            .into_iter()
            .flat_map(|(_, cfg, resolved)| [cfg, resolved])
            .collect();
        dirs.push(accounts::host_default_dir(home, provider));
        let registered = |dir: &Path| {
            let stripped = transcript::strip_verbatim_prefix(dir);
            dirs.iter().any(|d| {
                transcript::strip_verbatim_prefix(d) == stripped
                    || same_file::is_same_file(d, dir).unwrap_or(false)
            })
        };
        let conversation = |stored: &crate::agent::control::StoredLaunch| {
            transcript::session_id(provider, stored).ok().or_else(|| {
                stored
                    .transcript_path
                    .as_deref()
                    .and_then(|p| Path::new(p).canonicalize().ok())
                    .map(|p| p.to_string_lossy().into_owned())
            })
        };
        let keys: Vec<_> = live.iter().map(|l| conversation(&l.stored)).collect();
        let candidates = live
            .iter()
            .zip(&keys)
            .filter(|(l, _)| l.current.id != target_account_id)
            .map(|(l, key)| {
                let session = l.session;
                let pending = lock(&self.account_handoffs)
                    .get(&session)
                    .is_some_and(|h| h.to.is_none() && h.error.is_none())
                    || lock(&self.account_replacements).contains(&session)
                    || self.agents.account_binding_retiring(session);
                let reason = if pending {
                    Some(CantMoveReason::PendingHandoff)
                } else if !l.current.dir.is_empty() && !registered(Path::new(&l.current.dir)) {
                    Some(CantMoveReason::OwnFolder)
                } else if l.current.id.is_empty() || !known(&l.current.id) {
                    Some(CantMoveReason::UnknownAccount)
                } else if key.is_some() && keys.iter().filter(|k| *k == key).count() > 1 {
                    Some(CantMoveReason::SharedSession)
                } else if !self.has_resumable_conversation(home, provider, l) {
                    Some(CantMoveReason::NoSession)
                } else {
                    None
                };
                let group = match reason {
                    Some(reason) => SessionSwitchGroup::CantMove(reason),
                    None if previous_account_id.as_deref() == Some(l.current.id.as_str()) => {
                        SessionSwitchGroup::OnPreviousAccount
                    }
                    None => SessionSwitchGroup::Elsewhere,
                };
                let current_account_label = snapshot
                    .accounts
                    .iter()
                    .find(|a| a.provider == provider && a.id == l.current.id)
                    .map(|a| a.email.clone().unwrap_or_else(|| a.label.clone()))
                    .or_else(|| (!l.current.id.is_empty()).then(|| l.current.id.clone()))
                    .unwrap_or_else(|| "unknown".to_owned());
                SessionSwitchCandidate {
                    session,
                    cwd: l.cwd.clone(),
                    agent: l.stored.profile.clone(),
                    current_account_id: l.current.id.clone(),
                    current_account_label,
                    busy: self.account_switch_busy(session),
                    group,
                }
            })
            .collect();
        Ok(SessionSwitchPlan {
            provider,
            previous_account_id,
            target_account_id,
            candidates,
        })
    }

    /// The same evidence `prepare_account_switch_excluding` requires before an automatic handoff.
    fn has_resumable_conversation(&self, home: &Path, provider: ProviderKind, live: &Live) -> bool {
        let bound = transcript::session_id(provider, &live.stored).is_ok()
            && (provider != ProviderKind::Claude
                || transcript::has_claude_transcript(home, Some(&live.current), &live.stored));
        bound || {
            let fg_pid = effective_foreground_pid(live.root_pid, live.raw_fg, false);
            self.discover_live_identity(provider, home, Some(&live.stored), live.root_pid, fg_pid)
                .ok()
                .flatten()
                .is_some()
        }
    }
}
