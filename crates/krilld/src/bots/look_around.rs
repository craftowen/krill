// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Proactive "Look around" scanner, durable scheduler state (`scheduler-state.json`),
//! dismissal cooldowns, timezone-aware cron calendar calculations, and `Up Next` helpers.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, DirBuilder};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use krill_proto::bots::{
    BotAccessRequestStatus, BotId, BotItemLink, BotItemStatus, BotSuggestion,
    DEFAULT_LOOK_AROUND_MAX_SUGGESTIONS, ExternalSource, soul_description,
};
use krill_proto::os::fs::DirBuilderExt as _;
use krill_proto::os::write_atomic_0600;
use serde::{Deserialize, Serialize};

use crate::bots::items::BotItemsStore;
use crate::bots::secrets::ensure_no_symlink_path;
use crate::bots::store::BotStore;
use crate::session::lock;

const SCHEDULER_STATE_FILE: &str = "scheduler-state.json";
const MAX_SCHEDULER_STATE_BYTES: usize = 2 * 1024 * 1024;
const STALE_WORKING_MS: u64 = 24 * 60 * 60 * 1_000;
const SUGGESTION_DISMISS_COOLDOWN_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const MAX_CRON_CLAIMED_ENTRIES: usize = 2048;

static SUGG_SEQ: AtomicU64 = AtomicU64::new(1);

/// Persistent per-bot state for scheduled Memory Tidy runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedMemoryTidyState {
    #[serde(default)]
    pub last_run_ms: Option<u64>,
    #[serde(default)]
    pub last_tidy_id: Option<String>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub auto_disabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedSchedulerState {
    #[serde(default)]
    look_around_last_run_ms: BTreeMap<BotId, u64>,
    #[serde(default)]
    up_next_seen_at_ms: u64,
    #[serde(default)]
    up_next_seen_by_bot_ms: BTreeMap<BotId, u64>,
    #[serde(default)]
    dismissed_suggestions: BTreeMap<BotId, BTreeMap<String, u64>>,
    #[serde(default)]
    memory_tidy: BTreeMap<BotId, PersistedMemoryTidyState>,
    #[serde(default)]
    cron_claimed_minute: BTreeMap<String, u64>,
}

/// Strictly-lazy durable scheduler state store (`<bots_root>/scheduler-state.json`).
#[derive(Debug)]
pub struct SchedulerStateStore {
    bots_root: PathBuf,
    path: PathBuf,
    io_lock: Mutex<()>,
    cached: Mutex<Option<PersistedSchedulerState>>,
}

impl SchedulerStateStore {
    /// Creates a lazy [`SchedulerStateStore`] doing zero disk I/O until first read/write.
    #[must_use]
    pub fn new(bots_root: PathBuf) -> Self {
        let path = bots_root.join(SCHEDULER_STATE_FILE);
        Self {
            bots_root,
            path,
            io_lock: Mutex::new(()),
            cached: Mutex::new(None),
        }
    }

    pub fn look_around_is_due(
        &self,
        bot_id: &BotId,
        interval_secs: u64,
        weekdays_only: bool,
        tz: Option<&str>,
        now_ms: u64,
    ) -> bool {
        let secs = now_ms / 1_000;
        if weekdays_only && is_weekend_in_tz(secs, tz) {
            return false;
        }
        let interval_ms = interval_secs.max(60).saturating_mul(1_000);
        self.with_state_read(|st| {
            st.look_around_last_run_ms
                .get(bot_id)
                .is_none_or(|&last| now_ms.saturating_sub(last) >= interval_ms)
        })
        .unwrap_or(false)
    }

    pub fn record_look_around(&self, bot_id: &BotId, now_ms: u64) -> io::Result<()> {
        self.with_state_mut(|st| {
            st.look_around_last_run_ms.insert(bot_id.clone(), now_ms);
            Ok(())
        })
    }

    pub fn record_suggestion_dismissed(
        &self,
        bot_id: &BotId,
        suggestion: &BotSuggestion,
        now_ms: u64,
    ) -> io::Result<()> {
        let key = suggestion_stable_key(&suggestion.title, &suggestion.why, suggestion.source);
        self.with_state_mut(|st| {
            st.dismissed_suggestions
                .entry(bot_id.clone())
                .or_default()
                .insert(key, now_ms);
            Ok(())
        })
    }

    #[must_use]
    pub fn is_suggestion_suppressed(
        &self,
        bot_id: &BotId,
        title: &str,
        why: &str,
        source: Option<ExternalSource>,
        now_ms: u64,
    ) -> bool {
        let key = suggestion_stable_key(title, why, source);
        self.with_state_read(|st| {
            st.dismissed_suggestions
                .get(bot_id)
                .and_then(|map| map.get(&key))
                .is_some_and(|&dismissed_at| {
                    now_ms.saturating_sub(dismissed_at) < SUGGESTION_DISMISS_COOLDOWN_MS
                })
        })
        .unwrap_or(false)
    }

