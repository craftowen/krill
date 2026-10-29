//! Host-scoped running-session account menus and stable replacement positions.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, Task};
use krill_proto::usage::{AccountInfo, AccountStatus, AccountsSnapshot, ProviderKind};
use krill_proto::{SessionId, usage};

use super::{HostId, Row, Session, Sidebar, Snapshot, SwitchSessionAccount};

const SPINNER_GRACE: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct State {
    hosts: BTreeMap<HostId, HostProfiles>,
    spinner_generation: u64,
}

#[derive(Default)]
struct HostProfiles {
    features: u64,
    pending: BTreeSet<SessionId>,
    spinner_grace: BTreeMap<SessionId, SpinnerGrace>,
    accounts: Arc<AccountsSnapshot>,
    replacements: BTreeMap<SessionId, SessionId>,
    /// Keep the old row's footprint until a status poll sees its replacement.
    fallback: BTreeMap<SessionId, Fallback>,
}

struct SpinnerGrace {
    generation: u64,
    ready: bool,
    /// Dropping this record cancels the old deadline on completion or a fresh request.
    _timer: Task<()>,
}

struct Fallback {
    session: Session,
    missing_polls: u8,
}

impl HostProfiles {
    fn is_pending(&self, session: SessionId) -> bool {
        self.pending.contains(&session)
            || self.accounts.details.handoffs.iter().any(|handoff| {
                handoff.from == session && handoff.to.is_none() && handoff.error.is_none()
            })
    }

    fn destination(&self, mut id: SessionId) -> SessionId {
        for _ in 0..self.replacements.len() {
            let Some(next) = self
                .replacements
                .get(&id)
                .copied()
                .filter(|next| *next != id)
            else {
                break;
            };
            id = next;
        }
        id
    }

    fn order(&self, mut id: SessionId) -> SessionId {
        for _ in 0..self.replacements.len() {
            let Some((&previous, _)) = self.replacements.iter().find(|(_, next)| **next == id)
            else {
                break;
            };
            id = previous;
        }
        id
    }
}

impl State {
    pub(super) fn session_rank(
        &self,
        host: &HostId,
        mut session: SessionId,
        order: &BTreeMap<SessionId, usize>,
    ) -> Option<usize> {
        if let Some(rank) = order.get(&session) {
            return Some(*rank);
        }
        let state = self.hosts.get(host)?;
        for _ in 0..state.replacements.len() {
            let (&previous, _) = state
                .replacements
                .iter()
                .find(|(_, next)| **next == session)?;
            session = previous;
            if let Some(rank) = order.get(&session) {
                return Some(*rank);
            }
        }
        None
    }

    /// Only fresh status polls consume the one-poll grace, never unrelated redraws.
    pub(super) fn observe(&mut self, host: &HostId, snapshot: &Snapshot) -> bool {
        let Some(state) = self.hosts.get_mut(host) else {
            return false;
        };
        let before = state.fallback.len();
        state.fallback.retain(|id, fallback| {
            if snapshot.sessions.iter().any(|s| s.id == *id) {
                return false;
            }
            fallback.missing_polls = fallback.missing_polls.saturating_add(1);
            fallback.missing_polls <= 1
        });
        before != state.fallback.len()
    }

    pub(super) fn fallback(&self, host: &HostId, session: SessionId) -> Option<&Session> {
        self.hosts
            .get(host)?
            .fallback
            .get(&session)
            .map(|row| &row.session)
    }

    pub(super) fn remove_fallback(&mut self, host: &HostId, session: SessionId) {
        if let Some(state) = self.hosts.get_mut(host) {
            state.fallback.remove(&session);
        }
    }

    pub(super) fn snapshot<'a>(&self, host: &HostId, snapshot: &'a Snapshot) -> Cow<'a, Snapshot> {
        let Some(state) = self.hosts.get(host).filter(|s| !s.replacements.is_empty()) else {
            return Cow::Borrowed(snapshot);
        };
        let mut normalized = snapshot.clone();
        normalized
            .sessions
            .retain(|s| state.destination(s.id) == s.id);
        for (id, fallback) in &state.fallback {
            if state.destination(*id) == *id && !normalized.sessions.iter().any(|s| s.id == *id) {
                normalized.sessions.push(fallback.session.clone());
            }
        }
        normalized.sessions.sort_by_key(|s| state.order(s.id));
        Cow::Owned(normalized)
    }
}

