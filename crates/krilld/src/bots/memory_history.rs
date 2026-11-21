// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Append-only `MEMORY.md` change history, single-fact editing, undo, and topic files (`memory/<topic>.md`).

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use krill_proto::bots::{
    BOT_MEMORY_SOURCE_SEPARATOR, BotId, BotMemoryHistoryEntry, BotMemoryTopicInfo,
    parse_memory_facts,
};
use krill_proto::os::fs::DirBuilderExt as _;
use krill_proto::os::write_atomic_0600;

use crate::bots::secrets::ensure_no_symlink_path;
use crate::session::lock;

const HISTORY_FILE: &str = ".history.jsonl";
const MAX_HISTORY_ENTRIES: usize = 400;
const MAX_HISTORY_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOPIC_CONTENT_BYTES: usize = 512 * 1024;
const DEFAULT_HISTORY_LIMIT: usize = 50;
const MAX_HISTORY_LIMIT: usize = 500;

static ENTRY_SEQ: AtomicU64 = AtomicU64::new(1);
static HISTORY_IO_LOCK: Mutex<()> = Mutex::new(());

/// Records an immutable diff entry in `<bot_home>/memory/.history.jsonl` when `before != after`.
#[expect(
    clippy::too_many_arguments,
    reason = "memory change audit diff parameters"
)]
pub(crate) fn record_change(
    bot_home: &Path,
    bot_id: &BotId,
    actor: &str,
    action: &str,
    before: &str,
    after: &str,
    timestamp_ms: u64,
    undoable: bool,
) -> io::Result<Option<BotMemoryHistoryEntry>> {
    if before == after {
        return Ok(None);
    }
    let _guard = lock(&HISTORY_IO_LOCK);
    let mem_dir = ensure_memory_dir(bot_home, true)?;
    let history_path = mem_dir.join(HISTORY_FILE);
    ensure_no_symlink_path(&history_path)?;

    let (added, removed) = compute_fact_diff(before, after);
    let seq = ENTRY_SEQ.fetch_add(1, Ordering::Relaxed);
    let prefix = if action == "tidy" { "tidy" } else { "mem" };
    let entry = BotMemoryHistoryEntry {
        entry_id: format!("{prefix}-{timestamp_ms}-{seq}"),
        bot_id: bot_id.clone(),
        timestamp_ms,
        actor: actor.to_owned(),
        action: action.to_owned(),
        added,
        removed,
        before_content: before.to_owned(),
        after_content: after.to_owned(),
        undoable,
        undone: false,
    };

    let mut entries = read_entries_locked(&history_path)?;
    entries.push(entry.clone());
    write_entries_locked(&history_path, &mut entries)?;
    Ok(Some(entry))
}

/// Lists up to `limit` newest entries from `<bot_home>/memory/.history.jsonl`.
pub(crate) fn list_history(
    bot_home: &Path,
    limit: Option<u32>,
) -> io::Result<Vec<BotMemoryHistoryEntry>> {
    let _guard = lock(&HISTORY_IO_LOCK);
    let mem_dir = bot_home.join("memory");
    ensure_no_symlink_path(bot_home)?;
    ensure_no_symlink_path(&mem_dir)?;
    let history_path = mem_dir.join(HISTORY_FILE);
    ensure_no_symlink_path(&history_path)?;
    let mut entries = read_entries_locked(&history_path)?;
    entries.reverse();
    let cap = limit
        .and_then(|n| usize::try_from(n).ok())
        .map_or(DEFAULT_HISTORY_LIMIT, |n| n.clamp(1, MAX_HISTORY_LIMIT));
    entries.truncate(cap);
    Ok(entries)
}

/// Finds a single history entry by exact `entry_id`.
pub(crate) fn find_entry(
    bot_home: &Path,
    entry_id: &str,
) -> io::Result<Option<BotMemoryHistoryEntry>> {
    let _guard = lock(&HISTORY_IO_LOCK);
    let history_path = bot_home.join("memory").join(HISTORY_FILE);
    ensure_no_symlink_path(bot_home)?;
    ensure_no_symlink_path(&bot_home.join("memory"))?;
    ensure_no_symlink_path(&history_path)?;
    let entries = read_entries_locked(&history_path)?;
    Ok(entries.into_iter().rev().find(|e| e.entry_id == entry_id))
}