    pub fn mark_up_next_seen(&self, bot_id: Option<&BotId>, now_ms: u64) -> io::Result<()> {
        self.with_state_mut(|st| {
            if let Some(id) = bot_id {
                let entry = st.up_next_seen_by_bot_ms.entry(id.clone()).or_insert(0);
                *entry = (*entry).max(now_ms);
            } else {
                st.up_next_seen_at_ms = st.up_next_seen_at_ms.max(now_ms);
            }
            Ok(())
        })
    }

    #[must_use]
    pub fn up_next_seen_at(&self, bot_id: &BotId) -> u64 {
        self.with_state_read(|st| {
            st.up_next_seen_by_bot_ms
                .get(bot_id)
                .copied()
                .unwrap_or(0)
                .max(st.up_next_seen_at_ms)
        })
        .unwrap_or(0)
    }

    #[must_use]
    pub fn tidy_is_due(&self, bot_id: &BotId, interval_secs: u64, now_ms: u64) -> bool {
        let interval_ms = interval_secs.max(60).saturating_mul(1_000);
        self.with_state_read(|st| {
            let Some(rec) = st.memory_tidy.get(bot_id) else {
                return true;
            };
            if rec.auto_disabled {
                return false;
            }
            rec.last_run_ms
                .is_none_or(|last| now_ms.saturating_sub(last) >= interval_ms)
        })
        .unwrap_or(false)
    }

    pub fn record_tidy_success(
        &self,
        bot_id: &BotId,
        tidy_id: Option<String>,
        now_ms: u64,
    ) -> io::Result<()> {
        self.with_state_mut(|st| {
            let rec = st.memory_tidy.entry(bot_id.clone()).or_default();
            rec.last_run_ms = Some(now_ms);
            if let Some(id) = tidy_id {
                rec.last_tidy_id = Some(id);
            }
            rec.consecutive_failures = 0;
            rec.auto_disabled = false;
            Ok(())
        })
    }

    pub fn record_tidy_failure(
        &self,
        bot_id: &BotId,
        now_ms: u64,
    ) -> io::Result<PersistedMemoryTidyState> {
        self.with_state_mut(|st| {
            let rec = st.memory_tidy.entry(bot_id.clone()).or_default();
            rec.last_run_ms = Some(now_ms);
            rec.consecutive_failures = rec.consecutive_failures.saturating_add(1);
            if rec.consecutive_failures >= crate::bots::memory_tidy::AUTO_DISABLE_AFTER_FAILURES {
                rec.auto_disabled = true;
            }
            Ok(rec.clone())
        })
    }

    pub fn get_tidy_state(&self, bot_id: &BotId) -> io::Result<PersistedMemoryTidyState> {
        self.with_state_read(|st| st.memory_tidy.get(bot_id).cloned().unwrap_or_default())
    }

    pub fn clear_tidy_undo_id(&self, bot_id: &BotId, tidy_id: &str) -> io::Result<()> {
        self.with_state_mut(|st| {
            if let Some(rec) = st.memory_tidy.get_mut(bot_id)
                && rec.last_tidy_id.as_deref() == Some(tidy_id)
            {
                rec.last_tidy_id = None;
            }
            Ok(())
        })
    }

    pub fn reset_tidy_auto_disabled(&self, bot_id: &BotId) -> io::Result<()> {
        self.with_state_mut(|st| {
            if let Some(rec) = st.memory_tidy.get_mut(bot_id) {
                rec.consecutive_failures = 0;
                rec.auto_disabled = false;
            }
            Ok(())
        })
    }

    /// Claims `(bot_id, sched_id)` for `minute` persistently so restarts inside the same minute do
    /// not re-trigger the same cron firing.
    pub fn try_claim_cron_minute(
        &self,
        bot_id: &BotId,
        sched_id: &str,
        minute: u64,
    ) -> io::Result<bool> {
        let key = format!("{}:{sched_id}", bot_id.as_str());
        self.with_state_mut(|st| {
            if st.cron_claimed_minute.get(&key).copied() == Some(minute) {
                return Ok(false);
            }
            st.cron_claimed_minute.insert(key, minute);
            if st.cron_claimed_minute.len() > MAX_CRON_CLAIMED_ENTRIES {
                let cutoff = minute.saturating_sub(120);
                st.cron_claimed_minute.retain(|_, m| *m >= cutoff);
            }
            Ok(true)
        })
    }

    /// Removes all persisted state for a deleted bot.
    pub fn forget_bot(&self, bot_id: &BotId) -> io::Result<()> {
        let prefix = format!("{}:", bot_id.as_str());
        self.with_state_mut(|st| {
            st.look_around_last_run_ms.remove(bot_id);
            st.up_next_seen_by_bot_ms.remove(bot_id);
            st.dismissed_suggestions.remove(bot_id);
            st.memory_tidy.remove(bot_id);
            st.cron_claimed_minute
                .retain(|k, _| !k.starts_with(&prefix));
            Ok(())
        })
    }