/// Keep separate profiles identifiable even when their saved names or emails match.
pub(super) fn account_label(account: &AccountInfo, choices: &[AccountInfo]) -> String {
    fn display(account: &AccountInfo) -> String {
        let label = account.label.trim();
        let email = account
            .email
            .as_deref()
            .map(str::trim)
            .filter(|email| !email.is_empty());
        match (label.is_empty(), email) {
            (true, Some(email)) => email.to_owned(),
            (true, None) => account.id.clone(),
            (false, Some(email)) if !label.eq_ignore_ascii_case(email) => {
                format!("{label} · {email}")
            }
            (false, _) => label.to_owned(),
        }
    }
    let label = display(account);
    if choices
        .iter()
        .any(|other| other.id != account.id && display(other) == label)
    {
        format!("{label} · {}", account.id)
    } else {
        label
    }
}

impl Sidebar {
    /// Capability comes from this host's account IPC connection, never the active pane.
    pub fn set_session_account_features(
        &mut self,
        host: HostId,
        features: u64,
        cx: &mut Context<Self>,
    ) {
        let state = self.profiles.hosts.entry(host).or_default();
        if state.features != features {
            state.features = features;
            cx.notify();
        }
    }

    pub fn sync_session_accounts(
        &mut self,
        host: &HostId,
        accounts: Arc<AccountsSnapshot>,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot_for(host).clone();
        let state = self.profiles.hosts.entry(host.clone()).or_default();
        let mut changed = false;
        for handoff in &accounts.details.handoffs {
            let Some(to) = handoff.to.filter(|to| *to != handoff.from) else {
                continue;
            };
            if state.replacements.get(&handoff.from) == Some(&to) {
                continue;
            }
            let before = snapshot
                .sessions
                .iter()
                .find(|s| s.id == handoff.from)
                .or_else(|| {
                    state
                        .fallback
                        .get(&handoff.from)
                        .map(|fallback| &fallback.session)
                })
                .cloned();
            state.replacements.insert(handoff.from, to);
            if let Some(mut row) = before {
                row.id = to;
                if let Some(account) = accounts
                    .accounts
                    .iter()
                    .find(|a| a.id == handoff.account_id)
                {
                    row.account = Some(krill_proto::control::SessionAccount {
                        session: to,
                        account_id: account.id.clone(),
                        label: account.label.clone(),
                    });
                }
                state.fallback.insert(
                    to,
                    Fallback {
                        session: row,
                        missing_polls: 0,
                    },
                );
            }
            self.card_list.rebind_session(host, handoff.from, to);
            for (index, row) in self.rows.iter_mut().enumerate() {
                if self.row_hosts.get(index) == Some(host)
                    && let Row::Session(session, _) = row
                    && session.id == handoff.from
                {
                    session.id = to;
                }
            }
            if &self.focused_host == host && self.focused == Some(handoff.from) {
                self.focused = Some(to);
            }
            if host.is_local() {
                for titles in [
                    &mut self.custom_titles,
                    &mut self.titles,
                    &mut self.auto_titles,
                ] {
                    if let Some(title) = titles.remove(&handoff.from) {
                        titles.insert(to, title);
                    }
                }
            }
            changed = true;
        }
        let updated = state.accounts != accounts;
        state.accounts = accounts;
        self.reconcile_spinner_grace(host, cx);
        if changed {
            self.rebuild(cx);
        } else if updated {
            cx.notify();
        }
    }

    pub(super) fn session_account_error(&self, host: &HostId, session: SessionId) -> Option<&str> {
        self.profiles
            .hosts
            .get(host)?
            .accounts
            .details
            .handoffs
            .iter()
            .find(|handoff| handoff.from == session && handoff.to.is_none())?
            .error
            .as_deref()
    }

    pub(super) fn session_account_pending(&self, host: &HostId, session: SessionId) -> bool {
        self.profiles
            .hosts
            .get(host)
            .is_some_and(|state| state.is_pending(session))
    }

    pub(super) fn session_account_loading(&self, host: &HostId, session: SessionId) -> bool {
        self.profiles.hosts.get(host).is_some_and(|state| {
            state.is_pending(session)
                && state
                    .spinner_grace
                    .get(&session)
                    .is_some_and(|grace| grace.ready)
        })
    }

    pub(super) fn session_account_tooltip(
        &self,
        host: &HostId,
        session: SessionId,
        label: &str,
    ) -> String {
        if self.session_account_loading(host, session) {
            return crate::i18n::catalog::settings::ACCOUNT_SESSION_SWITCHING
                .get()
                .to_owned();
        }
        let account = super::text::SESSION_ACCOUNT.format(&[("label", label)]);
        if !self.session_account_pending(host, session)
            && let Some(error) = self.session_account_error(host, session)
        {
            format!("{account} · {error}")
        } else {
            account
        }
    }

