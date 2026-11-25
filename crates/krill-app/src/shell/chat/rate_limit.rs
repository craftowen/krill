// UI interaction patterns referenced from Orca native-chat (MIT).
// Copyright (c) 2026 Lovecast Inc.
//! Rate-limit banner details (which window, when it resets), durable cut tracking across
//! account switches and restarts (Orca #47221), and single-shot auto-continue.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{Context, Task};
use krill_app::accounts::display;
use krill_app::hosts::HostId;
use krill_app::i18n::catalog::chat as ctext;
use krill_proto::SessionId;
use krill_proto::control::AgentState;
use krill_proto::os::write_atomic_0600;
use krill_proto::usage::{AccountStatus, AccountsSnapshot};
use serde::{Deserialize, Serialize};

use super::{ChatKey, QueuedChatPrompt, Shell, build_submitted_chat_payload};

/// Resets further away than this get no clock in the banner and no auto-continue.
pub(super) const RESET_HORIZON_SECS: u64 = 24 * 3600;
/// Usage files lag the provider by a few seconds; resume a little after the reset.
const RESUME_GRACE_SECS: u64 = 5;
/// Persisted ledger filename under `state_dir`.
const RATE_LIMIT_LEDGER_FILE: &str = "chat-rate-limit-recovery.json";
const MAX_PERSISTED_CUTS: usize = 256;
const MAX_LEDGER_BYTES: usize = 512 * 1024;
/// Grace window associating a synchronous `cancel_auto_continue` from the rate-limit banner's
/// account switcher with the immediately following `SwitchSessionAccount` event.
const BANNER_SWITCH_CANCEL_WINDOW: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LimitKind {
    FiveHour,
    Weekly,
    Model,
    Extra,
    Generic,
}