    fn with_state_read<T>(&self, f: impl FnOnce(&PersistedSchedulerState) -> T) -> io::Result<T> {
        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let guard = lock(&self.cached);
        let st = guard
            .as_ref()
            .ok_or_else(|| io::Error::other("missing state"))?;
        Ok(f(st))
    }

    fn with_state_mut<T>(
        &self,
        f: impl FnOnce(&mut PersistedSchedulerState) -> io::Result<T>,
    ) -> io::Result<T> {
        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let mut guard = lock(&self.cached);
        let st = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("missing state"))?;
        let before = st.clone();
        let out = f(st)?;
        if *st != before {
            self.write_state_locked(st)?;
        }
        Ok(out)
    }

    fn ensure_loaded_locked(&self) -> io::Result<()> {
        if lock(&self.cached).is_some() {
            return Ok(());
        }
        ensure_no_symlink_path(&self.bots_root)?;
        ensure_no_symlink_path(&self.path)?;
        let loaded = match fs::read(&self.path) {
            Ok(bytes) => {
                if bytes.len() > MAX_SCHEDULER_STATE_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "scheduler state exceeds size limit",
                    ));
                }
                serde_json::from_slice::<PersistedSchedulerState>(&bytes).unwrap_or_default()
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => PersistedSchedulerState::default(),
            Err(err) => return Err(err),
        };
        *lock(&self.cached) = Some(loaded);
        Ok(())
    }

    fn write_state_locked(&self, state: &PersistedSchedulerState) -> io::Result<()> {
        ensure_no_symlink_path(&self.bots_root)?;
        ensure_no_symlink_path(&self.path)?;
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.bots_root)?;
        let mut bytes = serde_json::to_vec_pretty(state)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        bytes.push(b'\n');
        write_atomic_0600(&self.path, &bytes)
    }
}

/// Legacy in-memory Look Around tracker (kept for standalone calls/tests without [`SchedulerStateStore`]).
#[derive(Debug, Default)]
pub struct LookAroundTracker {
    last_run_ms: Mutex<HashMap<BotId, u64>>,
}

impl LookAroundTracker {
    #[must_use]
    pub fn is_due(
        &self,
        bot_id: &BotId,
        interval_secs: u64,
        weekdays_only: bool,
        now_ms: u64,
    ) -> bool {
        if weekdays_only && is_weekend_utc(now_ms / 1_000) {
            return false;
        }
        let interval_ms = interval_secs.max(60).saturating_mul(1_000);
        let guard = lock(&self.last_run_ms);
        guard
            .get(bot_id)
            .is_none_or(|&last| now_ms.saturating_sub(last) >= interval_ms)
    }

    pub fn record(&self, bot_id: &BotId, now_ms: u64) {
        lock(&self.last_run_ms).insert(bot_id.clone(), now_ms);
    }
}

/// Runs a proactive Look Around scan for `bot_id`, persisting and returning active suggestions.
pub fn run_look_around(
    store: &BotStore,
    items: &BotItemsStore,
    bot_id: &BotId,
    now_ms: u64,
) -> io::Result<Vec<BotSuggestion>> {
    run_look_around_tracked(store, items, None, bot_id, now_ms)
}