/// Returns the most recent `tidy` history entry for `bot_home`, if any.
pub(crate) fn latest_tidy_entry(bot_home: &Path) -> io::Result<Option<BotMemoryHistoryEntry>> {
    let _guard = lock(&HISTORY_IO_LOCK);
    let history_path = bot_home.join("memory").join(HISTORY_FILE);
    ensure_no_symlink_path(bot_home)?;
    ensure_no_symlink_path(&bot_home.join("memory"))?;
    ensure_no_symlink_path(&history_path)?;
    let entries = read_entries_locked(&history_path)?;
    Ok(entries.into_iter().rev().find(|e| e.action == "tidy"))
}

/// Marks `entry_id` as `undone = true` (and `undoable = false`) in `.history.jsonl`.
pub(crate) fn mark_undone(bot_home: &Path, entry_id: &str) -> io::Result<bool> {
    let _guard = lock(&HISTORY_IO_LOCK);
    let history_path = bot_home.join("memory").join(HISTORY_FILE);
    ensure_no_symlink_path(bot_home)?;
    ensure_no_symlink_path(&bot_home.join("memory"))?;
    ensure_no_symlink_path(&history_path)?;
    let mut entries = read_entries_locked(&history_path)?;
    let mut updated = false;
    for entry in &mut entries {
        if entry.entry_id == entry_id && !entry.undone {
            entry.undone = true;
            entry.undoable = false;
            updated = true;
        }
    }
    if updated {
        write_entries_locked(&history_path, &mut entries)?;
    }
    Ok(updated)
}

/// Rewrites a single fact line in `MEMORY.md` while keeping line endings clean.
pub(crate) fn rewrite_single_fact(
    existing: &str,
    old_fact: &str,
    new_fact: &str,
) -> io::Result<String> {
    let needle_raw = old_fact.trim();
    let replacement_raw = new_fact.trim();
    if needle_raw.is_empty() || replacement_raw.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "old_fact and new_fact must be non-empty",
        ));
    }
    if replacement_raw.contains('\n') || replacement_raw.contains('\r') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "new_fact must be a single line",
        ));
    }

    let parsed = parse_memory_facts(existing);
    let mut lines: Vec<String> = existing.lines().map(ToOwned::to_owned).collect();

    // Try matching by exact bullet line first, then by parsed fact text.
    let needle_no_bullet = needle_raw.strip_prefix("- ").unwrap_or(needle_raw).trim();
    let mut match_lines: Vec<(usize, Option<String>, Option<String>)> = Vec::new();
    for fact in &parsed {
        let raw_line = lines.get(fact.line).map_or("", String::as_str).trim();
        let norm_fact = normalize_fact_text(&fact.text);
        let norm_needle = normalize_fact_text(needle_no_bullet);
        if raw_line == needle_raw
            || raw_line.strip_prefix("- ").unwrap_or(raw_line).trim() == needle_no_bullet
            || (!norm_needle.is_empty() && norm_fact == norm_needle)
        {
            match_lines.push((fact.line, fact.date.clone(), fact.source.clone()));
        }
    }

    if match_lines.is_empty() {
        // Fall back to exact non-bullet line match.
        for (idx, line) in lines.iter().enumerate() {
            if line.trim() == needle_raw {
                match_lines.push((idx, None, None));
            }
        }
    }

    let Some((line_idx, orig_date, orig_src)) = match_lines.first().cloned() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "matching memory fact not found",
        ));
    };
    if match_lines.len() > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "old_fact matches multiple memory entries; provide the exact line",
        ));
    }

    let formatted =
        format_updated_fact_line(replacement_raw, orig_date.as_deref(), orig_src.as_deref());
    if let Some(slot) = lines.get_mut(line_idx) {
        *slot = formatted;
    }
    let mut rebuilt = lines.join("\n");
    if existing.ends_with('\n') || !rebuilt.is_empty() {
        rebuilt.push('\n');
    }
    Ok(rebuilt)
}

/// Validates and normalizes a memory topic stem (`memory/<topic>.md`).
pub(crate) fn validate_topic_name(raw: &str) -> io::Result<String> {
    let trimmed = raw.trim();
    let stem = trimmed.strip_suffix(".md").unwrap_or(trimmed).trim();
    if stem.is_empty()
        || stem.len() > 64
        || stem == "."
        || stem == ".."
        || stem.starts_with('.')
        || stem.starts_with('-')
        || stem.ends_with('-')
        || stem.eq_ignore_ascii_case("memory")
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid memory topic name",
        ));
    }
    Ok(stem.to_owned())
}

