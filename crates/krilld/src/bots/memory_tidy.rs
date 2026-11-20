// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Safe `MEMORY.md` tidy-up & consolidation engine.
//!
//! Preserves every unique non-duplicate fact (respecting `MemoryGuard`), removes exact
//! normalized duplicates keeping the newest dated/sourced copy, sorts dated facts newest-first,
//! collapses excess blank lines, and archives overflow beyond [`BOT_MEMORY_INDEX_LINE_CAP`] into
//! `memory/archive-YYYY-MM.md` before trimming the index.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::Mutex;

use krill_proto::bots::{
    BOT_MEMORY_INDEX_LINE_CAP, BOT_MEMORY_SOURCE_SEPARATOR, BotId, parse_memory_facts,
    validate_memory_edit,
};
use krill_proto::os::write_atomic_0600;

use crate::bots::store::BotStore;
use crate::session::lock;

/// Tracks per-bot last memory tidy timestamps for periodic scheduler ticks.
#[derive(Debug, Default)]
pub struct MemoryTidyTracker {
    last_run_ms: Mutex<HashMap<BotId, u64>>,
}

impl MemoryTidyTracker {
    #[must_use]
    pub fn is_due(&self, bot_id: &BotId, interval_secs: u64, now_ms: u64) -> bool {
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

/// Runs memory tidy-up on `bot_id`'s `MEMORY.md` and returns `(summary, change_count)`.
pub fn run_memory_tidy(store: &BotStore, bot_id: &BotId, now_ms: u64) -> io::Result<(String, u32)> {
    let bot = store.get_bot(bot_id)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("bot not found: {}", bot_id.as_str()),
        )
    })?;
    let old = store.get_memory(bot_id)?;
    let parsed = parse_memory_facts(&old);
    if parsed.is_empty() {
        return Ok(("MEMORY.md has no facts to tidy.".to_owned(), 0));
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
        let archive_path = store.bot_home(&bot).join("memory").join(&archive_name);
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
        store
            .update_memory(bot_id, &rebuilt, false)
            .map_err(|err| io::Error::other(err.to_string()))?;
    } else {
        // Overflow facts were durably archived to `memory/archive-YYYY-MM.md` first.
        store
            .update_memory(bot_id, &rebuilt, true)
            .map_err(|err| io::Error::other(err.to_string()))?;
    }

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

    Ok((summary, changes))
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

fn civil_from_unix_secs(unix_secs: u64) -> (i64, u32, u32) {
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

        let (summary, changes) = run_memory_tidy(&store, &bot.id, 1_795_000_000_000).unwrap();
        assert!(changes >= 1);
        assert!(summary.contains("1 duplicate"));

        let cleaned = store.get_memory(&bot.id).unwrap();
        let facts = parse_memory_facts(&cleaned);
        assert_eq!(facts.len(), 2);
        // Newest first.
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
        // Passes MemoryGuard against original messy input!
        assert!(validate_memory_edit(messy, &cleaned).is_ok());

        fs::remove_dir_all(&root).ok();
    }
}