/// Runs a proactive Look Around scan honoring durable [`SchedulerStateStore`] dismissals.
pub fn run_look_around_tracked(
    store: &BotStore,
    items: &BotItemsStore,
    scheduler_state: Option<&SchedulerStateStore>,
    bot_id: &BotId,
    now_ms: u64,
) -> io::Result<Vec<BotSuggestion>> {
    let bot = store.get_bot(bot_id)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("bot not found: {}", bot_id.as_str()),
        )
    })?;

    let cap = usize::try_from(if bot.look_around.max_suggestions == 0 {
        DEFAULT_LOOK_AROUND_MAX_SUGGESTIONS
    } else {
        bot.look_around.max_suggestions.min(20)
    })
    .unwrap_or(5);

    let previous = store.load_suggestions(bot_id).unwrap_or_default();
    let mut existing_dismissed: HashMap<String, BotSuggestion> = previous
        .into_iter()
        .filter(|s| s.dismissed)
        .map(|s| {
            let key = suggestion_stable_key(&s.title, &s.why, s.source);
            (key, s)
        })
        .collect();

    let mut candidates = Vec::new();
    let focus_hint = bot.look_around.focus.trim();

    // 1. Scan open bot items (NeedsYou decisions, stale Working items, Planned queue).
    let open_items = items.list_items(Some(bot_id), None, false, false, now_ms)?;
    for item in &open_items {
        let pr_link = item.links.iter().find_map(|l| match l {
            BotItemLink::Pr(url) => Some(url.clone()),
            _ => None,
        });
        let url_link = pr_link.clone().or_else(|| {
            item.links.iter().find_map(|l| match l {
                BotItemLink::Url(url) => Some(url.clone()),
                _ => None,
            })
        });

        match item.status {
            BotItemStatus::NeedsYou => {
                candidates.push(make_suggestion(
                    bot_id,
                    format!("Unblock #{}: {}", item.number, item.title),
                    "This item is waiting on an owner decision before the bot can continue."
                        .to_owned(),
                    format!("Review item #{} and reply with your decision.", item.number),
                    url_link,
                    pr_link.map(|_| ExternalSource::GitHub),
                    now_ms,
                ));
            }
            BotItemStatus::Working
                if now_ms.saturating_sub(item.last_activity_at_ms) >= STALE_WORKING_MS =>
            {
                candidates.push(make_suggestion(
                    bot_id,
                    format!("Check stale work on #{}: {}", item.number, item.title),
                    "This item has been marked Working with no recorded activity for over 24 hours."
                        .to_owned(),
                    format!(
                        "Ask {} for a progress update or mark item #{} done/canceled.",
                        bot.name, item.number
                    ),
                    url_link,
                    pr_link.map(|_| ExternalSource::GitHub),
                    now_ms,
                ));
            }
            BotItemStatus::Planned => {
                let why = if let Some(ref pr) = pr_link {
                    format!("Linked pull request ({pr}) is still queued in Planned.")
                } else if !focus_hint.is_empty() {
                    format!("Planned work aligned with focus: {focus_hint}")
                } else {
                    "Queued deliverable ready to start.".to_owned()
                };
                candidates.push(make_suggestion(
                    bot_id,
                    format!("Start #{}: {}", item.number, item.title),
                    why,
                    format!("Kick off {} on item #{}.", bot.name, item.number),
                    url_link,
                    pr_link.map(|_| ExternalSource::GitHub),
                    now_ms,
                ));
            }
            _ => {}
        }
    }

    // 2. Pending external access requests needing owner approval.
    let pending_access: Vec<_> = store
        .list_access_requests(Some(bot_id))?
        .into_iter()
        .filter(|r| r.status == BotAccessRequestStatus::Pending)
        .collect();
    if !pending_access.is_empty() {
        let count = pending_access.len();
        let first = pending_access.first();
        candidates.push(make_suggestion(
            bot_id,
            format!("Review {count} pending access request(s)"),
            format!(
                "{} requested access via {}.",
                first.map_or("An external user", |r| r.sender_name.as_str()),
                first.map_or("connector", |r| r.source.as_str())
            ),
            "Approve or decline pending external senders in Connectors & Access.".to_owned(),
            None,
            first.map(|r| r.source),
            now_ms,
        ));
    }

    // 3. Workspace health check (missing workspace path or dirty git worktree).
    if let Some(ws) = bot.workspace.as_deref().filter(|s| !s.trim().is_empty()) {
        let ws_path = Path::new(ws);
        if !ws_path.is_dir() {
            candidates.push(make_suggestion(
                bot_id,
                "Fix missing bot workspace folder".to_owned(),
                "The configured workspace directory does not exist on this machine.".to_owned(),
                "Update the bot workspace path or recreate the folder.".to_owned(),
                None,
                None,
                now_ms,
            ));
        } else if ws_path.join(".git").exists()
            && let Some(dirty_count) = quick_git_dirty_count(ws_path)
            && dirty_count > 0
        {
            candidates.push(make_suggestion(
                bot_id,
                format!("Review {dirty_count} uncommitted file(s) in workspace"),
                "The bot's configured git workspace has uncommitted changes.".to_owned(),
                format!(
                    "Ask {} to review or summarize local workspace changes.",
                    bot.name
                ),
                None,
                Some(ExternalSource::GitHub),
                now_ms,
            ));
        }
    }

    // 4. Uncustomized starter SOUL.md nudge if still few suggestions.
    if let Ok(soul) = store.get_soul(bot_id)
        && soul_description(&soul).is_none()
    {
        candidates.push(make_suggestion(
            bot_id,
            format!("Personalize {}'s SOUL.md", bot.name),
            "SOUL.md still uses the default starter description.".to_owned(),
            "Describe what this bot watches, how it works, and what it must never do.".to_owned(),
            None,
            None,
            now_ms,
        ));
    }

    let mut active = Vec::new();
    let mut preserved_dismissed = Vec::new();

    for cand in candidates {
        let key = suggestion_stable_key(&cand.title, &cand.why, cand.source);
        let suppressed_by_state = scheduler_state.is_some_and(|st| {
            st.is_suggestion_suppressed(bot_id, &cand.title, &cand.why, cand.source, now_ms)
        });
        let suppressed_prev = existing_dismissed.remove(&key);
        if suppressed_by_state || suppressed_prev.is_some() {
            let mut tomb = suppressed_prev.unwrap_or(cand);
            tomb.dismissed = true;
            preserved_dismissed.push(tomb);
        } else if active.len() < cap {
            active.push(cand);
        }
    }

    // Also retain recently dismissed entries (bounded to prevent unbounded growth).
    for (_, mut extra) in existing_dismissed {
        if preserved_dismissed.len() >= 30 {
            break;
        }
        if now_ms.saturating_sub(extra.created_at_ms) < SUGGESTION_DISMISS_COOLDOWN_MS {
            extra.dismissed = true;
            preserved_dismissed.push(extra);
        }
    }

    let mut persisted = active.clone();
    persisted.extend(preserved_dismissed);
    store.save_suggestions(bot_id, &persisted)?;
    if let Some(state) = scheduler_state {
        state.record_look_around(bot_id, now_ms).ok();
    }
    Ok(active)
}