impl LimitKind {
    fn of_meter(key: &str) -> Self {
        match key {
            "five_hour" | "5h" => Self::FiveHour,
            "seven_day" | "7d" => Self::Weekly,
            k if k.contains("extra") => Self::Extra,
            k if k.contains("weekly") => Self::Weekly,
            k if k.starts_with("seven_day_") => Self::Model,
            _ => Self::Generic,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LimitInfo {
    pub kind: LimitKind,
    /// Epoch seconds, always after `now`.
    pub reset_at: u64,
    /// The window is at 100%, so it is what blocks the account.
    pub full: bool,
}

/// Persisted lifecycle state of a single usage-limit cut for a chat session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CutDisposition {
    Pending,
    Continued,
    Cancelled,
}

/// Durable record of a usage-limit cut so account switches, runtime relaunches, and app/daemon
/// restarts continue pending chats at most once without looping or double-sending (Orca #47221).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RateLimitCutRecord {
    pub host: String,
    pub session_id: u64,
    /// Monotone cut identifier; continue attempts bind to this cut rather than a transient reset.
    pub cut_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limited_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<u64>,
    pub recorded_at: u64,
    #[serde(default)]
    pub relaunched: bool,
    pub disposition: CutDisposition,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RateLimitLedger {
    #[serde(default)]
    pub next_cut_id: u64,
    #[serde(default)]
    pub cuts: Vec<RateLimitCutRecord>,
}

impl RateLimitLedger {
    pub(super) fn load(state_dir: &Path) -> Self {
        let path = state_dir.join(RATE_LIMIT_LEDGER_FILE);
        let Ok(bytes) = fs::read(&path) else {
            return Self::default();
        };
        if bytes.len() > MAX_LEDGER_BYTES {
            return Self::default();
        }
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    pub(super) fn save(&self, state_dir: &Path) {
        if fs::create_dir_all(state_dir).is_err() {
            return;
        }
        let path = state_dir.join(RATE_LIMIT_LEDGER_FILE);
        if let Ok(mut bytes) = serde_json::to_vec_pretty(self) {
            bytes.push(b'\n');
            write_atomic_0600(&path, &bytes).ok();
        }
    }

    fn alloc_cut_id(&mut self) -> u64 {
        self.next_cut_id = self.next_cut_id.saturating_add(1).max(1);
        self.next_cut_id
    }

    pub(super) fn get(&self, host: &str, session_id: u64) -> Option<&RateLimitCutRecord> {
        self.cuts
            .iter()
            .find(|c| c.host == host && c.session_id == session_id)
    }

    pub(super) fn get_mut(
        &mut self,
        host: &str,
        session_id: u64,
    ) -> Option<&mut RateLimitCutRecord> {
        self.cuts
            .iter_mut()
            .find(|c| c.host == host && c.session_id == session_id)
    }

    /// Records a new pending cut if none exists for `(host, session_id)`.
    /// Updates missing `reset_at`/`limited_account_id` on an existing `Pending` cut for the same
    /// account without resetting `Continued` or `Cancelled` cuts.
    pub(super) fn record_pending_cut(
        &mut self,
        host: &str,
        session_id: u64,
        limited_account_id: Option<String>,
        reset_at: Option<u64>,
        now: u64,
    ) -> bool {
        if let Some(existing) = self.get_mut(host, session_id) {
            if existing.disposition != CutDisposition::Pending {
                let is_new_cycle = match (existing.reset_at, reset_at) {
                    (Some(prev), Some(next)) => next > prev,
                    _ => false,
                };
                if !is_new_cycle {
                    return false;
                }
                let cut_id = self.alloc_cut_id();
                if let Some(slot) = self.get_mut(host, session_id) {
                    *slot = RateLimitCutRecord {
                        host: host.to_owned(),
                        session_id,
                        cut_id,
                        limited_account_id,
                        reset_at,
                        recorded_at: now,
                        relaunched: false,
                        disposition: CutDisposition::Pending,
                    };
                }
                return true;
            }
            let mut changed = false;
            let same_account = existing.limited_account_id.is_none()
                || existing.limited_account_id.as_deref() == limited_account_id.as_deref();
            if existing.limited_account_id.is_none() && limited_account_id.is_some() {
                existing.limited_account_id = limited_account_id;
                changed = true;
            }
            if existing.reset_at.is_none() && reset_at.is_some() && same_account {
                existing.reset_at = reset_at;
                changed = true;
            }
            return changed;
        }
        let cut_id = self.alloc_cut_id();
        self.cuts.push(RateLimitCutRecord {
            host: host.to_owned(),
            session_id,
            cut_id,
            limited_account_id,
            reset_at,
            recorded_at: now,
            relaunched: false,
            disposition: CutDisposition::Pending,
        });
        if self.cuts.len() > MAX_PERSISTED_CUTS {
            let excess = self.cuts.len() - MAX_PERSISTED_CUTS;
            self.cuts.drain(..excess);
        }
        true
    }

    /// Explicitly arms or re-arms a cut (user clicked the auto-continue clock button).
    pub(super) fn arm_cut(
        &mut self,
        host: &str,
        session_id: u64,
        account_id: Option<String>,
        reset_at: Option<u64>,
        now: u64,
    ) -> u64 {
        if let Some(existing) = self.get_mut(host, session_id) {
            existing.limited_account_id = account_id;
            existing.reset_at = reset_at;
            existing.recorded_at = now;
            existing.disposition = CutDisposition::Pending;
            return existing.cut_id;
        }
        let cut_id = self.alloc_cut_id();
        self.cuts.push(RateLimitCutRecord {
            host: host.to_owned(),
            session_id,
            cut_id,
            limited_account_id: account_id,
            reset_at,
            recorded_at: now,
            relaunched: false,
            disposition: CutDisposition::Pending,
        });
        cut_id
    }

    pub(super) fn mark_disposition(
        &mut self,
        host: &str,
        session_id: u64,
        disposition: CutDisposition,
    ) -> bool {
        if let Some(existing) = self.get_mut(host, session_id)
            && existing.disposition != disposition
        {
            existing.disposition = disposition;
            return true;
        }
        false
    }

    pub(super) fn remove(&mut self, host: &str, session_id: u64) -> bool {
        let before = self.cuts.len();
        self.cuts
            .retain(|c| !(c.host == host && c.session_id == session_id));
        self.cuts.len() != before
    }

    /// Migrates a cut record when an account switch / handoff replaces `from_session` with `to_session`.
    pub(super) fn rebind_session(
        &mut self,
        host: &str,
        from_session: u64,
        to_session: u64,
    ) -> bool {
        if from_session == to_session {
            return false;
        }
        let Some(pos) = self
            .cuts
            .iter()
            .position(|c| c.host == host && c.session_id == from_session)
        else {
            return false;
        };
        let mut rec = self.cuts.remove(pos);
        self.cuts
            .retain(|c| !(c.host == host && c.session_id == to_session));
        rec.session_id = to_session;
        rec.relaunched = true;
        self.cuts.push(rec);
        true
    }
}

/// In-memory cache of the persisted rate-limit ledger, lazy-loaded per `state_dir`.
#[derive(Default)]
pub(super) struct RateLimitLedgerState {
    loaded_dir: Option<PathBuf>,
    pub(super) ledger: RateLimitLedger,
    /// Tracks the last `cancel_auto_continue` per chat so an immediate account switch from the
    /// rate-limit banner restores the cut for early continue on the switched account.
    pub(super) last_cancel_at: std::collections::HashMap<ChatKey, Instant>,
}

fn host_key(host: &HostId) -> String {
    match host {
        HostId::Local => "local".to_owned(),
        HostId::Remote(id) => format!("remote:{id}"),
    }
}

/// The window limiting `account_id` and its next reset.
///
/// Full windows win, and among them the latest reset, because the account stays
/// blocked until every full window has reset. Without a full window the most used
/// one is shown. Windows without a future reset are ignored.
pub(super) fn limit_info(snap: &AccountsSnapshot, account_id: &str, now: u64) -> Option<LimitInfo> {
    let future = |raw: Option<&str>| {
        raw.and_then(krill_proto::time::parse_reset_epoch)
            .and_then(|at| u64::try_from(at).ok())
            .filter(|at| *at > now)
    };
    let mut windows: Vec<(LimitKind, u8, u64)> = Vec::new();
    if let Some(info) = snap.accounts.iter().find(|a| a.id == account_id) {
        for (kind, window) in [
            (LimitKind::FiveHour, &info.usage.five_hour),
            (LimitKind::Weekly, &info.usage.seven_day),
        ] {
            if let Some(w) = window
                && let Some(at) = future(w.resets_at.as_deref())
            {
                windows.push((kind, w.percent, at));
            }
        }
    }
    for model in &snap.advanced.model_windows {
        if model.account_id == account_id
            && let Some(at) = future(model.seven_day.resets_at.as_deref())
        {
            windows.push((LimitKind::Model, model.seven_day.percent, at));
        }
    }
    for meter in snap.meters_for(account_id).map_or(&[][..], |m| &m.meters) {
        if let Some(w) = &meter.window
            && let Some(at) = future(w.resets_at.as_deref())
        {
            windows.push((LimitKind::of_meter(&meter.key), w.percent, at));
        }
    }
    let full = windows.iter().filter(|w| w.1 >= 100).max_by_key(|w| w.2);
    let best = full.or_else(|| {
        windows
            .iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)))
    });
    if let Some(&(kind, percent, reset_at)) = best {
        return Some(LimitInfo {
            kind,
            reset_at,
            full: percent >= 100,
        });
    }
    snap.advanced
        .rate_limits
        .iter()
        .filter(|r| r.account_id == account_id && r.clears_at > now)
        .map(|r| r.clears_at)
        .min()
        .map(|reset_at| LimitInfo {
            kind: LimitKind::Generic,
            reset_at,
            full: true,
        })
}

