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
    BotAccessRequestStatus, BotConfig, BotId, BotItemLink, BotItemStatus, BotScheduleUpcoming,
    BotSettingsExtras, BotSuggestion, BotUpcoming, DEFAULT_LOOK_AROUND_MAX_SUGGESTIONS,
    ExternalSource, LookAroundEvery, TidyEvery, soul_description,
};
use krill_proto::os::fs::DirBuilderExt as _;
use krill_proto::os::write_atomic_0600;
use krill_proto::schedule::Cron;
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
struct StoredScheduleError {
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refusal: Option<krill_proto::bots::BotSessionRefusal>,
    #[serde(default)]
    at_ms: u64,
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
    #[serde(default)]
    schedule_last_error: BTreeMap<String, StoredScheduleError>,
}

/// What [`SchedulerStateStore::last_runs`] reads.
#[derive(Debug, Default)]
struct LastRuns {
    look: Option<u64>,
    tidy: Option<u64>,
    tidy_off: bool,
    crons: BTreeMap<String, u64>,
    cron_errors: BTreeMap<String, StoredScheduleError>,
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
        every: LookAroundEvery,
        weekdays_only: bool,
        tz: Option<&str>,
        now_ms: u64,
    ) -> bool {
        let secs = now_ms / 1_000;
        if weekdays_only && is_weekend_in_tz(secs, tz) {
            return false;
        }
        let Some(slot) = latest_slot_ms(now_ms, tz, every.hours()) else {
            return false;
        };
        self.with_state_read(|st| {
            st.look_around_last_run_ms
                .get(bot_id)
                .is_none_or(|&last| last < slot)
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
        self.suggestion_dismissed_at(bot_id, title, why, source)
            .is_some_and(|dismissed_at| {
                now_ms.saturating_sub(dismissed_at) < SUGGESTION_DISMISS_COOLDOWN_MS
            })
    }

    #[must_use]
    fn suggestion_dismissed_at(
        &self,
        bot_id: &BotId,
        title: &str,
        why: &str,
        source: Option<ExternalSource>,
    ) -> Option<u64> {
        let key = suggestion_stable_key(title, why, source);
        self.with_state_read(|st| {
            st.dismissed_suggestions
                .get(bot_id)
                .and_then(|map| map.get(&key).copied())
        })
        .ok()
        .flatten()
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
    pub fn tidy_is_due(
        &self,
        bot_id: &BotId,
        every: TidyEvery,
        tz: Option<&str>,
        now_ms: u64,
    ) -> bool {
        let Some(after) = tidy_due_after_ms(every, tz, now_ms) else {
            return false;
        };
        self.with_state_read(|st| {
            let Some(rec) = st.memory_tidy.get(bot_id) else {
                return true;
            };
            if rec.auto_disabled {
                return false;
            }
            rec.last_run_ms.is_none_or(|last| last < after)
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

    pub fn record_schedule_error(
        &self,
        bot_id: &BotId,
        sched_id: &str,
        message: String,
        refusal: Option<krill_proto::bots::BotSessionRefusal>,
        now_ms: u64,
    ) -> io::Result<()> {
        let key = format!("{}:{sched_id}", bot_id.as_str());
        self.with_state_mut(|st| {
            st.schedule_last_error.insert(
                key,
                StoredScheduleError {
                    message,
                    refusal,
                    at_ms: now_ms,
                },
            );
            Ok(())
        })
    }

    pub fn clear_schedule_error(&self, bot_id: &BotId, sched_id: &str) -> io::Result<()> {
        let key = format!("{}:{sched_id}", bot_id.as_str());
        self.with_state_mut(|st| {
            st.schedule_last_error.remove(&key);
            Ok(())
        })
    }

    /// When `bot_id` last looked around and last tidied, whether tidying turned itself off, and
    /// the epoch minute each of its schedules last fired, for its Home.
    fn last_runs(&self, bot_id: &BotId) -> LastRuns {
        let prefix = format!("{}:", bot_id.as_str());
        self.with_state_read(|st| {
            let tidy = st.memory_tidy.get(bot_id);
            LastRuns {
                look: st.look_around_last_run_ms.get(bot_id).copied(),
                tidy: tidy.and_then(|r| r.last_run_ms),
                tidy_off: tidy.is_some_and(|r| r.auto_disabled),
                crons: st
                    .cron_claimed_minute
                    .iter()
                    .filter_map(|(k, m)| Some((k.strip_prefix(&prefix)?.to_owned(), *m)))
                    .collect(),
                cron_errors: st
                    .schedule_last_error
                    .iter()
                    .filter_map(|(k, err)| Some((k.strip_prefix(&prefix)?.to_owned(), err.clone())))
                    .collect(),
            }
        })
        .unwrap_or_default()
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
            st.schedule_last_error
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
        every: LookAroundEvery,
        weekdays_only: bool,
        tz: Option<&str>,
        now_ms: u64,
    ) -> bool {
        if weekdays_only && is_weekend_in_tz(now_ms / 1_000, tz) {
            return false;
        }
        let Some(slot) = latest_slot_ms(now_ms, tz, every.hours()) else {
            return false;
        };
        let guard = lock(&self.last_run_ms);
        guard.get(bot_id).is_none_or(|&last| last < slot)
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
    let mut existing_dismissed: HashMap<String, BotSuggestion> = HashMap::new();
    let mut existing_active: HashMap<String, BotSuggestion> = HashMap::new();
    for s in previous {
        let key = suggestion_stable_key(&s.title, &s.why, s.source);
        if s.dismissed {
            existing_dismissed.insert(key, s);
        } else {
            existing_active.entry(key).or_insert(s);
        }
    }

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

    for mut cand in candidates {
        let key = suggestion_stable_key(&cand.title, &cand.why, cand.source);
        let dismissed_at_state = scheduler_state
            .and_then(|st| st.suggestion_dismissed_at(bot_id, &cand.title, &cand.why, cand.source));
        let suppressed_by_state = dismissed_at_state.is_some_and(|dismissed_at| {
            now_ms.saturating_sub(dismissed_at) < SUGGESTION_DISMISS_COOLDOWN_MS
        });
        let suppressed_prev = existing_dismissed.remove(&key);
        let suppressed_by_prev = suppressed_prev.as_ref().is_some_and(|prev| {
            now_ms.saturating_sub(prev.created_at_ms) < SUGGESTION_DISMISS_COOLDOWN_MS
        });
        if suppressed_by_state || suppressed_by_prev {
            let mut tomb = suppressed_prev.unwrap_or_else(|| {
                let mut fallback = cand;
                if let Some(ts) = dismissed_at_state {
                    fallback.created_at_ms = ts;
                }
                fallback
            });
            tomb.dismissed = true;
            preserved_dismissed.push(tomb);
        } else if active.len() < cap {
            // Preserve existing active suggestion ID and timestamp across refreshes so unseen
            // counts do not reset and open UIs targeting this ID do not get NotFound (MB-37).
            if let Some(prev) = existing_active.remove(&key) {
                cand.id = prev.id;
                cand.created_at_ms = prev.created_at_ms;
            }
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

/// Builds the kickoff prompt for `StartSuggestion` WITHOUT mutating suggestion or cooldown state,
/// so callers only commit dismissal after `start_bot_session` succeeds.
pub fn preview_start_suggestion(
    store: &BotStore,
    bot_id: &BotId,
    suggestion_id: &str,
) -> io::Result<(BotSuggestion, String)> {
    let list = store.load_suggestions(bot_id)?;
    let Some(entry) = list.into_iter().find(|s| s.id == suggestion_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("suggestion not found: {suggestion_id}"),
        ));
    };
    let mut prompt = format!(
        "Owner acted on your Up Next suggestion:\nTitle: {}\nReason: {}\nAction: {}",
        entry.title, entry.why, entry.ask
    );
    if let Some(link) = &entry.link {
        prompt.push_str("\nReference: ");
        prompt.push_str(link);
    }
    Ok((entry, prompt))
}

/// Prepares the kickoff prompt for `StartSuggestion` and marks that suggestion dismissed/started.
pub fn prepare_start_suggestion(
    store: &BotStore,
    scheduler_state: &SchedulerStateStore,
    bot_id: &BotId,
    suggestion_id: &str,
    now_ms: u64,
) -> io::Result<(BotSuggestion, String)> {
    let (_, prompt) = preview_start_suggestion(store, bot_id, suggestion_id)?;
    let updated = dismiss_suggestion(store, scheduler_state, bot_id, suggestion_id, now_ms)?;
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

/// The bot's automations' last and next runs (Orca's Automations card). `running` says whether a
/// schedule's conversation is on a turn now.
pub fn bot_upcoming(
    bot: &BotConfig,
    state: &SchedulerStateStore,
    running: impl Fn(&str) -> bool,
    now_ms: u64,
) -> BotUpcoming {
    bot_upcoming_with_extras(bot, BotSettingsExtras::default(), state, running, now_ms)
}

/// Same as [`bot_upcoming`], using the persisted `settings_extras` presets (`look_around_every`,
/// `tidy_every`) and [`local_tz`] fallback — matching [`BotsManager::tick_periodic`].
pub fn bot_upcoming_with_extras(
    bot: &BotConfig,
    extras: BotSettingsExtras,
    state: &SchedulerStateStore,
    running: impl Fn(&str) -> bool,
    now_ms: u64,
) -> BotUpcoming {
    let LastRuns {
        look: look_last,
        tidy: tidy_last,
        tidy_off,
        crons,
        cron_errors,
    } = state.last_runs(&bot.id);
    let bot_tz = bot
        .schedules
        .iter()
        .find_map(|s| s.timezone.clone())
        .or_else(local_tz);
    let tz = bot_tz.as_deref();
    let look_next = bot.look_around.enabled.then(|| {
        next_look_around_ms(
            extras.look_around_every,
            bot.look_around.weekdays_only,
            tz,
            look_last,
            now_ms,
        )
    });
    let tidy_next = (bot.memory_tidy.enabled && !tidy_off)
        .then(|| next_tidy_ms(extras.tidy_every, tz, tidy_last, now_ms));
    let now_secs = now_ms / 1_000;
    let schedules = bot
        .schedules
        .iter()
        .map(|s| {
            let next_ms = s
                .enabled
                .then(|| Cron::parse(&s.cron).ok())
                .flatten()
                .and_then(|cron| next_cron_utc_ms(&cron, now_secs, s.timezone.as_deref()));
            let last_err = cron_errors.get(&s.id);
            BotScheduleUpcoming {
                id: s.id.clone(),
                next_ms,
                last_ms: crons.get(&s.id).map(|m| m.saturating_mul(60_000)),
                running: running(&s.id),
                last_error: last_err.map(|e| e.message.clone()),
                last_refusal: last_err.and_then(|e| e.refusal.clone()),
            }
        })
        .collect();
    BotUpcoming {
        look_around_last_ms: look_last,
        look_around_next_ms: look_next.flatten(),
        tidy_last_ms: tidy_last,
        tidy_next_ms: tidy_next.flatten(),
        schedules,
        up_next_seen_ms: state.up_next_seen_at(&bot.id),
        items_seen_event_id: 0,
    }
}

/// Resolves a local wall-clock minute (minutes since 1970-01-01 in `tz`) to all valid UTC minutes
/// (0 on a spring-forward gap, 1 normally, 2 on a fall-back ambiguous hour), sorted ascending.
fn resolve_local_minute_utc(local_min: i64, tz: Option<&str>) -> Vec<i64> {
    let mut results: Vec<i64> = Vec::new();
    let seed_secs = u64::try_from(local_min.max(0))
        .unwrap_or(0)
        .saturating_mul(60);
    let base_off = tz_offset_minutes(seed_secs, tz);
    let base_utc = local_min.saturating_sub(base_off);
    for shift in [-120_i64, -60, -30, 0, 30, 60, 120] {
        let mut cand = base_utc.saturating_add(shift);
        for _ in 0..3 {
            let Ok(cand_u64) = u64::try_from(cand) else {
                break;
            };
            let o = tz_offset_minutes(cand_u64.saturating_mul(60), tz);
            let refined = local_min.saturating_sub(o);
            if refined == cand {
                break;
            }
            cand = refined;
        }
        if let Ok(cand_u64) = u64::try_from(cand) {
            let check = tz_offset_minutes(cand_u64.saturating_mul(60), tz);
            if cand.saturating_add(check) == local_min && !results.contains(&cand) {
                results.push(cand);
            }
        }
    }
    results.sort_unstable();
    results
}

/// Computes the next UTC Unix ms after `now_secs` when `cron` fires in `tz`, re-evaluating the
/// target timestamp's UTC offset across DST spring-forward and fall-back transitions (MB-36).
fn next_cron_utc_ms(cron: &Cron, now_secs: u64, tz: Option<&str>) -> Option<u64> {
    let now_min = i64::try_from(now_secs / 60).ok()?;
    let off = tz_offset_minutes(now_secs, tz);
    let now_local = now_min.checked_add(off)?;

    // Forward scan from `now_local`, skipping across entire spring-forward DST gaps at once.
    let mut search_local = u64::try_from(now_local).ok()?;
    let mut forward_utc: Option<i64> = None;
    for _ in 0..16 {
        let Some(next_local) = cron.next_minute_after(search_local) else {
            break;
        };
        let next_local_i64 = i64::try_from(next_local).ok()?;
        let resolved = resolve_local_minute_utc(next_local_i64, tz);
        if let Some(&chosen) = resolved.iter().find(|&&u| u > now_min) {
            forward_utc = Some(chosen);
            break;
        }
        if resolved.is_empty() {
            // Spring-forward nonexistent wall-clock minute: binary search for the exact
            // first existing local minute after the gap so we jump the entire gap at once.
            let lo = next_local_i64;
            let hi = next_local_i64.saturating_add(180);
            if resolve_local_minute_utc(hi, tz).is_empty() {
                search_local = u64::try_from(hi).ok()?;
            } else {
                let mut left = lo;
                let mut right = hi;
                while left + 1 < right {
                    let mid = left + (right - left) / 2;
                    if resolve_local_minute_utc(mid, tz).is_empty() {
                        left = mid;
                    } else {
                        right = mid;
                    }
                }
                search_local = u64::try_from(left).ok()?;
            }
        } else {
            // All valid UTC instants for `next_local` were <= `now_min`; advance past it.
            search_local = next_local;
        }
    }

    // Fall-back check: if a backward DST offset transition occurs within the next 2 hours,
    // wall-clock local time will revisit minutes <= `now_local` (e.g. 01:00..01:59 EST after
    // 01:30 EDT). Scan `[min_future_local ..= now_local]` for any match whose UTC instant is
    // strictly after `now_min`, and return the earliest UTC instant overall.
    let mut min_future_off = off;
    for delta in [1_i64, 30, 60, 90, 120] {
        let probe_utc = now_min.saturating_add(delta);
        if let Ok(probe_u64) = u64::try_from(probe_utc) {
            let probe_off = tz_offset_minutes(probe_u64.saturating_mul(60), tz);
            if probe_off < min_future_off {
                min_future_off = probe_off;
            }
        }
    }
    let min_future_local = now_min.saturating_add(1).saturating_add(min_future_off);
    let mut fallback_utc: Option<i64> = None;
    if min_future_local <= now_local {
        let mut cursor = u64::try_from(min_future_local.saturating_sub(1).max(0)).ok()?;
        let limit = u64::try_from(now_local).ok()?;
        while let Some(cand_local) = cron.next_minute_after(cursor) {
            if cand_local > limit {
                break;
            }
            let cand_i64 = i64::try_from(cand_local).ok()?;
            let resolved = resolve_local_minute_utc(cand_i64, tz);
            if let Some(&chosen) = resolved.iter().find(|&&u| u > now_min) {
                fallback_utc = Some(fallback_utc.map_or(chosen, |prev| prev.min(chosen)));
            }
            cursor = cand_local;
        }
    }

    let best = match (forward_utc, fallback_utc) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }?;
    u64::try_from(best).ok()?.checked_mul(60_000)
}

/// Converts a local civil day (days since 1970-01-01 in `tz`) and `hour` (0..24) to UTC ms,
/// re-evaluating `tz`'s offset at the target timestamp so DST transitions stay aligned (MB-36).
fn local_day_hour_to_utc_ms(local_day: i64, hour: u32, tz: Option<&str>) -> Option<u64> {
    let target_local_min = local_day
        .checked_mul(1_440)?
        .checked_add(i64::from(hour) * 60)?;
    let mut utc_min = target_local_min.checked_sub(tz_offset_minutes(
        u64::try_from(target_local_min.max(0))
            .ok()?
            .saturating_mul(60),
        tz,
    ))?;
    for _ in 0..3 {
        let o = tz_offset_minutes(u64::try_from(utc_min.max(0)).ok()?.saturating_mul(60), tz);
        let refined = target_local_min.checked_sub(o)?;
        if refined == utc_min {
            break;
        }
        utc_min = refined;
    }
    let check_off = tz_offset_minutes(u64::try_from(utc_min.max(0)).ok()?.saturating_mul(60), tz);
    if utc_min + check_off == target_local_min {
        // On fall-back days prefer the earlier occurrence of a repeated wall-clock hour.
        for shift in [60_i64, 30] {
            let earlier = utc_min - shift;
            if earlier >= 0 {
                let eo = tz_offset_minutes(u64::try_from(earlier).ok()?.saturating_mul(60), tz);
                if earlier + eo == target_local_min {
                    return u64::try_from(earlier).ok()?.checked_mul(60_000);
                }
            }
        }
        return u64::try_from(utc_min).ok()?.checked_mul(60_000);
    }
    // Spring-forward gap (e.g. 02:00 jumped to 03:00): snap to the first UTC minute at/after the gap.
    for delta in -120..=120_i64 {
        let probe = utc_min.checked_add(delta)?;
        if probe < 0 {
            continue;
        }
        let po = tz_offset_minutes(u64::try_from(probe).ok()?.saturating_mul(60), tz);
        if probe + po >= target_local_min {
            return u64::try_from(probe).ok()?.checked_mul(60_000);
        }
    }
    u64::try_from(utc_min).ok()?.checked_mul(60_000)
}

fn local_civil_day(unix_secs: u64, tz: Option<&str>) -> Option<i64> {
    let off_secs = tz_offset_minutes(unix_secs, tz).checked_mul(60)?;
    let local_secs = i64::try_from(unix_secs).ok()?.checked_add(off_secs)?;
    Some(local_secs.div_euclid(86_400))
}

/// Computes the next Unix ms when Look Around will be due under `every`/`weekdays_only`/`tz`,
/// matching [`SchedulerStateStore::look_around_is_due`].
#[must_use]
pub fn next_look_around_ms(
    every: LookAroundEvery,
    weekdays_only: bool,
    tz: Option<&str>,
    last_ms: Option<u64>,
    now_ms: u64,
) -> Option<u64> {
    let hours = every.hours();
    let now_secs = now_ms / 1_000;
    if !(weekdays_only && is_weekend_in_tz(now_secs, tz))
        && let Some(slot) = latest_slot_ms(now_ms, tz, hours)
        && last_ms.is_none_or(|last| last < slot)
    {
        return Some(now_ms);
    }
    let today_day = local_civil_day(now_secs, tz)?;
    for day_offset in 0..=4_i64 {
        let day = today_day + day_offset;
        let weekday = (day + 4).rem_euclid(7); // 0 = Sun, 6 = Sat
        if weekdays_only && (weekday == 0 || weekday == 6) {
            continue;
        }
        for &slot_hour in hours {
            let Some(candidate_ms) = local_day_hour_to_utc_ms(day, slot_hour, tz) else {
                continue;
            };
            if candidate_ms > now_ms && last_ms.is_none_or(|last| last < candidate_ms) {
                return Some(candidate_ms);
            }
        }
    }
    None
}

/// Computes the next Unix ms when Memory Tidy will be due under `every`/`tz`, matching
/// [`SchedulerStateStore::tidy_is_due`].
#[must_use]
pub fn next_tidy_ms(
    every: TidyEvery,
    tz: Option<&str>,
    last_ms: Option<u64>,
    now_ms: u64,
) -> Option<u64> {
    if let Some(after) = tidy_due_after_ms(every, tz, now_ms)
        && last_ms.is_none_or(|last| last < after)
    {
        return Some(now_ms);
    }
    let now_secs = now_ms / 1_000;
    let today_day = local_civil_day(now_secs, tz)?;
    let last = last_ms.unwrap_or(0);
    let back_ms = match every {
        TidyEvery::Night => 0_u64,
        TidyEvery::Week => 6 * 86_400_000,
    };
    for day_offset in 0..=8_i64 {
        let Some(cand_3am) = local_day_hour_to_utc_ms(today_day + day_offset, 3, tz) else {
            continue;
        };
        if cand_3am > now_ms && cand_3am.saturating_sub(back_ms) > last {
            return Some(cand_3am);
        }
    }
    None
}

/// `tz`'s offset from UTC in minutes at `unix_secs` (within a day either way).
fn tz_offset_minutes(unix_secs: u64, tz: Option<&str>) -> i64 {
    let (Ok([lm, lh, _, _, lw]), Ok([um, uh, _, _, uw])) = (
        cron_calendar_for_tz(unix_secs, tz),
        crate::sys::cron_calendar(unix_secs),
    ) else {
        return 0;
    };
    let days = match (i64::from(lw) - i64::from(uw)).rem_euclid(7) {
        6 => -1,
        d => d,
    };
    days * 1_440 + (i64::from(lh) - i64::from(uh)) * 60 + i64::from(lm) - i64::from(um)
}

/// Start (unix ms) of today's latest `hours` slot (local 0-23) at or before `now_ms`; `None` before
/// the first one, as Orca's Look around runs nothing at night.
#[must_use]
pub fn latest_slot_ms(now_ms: u64, tz: Option<&str>, hours: &[u32]) -> Option<u64> {
    let secs = now_ms / 1_000;
    let [_, hour, ..] = cron_calendar_for_tz(secs, tz).ok()?;
    let slot = hours.iter().copied().filter(|&h| h <= hour).max()?;
    let today_day = local_civil_day(secs, tz)?;
    local_day_hour_to_utc_ms(today_day, slot, tz)
}

/// The last tidy-up must be older than this for the next to run: the latest 3 AM for each night,
/// six days before it for once a week. A computer asleep at 3 AM tidies once it is awake.
#[must_use]
pub fn tidy_due_after_ms(every: TidyEvery, tz: Option<&str>, now_ms: u64) -> Option<u64> {
    // Before 3 AM the latest 3 AM was yesterday's: look from four hours earlier.
    let night = latest_slot_ms(now_ms, tz, &[3])
        .or_else(|| latest_slot_ms(now_ms.saturating_sub(4 * 3_600_000), tz, &[3]))?;
    Some(match every {
        TidyEvery::Night => night,
        TidyEvery::Week => night.saturating_sub(6 * 86_400_000),
    })
}

/// This computer's local IANA zone (Unix `/etc/localtime`) or fixed UTC offset (`+HH:MM` on
/// Windows), so presets run on the owner's wall clock.
#[must_use]
pub fn local_tz() -> Option<String> {
    #[cfg(unix)]
    {
        if let Ok(target) = std::fs::read_link("/etc/localtime") {
            let target = target.to_string_lossy();
            if let Some((_, name)) = target.split_once("zoneinfo/") {
                return Some(name.to_owned());
            }
        }
        if let Ok(env_tz) = std::env::var("TZ") {
            let trimmed = env_tz.trim();
            if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("local") {
                return Some(trimmed.to_owned());
            }
        }
        None
    }
    #[cfg(windows)]
    {
        if let Ok(env_tz) = std::env::var("TZ") {
            let trimmed = env_tz.trim();
            if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("local") {
                return Some(trimmed.to_owned());
            }
        }
        windows_local_offset_secs().map(format_fixed_offset_secs)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
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
    #[cfg(windows)]
    {
        if spec.eq_ignore_ascii_case("local") {
            if let Ok(env_tz) = std::env::var("TZ")
                && !env_tz.trim().is_empty()
                && !env_tz.eq_ignore_ascii_case("local")
            {
                return cron_calendar_for_tz(unix_secs, Some(env_tz.trim()));
            }
            if let Some(offset) = windows_local_offset_secs() {
                return calendar_from_offset(unix_secs, offset);
            }
            return crate::sys::cron_calendar(unix_secs);
        }
    }
    if let Some(offset_secs) = fallback_iana_fixed_offset(spec) {
        return calendar_from_offset(unix_secs, offset_secs);
    }
    crate::sys::cron_calendar(unix_secs)
}

#[cfg(any(windows, test))]
fn format_fixed_offset_secs(offset_secs: i64) -> String {
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let abs_mins = offset_secs.unsigned_abs() / 60;
    let hours = abs_mins / 60;
    let mins = abs_mins % 60;
    format!("{sign}{hours:02}:{mins:02}")
}

#[cfg(any(windows, test))]
fn parse_windows_active_time_bias_secs(reg_output: &str) -> Option<i64> {
    for line in reg_output.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens
            .first()
            .is_some_and(|t| t.eq_ignore_ascii_case("ActiveTimeBias"))
            && let Some(val_str) = tokens.last()
        {
            let raw_u32 = if let Some(hex) = val_str
                .strip_prefix("0x")
                .or_else(|| val_str.strip_prefix("0X"))
            {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                val_str.parse::<u32>().ok()?
            };
            // Windows ActiveTimeBias is a signed 32-bit minute bias where UTC = local + bias.
            let bias_mins = i64::from(raw_u32.cast_signed());
            if (-840..=840).contains(&bias_mins) {
                return Some(-bias_mins * 60);
            }
        }
    }
    None
}

#[cfg(windows)]
fn windows_local_offset_secs() -> Option<i64> {
    let out = krill_proto::os::background_command("reg")
        .args([
            "query",
            r"HKLM\SYSTEM\CurrentControlSet\Control\TimeZoneInformation",
            "/v",
            "ActiveTimeBias",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_windows_active_time_bias_secs(&text)
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
    let last_table_transition: Option<i64> = if timecnt > 0 {
        let off = transitions_off + (timecnt - 1) * time_size;
        if time_size == 8 {
            let raw: [u8; 8] = bytes.get(off..off + 8)?.try_into().ok()?;
            Some(i64::from_be_bytes(raw))
        } else {
            let raw: [u8; 4] = bytes.get(off..off + 4)?.try_into().ok()?;
            Some(i64::from(i32::from_be_bytes(raw)))
        }
    } else {
        None
    };

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
        } else {
            break;
        }
    }

    // RFC 8536: for timestamps at or beyond the last explicit transition in v2/v3 (or when no
    // explicit transitions exist), evaluate the trailing POSIX TZ string including DST rules (MB-36).
    let beyond_table = timecnt == 0 || last_table_transition.is_some_and(|lt| target >= lt);
    if time_size == 8 && beyond_table {
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
                && let Some(off) = posix_tz_offset_secs(trimmed, target)
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
#[derive(Debug, Clone, Copy)]
enum PosixDstDateRule {
    /// `Mm.w.d`: month `1..=12`, week `1..=5` (`5` = last), weekday `0..=6` (0 = Sun).
    MonthWeekDay { month: u32, week: u32, day: u32 },
    /// `Jn`: 1-based Julian day `1..=365` (never counts Feb 29).
    JulianNoLeap(u32),
    /// `n`: 0-based Julian day `0..=365` (counts Feb 29 on leap years).
    ZeroBasedDay(u32),
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
struct PosixDstTransition {
    rule: PosixDstDateRule,
    time_secs: i64,
}

#[cfg(unix)]
fn parse_posix_tz_name(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.first() == Some(&b'<') {
        let mut i = 1;
        while i < bytes.len() && bytes.get(i) != Some(&b'>') {
            i += 1;
        }
        if bytes.get(i) == Some(&b'>') {
            return Some(i + 1);
        }
        return None;
    }
    let mut i = 0;
    while i < bytes.len() && bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
        i += 1;
    }
    (i > 0).then_some(i)
}

#[cfg(unix)]
fn parse_posix_hms(raw: &str, negate_posix_sign: bool) -> Option<i64> {
    if raw.is_empty() {
        return None;
    }
    let (sign, digits) = if let Some(d) = raw.strip_prefix('-') {
        (if negate_posix_sign { 1_i64 } else { -1_i64 }, d)
    } else if let Some(d) = raw.strip_prefix('+') {
        (if negate_posix_sign { -1_i64 } else { 1_i64 }, d)
    } else {
        (if negate_posix_sign { -1_i64 } else { 1_i64 }, raw)
    };
    let mut parts = digits.split(':');
    let h = parts.next()?.parse::<i64>().ok()?;
    let m = match parts.next() {
        Some(p) => p.parse::<i64>().ok()?,
        None => 0,
    };
    let s = match parts.next() {
        Some(p) => p.parse::<i64>().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some(sign * (h * 3_600 + m * 60 + s))
}

#[cfg(unix)]
fn parse_posix_offset_prefix(s: &str) -> Option<(i64, usize)> {
    let end = s
        .find(|c: char| !c.is_ascii_digit() && c != ':' && c != '+' && c != '-')
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let off = parse_posix_hms(s.get(..end)?, true)?;
    Some((off, end))
}

#[cfg(unix)]
fn parse_posix_transition_rule(raw: &str) -> Option<PosixDstTransition> {
    let (date_part, time_part) = match raw.split_once('/') {
        Some((d, t)) => (d, Some(t)),
        None => (raw, None),
    };
    let time_secs = match time_part {
        Some(t) => parse_posix_hms(t, false)?,
        None => 2 * 3_600,
    };
    let rule = if let Some(rest) = date_part.strip_prefix('M') {
        let mut parts = rest.split('.');
        let month = parts.next()?.parse::<u32>().ok()?;
        let week = parts.next()?.parse::<u32>().ok()?;
        let day = parts.next()?.parse::<u32>().ok()?;
        if parts.next().is_some()
            || !(1..=12).contains(&month)
            || !(1..=5).contains(&week)
            || day > 6
        {
            return None;
        }
        PosixDstDateRule::MonthWeekDay { month, week, day }
    } else if let Some(rest) = date_part.strip_prefix('J') {
        let j = rest.parse::<u32>().ok()?;
        if !(1..=365).contains(&j) {
            return None;
        }
        PosixDstDateRule::JulianNoLeap(j)
    } else {
        let n = date_part.parse::<u32>().ok()?;
        if n > 365 {
            return None;
        }
        PosixDstDateRule::ZeroBasedDay(n)
    };
    Some(PosixDstTransition { rule, time_secs })
}

#[cfg(unix)]
const fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(unix)]
const fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

#[cfg(unix)]
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(unix)]
fn civil_year_from_unix_secs(unix_secs: i64) -> i64 {
    let z = unix_secs.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    y + i64::from(m <= 2)
}

#[cfg(unix)]
fn posix_transition_utc_secs(year: i64, tr: PosixDstTransition, active_offset_secs: i64) -> i64 {
    let day_num = match tr.rule {
        PosixDstDateRule::MonthWeekDay { month, week, day } => {
            let first_days = days_from_civil(year, month, 1);
            let first_dow = u32::try_from((first_days + 4).rem_euclid(7)).unwrap_or(0);
            let delta = (day + 7 - first_dow) % 7;
            let mut dom = 1 + delta + (week - 1) * 7;
            let dim = days_in_month(year, month);
            if dom > dim {
                dom = dom.saturating_sub(7);
            }
            days_from_civil(year, month, dom)
        }
        PosixDstDateRule::JulianNoLeap(j) => {
            let jan1 = days_from_civil(year, 1, 1);
            let leap_adj = i64::from(is_leap_year(year) && j >= 60);
            jan1 + i64::from(j - 1) + leap_adj
        }
        PosixDstDateRule::ZeroBasedDay(n) => {
            let jan1 = days_from_civil(year, 1, 1);
            jan1 + i64::from(n)
        }
    };
    day_num * 86_400 + tr.time_secs - active_offset_secs
}

#[cfg(unix)]
fn posix_tz_offset_secs(posix: &str, unix_secs: i64) -> Option<i64> {
    let mut parts = posix.split(',');
    let spec = parts.next()?;
    let std_name_end = parse_posix_tz_name(spec)?;
    let after_std = spec.get(std_name_end..)?;
    let (std_off, std_off_len) = parse_posix_offset_prefix(after_std)?;
    let rest = after_std.get(std_off_len..)?;

    if rest.is_empty() {
        return Some(std_off);
    }

    let dst_name_end = parse_posix_tz_name(rest)?;
    let after_dst = rest.get(dst_name_end..)?;
    let dst_off = if after_dst.is_empty() {
        std_off + 3_600
    } else {
        parse_posix_offset_prefix(after_dst)?.0
    };

    let (Some(start_raw), Some(end_raw)) = (parts.next(), parts.next()) else {
        return Some(std_off);
    };
    if parts.next().is_some() {
        return Some(std_off);
    }
    let start_rule = parse_posix_transition_rule(start_raw)?;
    let end_rule = parse_posix_transition_rule(end_raw)?;

    let year = civil_year_from_unix_secs(unix_secs.saturating_add(std_off));
    let start_utc = posix_transition_utc_secs(year, start_rule, std_off);
    let end_utc = posix_transition_utc_secs(year, end_rule, dst_off);

    let in_dst = if start_utc < end_utc {
        // Northern hemisphere: DST is active between spring start and autumn end.
        unix_secs >= start_utc && unix_secs < end_utc
    } else {
        // Southern hemisphere: DST wraps across Jan 1 (active from Oct..Apr).
        unix_secs >= start_utc || unix_secs < end_utc
    };
    Some(if in_dst { dst_off } else { std_off })
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
    fn upcoming_says_when_each_automation_runs_next() {
        let root = temp_dir("upcoming");
        let sched = SchedulerStateStore::new(root);
        let mut bot = BotConfig::new(BotId::new("b1").unwrap(), "Scout", "claude", 0).unwrap();
        bot.look_around.weekdays_only = false;
        bot.look_around.interval_secs = 3_600;
        bot.schedules.push(krill_proto::bots::BotScheduleConfig {
            id: "s1".to_owned(),
            name: "Morning".to_owned(),
            cron: "0 9 * * *".to_owned(),
            prompt: "hi".to_owned(),
            enabled: true,
            timezone: Some("+02:00".to_owned()),
        });
        // 2026-11-23 (a Monday) 06:30 UTC = 08:30 at +02:00.
        let now = 1_795_415_400_000;
        let up = bot_upcoming(&bot, &sched, |_| false, now);
        // Never looked and 08:00 slot passed: due now.
        assert_eq!(up.look_around_next_ms, Some(now));
        assert_eq!(up.schedules[0].next_ms, Some(now + 30 * 60_000));
        sched.record_look_around(&bot.id, now).unwrap();
        let up = bot_upcoming(&bot, &sched, |id| id == "s1", now + 60_000);
        assert_eq!(up.look_around_last_ms, Some(now));
        // Default Every4h slots are 08:00, 12:00, 16:00, 20:00 -> next slot after 08:31 is 12:00 (+3h30m from 08:30).
        assert_eq!(up.look_around_next_ms, Some(now + 210 * 60_000));
        assert!(up.schedules[0].running);
        // Custom settings_extras (Mornings = [8]): after 08:30, next 08:00 slot is tomorrow (+23h30m from 08:30).
        let extras = BotSettingsExtras {
            look_around_every: LookAroundEvery::Mornings,
            tidy_every: TidyEvery::Week,
            ..BotSettingsExtras::default()
        };
        let up_mornings = bot_upcoming_with_extras(&bot, extras, &sched, |_| false, now + 60_000);
        assert_eq!(up_mornings.look_around_next_ms, Some(now + 1_410 * 60_000));
        bot.look_around.enabled = false;
        bot.schedules[0].enabled = false;
        let up = bot_upcoming(&bot, &sched, |_| false, now);
        assert_eq!(up.look_around_next_ms, None);
        assert_eq!(up.schedules[0].next_ms, None);
    }

    #[test]
    fn parses_windows_active_time_bias_registry_output() {
        let kst = "\r\nHKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Control\\TimeZoneInformation\r\n    ActiveTimeBias    REG_DWORD    0xfffffde4\r\n";
        assert_eq!(parse_windows_active_time_bias_secs(kst), Some(9 * 3_600));
        assert_eq!(format_fixed_offset_secs(9 * 3_600), "+09:00");

        let edt = "    ActiveTimeBias    REG_DWORD    0xf0\r\n";
        assert_eq!(parse_windows_active_time_bias_secs(edt), Some(-4 * 3_600));
        assert_eq!(format_fixed_offset_secs(-4 * 3_600), "-04:00");
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

    #[test]
    fn look_around_preserves_active_suggestion_ids_and_timestamps_on_refresh() {
        let root = temp_dir("mb37-preserve");
        let store = BotStore::new(root.clone());
        let items = BotItemsStore::new(root.join("bot-items.db"));
        let sched = SchedulerStateStore::new(root.clone());

        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "Stable Bot".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: Some("# Stable Bot\n\nTracks deploys.\n".to_owned()),
                },
                1_000,
            )
            .unwrap();
        items
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot.id.clone(),
                    title: "Ship v2".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    links: Vec::new(),
                },
                &BotItemActor::Owner { device: None },
                1_500,
            )
            .unwrap();

        let first = run_look_around_tracked(&store, &items, Some(&sched), &bot.id, 3_000).unwrap();
        assert_eq!(first.len(), 1);
        let first_id = first[0].id.clone();
        let first_created = first[0].created_at_ms;

        mark_up_next_seen(&sched, Some(&bot.id), 4_000).unwrap();
        assert_eq!(up_next_unseen_count(&store, &sched, Some(&bot.id)), 0);

        // Re-running look-around at t=9_000 must preserve the unchanged suggestion's ID & created_at_ms.
        let second = run_look_around_tracked(&store, &items, Some(&sched), &bot.id, 9_000).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].id, first_id);
        assert_eq!(second[0].created_at_ms, first_created);
        assert_eq!(
            up_next_unseen_count(&store, &sched, Some(&bot.id)),
            0,
            "refreshing unchanged suggestions must not reset unseen count"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn dst_spring_forward_and_posix_tz_rules_evaluate_accurately() {
        // MB-36(b): POSIX TZ DST evaluator handles Northern & Southern hemispheres past 2038.
        // 2040-07-15 12:00:00 UTC (summer in NH -> EDT UTC-4, winter in SH -> AEST UTC+10).
        let july_2040 = days_from_civil(2040, 7, 15) * 86_400 + 12 * 3_600;
        assert_eq!(
            posix_tz_offset_secs("EST5EDT,M3.2.0,M11.1.0", july_2040),
            Some(-4 * 3_600)
        );
        assert_eq!(
            posix_tz_offset_secs("AEST-10AEDT,M10.1.0,M4.1.0/3", july_2040),
            Some(10 * 3_600)
        );
        // 2040-01-15 12:00:00 UTC (winter in NH -> EST UTC-5, summer in SH -> AEDT UTC+11).
        let jan_2040 = days_from_civil(2040, 1, 15) * 86_400 + 12 * 3_600;
        assert_eq!(
            posix_tz_offset_secs("EST5EDT,M3.2.0,M11.1.0", jan_2040),
            Some(-5 * 3_600)
        );
        assert_eq!(
            posix_tz_offset_secs("AEST-10AEDT,M10.1.0,M4.1.0/3", jan_2040),
            Some(11 * 3_600)
        );

        // MB-36(a): Across US spring-forward (2026-03-08 02:00 EST -> 03:00 EDT),
        // at 01:00 EST (06:00 UTC), a `0 9 * * *` cron in America/New_York fires at 09:00 EDT (13:00 UTC),
        // NOT 14:00 UTC!
        let pre_dst_secs = u64::try_from(days_from_civil(2026, 3, 8) * 86_400 + 6 * 3_600).unwrap();
        let expected_9am_edt_ms =
            u64::try_from(days_from_civil(2026, 3, 8) * 86_400 + 13 * 3_600).unwrap() * 1_000;
        // Only run IANA zone assertions if system zoneinfo has America/New_York.
        if tz_offset_minutes(pre_dst_secs, Some("America/New_York")) == -300
            && tz_offset_minutes(pre_dst_secs + 4 * 3_600, Some("America/New_York")) == -240
        {
            let cron = Cron::parse("0 9 * * *").unwrap();
            assert_eq!(
                next_cron_utc_ms(&cron, pre_dst_secs, Some("America/New_York")),
                Some(expected_9am_edt_ms)
            );
            let expected_8am_edt_ms =
                u64::try_from(days_from_civil(2026, 3, 8) * 86_400 + 12 * 3_600).unwrap() * 1_000;
            assert_eq!(
                next_look_around_ms(
                    LookAroundEvery::Mornings,
                    false,
                    Some("America/New_York"),
                    None,
                    pre_dst_secs * 1_000
                ),
                Some(expected_8am_edt_ms)
            );

            // R2M-18: Spring-forward gap (`02:00..02:59` nonexistent) must NOT exhaust retries on `* * * * *` or `*/5 * * * *`.
            let just_before_gap =
                u64::try_from(days_from_civil(2026, 3, 8) * 86_400 + 6 * 3_600 + 59 * 60).unwrap(); // 01:59 EST
            let expected_3am_edt_ms =
                u64::try_from(days_from_civil(2026, 3, 8) * 86_400 + 7 * 3_600).unwrap() * 1_000; // 03:00 EDT (07:00 UTC)
            assert_eq!(
                next_cron_utc_ms(
                    &Cron::parse("* * * * *").unwrap(),
                    just_before_gap,
                    Some("America/New_York")
                ),
                Some(expected_3am_edt_ms)
            );
            assert_eq!(
                next_cron_utc_ms(
                    &Cron::parse("*/5 * * * *").unwrap(),
                    just_before_gap,
                    Some("America/New_York")
                ),
                Some(expected_3am_edt_ms)
            );
        }

        // R2M-18: Fall-back (`2026-11-01 01:00..01:59` repeated in America/New_York).
        let fb_0130_edt =
            u64::try_from(days_from_civil(2026, 11, 1) * 86_400 + 5 * 3_600 + 30 * 60).unwrap(); // 01:30 EDT = 05:30 UTC
        let fb_0159_edt =
            u64::try_from(days_from_civil(2026, 11, 1) * 86_400 + 5 * 3_600 + 59 * 60).unwrap(); // 01:59 EDT = 05:59 UTC
        let fb_0145_edt =
            u64::try_from(days_from_civil(2026, 11, 1) * 86_400 + 5 * 3_600 + 45 * 60).unwrap(); // 01:45 EDT = 05:45 UTC
        if tz_offset_minutes(fb_0130_edt, Some("America/New_York")) == -240
            && tz_offset_minutes(fb_0130_edt + 3_600, Some("America/New_York")) == -300
        {
            let expected_0130_est_ms = (fb_0130_edt + 3_600) * 1_000; // 06:30 UTC
            let expected_0145_edt_ms = fb_0145_edt * 1_000; // 05:45 UTC
            let expected_0105_est_ms =
                u64::try_from(days_from_civil(2026, 11, 1) * 86_400 + 6 * 3_600 + 5 * 60).unwrap()
                    * 1_000; // 06:05 UTC
            assert_eq!(
                next_cron_utc_ms(
                    &Cron::parse("30 1 * * *").unwrap(),
                    fb_0130_edt,
                    Some("America/New_York")
                ),
                Some(expected_0130_est_ms),
                "30 1 * * * at 01:30 EDT must hit 01:30 EST 1h later"
            );
            assert_eq!(
                next_cron_utc_ms(
                    &Cron::parse("30 1 * * *").unwrap(),
                    fb_0159_edt,
                    Some("America/New_York")
                ),
                Some(expected_0130_est_ms),
                "30 1 * * * at 01:59 EDT must hit 01:30 EST 31m later"
            );
            assert_eq!(
                next_cron_utc_ms(
                    &Cron::parse("*/15 1 * * *").unwrap(),
                    fb_0130_edt,
                    Some("America/New_York")
                ),
                Some(expected_0145_edt_ms),
                "*/15 1 * * * at 01:30 EDT must hit 01:45 EDT before 01:00 EST"
            );
            assert_eq!(
                next_cron_utc_ms(
                    &Cron::parse("5 1 * * *").unwrap(),
                    fb_0145_edt,
                    Some("America/New_York")
                ),
                Some(expected_0105_est_ms),
                "5 1 * * * at 01:45 EDT must hit 01:05 EST 20m later"
            );
        }
    }

    #[test]
    fn r2m19_dismissed_suggestion_resurfaces_after_7_day_cooldown() {
        let root = temp_dir("r2m19-cooldown");
        let store = BotStore::new(root.clone());
        let items = BotItemsStore::new(root.join("bot-items.db"));
        let sched = SchedulerStateStore::new(root.clone());

        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "Cooldown Bot".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: Some("# Cooldown Bot\n\nWatches tasks.\n".to_owned()),
                },
                1_000,
            )
            .unwrap();
        items
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot.id.clone(),
                    title: "Unblock release".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    links: Vec::new(),
                },
                &BotItemActor::Owner { device: None },
                1_500,
            )
            .unwrap();

        let initial =
            run_look_around_tracked(&store, &items, Some(&sched), &bot.id, 3_000).unwrap();
        assert_eq!(initial.len(), 1);
        dismiss_suggestion(&store, &sched, &bot.id, &initial[0].id, 4_000).unwrap();

        // Within 7-day cooldown -> stays suppressed.
        let mid = run_look_around_tracked(
            &store,
            &items,
            Some(&sched),
            &bot.id,
            4_000 + 3 * 24 * 3_600 * 1_000,
        )
        .unwrap();
        assert!(mid.is_empty(), "must remain suppressed during 7d cooldown");

        // Past 7-day cooldown -> MUST resurface instead of staying permanently suppressed.
        let after = run_look_around_tracked(
            &store,
            &items,
            Some(&sched),
            &bot.id,
            4_000 + SUGGESTION_DISMISS_COOLDOWN_MS + 1_000,
        )
        .unwrap();
        assert_eq!(
            after.len(),
            1,
            "dismissed suggestion must resurface after 7-day cooldown expires"
        );

        fs::remove_dir_all(&root).ok();
    }
}