/// Lists persisted Look Around suggestions across one or all bots.
pub fn list_suggestions(
    store: &BotStore,
    bot_id: Option<&BotId>,
    include_dismissed: bool,
) -> io::Result<Vec<BotSuggestion>> {
    let bots = match bot_id {
        Some(id) => vec![store.get_bot(id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("bot not found: {}", id.as_str()),
            )
        })?],
        None => store.list_bots()?,
    };
    let mut out = Vec::new();
    for bot in &bots {
        let list = store.load_suggestions(&bot.id).unwrap_or_default();
        for sugg in list {
            if include_dismissed || !sugg.dismissed {
                out.push(sugg);
            }
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.created_at_ms));
    Ok(out)
}

/// Dismisses a single suggestion persistently (across restarts until cooldown expires).
pub fn dismiss_suggestion(
    store: &BotStore,
    scheduler_state: &SchedulerStateStore,
    bot_id: &BotId,
    suggestion_id: &str,
    now_ms: u64,
) -> io::Result<BotSuggestion> {
    let mut list = store.load_suggestions(bot_id)?;
    let Some(pos) = list.iter().position(|s| s.id == suggestion_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("suggestion not found: {suggestion_id}"),
        ));
    };
    let updated = {
        let Some(entry) = list.get_mut(pos) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "suggestion disappeared",
            ));
        };
        entry.dismissed = true;
        entry.clone()
    };
    scheduler_state.record_suggestion_dismissed(bot_id, &updated, now_ms)?;
    store.save_suggestions(bot_id, &list)?;
    Ok(updated)
}

/// Prepares the kickoff prompt for `StartSuggestion` and marks that suggestion dismissed/started.
pub fn prepare_start_suggestion(
    store: &BotStore,
    scheduler_state: &SchedulerStateStore,
    bot_id: &BotId,
    suggestion_id: &str,
    now_ms: u64,
) -> io::Result<(BotSuggestion, String)> {
    let mut list = store.load_suggestions(bot_id)?;
    let Some(pos) = list.iter().position(|s| s.id == suggestion_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("suggestion not found: {suggestion_id}"),
        ));
    };
    let updated = {
        let Some(entry) = list.get_mut(pos) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "suggestion disappeared",
            ));
        };
        entry.dismissed = true;
        entry.clone()
    };
    scheduler_state.record_suggestion_dismissed(bot_id, &updated, now_ms)?;
    store.save_suggestions(bot_id, &list)?;

    let mut prompt = format!(
        "Owner acted on your Up Next suggestion:\nTitle: {}\nReason: {}\nAction: {}",
        updated.title, updated.why, updated.ask
    );
    if let Some(link) = &updated.link {
        prompt.push_str("\nReference: ");
        prompt.push_str(link);
    }
    Ok((updated, prompt))
}

/// Marks `Up Next` suggestions seen up to `now_ms`.
pub fn mark_up_next_seen(
    scheduler_state: &SchedulerStateStore,
    bot_id: Option<&BotId>,
    now_ms: u64,
) -> io::Result<()> {
    scheduler_state.mark_up_next_seen(bot_id, now_ms)
}

/// Counts unseen active suggestions for `bot_id` (or all bots when `None`).
#[must_use]
pub fn up_next_unseen_count(
    store: &BotStore,
    scheduler_state: &SchedulerStateStore,
    bot_id: Option<&BotId>,
) -> usize {
    let Ok(active) = list_suggestions(store, bot_id, false) else {
        return 0;
    };
    active
        .iter()
        .filter(|s| s.created_at_ms > scheduler_state.up_next_seen_at(&s.bot_id))
        .count()
}

/// Computes `[minute, hour, day, month, weekday]` (`weekday` 0=Sun..6=Sat) in `tz` (default UTC).
pub fn cron_calendar_for_tz(unix_secs: u64, tz: Option<&str>) -> io::Result<[u32; 5]> {
    let raw = tz.map(str::trim).filter(|s| !s.is_empty());
    let Some(spec) = raw else {
        return crate::sys::cron_calendar(unix_secs);
    };
    if matches!(spec, "UTC" | "Z" | "GMT" | "Etc/UTC" | "Etc/GMT") {
        return crate::sys::cron_calendar(unix_secs);
    }
    if let Some(offset_secs) = parse_fixed_offset_secs(spec) {
        return calendar_from_offset(unix_secs, offset_secs);
    }
    #[cfg(unix)]
    {
        if spec.eq_ignore_ascii_case("local") {
            if let Ok(bytes) = fs::read("/etc/localtime")
                && let Some(offset) = tzif_offset_for_unix_secs(&bytes, unix_secs)
            {
                return calendar_from_offset(unix_secs, offset);
            }
            if let Ok(env_tz) = std::env::var("TZ")
                && !env_tz.trim().is_empty()
                && !env_tz.eq_ignore_ascii_case("local")
            {
                return cron_calendar_for_tz(unix_secs, Some(env_tz.trim()));
            }
            return crate::sys::cron_calendar(unix_secs);
        }
        if is_safe_iana_name(spec) {
            for base in [
                "/usr/share/zoneinfo",
                "/var/db/timezone/zoneinfo",
                "/usr/share/lib/zoneinfo",
            ] {
                let path = Path::new(base).join(spec);
                if let Ok(bytes) = fs::read(path)
                    && let Some(offset) = tzif_offset_for_unix_secs(&bytes, unix_secs)
                {
                    return calendar_from_offset(unix_secs, offset);
                }
            }
        }
    }
    if let Some(offset_secs) = fallback_iana_fixed_offset(spec) {
        return calendar_from_offset(unix_secs, offset_secs);
    }
    crate::sys::cron_calendar(unix_secs)
}

