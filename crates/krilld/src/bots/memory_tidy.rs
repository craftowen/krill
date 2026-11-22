// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Safe `MEMORY.md` tidy-up & consolidation engine.
//!
//! Preserves every unique non-duplicate fact (respecting `MemoryGuard`), removes exact
//! normalized duplicates keeping the newest dated/sourced copy, sorts dated facts newest-first,
//! collapses excess blank lines, archives overflow beyond [`BOT_MEMORY_INDEX_LINE_CAP`] into
//! `memory/archive-YYYY-MM.md`, records durable undo history, and tracks consecutive failures
//! with automatic disablement after [`AUTO_DISABLE_AFTER_FAILURES`] failures.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::Mutex;

use krill_proto::bots::{
    BOT_MEMORY_INDEX_LINE_CAP, BOT_MEMORY_SOURCE_SEPARATOR, BotId, BotMemoryTidyStatus, TidyEvery,
    parse_memory_facts, validate_memory_edit,
};
use krill_proto::os::write_atomic_0600;

use crate::bots::look_around::SchedulerStateStore;
use crate::bots::memory_history;
use crate::bots::secrets::ensure_no_symlink_path;
use crate::bots::store::BotStore;
use crate::session::lock;

/// Consecutive periodic tidy failures before auto-disabling scheduled tidy for that bot.
pub const AUTO_DISABLE_AFTER_FAILURES: u32 = 10;

/// In-memory fallback tracker when no [`SchedulerStateStore`] is attached (used in isolated tests).
#[derive(Debug, Default)]
pub struct MemoryTidyTracker {
    last_run_ms: Mutex<HashMap<BotId, u64>>,
}

impl MemoryTidyTracker {
    #[must_use]
    pub fn is_due(&self, bot_id: &BotId, every: TidyEvery, tz: Option<&str>, now_ms: u64) -> bool {
        let Some(after) = super::look_around::tidy_due_after_ms(every, tz, now_ms) else {
            return false;
        };
        let guard = lock(&self.last_run_ms);
        guard.get(bot_id).is_none_or(|&last| last < after)
    }

    pub fn record(&self, bot_id: &BotId, now_ms: u64) {
        lock(&self.last_run_ms).insert(bot_id.clone(), now_ms);
    }
}

/// Runs memory tidy-up on `bot_id`'s `MEMORY.md` and returns `(summary, change_count)`.
pub fn run_memory_tidy(store: &BotStore, bot_id: &BotId, now_ms: u64) -> io::Result<(String, u32)> {
    run_memory_tidy_tracked(store, None, bot_id, now_ms)
}

/// Runs memory tidy-up with durable [`SchedulerStateStore`] failure/success/undo tracking.
pub fn run_memory_tidy_tracked(
    store: &BotStore,
    scheduler_state: Option<&SchedulerStateStore>,
    bot_id: &BotId,
    now_ms: u64,
) -> io::Result<(String, u32)> {
    match run_memory_tidy_inner(store, bot_id, now_ms) {
        Ok((summary, changes, tidy_id)) => {
            if let Some(state) = scheduler_state {
                state.record_tidy_success(bot_id, tidy_id, now_ms)?;
            }
            Ok((summary, changes))
        }
        Err(err) => {
            if let Some(state) = scheduler_state {
                state.record_tidy_failure(bot_id, now_ms).ok();
            }
            Err(err)
        }
    }
}

/// Returns persistent tidy status for `bot_id`.
pub fn get_memory_tidy_status(
    store: &BotStore,
    scheduler_state: &SchedulerStateStore,
    bot_id: &BotId,
) -> io::Result<BotMemoryTidyStatus> {
    let bot = store.get_bot(bot_id)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("bot not found: {}", bot_id.as_str()),
        )
    })?;
    let home = store.bot_home(&bot);
    let state = scheduler_state.get_tidy_state(bot_id)?;
    let last_tidy_id = state.last_tidy_id.or_else(|| {
        memory_history::latest_tidy_entry(&home)
            .ok()
            .flatten()
            .map(|e| e.entry_id)
    });
    let undoable = last_tidy_id.as_deref().is_some_and(|id| {
        memory_history::find_entry(&home, id)
            .ok()
            .flatten()
            .is_some_and(|e| e.undoable && !e.undone)
    });

    Ok(BotMemoryTidyStatus {
        enabled: bot.memory_tidy.enabled,
        interval_secs: bot.memory_tidy.interval_secs,
        last_run_ms: state.last_run_ms,
        last_tidy_id,
        consecutive_failures: state.consecutive_failures,
        auto_disabled: state.auto_disabled,
        undoable,
    })
}