/// Lists all valid markdown topic files stored under `<bot_home>/memory/<topic>.md`.
pub(crate) fn list_topics(bot_home: &Path) -> io::Result<Vec<BotMemoryTopicInfo>> {
    ensure_no_symlink_path(bot_home)?;
    let mem_dir = bot_home.join("memory");
    ensure_no_symlink_path(&mem_dir)?;
    let Ok(entries) = fs::read_dir(&mem_dir) else {
        return Ok(Vec::new());
    };
    let mut topics = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.')
            || !Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let Some(stem) = Path::new(name).file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if validate_topic_name(stem).is_err() {
            continue;
        }
        topics.push(topic_info_from_meta(stem.to_owned(), &meta));
    }
    topics.sort_by(|a, b| a.topic.cmp(&b.topic));
    Ok(topics)
}

/// Reads `<bot_home>/memory/<topic>.md` with symlink protection.
pub(crate) fn get_topic(bot_home: &Path, topic: &str) -> io::Result<String> {
    let stem = validate_topic_name(topic)?;
    let mem_dir = ensure_memory_dir(bot_home, false)?;
    let path = mem_dir.join(format!("{stem}.md"));
    ensure_no_symlink_path(&path)?;
    fs::read_to_string(&path)
}

/// Writes `<bot_home>/memory/<topic>.md` atomically with `0o600` permissions and symlink protection.
pub(crate) fn set_topic(
    bot_home: &Path,
    topic: &str,
    content: &str,
) -> io::Result<BotMemoryTopicInfo> {
    if content.len() > MAX_TOPIC_CONTENT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "memory topic exceeds 512 KiB size limit",
        ));
    }
    let stem = validate_topic_name(topic)?;
    let mem_dir = ensure_memory_dir(bot_home, true)?;
    let path = mem_dir.join(format!("{stem}.md"));
    ensure_no_symlink_path(&path)?;
    write_atomic_0600(&path, content.as_bytes())?;
    let meta = fs::symlink_metadata(&path)?;
    Ok(topic_info_from_meta(stem, &meta))
}

fn ensure_memory_dir(bot_home: &Path, create: bool) -> io::Result<PathBuf> {
    ensure_no_symlink_path(bot_home)?;
    let mem_dir = bot_home.join("memory");
    ensure_no_symlink_path(&mem_dir)?;
    if create {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&mem_dir)?;
        ensure_no_symlink_path(&mem_dir)?;
    }
    Ok(mem_dir)
}

fn topic_info_from_meta(topic: String, meta: &fs::Metadata) -> BotMemoryTopicInfo {
    let updated_at_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    BotMemoryTopicInfo {
        topic,
        bytes: meta.len(),
        updated_at_ms,
    }
}

fn format_updated_fact_line(
    replacement: &str,
    orig_date: Option<&str>,
    orig_src: Option<&str>,
) -> String {
    if replacement.starts_with("- ") {
        return replacement.to_owned();
    }
    let mut out = String::from("- ");
    let parsed_new = parse_memory_facts(&format!("- {replacement}"));
    let has_new_date = parsed_new.first().and_then(|f| f.date.as_ref()).is_some();
    if !has_new_date && let Some(date) = orig_date {
        out.push_str(date);
        out.push(' ');
    }
    out.push_str(replacement);
    if !replacement.contains(BOT_MEMORY_SOURCE_SEPARATOR)
        && let Some(src) = orig_src
    {
        out.push_str(BOT_MEMORY_SOURCE_SEPARATOR);
        out.push_str(src);
    }
    out
}

fn normalize_fact_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn compute_fact_diff(before: &str, after: &str) -> (Vec<String>, Vec<String>) {
    let before_lines = extract_diff_units(before);
    let after_lines = extract_diff_units(after);

    let mut before_counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in &before_lines {
        *before_counts.entry(line.clone()).or_insert(0) += 1;
    }
    let mut after_counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in &after_lines {
        *after_counts.entry(line.clone()).or_insert(0) += 1;
    }

    let mut added = Vec::new();
    let mut seen_added: BTreeMap<String, usize> = BTreeMap::new();
    for line in &after_lines {
        let count_after = *after_counts.get(line).unwrap_or(&0);
        let count_before = *before_counts.get(line).unwrap_or(&0);
        let emitted = seen_added.get(line).copied().unwrap_or(0);
        if count_after > count_before + emitted {
            added.push(line.clone());
            seen_added.insert(line.clone(), emitted + 1);
        }
    }

    let mut removed = Vec::new();
    let mut seen_removed: BTreeMap<String, usize> = BTreeMap::new();
    for line in &before_lines {
        let count_before = *before_counts.get(line).unwrap_or(&0);
        let count_after = *after_counts.get(line).unwrap_or(&0);
        let emitted = seen_removed.get(line).copied().unwrap_or(0);
        if count_before > count_after + emitted {
            removed.push(line.clone());
            seen_removed.insert(line.clone(), emitted + 1);
        }
    }

    (added, removed)
}