fn calendar_from_offset(unix_secs: u64, offset_secs: i64) -> io::Result<[u32; 5]> {
    let shifted = i64::try_from(unix_secs)
        .unwrap_or(i64::MAX)
        .saturating_add(offset_secs);
    let secs = u64::try_from(shifted.max(0)).unwrap_or(0);
    crate::sys::cron_calendar(secs)
}

fn is_weekend_in_tz(unix_secs: u64, tz: Option<&str>) -> bool {
    if let Ok([_, _, _, _, weekday]) = cron_calendar_for_tz(unix_secs, tz) {
        return weekday == 0 || weekday == 6;
    }
    is_weekend_utc(unix_secs)
}

fn parse_fixed_offset_secs(raw: &str) -> Option<i64> {
    let s = raw
        .trim()
        .strip_prefix("UTC")
        .or_else(|| raw.trim().strip_prefix("GMT"))
        .unwrap_or(raw.trim());
    let (sign, rest) = match s.as_bytes().first().copied() {
        Some(b'+') => (1_i64, s.get(1..).unwrap_or_default()),
        Some(b'-') => (-1_i64, s.get(1..).unwrap_or_default()),
        _ => return None,
    };
    if rest.is_empty() {
        return None;
    }
    let (hours_str, mins_str) = if let Some((h, m)) = rest.split_once(':') {
        (h, m)
    } else if rest.len() == 4 && rest.bytes().all(|b| b.is_ascii_digit()) {
        (rest.get(..2).unwrap_or("0"), rest.get(2..).unwrap_or("0"))
    } else if rest.len() <= 2 && rest.bytes().all(|b| b.is_ascii_digit()) {
        (rest, "0")
    } else {
        return None;
    };
    let hours: i64 = hours_str.parse().ok()?;
    let mins: i64 = mins_str.parse().ok()?;
    if hours > 23 || mins > 59 {
        return None;
    }
    Some(sign * (hours * 3_600 + mins * 60))
}

#[cfg(unix)]
fn is_safe_iana_name(tz: &str) -> bool {
    !tz.is_empty()
        && tz.len() <= 128
        && !tz.starts_with('/')
        && !tz.contains("..")
        && !tz.contains('\\')
        && tz
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+' | b'.'))
}