/// Reverts the specified (or latest) tidy change on `bot_id`'s `MEMORY.md`.
pub fn undo_memory_tidy(
    store: &BotStore,
    scheduler_state: &SchedulerStateStore,
    bot_id: &BotId,
    tidy_id: Option<&str>,
    actor: &str,
    now_ms: u64,
) -> io::Result<(String, Vec<String>, usize)> {
    let bot = store.get_bot(bot_id)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("bot not found: {}", bot_id.as_str()),
        )
    })?;
    let home = store.bot_home(&bot);
    let target_id = if let Some(explicit) = tidy_id.map(str::trim).filter(|s| !s.is_empty()) {
        explicit.to_owned()
    } else if let Some(recorded) = scheduler_state.get_tidy_state(bot_id)?.last_tidy_id {
        recorded
    } else if let Some(latest) = memory_history::latest_tidy_entry(&home)? {
        latest.entry_id
    } else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no memory tidy run found to undo",
        ));
    };

    let restored = store.undo_memory_entry(bot_id, &target_id, actor, now_ms)?;
    scheduler_state.clear_tidy_undo_id(bot_id, &target_id).ok();
    Ok((restored, Vec::new(), 0))
}

fn run_memory_tidy_inner(
    store: &BotStore,
    bot_id: &BotId,
    now_ms: u64,
) -> io::Result<(String, u32, Option<String>)> {
    let bot = store.get_bot(bot_id)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("bot not found: {}", bot_id.as_str()),
        )
    })?;
    let home = store.bot_home(&bot);
    ensure_no_symlink_path(&home)?;
    let mem_dir = home.join("memory");
    ensure_no_symlink_path(&mem_dir)?;
    let old = store.get_memory(bot_id)?;
    let parsed = parse_memory_facts(&old);
    if parsed.is_empty() {
        return Ok(("MEMORY.md has no facts to tidy.".to_owned(), 0, None));
    }

    // Preserve header lines above the first fact (or `# Memory` fallback).
    let first_fact_line = parsed.first().map_or(0, |f| f.line);
    let raw_lines: Vec<&str> = old.lines().collect();
    let mut header_lines: Vec<String> = raw_lines
        .get(..first_fact_line)
        .unwrap_or_default()
        .iter()
        .map(|l| (*l).trim_end().to_owned())
        .collect();
    while header_lines.last().is_some_and(String::is_empty) {
        header_lines.pop();
    }
    if header_lines.is_empty() {
        header_lines.push("# Memory".to_owned());
    }

    // Deduplicate facts by normalized text key, preferring the newest date and richer provenance.
    let mut deduped: BTreeMap<String, (Option<String>, String, Option<String>, usize)> =
        BTreeMap::new();
    let mut duplicate_count: u32 = 0;

    for (order, fact) in parsed.iter().enumerate() {
        let key = normalize_key(&fact.text);
        if key.is_empty() {
            continue;
        }
        match deduped.get_mut(&key) {
            Some((existing_date, existing_text, existing_src, existing_order)) => {
                duplicate_count = duplicate_count.saturating_add(1);
                let replace = match (existing_date.as_deref(), fact.date.as_deref()) {
                    (Some(a), Some(b)) => b > a,
                    (None, Some(_)) => true,
                    _ => false,
                };
                if replace {
                    existing_date.clone_from(&fact.date);
                    *existing_text = normalize_key(&fact.text);
                    *existing_order = order;
                }
                if existing_src.is_none() && fact.source.is_some() {
                    existing_src.clone_from(&fact.source);
                }
            }
            None => {
                deduped.insert(
                    key,
                    (
                        fact.date.clone(),
                        normalize_key(&fact.text),
                        fact.source.clone(),
                        order,
                    ),
                );
            }
        }
    }

    // Sort newest date first, preserving original order as tie-breaker.
    let mut ordered: Vec<(Option<String>, String, Option<String>, usize)> =
        deduped.into_values().collect();
    ordered.sort_by(|a, b| match (&b.0, &a.0) {
        (Some(db), Some(da)) => db.cmp(da).then_with(|| a.3.cmp(&b.3)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.3.cmp(&b.3),
    });

    let formatted_facts: Vec<String> = ordered
        .iter()
        .map(|(date, text, src, _)| format_fact(date.as_deref(), text, src.as_deref()))
        .collect();

    // Reserve header + blank separator line inside `BOT_MEMORY_INDEX_LINE_CAP`.
    let header_overhead = header_lines.len().saturating_add(1);
    let max_facts = BOT_MEMORY_INDEX_LINE_CAP.saturating_sub(header_overhead);

    let mut archived_count: u32 = 0;
    let final_facts = if formatted_facts.len() > max_facts && max_facts > 1 {
        let keep_count = max_facts.saturating_sub(1);
        let kept = formatted_facts
            .get(..keep_count)
            .unwrap_or_default()
            .to_vec();
        let overflow = formatted_facts.get(keep_count..).unwrap_or_default();
        archived_count = u32::try_from(overflow.len()).unwrap_or(u32::MAX);

        let ym = format_utc_ym(now_ms / 1_000);
        let archive_name = format!("archive-{ym}.md");
        let archive_path = mem_dir.join(&archive_name);
        ensure_no_symlink_path(&archive_path)?;
        let mut archive_body = std::fs::read_to_string(&archive_path)
            .unwrap_or_else(|_| format!("# Archived Memory ({ym})\n\n"));
        for line in overflow {
            if !archive_body.contains(line.as_str()) {
                archive_body.push_str(line);
                archive_body.push('\n');
            }
        }
        write_atomic_0600(&archive_path, archive_body.as_bytes())?;

        let mut next = kept;
        let today = format_utc_ymd(now_ms / 1_000);
        next.push(format!(
            "- {today} Archived {archived_count} older memory fact(s) in memory/{archive_name}"
        ));
        next
    } else {
        formatted_facts
    };

    let mut rebuilt = header_lines.join("\n");
    rebuilt.push_str("\n\n");
    rebuilt.push_str(&final_facts.join("\n"));
    rebuilt.push('\n');

    let changed = rebuilt != old;
    if !changed {
        return Ok((
            "MEMORY.md is already tidy (no changes needed).".to_owned(),
            0,
            None,
        ));
    }

    if archived_count == 0 {
        // Without archival, every single unique fact MUST pass `validate_memory_edit`.
        validate_memory_edit(&old, &rebuilt).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("memory tidy refused to drop facts: {err}"),
            )
        })?;
    }

    let entry = store
        .apply_memory_with_history(bot_id, &rebuilt, true, "system:tidy", "tidy", now_ms)
        .map_err(|err| io::Error::other(err.to_string()))?;

    let changes = duplicate_count
        .saturating_add(archived_count)
        .max(u32::from(changed));
    let summary = if archived_count > 0 {
        format!(
            "Tidied MEMORY.md: removed {duplicate_count} duplicate(s) and archived {archived_count} overflow fact(s)."
        )
    } else if duplicate_count > 0 {
        format!(
            "Tidied MEMORY.md: removed {duplicate_count} duplicate fact(s) and normalized ordering."
        )
    } else {
        "Tidied MEMORY.md: normalized formatting and date ordering.".to_owned()
    };

    Ok((summary, changes, entry.map(|e| e.entry_id)))
}