fn extract_diff_units(content: &str) -> Vec<String> {
    let facts: Vec<String> = parse_memory_facts(content)
        .into_iter()
        .map(|f| normalize_fact_text(&f.text))
        .filter(|s| !s.is_empty())
        .collect();
    if !facts.is_empty() {
        return facts;
    }
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect()
}

fn read_entries_locked(path: &Path) -> io::Result<Vec<BotMemoryHistoryEntry>> {
    let raw = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<BotMemoryHistoryEntry>(trimmed) {
            out.push(entry);
        }
    }
    Ok(out)
}

fn write_entries_locked(path: &Path, entries: &mut Vec<BotMemoryHistoryEntry>) -> io::Result<()> {
    if entries.len() > MAX_HISTORY_ENTRIES {
        let drop_n = entries.len().saturating_sub(MAX_HISTORY_ENTRIES);
        entries.drain(..drop_n);
    }
    loop {
        let mut buf = String::new();
        for entry in entries.iter() {
            if let Ok(line) = serde_json::to_string(entry) {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        if buf.len() <= MAX_HISTORY_BYTES || entries.len() <= 1 {
            return write_atomic_0600(path, buf.as_bytes());
        }
        entries.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(label: &str) -> PathBuf {
        let seq = ENTRY_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-mem-hist-{label}-{}-{seq}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn records_lists_and_marks_undone_and_topics() {
        let home = temp_home("basic");
        let bot = BotId::new("hist-bot").unwrap();

        let e1 = record_change(
            &home,
            &bot,
            "owner",
            "update",
            "# Memory\n\n- 2026-11-10 Fact A\n",
            "# Memory\n\n- 2026-11-10 Fact A\n- 2026-11-11 Fact B\n",
            1_000,
            true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(e1.added, vec!["Fact B".to_owned()]);
        assert!(e1.removed.is_empty());

        let e2 = record_change(
            &home,
            &bot,
            "system:tidy",
            "tidy",
            &e1.after_content,
            "# Memory\n\n- 2026-11-11 Fact B\n",
            2_000,
            true,
        )
        .unwrap()
        .unwrap();
        assert!(e2.entry_id.starts_with("tidy-"));
        assert_eq!(
            latest_tidy_entry(&home).unwrap().unwrap().entry_id,
            e2.entry_id
        );

        let history = list_history(&home, Some(10)).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history.first().unwrap().entry_id, e2.entry_id);

        assert!(mark_undone(&home, &e2.entry_id).unwrap());
        assert!(find_entry(&home, &e2.entry_id).unwrap().unwrap().undone);

        // Topic CRUD + path validation.
        assert!(validate_topic_name("../etc/passwd").is_err());
        assert!(validate_topic_name("MEMORY").is_err());
        let info = set_topic(&home, "repos", "# Repos\n\n- krill\n").unwrap();
        assert_eq!(info.topic, "repos");
        assert_eq!(get_topic(&home, "repos").unwrap(), "# Repos\n\n- krill\n");
        let listed = list_topics(&home).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed.first().unwrap().topic, "repos");

        #[cfg(unix)]
        {
            let outside = home.join("outside.md");
            fs::write(&outside, b"secret").unwrap();
            std::os::unix::fs::symlink(&outside, home.join("memory/evil.md")).unwrap();
            assert!(get_topic(&home, "evil").is_err());
            assert!(
                list_topics(&home)
                    .unwrap()
                    .iter()
                    .all(|t| t.topic != "evil")
            );
        }

        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn rewrite_single_fact_updates_target_and_preserves_date_and_source() {
        let initial = "# Memory\n\n- 2026-11-10 Primary repo is old/repo · from: Slack\n- 2026-11-12 Another fact\n";
        let updated = rewrite_single_fact(
            initial,
            "Primary repo is old/repo",
            "Primary repo is new/repo",
        )
        .unwrap();
        assert!(updated.contains("- 2026-11-10 Primary repo is new/repo · from: Slack"));
        assert!(updated.contains("- 2026-11-12 Another fact"));
    }
}