/// Minimal safe RFC 8536 `TZif` parser supporting v1/v2/v3 transition tables and trailing POSIX TZ.
#[cfg(unix)]
fn tzif_offset_for_unix_secs(bytes: &[u8], unix_secs: u64) -> Option<i64> {
    if bytes.len() < 44 || bytes.get(..4) != Some(b"TZif") {
        return None;
    }
    let version = bytes.get(4).copied()?;
    let read_u32 = |off: usize| -> Option<usize> {
        let s: [u8; 4] = bytes.get(off..off + 4)?.try_into().ok()?;
        usize::try_from(u32::from_be_bytes(s)).ok()
    };

    let (data_start, time_size) = if version == 0 {
        (0_usize, 4_usize)
    } else {
        // Skip v1 header + data block to locate the 64-bit v2/v3 block.
        let tzh_ttisgmtcnt = read_u32(20)?;
        let tzh_ttisstdcnt = read_u32(24)?;
        let tzh_leapcnt = read_u32(28)?;
        let tzh_timecnt = read_u32(32)?;
        let tzh_typecnt = read_u32(36)?;
        let tzh_charcnt = read_u32(40)?;
        let v1_len = 44_usize
            .checked_add(tzh_timecnt.checked_mul(5)?)?
            .checked_add(tzh_typecnt.checked_mul(6)?)?
            .checked_add(tzh_charcnt)?
            .checked_add(tzh_leapcnt.checked_mul(8)?)?
            .checked_add(tzh_ttisstdcnt)?
            .checked_add(tzh_ttisgmtcnt)?;
        if bytes.get(v1_len..v1_len + 4) == Some(b"TZif") {
            (v1_len, 8_usize)
        } else {
            (0_usize, 4_usize)
        }
    };

    let read_hdr_u32 = |rel_off: usize| -> Option<usize> {
        let off = data_start.checked_add(rel_off)?;
        let s: [u8; 4] = bytes.get(off..off + 4)?.try_into().ok()?;
        usize::try_from(u32::from_be_bytes(s)).ok()
    };
    let timecnt = read_hdr_u32(32)?;
    let typecnt = read_hdr_u32(36)?;
    if typecnt == 0 {
        return None;
    }
    let transitions_off = data_start.checked_add(44)?;
    let type_idx_off = transitions_off.checked_add(timecnt.checked_mul(time_size)?)?;
    let ttinfo_off = type_idx_off.checked_add(timecnt)?;
    let ttinfo_end = ttinfo_off.checked_add(typecnt.checked_mul(6)?)?;
    if ttinfo_end > bytes.len() {
        return None;
    }

    let read_tt_utoff = |idx: usize| -> Option<i64> {
        let off = ttinfo_off.checked_add(idx.checked_mul(6)?)?;
        let raw: [u8; 4] = bytes.get(off..off + 4)?.try_into().ok()?;
        Some(i64::from(i32::from_be_bytes(raw)))
    };

    let target = i64::try_from(unix_secs).unwrap_or(i64::MAX);
    let mut chosen_type: Option<usize> = None;
    let mut last_transition: Option<i64> = None;

    for i in 0..timecnt {
        let off = transitions_off + i * time_size;
        let t = if time_size == 8 {
            let raw: [u8; 8] = bytes.get(off..off + 8)?.try_into().ok()?;
            i64::from_be_bytes(raw)
        } else {
            let raw: [u8; 4] = bytes.get(off..off + 4)?.try_into().ok()?;
            i64::from(i32::from_be_bytes(raw))
        };
        if t <= target {
            chosen_type = bytes.get(type_idx_off + i).copied().map(usize::from);
            last_transition = Some(t);
        } else {
            break;
        }
    }

    // For timestamps beyond the last explicit transition in v2/v3, check the trailing POSIX TZ string
    // (critical for future years past 2038).
    if time_size == 8 && last_transition.is_some() && chosen_type.is_some() {
        let charcnt = read_hdr_u32(40)?;
        let leapcnt = read_hdr_u32(28)?;
        let ttisstdcnt = read_hdr_u32(24)?;
        let ttisgmtcnt = read_hdr_u32(20)?;
        let footer_start = ttinfo_end
            .checked_add(charcnt)?
            .checked_add(leapcnt.checked_mul(12)?)?
            .checked_add(ttisstdcnt)?
            .checked_add(ttisgmtcnt)?;
        if let Some(tail) = bytes.get(footer_start..)
            && let Some(posix) = std::str::from_utf8(tail).ok()
        {
            let trimmed = posix.trim_matches(|c| c == '\n' || c == '\r' || c == '\0');
            if !trimmed.is_empty()
                && last_transition.is_some_and(|lt| target > lt + 365 * 86_400)
                && let Some(off) = posix_tz_std_offset_secs(trimmed)
            {
                return Some(off);
            }
        }
    }

    if let Some(idx) = chosen_type {
        return read_tt_utoff(idx);
    }
    read_tt_utoff(0)
}

#[cfg(unix)]
fn posix_tz_std_offset_secs(posix: &str) -> Option<i64> {
    let std_part = posix.split(',').next()?;
    let bytes = std_part.as_bytes();
    let mut i = 0;
    if bytes.first() == Some(&b'<') {
        while i < bytes.len() && bytes.get(i) != Some(&b'>') {
            i += 1;
        }
        i += 1;
    } else {
        while i < bytes.len() && bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
            i += 1;
        }
    }
    let rest = std_part.get(i..)?;
    if rest.is_empty() {
        return None;
    }
    // POSIX offset has reversed sign: `UTC+5` or `EST5` means UTC-5.
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != ':' && c != '+' && c != '-')
        .unwrap_or(rest.len());
    let num = rest.get(..end)?;
    let (sign, digits) = if let Some(d) = num.strip_prefix('-') {
        (1_i64, d)
    } else if let Some(d) = num.strip_prefix('+') {
        (-1_i64, d)
    } else {
        (-1_i64, num)
    };
    let (h, m, s) = match digits.split(':').collect::<Vec<_>>().as_slice() {
        [h] => (h.parse::<i64>().ok()?, 0_i64, 0_i64),
        [h, m] => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?, 0_i64),
        [h, m, s] => (
            h.parse::<i64>().ok()?,
            m.parse::<i64>().ok()?,
            s.parse::<i64>().ok()?,
        ),
        _ => return None,
    };
    Some(sign * (h * 3_600 + m * 60 + s))
}

fn fallback_iana_fixed_offset(tz: &str) -> Option<i64> {
    match tz {
        "Asia/Seoul" | "Asia/Tokyo" => Some(9 * 3_600),
        "Asia/Shanghai" | "Asia/Singapore" | "Asia/Hong_Kong" => Some(8 * 3_600),
        "Asia/Kolkata" => Some(5 * 3_600 + 1_800),
        "Europe/London" => Some(0),
        "Europe/Berlin" | "Europe/Paris" => Some(3_600),
        "America/Los_Angeles" => Some(-8 * 3_600),
        "America/Denver" => Some(-7 * 3_600),
        "America/Chicago" => Some(-6 * 3_600),
        "America/New_York" => Some(-5 * 3_600),
        _ => None,
    }
}