/// Returns `true` when `account_id` exists in `snap`, is signed in, and has no full/active limit.
pub(super) fn account_has_fresh_headroom(
    snap: &AccountsSnapshot,
    account_id: &str,
    now: u64,
) -> bool {
    if !snap.accounts.iter().any(|a| a.id == account_id) {
        return false;
    }
    if snap.status_for(account_id).is_some_and(|s| {
        matches!(
            s.status,
            AccountStatus::NeedsLogin | AccountStatus::RateLimited | AccountStatus::Error
        )
    }) {
        return false;
    }
    !limit_info(snap, account_id, now).is_some_and(|i| i.full)
}

/// Returns `true` when a pending usage-limit cut can immediately continue early because the chat
/// is now running on a different account (or relaunched onto an account) with fresh headroom.
pub(super) fn should_continue_early_on_account_switch(
    record: &RateLimitCutRecord,
    current_account: Option<&str>,
    snap: &AccountsSnapshot,
    now: u64,
) -> bool {
    if record.disposition != CutDisposition::Pending {
        return false;
    }
    let Some(acct) = current_account.filter(|s| !s.is_empty()) else {
        return false;
    };
    if !account_has_fresh_headroom(snap, acct, now) {
        return false;
    }
    match record.limited_account_id.as_deref() {
        Some(limited) => limited != acct,
        None => record.relaunched,
    }
}

/// Returns `true` when a pending usage-limit cut's known reset has elapsed (plus grace) and the
/// current account is no longer blocked.
pub(super) fn should_continue_at_reset(
    record: &RateLimitCutRecord,
    current_account: Option<&str>,
    snap: &AccountsSnapshot,
    now: u64,
) -> bool {
    if record.disposition != CutDisposition::Pending {
        return false;
    }
    let Some(reset_at) = record.reset_at else {
        return false;
    };
    if now < reset_at.saturating_add(RESUME_GRACE_SECS) {
        return false;
    }
    if let Some(acct) = current_account {
        if snap
            .status_for(acct)
            .is_some_and(|s| s.status == AccountStatus::NeedsLogin)
        {
            return false;
        }
        if limit_info(snap, acct, now).is_some_and(|i| i.full) {
            return false;
        }
    }
    true
}

/// Banner sentence: the limit kind and its local reset clock, or the generic text.
/// Orca's `NativeChatUsageLimitRow` words: "{agent} 주간 한도에 도달했습니다" and "{time}에 계속 가능"
/// ("{time}에 계속" once it will continue by itself).
pub(super) fn banner_words(
    info: Option<LimitInfo>,
    agent: &str,
    scheduled: bool,
) -> (String, String) {
    let agent = krill_proto::agent_history::VaultAgent::parse(agent)
        .map_or_else(|| ctext::LIMIT_AGENT.get(), |a| a.label());
    let title = match info.map(|i| i.kind) {
        Some(LimitKind::Weekly) => &ctext::LIMIT_TITLE_WEEKLY,
        Some(LimitKind::Extra) => &ctext::LIMIT_TITLE_EXTRA,
        _ => &ctext::LIMIT_TITLE_USAGE,
    }
    .format(&[("agent", agent)]);
    let status = info.and_then(|i| reset_clock(i.reset_at)).map_or_else(
        || ctext::LIMIT_RESET_UNKNOWN.get().to_owned(),
        |time| {
            if scheduled {
                ctext::LIMIT_CONTINUES_AT.format(&[("time", &time)])
            } else {
                ctext::LIMIT_CAN_CONTINUE_AT.format(&[("time", &time)])
            }
        },
    );
    (title, status)
}