fn normalize_key(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_fact(date: Option<&str>, text: &str, source: Option<&str>) -> String {
    let mut line = String::from("- ");
    if let Some(d) = date {
        line.push_str(d);
        line.push(' ');
    }
    line.push_str(text);
    if let Some(src) = source {
        line.push_str(BOT_MEMORY_SOURCE_SEPARATOR);
        line.push_str(src);
    }
    line
}

pub(crate) fn format_utc_ymd(unix_secs: u64) -> String {
    let (y, m, d) = civil_from_unix_secs(unix_secs);
    format!("{y:04}-{m:02}-{d:02}")
}

fn format_utc_ym(unix_secs: u64) -> String {
    let (y, m, _) = civil_from_unix_secs(unix_secs);
    format!("{y:04}-{m:02}")
}

pub(crate) fn civil_from_unix_secs(unix_secs: u64) -> (i64, u32, u32) {
    let days = i64::try_from(unix_secs / 86_400).unwrap_or(0);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = u64::try_from(z - era * 146_097).unwrap_or(0);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::try_from(yoe).unwrap_or(0) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    (
        year,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::CreateBotParams;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-bot-tidy-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn memory_tidy_deduplicates_sorts_and_preserves_all_unique_facts() {
        let root = temp_dir("dedup");
        let store = BotStore::new(root.clone());
        let sched = SchedulerStateStore::new(root.clone());
        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "Tidy Bot".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: None,
                },
                1_000,
            )
            .unwrap();

        let messy = "# Memory\n\n\
                     - 2026-11-10 Primary repo is craftowen/krill\n\
                     - 2026-11-15 Primary repo is   craftowen/krill · from: Slack\n\
                     - 2026-11-18 Always run clippy before PR\n";
        store.update_memory(&bot.id, messy, true).unwrap();

        let (summary, changes) =
            run_memory_tidy_tracked(&store, Some(&sched), &bot.id, 1_795_000_000_000).unwrap();
        assert!(changes >= 1);
        assert!(summary.contains("1 duplicate"));

        let cleaned = store.get_memory(&bot.id).unwrap();
        let facts = parse_memory_facts(&cleaned);
        assert_eq!(facts.len(), 2);
        assert_eq!(
            facts.first().and_then(|f| f.date.as_deref()),
            Some("2026-11-18")
        );
        assert_eq!(
            facts.get(1).and_then(|f| f.date.as_deref()),
            Some("2026-11-15")
        );
        assert_eq!(
            facts.get(1).and_then(|f| f.source.as_deref()),
            Some("Slack")
        );
        assert!(validate_memory_edit(messy, &cleaned).is_ok());

        let status = get_memory_tidy_status(&store, &sched, &bot.id).unwrap();
        assert!(status.undoable);
        assert_eq!(status.consecutive_failures, 0);
        assert!(!status.auto_disabled);

        let (restored, _, _) =
            undo_memory_tidy(&store, &sched, &bot.id, None, "owner", 1_795_000_010_000).unwrap();
        assert_eq!(restored, messy);
        let status_after = get_memory_tidy_status(&store, &sched, &bot.id).unwrap();
        assert!(!status_after.undoable);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn auto_disables_after_ten_consecutive_failures() {
        let root = temp_dir("auto-disable");
        let store = BotStore::new(root.clone());
        let sched = SchedulerStateStore::new(root.clone());
        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "Fail Bot".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: None,
                },
                1_000,
            )
            .unwrap();

        let mem_dir = store.bot_home(&bot).join("memory");
        fs::remove_dir_all(&mem_dir).ok();
        #[cfg(unix)]
        {
            let outside = root.join("outside.md");
            fs::write(&outside, b"- 2026-11-10 outside\n").unwrap();
            std::os::unix::fs::symlink(&outside, &mem_dir).unwrap();
        }
        #[cfg(not(unix))]
        {
            fs::write(&mem_dir, b"not-a-directory\n").unwrap();
        }

        for attempt in 1..=AUTO_DISABLE_AFTER_FAILURES {
            assert!(
                run_memory_tidy_tracked(&store, Some(&sched), &bot.id, 10_000 + u64::from(attempt))
                    .is_err()
            );
        }
        let status = sched.get_tidy_state(&bot.id).unwrap();
        assert_eq!(status.consecutive_failures, AUTO_DISABLE_AFTER_FAILURES);
        assert!(status.auto_disabled);
        assert!(!sched.tidy_is_due(&bot.id, TidyEvery::Night, None, 1_000_000));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn presets_run_at_their_local_hours() {
        use super::super::look_around::{latest_slot_ms, tidy_due_after_ms};
        use krill_proto::bots::LookAroundEvery;
        const H: u64 = 3_600_000;
        let day = 20_000 * 24 * H; // a UTC midnight
        // Before 8 AM nothing has come due today; at 13:30 the 12:00 slot of every 4 hours has.
        assert_eq!(
            latest_slot_ms(day + 7 * H, None, LookAroundEvery::Every4h.hours()),
            None
        );
        assert_eq!(
            latest_slot_ms(
                day + 13 * H + 30 * 60_000,
                None,
                LookAroundEvery::Every4h.hours()
            ),
            Some(day + 12 * H)
        );
        assert_eq!(
            latest_slot_ms(
                day + 23 * H,
                Some("+09:00"),
                LookAroundEvery::Mornings.hours()
            ),
            Some(day + 23 * H)
        );
        // Each night: today's 3 AM from 3 AM on, yesterday's before it; a week is six days earlier.
        assert_eq!(
            tidy_due_after_ms(TidyEvery::Night, None, day + 5 * H),
            Some(day + 3 * H)
        );
        assert_eq!(
            tidy_due_after_ms(TidyEvery::Night, None, day + H),
            Some(day + 3 * H - 24 * H)
        );
        assert_eq!(
            tidy_due_after_ms(TidyEvery::Week, None, day + 5 * H),
            Some(day + 3 * H - 6 * 24 * H)
        );
    }
}