fn suggestion_stable_key(title: &str, why: &str, source: Option<ExternalSource>) -> String {
    let src = source.map_or("none", ExternalSource::as_str);
    format!(
        "{src}::{}::{}",
        title.trim().to_ascii_lowercase(),
        why.trim().to_ascii_lowercase()
    )
}

fn make_suggestion(
    bot_id: &BotId,
    title: String,
    why: String,
    ask: String,
    link: Option<String>,
    source: Option<ExternalSource>,
    now_ms: u64,
) -> BotSuggestion {
    let seq = SUGG_SEQ.fetch_add(1, Ordering::Relaxed);
    BotSuggestion {
        id: format!("sugg-{now_ms}-{seq}"),
        bot_id: bot_id.clone(),
        title,
        why,
        ask,
        link,
        source,
        dismissed: false,
        created_at_ms: now_ms,
    }
}

fn is_weekend_utc(unix_secs: u64) -> bool {
    let days = unix_secs / 86_400;
    let weekday = (days + 4) % 7; // 0 = Sun, 6 = Sat
    weekday == 0 || weekday == 6
}

fn quick_git_dirty_count(workspace: &Path) -> Option<usize> {
    let output = krill_proto::os::background_command("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let count = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    Some(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::{BotItemActor, CreateBotItemParams, CreateBotParams};
    use std::fs;
    use std::path::PathBuf;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-bot-look-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn look_around_generates_bounded_suggestions_and_persists_dismissals() {
        let root = temp_dir("scan");
        let store = BotStore::new(root.clone());
        let items = BotItemsStore::new(root.join("bot-items.db"));
        let sched = SchedulerStateStore::new(root.clone());

        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "Ops Scout".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: Some("# Ops Scout\n\nWatches open PRs.\n".to_owned()),
                },
                1_000,
            )
            .unwrap();

        let actor = BotItemActor::Owner { device: None };
        items
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot.id.clone(),
                    title: "Approve prod deploy".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    links: vec![BotItemLink::Pr(
                        "https://github.com/craftowen/krill/pull/99".to_owned(),
                    )],
                },
                &actor,
                1_500,
            )
            .unwrap();

        let _ = store
            .check_or_request_access(&bot.id, ExternalSource::Discord, "u1", "Bob", "ch1", 2_000)
            .unwrap();

        let suggestions =
            run_look_around_tracked(&store, &items, Some(&sched), &bot.id, 3_000).unwrap();
        assert_eq!(suggestions.len(), 2);
        assert!(
            suggestions
                .iter()
                .any(|s| s.title.contains("Approve prod deploy"))
        );
        assert!(
            suggestions
                .iter()
                .any(|s| s.title.contains("access request"))
        );
        assert_eq!(up_next_unseen_count(&store, &sched, Some(&bot.id)), 2);

        let deploy_id = suggestions
            .iter()
            .find(|s| s.title.contains("Approve prod deploy"))
            .unwrap()
            .id
            .clone();
        dismiss_suggestion(&store, &sched, &bot.id, &deploy_id, 4_000).unwrap();
        assert_eq!(up_next_unseen_count(&store, &sched, Some(&bot.id)), 1);

        // Reloading SchedulerStateStore from disk preserves dismissal across restart.
        let sched_restarted = SchedulerStateStore::new(root.clone());
        let after = run_look_around_tracked(&store, &items, Some(&sched_restarted), &bot.id, 5_000)
            .unwrap();
        assert_eq!(after.len(), 1);
        assert!(
            !after
                .iter()
                .any(|s| s.title.contains("Approve prod deploy"))
        );
        assert_eq!(
            list_suggestions(&store, Some(&bot.id), true).unwrap().len(),
            2
        );

        mark_up_next_seen(&sched_restarted, Some(&bot.id), 6_000).unwrap();
        assert_eq!(
            up_next_unseen_count(&store, &sched_restarted, Some(&bot.id)),
            0
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cron_calendar_respects_offsets_and_iana_timezones() {
        // 2026-11-20 00:15:00 UTC -> Asia/Seoul is 09:15 on 2026-11-20.
        let utc_secs = 1_795_133_700;
        let utc = cron_calendar_for_tz(utc_secs, Some("UTC")).unwrap();
        let seoul = cron_calendar_for_tz(utc_secs, Some("Asia/Seoul")).unwrap();
        let plus9 = cron_calendar_for_tz(utc_secs, Some("+09:00")).unwrap();
        assert_eq!(utc[1], 0);
        assert_eq!(utc[0], 15);
        assert_eq!(seoul, plus9);
        assert_eq!(seoul[1], 9);
        assert_eq!(seoul[0], 15);
    }
}