    fn start_spinner_grace(&mut self, host: HostId, session: SessionId, cx: &mut Context<Self>) {
        // Start once for the API request or a newly observed global handoff. The
        // retained task is cancelled on completion/reset, including its pending callback.
        self.profiles.spinner_generation = self.profiles.spinner_generation.wrapping_add(1);
        let generation = self.profiles.spinner_generation;
        let deadline = cx.background_executor().timer(SPINNER_GRACE);
        let timer_host = host.clone();
        let timer = cx.spawn(async move |this, cx| {
            deadline.await;
            this.update(cx, |sidebar, cx| {
                if let Some(state) = sidebar.profiles.hosts.get_mut(&timer_host)
                    && state.is_pending(session)
                    && let Some(grace) = state.spinner_grace.get_mut(&session)
                    && grace.generation == generation
                {
                    grace.ready = true;
                    cx.notify();
                }
            })
            .ok();
        });
        self.profiles
            .hosts
            .entry(host)
            .or_default()
            .spinner_grace
            .insert(
                session,
                SpinnerGrace {
                    generation,
                    ready: false,
                    _timer: timer,
                },
            );
    }

    fn reconcile_spinner_grace(&mut self, host: &HostId, cx: &mut Context<Self>) {
        let Some(state) = self.profiles.hosts.get_mut(host) else {
            return;
        };
        let mut active = state.pending.clone();
        active.extend(
            state
                .accounts
                .details
                .handoffs
                .iter()
                .filter(|handoff| handoff.to.is_none() && handoff.error.is_none())
                .map(|handoff| handoff.from),
        );
        state
            .spinner_grace
            .retain(|session, _| active.contains(session));
        let new: Vec<_> = active
            .into_iter()
            .filter(|session| !state.spinner_grace.contains_key(session))
            .collect();
        for session in new {
            self.start_spinner_grace(host.clone(), session, cx);
        }
    }

    pub(super) fn session_account_choices(
        &self,
        host: &HostId,
        session: &Session,
    ) -> Vec<AccountInfo> {
        let Some(state) = self.profiles.hosts.get(host) else {
            return Vec::new();
        };
        let connected = if host.is_local() {
            self.local_status == super::HostStatus::Connected
        } else {
            self.remote_hosts.iter().any(|r| {
                host.remote_id() == Some(r.id.as_str()) && r.status == super::HostStatus::Connected
            })
        };
        if !connected
            || session.is_exited()
            || state.pending.contains(&session.id)
            || state.features & (usage::FEAT_SESSION_ACCOUNT_SWITCH | usage::FEAT_PROVIDER_DETAILS)
                != (usage::FEAT_SESSION_ACCOUNT_SWITCH | usage::FEAT_PROVIDER_DETAILS)
            || state
                .accounts
                .details
                .handoffs
                .iter()
                .any(|h| h.from == session.id && h.to.is_none() && h.error.is_none())
        {
            return Vec::new();
        }
        let provider = session
            .account
            .as_ref()
            .and_then(|current| {
                state
                    .accounts
                    .accounts
                    .iter()
                    .find(|a| a.id == current.account_id)
            })
            .map(|a| a.provider)
            .or_else(|| ProviderKind::parse(&session.status.agent));
        let Some(provider) = provider else {
            return Vec::new();
        };
        let request = usage::UsageRequest::SwitchSessionAccount {
            session: session.id,
            provider,
            account_id: String::new(),
        };
        let required = request.required_features();
        if request.contains_removed_provider() || state.features & required != required {
            return Vec::new();
        }
        state
            .accounts
            .accounts
            .iter()
            .filter(|a| {
                a.provider == provider
                    && (a.host_default || !a.profile_dir.is_empty())
                    && !state.accounts.status_for(&a.id).is_some_and(|s| {
                        matches!(
                            s.status,
                            AccountStatus::NeedsLogin | AccountStatus::RateLimited
                        )
                    })
            })
            .cloned()
            .collect()
    }

    pub fn finish_session_account_switch(
        &mut self,
        host: &HostId,
        session: SessionId,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.profiles.hosts.get_mut(host) {
            state.pending.remove(&session);
            self.reconcile_spinner_grace(host, cx);
            cx.notify();
        }
    }

    pub(super) fn switch_session_account(
        &mut self,
        target: SwitchSessionAccount,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self
            .profiles
            .snapshot(&target.host, self.snapshot_for(&target.host));
        let Some(session) = snapshot.sessions.iter().find(|s| s.id == target.session) else {
            return;
        };
        if session
            .account
            .as_ref()
            .is_some_and(|a| a.account_id == target.account_id)
            || !self
                .session_account_choices(&target.host, session)
                .iter()
                .any(|a| a.provider == target.provider && a.id == target.account_id)
        {
            return;
        }
        self.profiles
            .hosts
            .entry(target.host.clone())
            .or_default()
            .pending
            .insert(target.session);
        self.start_spinner_grace(target.host.clone(), target.session, cx);
        cx.emit(target);
        cx.notify();
    }
}

#[cfg(all(test, unix))]
mod tests;