/// The reset in the reader's clock, as Orca's message times read (`오후 6:18`).
pub(super) fn reset_clock(epoch: u64) -> Option<String> {
    let epoch = i64::try_from(epoch).ok()?;
    let now = i64::try_from(display::now()).ok()?;
    krill_app::chat_message_time(epoch, now).map(|(short, _)| short)
}

/// A scheduled "continue" prompt for one chat after its limit resets.
/// Dropping the entry drops the timer task, which cancels it.
pub(super) struct AutoContinue {
    pub reset_at: u64,
    account_id: String,
    cut_id: u64,
    _task: Task<()>,
}

/// Why auto-arming stays off for a chat until its rate limit clears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Declined {
    /// Cancelled or answered by hand for the cycle ending at this reset.
    Cycle(u64),
    /// Already resumed once; another limit needs the user's decision.
    Episode,
}

impl Shell {
    #[cfg_attr(
        not(test),
        expect(clippy::unused_self, reason = "tests override the clock on self")
    )]
    pub(super) fn chat_now_secs(&self) -> u64 {
        #[cfg(test)]
        if let Some(now) = self.chat.now_override {
            return now;
        }
        display::now()
    }

    fn ensure_rate_limit_ledger_loaded(&mut self, cx: &gpui::App) {
        let Some(dir) = self.state_dir(cx) else {
            return;
        };
        if self.chat.rate_limit_ledger.loaded_dir.as_deref() == Some(dir.as_path()) {
            return;
        }
        self.chat.rate_limit_ledger.ledger = RateLimitLedger::load(&dir);
        self.chat.rate_limit_ledger.loaded_dir = Some(dir);
    }

    fn save_rate_limit_ledger(&self, cx: &gpui::App) {
        if let Some(dir) = self.state_dir(cx) {
            self.chat.rate_limit_ledger.ledger.save(&dir);
        }
    }

    fn chat_account_id(&self, key: &ChatKey, cx: &gpui::App) -> Option<String> {
        self.sidebar
            .read(cx)
            .session_on(&key.0, key.1)?
            .account
            .as_ref()
            .map(|a| a.account_id.clone())
    }

    /// A chat stopped on its usage limit, for the sidebar's gauge (Orca `UsageLimitStop`):
    /// whether Krill continues it at the reset, and the reset's local clock. Only chats whose
    /// log has been read know.
    pub(in crate::shell) fn chat_usage_stop(
        &self,
        key: &ChatKey,
        cx: &gpui::App,
    ) -> Option<(bool, Option<String>)> {
        if !self.chat.logs.get(key)?.meta.rate_limited {
            return None;
        }
        let auto = self.chat.auto_continue.get(key).map(|e| e.reset_at);
        let reset = auto.or_else(|| self.chat_limit_info(key, cx).map(|i| i.reset_at));
        Some((auto.is_some(), reset.and_then(reset_clock)))
    }

    /// The limit blocking this chat's current account, from the host's account snapshot.
    pub(super) fn chat_limit_info(&self, key: &ChatKey, cx: &gpui::App) -> Option<LimitInfo> {
        let account = self.chat_account_id(key, cx)?;
        limit_info(
            &self.accounts_for_host(&key.0),
            &account,
            self.chat_now_secs(),
        )
    }

    /// Schedules the continue prompt for the next reset. Returns whether one is scheduled.
    pub(super) fn arm_auto_continue(&mut self, key: &ChatKey, cx: &mut Context<Self>) -> bool {
        self.ensure_rate_limit_ledger_loaded(cx);
        let now = self.chat_now_secs();
        let Some(account_id) = self.chat_account_id(key, cx) else {
            return false;
        };
        let Some(info) = self.chat_limit_info(key, cx) else {
            return false;
        };
        if info.reset_at.saturating_sub(now) > RESET_HORIZON_SECS {
            return false;
        }
        let hk = host_key(&key.0);
        let cut_id = self.chat.rate_limit_ledger.ledger.arm_cut(
            &hk,
            key.1.0,
            Some(account_id.clone()),
            Some(info.reset_at),
            now,
        );
        self.save_rate_limit_ledger(cx);
        self.chat.auto_continue_declined.remove(key);
        if self
            .chat
            .auto_continue
            .get(key)
            .is_some_and(|e| e.reset_at == info.reset_at && e.account_id == account_id)
        {
            return true;
        }
        let wait = Duration::from_secs(info.reset_at.saturating_sub(now) + RESUME_GRACE_SECS);
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| this.fire_auto_continue(&task_key, cx))
                .ok();
        });
        self.chat.auto_continue.insert(
            key.clone(),
            AutoContinue {
                reset_at: info.reset_at,
                account_id,
                cut_id,
                _task: task,
            },
        );
        cx.notify();
        true
    }

    /// Banner Cancel, a hand-sent prompt or an account switch: drop the timer for this cycle.
    pub(super) fn cancel_auto_continue(&mut self, key: &ChatKey, cx: &mut Context<Self>) {
        self.ensure_rate_limit_ledger_loaded(cx);
        let hk = host_key(&key.0);
        self.chat
            .rate_limit_ledger
            .last_cancel_at
            .insert(key.clone(), Instant::now());
        if self.chat.rate_limit_ledger.ledger.mark_disposition(
            &hk,
            key.1.0,
            CutDisposition::Cancelled,
        ) {
            self.save_rate_limit_ledger(cx);
        }
        if let Some(entry) = self.chat.auto_continue.remove(key) {
            self.chat
                .auto_continue_declined
                .insert(key.clone(), Declined::Cycle(entry.reset_at));
            cx.notify();
        }
    }

    /// Permanently clears any pending usage-limit cut when the user submits a prompt by hand.
    pub(super) fn mark_auto_continue_resolved_by_user(
        &mut self,
        key: &ChatKey,
        cx: &mut Context<Self>,
    ) {
        self.ensure_rate_limit_ledger_loaded(cx);
        self.chat.rate_limit_ledger.last_cancel_at.remove(key);
        let hk = host_key(&key.0);
        if self.chat.rate_limit_ledger.ledger.remove(&hk, key.1.0) {
            self.save_rate_limit_ledger(cx);
        }
    }

    /// Preserves a pending usage-limit cut across an account switch triggered from the rate-limit
    /// banner or sidebar so the switched account can continue early once headroom is confirmed.
    pub(in crate::shell) fn on_chat_account_switch_requested(
        &mut self,
        key: &ChatKey,
        target_account_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.ensure_rate_limit_ledger_loaded(cx);
        let hk = host_key(&key.0);
        let just_cancelled_by_banner = self
            .chat
            .rate_limit_ledger
            .last_cancel_at
            .remove(key)
            .is_some_and(|at| at.elapsed() <= BANNER_SWITCH_CANCEL_WINDOW);
        let rate_limited = self
            .chat
            .logs
            .get(key)
            .is_some_and(|log| log.meta.rate_limited);
        let now = self.chat_now_secs();
        let current_account = self.chat_account_id(key, cx);
        let reset_at = self.chat_limit_info(key, cx).map(|i| i.reset_at);

        let mut changed = false;
        if let Some(rec) = self.chat.rate_limit_ledger.ledger.get_mut(&hk, key.1.0) {
            if rec.disposition == CutDisposition::Cancelled && just_cancelled_by_banner {
                rec.disposition = CutDisposition::Pending;
                changed = true;
            }
            if rec.disposition == CutDisposition::Pending
                && rec.limited_account_id.as_deref() != Some(target_account_id)
            {
                rec.relaunched = true;
                self.chat.auto_continue_declined.remove(key);
                changed = true;
            }
        } else if rate_limited && current_account.as_deref() != Some(target_account_id) {
            changed |= self.chat.rate_limit_ledger.ledger.record_pending_cut(
                &hk,
                key.1.0,
                current_account,
                reset_at,
                now,
            );
            if let Some(rec) = self.chat.rate_limit_ledger.ledger.get_mut(&hk, key.1.0) {
                rec.relaunched = true;
            }
        }
        if changed {
            self.save_rate_limit_ledger(cx);
        }
    }

    /// Migrates a persisted usage-limit cut when a session account handoff replaces `from` with `to`.
    pub(in crate::shell) fn on_chat_session_rebound(
        &mut self,
        host: &HostId,
        from: SessionId,
        to: SessionId,
        cx: &mut Context<Self>,
    ) {
        self.ensure_rate_limit_ledger_loaded(cx);
        let from_key = (host.clone(), from);
        let to_key = (host.clone(), to);
        self.chat.auto_continue.remove(&from_key);
        self.chat.auto_continue_declined.remove(&from_key);
        self.chat.auto_continue_declined.remove(&to_key);
        let hk = host_key(host);
        if self
            .chat
            .rate_limit_ledger
            .ledger
            .rebind_session(&hk, from.0, to.0)
        {
            self.save_rate_limit_ledger(cx);
        }
    }

    /// Re-evaluates usage-limit auto-continues across all chat sessions on `host` after an
    /// `AccountsSnapshot` update (catches early continues on account switch and post-restart recovery).
    pub(in crate::shell) fn sync_auto_continue_for_host(
        &mut self,
        host: &HostId,
        cx: &mut Context<Self>,
    ) {
        self.ensure_rate_limit_ledger_loaded(cx);
        let hk = host_key(host);
        let mut keys: Vec<ChatKey> = self
            .chat
            .logs
            .keys()
            .filter(|k| &k.0 == host)
            .cloned()
            .collect();
        for rec in &self.chat.rate_limit_ledger.ledger.cuts {
            if rec.host == hk && rec.disposition == CutDisposition::Pending {
                let key = (host.clone(), SessionId(rec.session_id));
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        for key in keys {
            self.sync_auto_continue(&key, cx);
        }
    }

    /// Re-evaluates the schedule after the composer status (and `rate_limited`) or accounts refreshed.
    pub(super) fn sync_auto_continue(&mut self, key: &ChatKey, cx: &mut Context<Self>) {
        self.ensure_rate_limit_ledger_loaded(cx);
        let hk = host_key(&key.0);
        let rate_limited = self
            .chat
            .logs
            .get(key)
            .is_some_and(|log| log.meta.rate_limited);
        let now = self.chat_now_secs();
        let current_account = self.chat_account_id(key, cx);
        if let Some(log) = self.chat.logs.get_mut(key) {
            if rate_limited {
                if log.rate_limited_account.is_none() {
                    log.rate_limited_account.clone_from(&current_account);
                }
            } else {
                log.rate_limited_account = None;
                log.rate_limit_account_switched = false;
            }
        }
        let snap = self.accounts_for_host(&key.0);
        let current_info = current_account
            .as_deref()
            .and_then(|acct| limit_info(&snap, acct, now));

        if rate_limited {
            let changed = self.chat.rate_limit_ledger.ledger.record_pending_cut(
                &hk,
                key.1.0,
                current_account.clone(),
                current_info.map(|i| i.reset_at),
                now,
            );
            if changed {
                self.save_rate_limit_ledger(cx);
            }
        }

        // Check persisted pending cut for early continue (switched account with fresh headroom)
        // or overdue reset (e.g., reset elapsed while the app/daemon was closed/restarting).
        if let Some(record) = self
            .chat
            .rate_limit_ledger
            .ledger
            .get(&hk, key.1.0)
            .cloned()
            && record.disposition == CutDisposition::Pending
        {
            if krill_app::settings::chat_auto_continue_after_rate_limit(cx) {
                let early = should_continue_early_on_account_switch(
                    &record,
                    current_account.as_deref(),
                    &snap,
                    now,
                );
                let reset_due =
                    should_continue_at_reset(&record, current_account.as_deref(), &snap, now);
                if early || reset_due {
                    self.chat.auto_continue.remove(key);
                    self.dispatch_continue_once(key, record.cut_id, cx);
                    return;
                }
            } else if self.chat.auto_continue.contains_key(key) {
                // Keep manual clock-armed timers running; only auto-arming checks the setting.
            }
        }

        if let Some(entry) = self.chat.auto_continue.get(key) {
            // Cleared before the reset means someone resolved it; after the reset the timer is due.
            if current_account.as_deref() != Some(entry.account_id.as_str())
                || (!rate_limited && now < entry.reset_at)
            {
                self.chat.auto_continue.remove(key);
                cx.notify();
            }
            return;
        }
        if !rate_limited {
            self.chat.auto_continue_declined.remove(key);
            return;
        }
        if !krill_app::settings::chat_auto_continue_after_rate_limit(cx) {
            return;
        }
        if let Some(rec) = self.chat.rate_limit_ledger.ledger.get(&hk, key.1.0)
            && rec.disposition != CutDisposition::Pending
        {
            return;
        }
        let declined = self.chat.auto_continue_declined.get(key).copied();
        let reset_at = current_info.map(|i| i.reset_at);
        if declined == Some(Declined::Episode)
            || reset_at.is_some_and(|at| declined == Some(Declined::Cycle(at)))
        {
            return;
        }
        self.arm_auto_continue(key, cx);
    }

    /// Orca `#47079`: switching accounts on a usage-limited chat makes it resumable immediately
    /// even when the original provider reset time is unknown.
    pub(super) fn rate_limit_ready_after_switch(&self, key: &ChatKey, cx: &gpui::App) -> bool {
        let Some(log) = self.chat.logs.get(key) else {
            return false;
        };
        if !log.meta.rate_limited {
            return false;
        }
        if log.rate_limit_account_switched {
            return true;
        }
        let current = self.chat_account_id(key, cx);
        matches!(
            (&log.rate_limited_account, current),
            (Some(prev), Some(curr)) if prev != &curr
        )
    }

    /// Sends the manual continue prompt after an account switch or reset (`#47079`).
    pub(super) fn continue_after_rate_limit(&mut self, key: &ChatKey, cx: &mut Context<Self>) {
        self.cancel_auto_continue(key, cx);
        let text = ctext::AUTO_CONTINUE_PROMPT.get().to_owned();
        let Some(submitted_payload) = build_submitted_chat_payload(&text, &[]) else {
            return;
        };
        self.chat.next_pending_id = self.chat.next_pending_id.wrapping_add(1);
        let id = self.chat.next_pending_id;
        let log = self.chat.logs.entry(key.clone()).or_default();
        log.meta.rate_limited = false;
        log.rate_limited_account = None;
        log.rate_limit_account_switched = false;
        log.queued_prompts.push_front(QueuedChatPrompt {
            id,
            display_text: text,
            submitted_payload,
            attachments: Vec::new(),
        });
        self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
        self.flush_queued_chat_prompt_if_ready(key, cx);
        cx.notify();
    }

    fn fire_auto_continue(&mut self, key: &ChatKey, cx: &mut Context<Self>) {
        let Some(entry) = self.chat.auto_continue.remove(key) else {
            return;
        };
        cx.notify();
        let snap = self.accounts_for_host(&key.0);
        let current_account = self.chat_account_id(key, cx);
        if current_account.as_deref() != Some(entry.account_id.as_str()) {
            // Account changed before timer fired; re-evaluate early continue on the new account.
            self.sync_auto_continue(key, cx);
            return;
        }
        if snap
            .status_for(&entry.account_id)
            .is_some_and(|s| s.status == AccountStatus::NeedsLogin)
        {
            return;
        }
        // Another full window (say weekly behind a 5h reset) still blocks: wait for that one.
        if limit_info(&snap, &entry.account_id, self.chat_now_secs()).is_some_and(|i| i.full) {
            self.arm_auto_continue(key, cx);
            return;
        }
        self.dispatch_continue_once(key, entry.cut_id, cx);
    }

    /// Sends the single auto-continue prompt for `cut_id` after verifying pane readiness, and
    /// persists `CutDisposition::Continued` BEFORE delivery so restarts/re-syncs never double-send.
    fn dispatch_continue_once(&mut self, key: &ChatKey, cut_id: u64, cx: &mut Context<Self>) {
        self.ensure_rate_limit_ledger_loaded(cx);
        let hk = host_key(&key.0);
        if let Some(rec) = self.chat.rate_limit_ledger.ledger.get(&hk, key.1.0)
            && (rec.cut_id != cut_id || rec.disposition != CutDisposition::Pending)
        {
            return;
        }
        let Some(pane) = self.chat_pane(key) else {
            return;
        };
        if !self.is_pane_chat_view(pane, cx)
            || self
                .live
                .get(&pane)
                .is_some_and(|live| live.pane.account_switching())
            || self.session_agent_state(&key.0, key.1, cx) == AgentState::Exited
        {
            return;
        }
        let text = ctext::AUTO_CONTINUE_PROMPT.get().to_owned();
        let Some(submitted_payload) = build_submitted_chat_payload(&text, &[]) else {
            return;
        };
        // Persist `Continued` BEFORE sending so a crash or duplicate trigger never double-sends.
        if self.chat.rate_limit_ledger.ledger.mark_disposition(
            &hk,
            key.1.0,
            CutDisposition::Continued,
        ) {
            self.save_rate_limit_ledger(cx);
        }
        self.chat.next_pending_id = self.chat.next_pending_id.wrapping_add(1);
        let id = self.chat.next_pending_id;
        self.chat
            .logs
            .entry(key.clone())
            .or_default()
            .queued_prompts
            .push_front(QueuedChatPrompt {
                id,
                display_text: text,
                submitted_payload,
                attachments: Vec::new(),
            });
        self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
        self.chat
            .auto_continue_declined
            .insert(key.clone(), Declined::Episode);
        self.flush_queued_chat_prompt_if_ready(key, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::usage::{
        AccountInfo, AccountMeters, AccountStatusInfo, ProviderKind, RateLimitExpiry, Snapshot,
        UsageMeter, Window,
    };

    const NOW: u64 = 1_900_000_000;

    fn win(percent: u8, at: u64) -> Window {
        Window {
            percent,
            resets_at: Some(at.to_string()),
        }
    }

    fn acct(id: &str, five: Option<Window>, seven: Option<Window>) -> AccountInfo {
        AccountInfo {
            id: id.into(),
            provider: ProviderKind::Claude,
            label: id.into(),
            email: None,
            profile_dir: format!("/synthetic/{id}"),
            active: id == "main",
            host_default: false,
            stale: false,
            usage: Snapshot {
                five_hour: five,
                seven_day: seven,
            },
        }
    }

    fn snap(five: Option<Window>, seven: Option<Window>) -> AccountsSnapshot {
        AccountsSnapshot {
            accounts: vec![acct("main", five, seven)],
            ..AccountsSnapshot::default()
        }
    }

    #[test]
    fn full_windows_pick_the_latest_reset_and_others_the_most_used() {
        let five_full = snap(Some(win(100, NOW + 600)), Some(win(40, NOW + 86_000)));
        assert_eq!(
            limit_info(&five_full, "main", NOW),
            Some(LimitInfo {
                kind: LimitKind::FiveHour,
                reset_at: NOW + 600,
                full: true
            })
        );
        let both_full = snap(Some(win(100, NOW + 600)), Some(win(100, NOW + 7_200)));
        let info = limit_info(&both_full, "main", NOW).unwrap();
        assert_eq!((info.kind, info.reset_at), (LimitKind::Weekly, NOW + 7_200));
        let none_full = snap(Some(win(30, NOW + 600)), Some(win(90, NOW + 7_200)));
        let info = limit_info(&none_full, "main", NOW).unwrap();
        assert_eq!((info.kind, info.full), (LimitKind::Weekly, false));
        // Past resets do not count; unknown accounts have nothing.
        let past = snap(Some(win(100, NOW - 1)), None);
        assert_eq!(limit_info(&past, "main", NOW), None);
        assert_eq!(limit_info(&five_full, "other", NOW), None);
    }

    #[test]
    fn meters_and_daemon_expiry_fill_in_when_windows_are_missing() {
        let mut s = snap(None, None);
        s.details.accounts.push(AccountMeters {
            account_id: "main".into(),
            plan: None,
            meters: vec![UsageMeter {
                key: "extra_usage".into(),
                label: "Extra".into(),
                window: Some(win(100, NOW + 900)),
                value: None,
            }],
        });
        assert_eq!(
            limit_info(&s, "main", NOW).map(|i| i.kind),
            Some(LimitKind::Extra)
        );
        let mut s = snap(None, None);
        s.advanced.rate_limits.push(RateLimitExpiry {
            account_id: "main".into(),
            clears_at: NOW + 300,
        });
        assert_eq!(
            limit_info(&s, "main", NOW),
            Some(LimitInfo {
                kind: LimitKind::Generic,
                reset_at: NOW + 300,
                full: true
            })
        );
    }

    #[test]
    fn banner_names_the_agent_limit_and_when_it_can_continue() {
        let reset_at = NOW + 3_600;
        let info = limit_info(&snap(None, Some(win(100, reset_at))), "main", NOW);
        let clock = reset_clock(reset_at).unwrap();
        assert_eq!(
            banner_words(info, "claude", false),
            (
                "Claude 주간 한도에 도달했습니다".to_owned(),
                format!("{clock}에 계속 가능")
            )
        );
        assert_eq!(
            banner_words(info, "claude", true).1,
            format!("{clock}에 계속")
        );
        assert_eq!(
            banner_words(None, "mystery", false),
            (
                "에이전트 사용량 한도에 도달했습니다".to_owned(),
                "초기화 시각 알 수 없음".to_owned()
            )
        );
    }

    #[test]
    fn early_continue_fires_on_account_switch_with_fresh_headroom_and_respects_blocked_targets() {
        let mut multi = AccountsSnapshot {
            accounts: vec![
                acct("main", Some(win(100, NOW + 3_600)), None),
                acct("fresh", Some(win(20, NOW + 3_600)), None),
                acct("also_full", Some(win(100, NOW + 7_200)), None),
                acct("logged_out", None, None),
            ],
            ..AccountsSnapshot::default()
        };
        multi.statuses.push(AccountStatusInfo {
            account_id: "logged_out".into(),
            status: AccountStatus::NeedsLogin,
            updated_at: Some(NOW),
            detail: None,
        });

        let cut = RateLimitCutRecord {
            host: "local".into(),
            session_id: 7,
            cut_id: 1,
            limited_account_id: Some("main".into()),
            reset_at: Some(NOW + 3_600),
            recorded_at: NOW,
            relaunched: false,
            disposition: CutDisposition::Pending,
        };

        // Same limited account stays blocked until reset.
        assert!(!should_continue_early_on_account_switch(
            &cut,
            Some("main"),
            &multi,
            NOW
        ));
        // Switching to an account that is also at 100% or NeedsLogin does NOT continue early.
        assert!(!should_continue_early_on_account_switch(
            &cut,
            Some("also_full"),
            &multi,
            NOW
        ));
        assert!(!should_continue_early_on_account_switch(
            &cut,
            Some("logged_out"),
            &multi,
            NOW
        ));
        // Switching to a different account with fresh headroom immediately allows continuing early!
        assert!(should_continue_early_on_account_switch(
            &cut,
            Some("fresh"),
            &multi,
            NOW
        ));

        // Unknown reset + relaunched onto an account with fresh headroom also continues early.
        let unknown_reset_relaunched = RateLimitCutRecord {
            limited_account_id: None,
            reset_at: None,
            relaunched: true,
            ..cut.clone()
        };
        assert!(should_continue_early_on_account_switch(
            &unknown_reset_relaunched,
            Some("fresh"),
            &multi,
            NOW
        ));

        // Once marked Continued or Cancelled, it must NEVER continue again.
        let continued = RateLimitCutRecord {
            disposition: CutDisposition::Continued,
            ..cut
        };
        assert!(!should_continue_early_on_account_switch(
            &continued,
            Some("fresh"),
            &multi,
            NOW
        ));
    }

    #[test]
    fn ledger_persists_across_restart_rebinds_sessions_and_prevents_double_continue() {
        let dir = std::env::temp_dir().join(format!(
            "krill-rate-limit-ledger-test-{}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        let mut ledger = RateLimitLedger::default();
        assert!(ledger.record_pending_cut("local", 42, Some("main".into()), Some(NOW + 60), NOW));
        ledger.save(&dir);

        // Simulated restart loads the exact cut record.
        let mut reloaded = RateLimitLedger::load(&dir);
        let rec = reloaded.get("local", 42).unwrap();
        assert_eq!(rec.limited_account_id.as_deref(), Some("main"));
        assert_eq!(rec.reset_at, Some(NOW + 60));
        assert_eq!(rec.disposition, CutDisposition::Pending);

        // Rebind across account handoff carries the cut and marks `relaunched = true`.
        assert!(reloaded.rebind_session("local", 42, 99));
        assert!(reloaded.get("local", 42).is_none());
        let moved = reloaded.get("local", 99).unwrap();
        assert!(moved.relaunched);
        assert_eq!(moved.limited_account_id.as_deref(), Some("main"));

        // Mark Continued and reload: subsequent record_pending_cut must NOT revive it.
        assert!(reloaded.mark_disposition("local", 99, CutDisposition::Continued));
        reloaded.save(&dir);

        let mut after_second_restart = RateLimitLedger::load(&dir);
        assert!(!after_second_restart.record_pending_cut(
            "local",
            99,
            Some("main".into()),
            Some(NOW + 60),
            NOW + 70
        ));
        assert_eq!(
            after_second_restart.get("local", 99).unwrap().disposition,
            CutDisposition::Continued
        );

        fs::remove_dir_all(&dir).ok();
    }
}
