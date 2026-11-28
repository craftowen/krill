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

struct WinnerFact {
    date: Option<String>,
    text: String,
    source: Option<String>,
    order: usize,
    winning_idx: usize,
}

enum TidySegment {
    Raw(String),
    Facts(Vec<usize>),
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
    if mem_dir.exists() && !mem_dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bot memory path is not a directory: {}", mem_dir.display()),
        ));
    }
    for attempt in 0..4 {
        let old = store.get_memory(bot_id)?;
        let parsed = parse_memory_facts(&old);
        if parsed.is_empty() {
            return Ok(("MEMORY.md has no facts to tidy.".to_owned(), 0, None));
        }

        let raw_lines: Vec<&str> = old.lines().collect();
        let mut fact_by_line: HashMap<usize, usize> = HashMap::with_capacity(parsed.len());
        for (idx, fact) in parsed.iter().enumerate() {
            fact_by_line.insert(fact.line, idx);
        }

        // Deduplicate facts globally by normalized text key, preferring the newest date and richer
        // provenance, while tracking the winning parsed index so each surviving fact stays in its
        // own section.
        let mut deduped: BTreeMap<String, WinnerFact> = BTreeMap::new();
        let mut duplicate_count: u32 = 0;

        for (order, fact) in parsed.iter().enumerate() {
            let key = normalize_key(&fact.text);
            if key.is_empty() {
                continue;
            }
            match deduped.get_mut(&key) {
                Some(existing) => {
                    duplicate_count = duplicate_count.saturating_add(1);
                    let replace = match (existing.date.as_deref(), fact.date.as_deref()) {
                        (Some(a), Some(b)) => b > a,
                        (None, Some(_)) => true,
                        _ => false,
                    };
                    if replace {
                        existing.date.clone_from(&fact.date);
                        existing.text = key;
                        existing.order = order;
                        existing.winning_idx = order;
                    }
                    if existing.source.is_none() && fact.source.is_some() {
                        existing.source.clone_from(&fact.source);
                    }
                }
                None => {
                    deduped.insert(
                        key.clone(),
                        WinnerFact {
                            date: fact.date.clone(),
                            text: key,
                            source: fact.source.clone(),
                            order,
                            winning_idx: order,
                        },
                    );
                }
            }
        }

        // Rank all surviving facts newest-date-first so overflow archival always archives the oldest.
        let mut ranked: Vec<WinnerFact> = deduped.into_values().collect();
        ranked.sort_by(|a, b| match (&b.date, &a.date) {
            (Some(db), Some(da)) => db.cmp(da).then_with(|| a.order.cmp(&b.order)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.order.cmp(&b.order),
        });

        // Partition raw lines into preserved non-fact segments (headings, subheadings, prose, blanks)
        // and contiguous fact blocks so no `##` heading or prose after the first fact is dropped.
        let mut segments: Vec<TidySegment> = Vec::new();
        let has_leading_non_fact = raw_lines
            .iter()
            .enumerate()
            .take_while(|(line_idx, _)| !fact_by_line.contains_key(line_idx))
            .any(|(_, line)| !line.trim().is_empty());
        if !has_leading_non_fact {
            segments.push(TidySegment::Raw("# Memory".to_owned()));
            segments.push(TidySegment::Raw(String::new()));
        }
        for (line_idx, line) in raw_lines.iter().enumerate() {
            if let Some(&fact_idx) = fact_by_line.get(&line_idx) {
                if let Some(TidySegment::Facts(group)) = segments.last_mut() {
                    group.push(fact_idx);
                } else {
                    segments.push(TidySegment::Facts(vec![fact_idx]));
                }
            } else {
                let trimmed = (*line).trim_end().to_owned();
                // Collapse consecutive blank lines while preserving section spacing.
                if trimmed.is_empty()
                    && matches!(segments.last(), Some(TidySegment::Raw(prev)) if prev.is_empty())
                {
                    continue;
                }
                segments.push(TidySegment::Raw(trimmed));
            }
        }
        while matches!(segments.last(), Some(TidySegment::Raw(s)) if s.is_empty()) {
            segments.pop();
        }

        let raw_line_count = segments
            .iter()
            .filter(|seg| matches!(seg, TidySegment::Raw(_)))
            .count();
        let max_facts = BOT_MEMORY_INDEX_LINE_CAP.saturating_sub(raw_line_count);

        let mut archived_count: u32 = 0;
        let mut archive_notice: Option<String> = None;
        let mut archived_winners: std::collections::HashSet<usize> =
            std::collections::HashSet::new();
        let mut pending_archive: Option<(std::path::PathBuf, Vec<String>, String)> = None;
        if ranked.len() > max_facts && max_facts > 1 {
            let keep_count = max_facts.saturating_sub(1);
            let overflow = ranked.get(keep_count..).unwrap_or_default();
            archived_count = u32::try_from(overflow.len()).unwrap_or(u32::MAX);

            let ym = format_utc_ym(now_ms / 1_000);
            let archive_name = format!("archive-{ym}.md");
            let archive_path = mem_dir.join(&archive_name);
            ensure_no_symlink_path(&archive_path)?;
            let mut overflow_lines = Vec::with_capacity(overflow.len());
            for fact in overflow {
                archived_winners.insert(fact.winning_idx);
                overflow_lines.push(format_fact(
                    fact.date.as_deref(),
                    &fact.text,
                    fact.source.as_deref(),
                ));
            }
            pending_archive = Some((archive_path, overflow_lines, ym));

            let today = format_utc_ymd(now_ms / 1_000);
            archive_notice = Some(format!(
                "- {today} Archived {archived_count} older memory fact(s) in memory/{archive_name}"
            ));
        }

        let mut winner_by_idx: HashMap<usize, &WinnerFact> = HashMap::with_capacity(ranked.len());
        for fact in &ranked {
            if !archived_winners.contains(&fact.winning_idx) {
                winner_by_idx.insert(fact.winning_idx, fact);
            }
        }

        let mut rebuilt_lines: Vec<String> = Vec::new();
        for seg in segments {
            match seg {
                TidySegment::Raw(line) => {
                    if line.is_empty() && rebuilt_lines.last().is_some_and(String::is_empty) {
                        continue;
                    }
                    rebuilt_lines.push(line);
                }
                TidySegment::Facts(group) => {
                    let mut surviving: Vec<&WinnerFact> = group
                        .into_iter()
                        .filter_map(|idx| winner_by_idx.get(&idx).copied())
                        .collect();
                    surviving.sort_by(|a, b| match (&b.date, &a.date) {
                        (Some(db), Some(da)) => db.cmp(da).then_with(|| a.order.cmp(&b.order)),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => a.order.cmp(&b.order),
                    });
                    for fact in surviving {
                        rebuilt_lines.push(format_fact(
                            fact.date.as_deref(),
                            &fact.text,
                            fact.source.as_deref(),
                        ));
                    }
                }
            }
        }
        if let Some(notice) = archive_notice {
            rebuilt_lines.push(notice);
        }
        while rebuilt_lines.last().is_some_and(String::is_empty) {
            rebuilt_lines.pop();
        }

        let mut rebuilt = rebuilt_lines.join("\n");
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

        // Write archive file before CAS commit (append-only deduplicated by line, so retries are safe).
        if let Some((archive_path, overflow_lines, ym)) = pending_archive {
            let mut archive_body = std::fs::read_to_string(&archive_path)
                .unwrap_or_else(|_| format!("# Archived Memory ({ym})\n\n"));
            if !archive_body.is_empty() && !archive_body.ends_with('\n') {
                archive_body.push('\n');
            }
            for line in overflow_lines {
                if !archive_body.lines().any(|existing| existing == line) {
                    archive_body.push_str(&line);
                    archive_body.push('\n');
                }
            }
            write_atomic_0600(&archive_path, archive_body.as_bytes())?;
        }

        match store.apply_memory_with_history_cas(
            bot_id,
            &old,
            &rebuilt,
            true,
            "system:tidy",
            "tidy",
            now_ms,
        ) {
            Ok(entry) => {
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
                return Ok((summary, changes, entry.map(|e| e.entry_id)));
            }
            Err(crate::bots::store::MemoryUpdateError::Io(ref err))
                if err.kind() == io::ErrorKind::WouldBlock && attempt < 3 =>
            {
                // Concurrent write to MEMORY.md occurred between read and commit; retry with
                // fresh content so newly added facts are never overwritten (`BOT-13`).
            }
            Err(err) => return Err(io::Error::other(err.to_string())),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "MEMORY.md changed concurrently during tidy; please retry",
    ))
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

    #[test]
    fn memory_tidy_preserves_subheadings_and_prose() {
        let root = temp_dir("subheadings");
        let store = BotStore::new(root.clone());
        let sched = SchedulerStateStore::new(root.clone());
        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "Section Bot".to_owned(),
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

        let structured = "# Memory\n\n\
                          ## Preferences\n\
                          Always prefer concise answers.\n\
                          - 2026-11-01 Uses Rust\n\
                          - 2026-11-10 Uses   Rust · from: Chat\n\n\
                          ## Architecture\n\
                          Daemon owns PTY state.\n\
                          - 2026-11-02 Target dir is external SSD\n\
                          - 2026-11-12 Never drop user headings\n";
        store.update_memory(&bot.id, structured, true).unwrap();

        let (_, changes) =
            run_memory_tidy_tracked(&store, Some(&sched), &bot.id, 1_795_000_000_000).unwrap();
        assert!(changes >= 1);

        let cleaned = store.get_memory(&bot.id).unwrap();
        assert!(cleaned.contains("## Preferences"), "{cleaned}");
        assert!(
            cleaned.contains("Always prefer concise answers."),
            "{cleaned}"
        );
        assert!(cleaned.contains("## Architecture"), "{cleaned}");
        assert!(cleaned.contains("Daemon owns PTY state."), "{cleaned}");
        assert!(cleaned.contains("Never drop user headings"), "{cleaned}");
        assert!(cleaned.contains("Target dir is external SSD"), "{cleaned}");
        assert_eq!(parse_memory_facts(&cleaned).len(), 3);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn memory_tidy_archive_uses_exact_line_dedup_and_trailing_newline() {
        use std::fmt::Write as _;
        let root = temp_dir("archive-exact");
        let store = BotStore::new(root.clone());
        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "Archive Bot".to_owned(),
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

        let now_ms = 1_795_000_000_000_u64;
        let ym = format_utc_ym(now_ms / 1_000);
        let home = store.bot_home(&bot);
        let mem_dir = home.join("memory");
        fs::create_dir_all(&mem_dir).unwrap();
        let archive_path = mem_dir.join(format!("archive-{ym}.md"));
        // Seed archive WITHOUT trailing newline and containing a longer fact whose prefix is "- 2020-01-01 foo".
        fs::write(
            &archive_path,
            "# Archived Memory\n\n- 2020-01-01 foo bar baz",
        )
        .unwrap();

        let mut content = String::from("# Memory\n\n");
        for i in 0..220 {
            writeln!(content, "- 2026-11-10 Newer fact #{i}").unwrap();
        }
        content.push_str("- 2020-01-01 foo\n");
        store.update_memory(&bot.id, &content, true).unwrap();

        let (_, changes) = run_memory_tidy_tracked(&store, None, &bot.id, now_ms).unwrap();
        assert!(changes >= 1);

        let archived = fs::read_to_string(&archive_path).unwrap();
        let lines: Vec<&str> = archived.lines().collect();
        assert!(
            lines.contains(&"- 2020-01-01 foo bar baz"),
            "existing line must not be glued: {archived}"
        );
        assert!(
            lines.contains(&"- 2020-01-01 foo"),
            "substring match must not drop shorter distinct fact: {archived}"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn memory_cas_detects_concurrent_append_without_overwriting() {
        let root = std::env::temp_dir().join(format!("krill-tidy-cas-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let store = BotStore::new(root.clone());
        let bot = store
            .create_bot(
                krill_proto::bots::CreateBotParams {
                    name: "CasBot".to_owned(),
                    description: String::new(),
                    agent_id: "claude".to_owned(),
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

        let initial = "# Memory\n\n- 2026-11-01 Fact A\n";
        store.update_memory(&bot.id, initial, false).unwrap();

        // Simulate a concurrent writer appending Fact B after tidy read `initial` (`BOT-13`).
        let concurrent = "# Memory\n\n- 2026-11-01 Fact A\n- 2026-11-28 Concurrent Fact B\n";
        store.update_memory(&bot.id, concurrent, false).unwrap();

        let err = store
            .apply_memory_with_history_cas(
                &bot.id,
                initial,
                "# Memory\n\n- 2026-11-01 Fact A (tidied)\n",
                false,
                "tidy",
                "tidy",
                2_000,
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                crate::bots::store::MemoryUpdateError::Io(ref e)
                    if e.kind() == io::ErrorKind::WouldBlock
            ),
            "expected WouldBlock CAS error, got {err:?}"
        );
        let after = store.get_memory(&bot.id).unwrap();
        assert!(
            after.contains("Concurrent Fact B"),
            "CAS mismatch must preserve concurrent MEMORY.md addition: {after}"
        );

        fs::remove_dir_all(&root).ok();
    }
}
