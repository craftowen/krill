//! Zero-State 증분 트랜스크립트 리더(OWE-945 / MK-242).
//!
//! 상주 DB나 워커 스레드 없이 요청 시 `offset` 바이트부터 트랜스크립트 파일을 읽어
//! 4대 에이전트(`claude`·`codex`·`agy`·`opencode`)의 대화를 단일 `ChatItem` 스키마
//! (`User`·`Assistant`·`Reasoning`·`ToolCall`·`ToolResult`·`Question`·`Permission`)로 정규화한다.

use crate::session::lock;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use krill_proto::agent_history::{
    ChatItem, ChatItemKind, ChatPollSnapshot, ChatQuestionOption, MAX_CHAT_ITEMS, MAX_CHAT_TEXT,
    MAX_QUESTION_OPTIONS, MAX_TEXT, VaultAgent,
};
use serde_json::Value;

use crate::agent::control::StoredLaunch;

use super::vault::{
    VaultRoots, agy_candidate_dirs, collect_roots, extract_agy_user_request,
    extract_agy_workspace_from_record, is_injected_user_context, normalize_path,
    opencode_candidate_bases,
};

/// 폴링 1회당 읽는 최대 바이트(512 KiB).
const MAX_POLL_BYTES: u64 = 512 * 1024;
const MAX_SKIP_BYTES: u64 = 64 * 1024 * 1024;
// End cursors carry a fingerprint; historical boundaries are item indexes. Both fit JS numbers.
const OPENCODE_END: u64 = 1 << 52;

/// Global per-resolution work budget shared by recursive directory walks.
struct ScanBudget {
    remaining: usize,
    deadline: Instant,
}
impl ScanBudget {
    fn new() -> Self {
        Self {
            remaining: 2048,
            deadline: Instant::now() + Duration::from_millis(100),
        }
    }
    fn take(&mut self) -> bool {
        if self.remaining == 0 || Instant::now() >= self.deadline {
            return false;
        }
        self.remaining -= 1;
        true
    }
}

fn read_json_bounded(path: &Path, remaining: &mut u64) -> std::io::Result<Value> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    if len > *remaining {
        *remaining = 0;
        return Err(std::io::Error::other("chat JSON byte budget exceeded"));
    }
    let mut bytes = Vec::new();
    file.take(len.saturating_add(1).min(remaining.saturating_add(1)))
        .read_to_end(&mut bytes)?;
    let read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    *remaining = remaining.saturating_sub(read);
    if read > len {
        return Err(std::io::Error::other("chat JSON changed while reading"));
    }
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

#[cfg(all(test, unix))]
pub(crate) fn poll_session_in(
    roots: &VaultRoots,
    stored: Option<&StoredLaunch>,
    fallback_cwd: &[u8],
    offset: u64,
) -> ChatPollSnapshot {
    let empty_claimed = (BTreeSet::new(), BTreeSet::new());
    poll_session_in_bounded(roots, stored, fallback_cwd, offset, None, &empty_claimed).0
}

/// 세션의 `StoredLaunch`, 시작 시각 하한(`min_birth_secs`), 다른 세션이 이미 점유한 트랜스크립트(`claimed`)를
/// 바탕으로 트랜스크립트를 증분 폴링하고, 해석된 `(path, session_id)`도 함께 반환한다.
pub(crate) fn poll_session_bounded(
    state_dir: Option<&Path>,
    stored: Option<&StoredLaunch>,
    fallback_cwd: &[u8],
    offset: u64,
    min_birth_secs: Option<u64>,
    claimed: &(BTreeSet<String>, BTreeSet<PathBuf>),
) -> (ChatPollSnapshot, Option<(PathBuf, String)>) {
    let roots = collect_roots(state_dir);
    poll_session_in_bounded(
        &roots,
        stored,
        fallback_cwd,
        offset,
        min_birth_secs,
        claimed,
    )
}

pub(crate) fn poll_session_in_bounded(
    roots: &VaultRoots,
    stored: Option<&StoredLaunch>,
    fallback_cwd: &[u8],
    offset: u64,
    min_birth_secs: Option<u64>,
    claimed: &(BTreeSet<String>, BTreeSet<PathBuf>),
) -> (ChatPollSnapshot, Option<(PathBuf, String)>) {
    let Some(stored) = stored else {
        return (ChatPollSnapshot::default(), None);
    };
    let Some(agent) = agent_for_session(stored) else {
        return (
            ChatPollSnapshot {
                agent: bounded(&stored.profile, MAX_TEXT),
                reset: offset > 0,
                ..ChatPollSnapshot::default()
            },
            None,
        );
    };
    let cwd_bytes = if stored.cwd.is_empty() {
        fallback_cwd
    } else {
        stored.cwd.as_slice()
    };
    let cwd_str = String::from_utf8_lossy(cwd_bytes);
    let filter = CwdFallbackFilter {
        min_birth_secs,
        claimed_ids: &claimed.0,
        claimed_paths: &claimed.1,
    };
    let resolved = resolve_transcript(roots, agent, stored, &cwd_str, &filter);
    let Some((path, session_id)) = resolved else {
        return (
            ChatPollSnapshot {
                agent: bounded(agent.profile(), MAX_TEXT),
                session_id: bounded(stored.session_id.as_deref().unwrap_or(""), MAX_TEXT),
                transcript_path: bounded(stored.transcript_path.as_deref().unwrap_or(""), MAX_TEXT),
                next_offset: 0,
                reset: offset > 0,
                bound: false,
                items: Vec::new(),
            },
            None,
        );
    };
    let snapshot = poll_transcript_file(agent, &session_id, &path, offset);
    (snapshot, Some((path, session_id)))
}

fn agent_for_session(stored: &StoredLaunch) -> Option<VaultAgent> {
    VaultAgent::parse(&stored.profile)
}

/// 트랜스크립트 파일 하나를 `offset` 바이트부터 증분 정규화해 읽는다.
pub(crate) fn poll_transcript_file(
    agent: VaultAgent,
    session_id: &str,
    path: &Path,
    offset: u64,
) -> ChatPollSnapshot {
    poll_transcript_file_until(agent, session_id, path, offset, None)
}

/// Replays an observed mobile page without reading records appended beyond its end.
/// The registry has already resolved the transcript; client offsets cannot select a path.
pub(crate) fn replay_page(
    snapshot: ChatPollSnapshot,
    offset: u64,
    end_offset: u64,
) -> std::io::Result<ChatPollSnapshot> {
    if end_offset <= offset {
        return Err(std::io::Error::other("invalid chat page boundary"));
    }
    if snapshot.reset || !snapshot.bound {
        return Ok(snapshot);
    }
    let agent = VaultAgent::parse(&snapshot.agent)
        .ok_or_else(|| std::io::Error::other("unknown chat transcript agent"))?;
    let path = Path::new(&snapshot.transcript_path);
    if agent == VaultAgent::OpenCode && path.extension().is_some_and(|e| e == "json" || e == "db") {
        return Ok(poll_opencode_session(
            &snapshot.agent,
            &snapshot.session_id,
            path,
            &snapshot.transcript_path,
            offset,
            Some(end_offset),
        ));
    }
    Ok(poll_transcript_file_until(
        agent,
        &snapshot.session_id,
        path,
        offset,
        Some(end_offset),
    ))
}

/// A bounded tail window for mobile's first paint, or the page before its oldest row.
/// The resolved snapshot supplies the path; the client only supplies a byte boundary.
pub(crate) fn latest_page(
    snapshot: ChatPollSnapshot,
    before: Option<u64>,
) -> std::io::Result<(ChatPollSnapshot, u64)> {
    if !snapshot.bound {
        return Ok((snapshot, 0));
    }
    let agent = VaultAgent::parse(&snapshot.agent)
        .ok_or_else(|| std::io::Error::other("unknown chat transcript agent"))?;
    let path = Path::new(&snapshot.transcript_path);
    if agent == VaultAgent::OpenCode && path.extension().is_some_and(|e| e == "db") {
        return latest_opencode_database(&snapshot, before);
    }
    if agent == VaultAgent::OpenCode && path.extension().is_some_and(|e| e == "json" || e == "db") {
        let mut page = load_opencode_session(
            &snapshot.agent,
            &snapshot.session_id,
            path,
            &snapshot.transcript_path,
        );
        if !page.bound {
            return Ok((page, 0));
        }
        let end = before.map_or(page.items.len(), |n| {
            usize::try_from(n)
                .unwrap_or(usize::MAX)
                .min(page.items.len())
        });
        let start = end.saturating_sub(MAX_CHAT_ITEMS);
        page.items = page.items.get(start..end).unwrap_or_default().to_vec();
        if before.is_some() {
            page.next_offset = u64::try_from(end).unwrap_or(0);
        }
        return Ok((page, u64::try_from(start).unwrap_or(0)));
    }
    let mut file = File::open(path)?;
    let end = before.unwrap_or(u64::MAX).min(file.metadata()?.len());
    let scan_start = end.saturating_sub(MAX_POLL_BYTES);
    file.seek(SeekFrom::Start(scan_start))?;
    let mut bytes = Vec::new();
    (&mut file).take(end - scan_start).read_to_end(&mut bytes)?;
    // Drop a leading fragment and a trailing unflushed record. Never parse half a JSONL row.
    let leading = if scan_start > 0 {
        bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1)
    } else {
        0
    };
    let complete = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let mut start = scan_start + u64::try_from(complete.max(leading)).unwrap_or(0);
    // A giant final record may occupy the entire window. Find its beginning in
    // bounded chunks so the next historical request can pass it.
    if start == end && leading == complete && scan_start > 0 {
        let floor = end.saturating_sub(MAX_SKIP_BYTES);
        let mut cursor = scan_start;
        while cursor > floor {
            let chunk_start = cursor.saturating_sub(MAX_POLL_BYTES).max(floor);
            file.seek(SeekFrom::Start(chunk_start))?;
            let mut chunk = Vec::new();
            (&mut file)
                .take(cursor - chunk_start)
                .read_to_end(&mut chunk)?;
            if let Some(index) = chunk.iter().rposition(|b| *b == b'\n') {
                start = chunk_start + u64::try_from(index + 1).unwrap_or(0);
                break;
            }
            cursor = chunk_start;
            if cursor == 0 {
                start = 0;
            }
        }
    }
    let mut count = 0;
    for line in bytes
        .get(leading..complete)
        .unwrap_or_default()
        .split_inclusive(|b| *b == b'\n')
        .rev()
    {
        let mut items = Vec::new();
        if let Ok(value) = serde_json::from_slice::<Value>(line) {
            parse_line_into_items(agent, &value, &mut items);
        }
        if count > 0 && count + items.len() > MAX_CHAT_ITEMS {
            break;
        }
        count += items.len();
        start = start.saturating_sub(u64::try_from(line.len()).unwrap_or(0));
        if count >= MAX_CHAT_ITEMS {
            break;
        }
    }
    let page = poll_transcript_file_until(agent, &snapshot.session_id, path, start, Some(end));
    Ok((page, start))
}

fn poll_transcript_file_until(
    agent: VaultAgent,
    session_id: &str,
    path: &Path,
    offset: u64,
    end_offset: Option<u64>,
) -> ChatPollSnapshot {
    let path_str = bounded(&path.to_string_lossy(), MAX_TEXT);
    let agent_str = bounded(agent.profile(), MAX_TEXT);
    let sid_str = bounded(session_id, MAX_TEXT);

    if agent == VaultAgent::OpenCode && path.extension().is_some_and(|e| e == "json" || e == "db") {
        return poll_opencode_session(&agent_str, &sid_str, path, &path_str, offset, end_offset);
    }

    let Ok(mut file) = File::open(path) else {
        return ChatPollSnapshot {
            agent: agent_str,
            session_id: sid_str,
            transcript_path: path_str,
            next_offset: 0,
            reset: offset > 0,
            bound: false,
            items: Vec::new(),
        };
    };
    let file_len = file.metadata().map_or(0, |m| m.len());
    let (start_offset, reset) = if offset > file_len {
        (0, true)
    } else {
        (offset, false)
    };
    if file.seek(SeekFrom::Start(start_offset)).is_err() {
        return ChatPollSnapshot {
            agent: agent_str,
            session_id: sid_str,
            transcript_path: path_str,
            next_offset: start_offset,
            reset,
            bound: true,
            items: Vec::new(),
        };
    }

    // Limit the underlying reader itself so even read-ahead and oversized-line scanning
    // cannot cross a historical page's observed end. Ordinary polling keeps its byte cap.
    let readable = end_offset.map_or(u64::MAX, |end| end.saturating_sub(start_offset));
    let mut reader = BufReader::with_capacity(8192, file.take(readable));
    let mut line = Vec::new();
    let mut consumed = start_offset;
    let mut batch_bytes = 0_u64;
    let mut items: Vec<ChatItem> = Vec::new();
    let mut previous_codex_record = None;
    let max_line = usize::try_from(MAX_POLL_BYTES).unwrap_or(512 * 1024);

    while batch_bytes < MAX_POLL_BYTES {
        line.clear();
        let n = match (&mut reader)
            .take(MAX_POLL_BYTES)
            .read_until(b'\n', &mut line)
        {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let ends_with_newline = line.last() == Some(&b'\n');
        if !ends_with_newline {
            if n < max_line {
                // 에이전트가 아직 줄바꿈 전까지 flush 중인 부분 줄이면 오프셋을 전진하지 않는다.
                break;
            }
            // 단일 JSONL 줄이 512 KiB(MAX_POLL_BYTES)를 초과한 경우:
            // 8 KiB 고정 버퍼로 줄 끝(`\n`)까지 스캔해 오프셋 고착(무한 루프)을 방지한다.
            let mut skipped = u64::try_from(n).unwrap_or(0);
            let mut found_newline = false;
            while skipped < MAX_SKIP_BYTES {
                let buf = match reader.fill_buf() {
                    Ok([]) | Err(_) => break,
                    Ok(b) => b,
                };
                if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let step = pos + 1;
                    reader.consume(step);
                    skipped = skipped.saturating_add(u64::try_from(step).unwrap_or(0));
                    found_newline = true;
                    break;
                }
                let len = buf.len();
                reader.consume(len);
                skipped = skipped.saturating_add(u64::try_from(len).unwrap_or(0));
            }
            if !found_newline {
                // 거대 줄조차 아직 `\n`이 디스크에 기록되지 않았으면 다음 폴링에서 다시 읽는다.
                break;
            }
            consumed = consumed.saturating_add(skipped);
            batch_bytes = batch_bytes.saturating_add(64 * 1024);
            continue;
        }
        let n_u64 = u64::try_from(n).unwrap_or(0);
        batch_bytes = batch_bytes.saturating_add(n_u64);
        let trimmed = trim_ascii_whitespace(&line);
        if trimmed.is_empty() {
            consumed = consumed.saturating_add(n_u64);
            continue;
        }
        let Ok(val) = serde_json::from_slice::<Value>(trimmed) else {
            consumed = consumed.saturating_add(n_u64);
            continue;
        };
        let codex_record = if agent == VaultAgent::Codex {
            match val.get("type").and_then(Value::as_str) {
                Some("response_item") => Some(false),
                Some("event_msg") => Some(true),
                _ => None,
            }
        } else {
            None
        };
        let mut line_items = Vec::new();
        parse_line_into_items(agent, &val, &mut line_items);
        // Only Codex's two representations of the same adjacent event may merge.
        // Separate records with the same text must remain separate messages.
        if previous_codex_record
            .zip(codex_record)
            .is_some_and(|(a, b)| a != b)
            && let Some(previous) = items.last()
            && let Some(current) = line_items.first()
            && matches!(
                current.kind,
                ChatItemKind::User | ChatItemKind::Assistant | ChatItemKind::Reasoning
            )
            && previous.kind == current.kind
            && previous.text == current.text
        {
            line_items.remove(0);
            previous_codex_record = None;
        } else {
            previous_codex_record = codex_record;
        }
        if !line_items.is_empty()
            && !items.is_empty()
            && items.len().saturating_add(line_items.len()) > MAX_CHAT_ITEMS
        {
            break;
        }
        consumed = consumed.saturating_add(n_u64);
        items.extend(line_items);
        if items.len() > MAX_CHAT_ITEMS {
            break;
        }
    }

    ChatPollSnapshot {
        agent: agent_str,
        session_id: sid_str,
        transcript_path: path_str,
        next_offset: consumed,
        reset,
        bound: true,
        items,
    }
}

fn opencode_storage_root(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    let grandparent = parent.parent()?;
    if grandparent
        .file_name()
        .is_some_and(|name| name == "session")
    {
        grandparent.parent()
    } else {
        Some(grandparent)
    }
}

fn sorted_json_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json") && path.is_file())
        .collect();
    paths.sort();
    paths
}

fn poll_opencode_session(
    agent: &str,
    sid: &str,
    path: &Path,
    path_str: &str,
    offset: u64,
    limit: Option<u64>,
) -> ChatPollSnapshot {
    if path.extension().is_some_and(|e| e == "db") {
        return poll_opencode_database(agent, sid, path, path_str, offset, limit);
    }
    let mut page = load_opencode_session(agent, sid, path, path_str);
    if !page.bound {
        page.reset = offset > 0;
        return page;
    }
    let end_cursor = page.next_offset;
    let total = page.items.len();
    if offset == end_cursor && limit.is_none() {
        page.items.clear();
        return page;
    }
    let stale = offset >= OPENCODE_END;
    let requested = usize::try_from(offset).unwrap_or(usize::MAX);
    let reset = stale || requested > total;
    let start = if reset { 0 } else { requested };
    let end = limit
        .filter(|n| *n < OPENCODE_END)
        .map_or(total, |n| {
            usize::try_from(n).unwrap_or(usize::MAX).min(total)
        })
        .min(start.saturating_add(MAX_CHAT_ITEMS));
    page.items = page.items.get(start..end).unwrap_or_default().to_vec();
    page.next_offset = if end < total || limit.is_some_and(|n| n < OPENCODE_END) {
        u64::try_from(end).unwrap_or(0)
    } else {
        end_cursor
    };
    page.reset = reset;
    page
}

// Database cursors address message rows, so bounded pages can traverse conversations
// larger than an export budget. End fingerprints notice rewrites; no wire fields change.
const DB_ROW_MASK: u64 = (1 << 32) - 1;

fn database_end(path: &Path, total: u64) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    crate::agent::control::cache::stamp(path).hash(&mut hash);
    crate::agent::control::cache::stamp(&path.with_extension("db-wal")).hash(&mut hash);
    OPENCODE_END | ((hash.finish() & ((1 << 20) - 1)) << 32) | (total & DB_ROW_MASK)
}

fn poll_opencode_database(
    agent: &str,
    sid: &str,
    path: &Path,
    path_str: &str,
    offset: u64,
    limit: Option<u64>,
) -> ChatPollSnapshot {
    let mut page = ChatPollSnapshot {
        agent: agent.into(),
        session_id: sid.into(),
        transcript_path: path_str.into(),
        ..Default::default()
    };
    let was_end = offset >= OPENCODE_END;
    let mut start = if was_end {
        offset & DB_ROW_MASK
    } else {
        offset
    };
    let Ok(mut batch) = super::opencode_sqlite::read_page(path, sid, start) else {
        page.reset = offset > 0;
        return page;
    };
    if batch.truncated || batch.skipped {
        page.reset = offset > 0;
        return page;
    }
    let Some(session) = batch.sessions.first() else {
        page.reset = offset > 0;
        return page;
    };
    let total = session.message_count;
    if total > DB_ROW_MASK {
        page.reset = offset > 0;
        return page;
    }
    let end_cursor = database_end(path, total);
    // Appends continue after the last message row. Same-count edits and truncation reset.
    let reset = start > total
        || (was_end && offset != end_cursor && start == total)
        || limit.is_some_and(|end| end >= OPENCODE_END && end != end_cursor);
    if reset {
        start = 0;
        let Ok(fresh) = super::opencode_sqlite::read_page(path, sid, 0) else {
            page.reset = true;
            return page;
        };
        batch = fresh;
    }
    if batch.truncated || batch.skipped {
        page.reset = offset > 0;
        return page;
    }
    let Some(session) = batch.sessions.into_iter().next() else {
        page.reset = offset > 0;
        return page;
    };
    let mut consumed = start;
    let end = limit
        .filter(|n| *n < OPENCODE_END)
        .unwrap_or(total)
        .min(total);
    let mut remaining = MAX_SKIP_BYTES;
    for record in session.messages {
        if consumed >= end || page.items.len() >= MAX_CHAT_ITEMS {
            break;
        }
        parse_opencode_message_record(&record, None, &mut page.items, &mut remaining);
        consumed = consumed.saturating_add(1);
    }
    page.next_offset = if consumed >= total && limit.is_none_or(|n| n >= OPENCODE_END) {
        database_end(path, total)
    } else {
        consumed
    };
    page.bound = true;
    page.reset = reset;
    page
}

fn latest_opencode_database(
    snapshot: &ChatPollSnapshot,
    before: Option<u64>,
) -> std::io::Result<(ChatPollSnapshot, u64)> {
    let path = Path::new(&snapshot.transcript_path);
    let meta = super::opencode_sqlite::read_page(path, &snapshot.session_id, 0)
        .map_err(std::io::Error::other)?;
    if meta.truncated || meta.skipped {
        return Err(std::io::Error::other("incomplete OpenCode database page"));
    }
    let total = meta
        .sessions
        .first()
        .ok_or_else(|| std::io::Error::other("missing OpenCode session"))?
        .message_count;
    let end = before.unwrap_or(total).min(total);
    let start = end.saturating_sub(u64::try_from(MAX_CHAT_ITEMS).unwrap_or(128));
    let batch = super::opencode_sqlite::read_page(path, &snapshot.session_id, start)
        .map_err(std::io::Error::other)?;
    if batch.truncated || batch.skipped {
        return Err(std::io::Error::other("incomplete OpenCode database page"));
    }
    let session = batch
        .sessions
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("missing OpenCode session"))?;
    let take = usize::try_from(end.saturating_sub(start)).unwrap_or(0);
    let mut records = Vec::new();
    let mut count = 0usize;
    let mut remaining = MAX_SKIP_BYTES;
    for record in session
        .messages
        .into_iter()
        .take(take)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let mut items = Vec::new();
        parse_opencode_message_record(&record, None, &mut items, &mut remaining);
        count = count.saturating_add(items.len());
        records.push(items);
        if count >= MAX_CHAT_ITEMS {
            break;
        }
    }
    let start = end.saturating_sub(u64::try_from(records.len()).unwrap_or(0));
    let mut page = snapshot.clone();
    page.items = records.into_iter().rev().flatten().collect();
    page.next_offset = if before.is_some() {
        end
    } else {
        database_end(path, total)
    };
    page.reset = false;
    page.bound = true;
    Ok((page, start))
}

fn load_opencode_session(agent: &str, sid: &str, path: &Path, path_str: &str) -> ChatPollSnapshot {
    let mut page = ChatPollSnapshot {
        agent: agent.to_owned(),
        session_id: sid.to_owned(),
        transcript_path: path_str.to_owned(),
        ..ChatPollSnapshot::default()
    };
    let mut remaining = MAX_POLL_BYTES;
    let Ok(embedded) = read_json_bounded(path, &mut remaining) else {
        return page;
    };
    let storage_root = opencode_storage_root(path);
    if page.session_id.is_empty() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .clone_into(&mut page.session_id);
    }
    let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
    crate::agent::control::cache::stamp(path).hash(&mut fingerprint);
    // The session document is bounded individually; external messages share a 64 MiB budget.
    remaining = MAX_SKIP_BYTES;
    parse_opencode_value(&embedded, storage_root, &mut page.items, &mut remaining);
    if let Some(root) = storage_root {
        for message in sorted_json_files(&root.join("message").join(&page.session_id)) {
            message.hash(&mut fingerprint);
            crate::agent::control::cache::stamp(&message).hash(&mut fingerprint);
            if let Some(id) = message.file_stem() {
                for part in sorted_json_files(&root.join("part").join(id)) {
                    part.hash(&mut fingerprint);
                    crate::agent::control::cache::stamp(&part).hash(&mut fingerprint);
                }
            }
            let Ok(value) = read_json_bounded(&message, &mut remaining) else {
                return page;
            };
            parse_opencode_message_record(&value, storage_root, &mut page.items, &mut remaining);
        }
    }
    if remaining == 0 {
        page.items.clear();
        return page;
    }
    page.next_offset = OPENCODE_END | (fingerprint.finish() & (OPENCODE_END - 1));
    page.bound = true;
    page
}

/// Appends do not change identity; replacement/rebinding changes the generation.
pub(crate) fn generation(snapshot: &ChatPollSnapshot) -> String {
    let created = fs::metadata(&snapshot.transcript_path)
        .and_then(|meta| meta.created())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    format!(
        "{}:{}:{created}",
        snapshot.session_id, snapshot.transcript_path
    )
}

struct CwdFallbackFilter<'a> {
    min_birth_secs: Option<u64>,
    claimed_ids: &'a BTreeSet<String>,
    claimed_paths: &'a BTreeSet<PathBuf>,
}

impl CwdFallbackFilter<'_> {
    fn allows(&self, path: &Path, sid: &str) -> bool {
        if !sid.is_empty() && self.claimed_ids.contains(sid) {
            return false;
        }
        if self.claimed_paths.contains(path) {
            return false;
        }
        if let Some(min_birth) = self.min_birth_secs
            && let Some(birth) = file_birth_or_mtime(path)
            && birth < min_birth.saturating_sub(2)
        {
            return false;
        }
        true
    }

    /// Codex files each rollout under its local creation date. A `YYYY[-MM[-DD]]` period that
    /// ended two days before the session started (one for any time zone, one of margin)
    /// cannot hold its transcript. Only folder names decide; file names and times never prune.
    fn predates_codex_period(&self, period: &str) -> bool {
        let Some(min_birth) = self.min_birth_secs else {
            return false;
        };
        let mut parts = period.split('-').map(str::parse::<i64>);
        let end = match (parts.next(), parts.next(), parts.next()) {
            (Some(Ok(y)), None | Some(Ok(12)), None) => format!("{:04}-01-01T00:00:00Z", y + 1),
            (Some(Ok(y)), Some(Ok(m)), None) => format!("{y:04}-{:02}-01T00:00:00Z", m + 1),
            (Some(Ok(y)), Some(Ok(m)), Some(Ok(d))) => format!("{y:04}-{m:02}-{d:02}T23:59:59Z"),
            _ => return false,
        };
        krill_proto::time::parse_rfc3339_epoch(&end)
            .and_then(|end| u64::try_from(end).ok())
            .is_some_and(|end| end.saturating_add(2 * 86_400) < min_birth)
    }
}

fn resolve_transcript(
    roots: &VaultRoots,
    agent: VaultAgent,
    stored: &StoredLaunch,
    cwd: &str,
    filter: &CwdFallbackFilter<'_>,
) -> Option<(PathBuf, String)> {
    type PathCache = BTreeMap<String, (Instant, Option<(PathBuf, String)>)>;
    static CACHE: OnceLock<Mutex<PathCache>> = OnceLock::new();
    let key = format!(
        "{agent:?}|{roots:?}|{stored:?}|{cwd}|{:?}|{:?}|{:?}",
        filter.min_birth_secs, filter.claimed_ids, filter.claimed_paths
    );
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some((at, result)) = lock(cache).get(&key) {
        let ttl = if result.is_some() {
            Duration::from_secs(3)
        } else {
            Duration::from_millis(500)
        };
        if at.elapsed() < ttl && result.as_ref().is_none_or(|(path, _)| path.is_file()) {
            return result.clone();
        }
    }
    let result =
        resolve_transcript_uncached(roots, agent, stored, cwd, filter, &mut ScanBudget::new());
    let mut cached = lock(cache);
    cached.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(3));
    if cached.len() >= 256 {
        cached.clear();
    }
    cached.insert(key, (Instant::now(), result.clone()));
    result
}

fn resolve_transcript_uncached(
    roots: &VaultRoots,
    agent: VaultAgent,
    stored: &StoredLaunch,
    cwd: &str,
    filter: &CwdFallbackFilter<'_>,
    budget: &mut ScanBudget,
) -> Option<(PathBuf, String)> {
    let mut sid = stored.session_id.as_deref().unwrap_or("").trim().to_owned();
    let mut agy_bound_root: Option<PathBuf> = None;
    if let Some(tp) = stored.transcript_path.as_deref()
        && crate::agent::control::valid_transcript_path(tp)
    {
        let p = Path::new(tp);
        if crate::agent::control::is_agy_conversation_db_path(tp) {
            if let Some((root, db_sid)) = super::vault::agy_root_and_id_from_path(p) {
                if sid.is_empty() {
                    sid = db_sid;
                }
                agy_bound_root = Some(root);
            }
        } else if p.is_file() {
            let inferred_sid = if sid.is_empty() {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_owned()
            } else {
                sid
            };
            return Some((p.to_path_buf(), inferred_sid));
        }
    }

    let mut search_dirs: Vec<PathBuf> = Vec::new();
    if let Some(root) = agy_bound_root {
        search_dirs.push(root);
    }
    if let Some(acc) = &stored.account
        && !acc.dir.is_empty()
    {
        let p = PathBuf::from(&acc.dir);
        if p.is_absolute() {
            search_dirs.push(p);
        }
    }
    let default_roots = match agent {
        VaultAgent::Claude => &roots.claude,
        VaultAgent::Codex => &roots.codex,
        VaultAgent::Agy => &roots.agy,
        VaultAgent::OpenCode => &roots.opencode,
    };
    for r in default_roots {
        if !search_dirs.contains(r) {
            search_dirs.push(r.clone());
        }
    }

    let norm_cwd = normalize_path(cwd);
    // Claude keeps one folder per cwd. Look there first so a large `projects/` never hides the session.
    let claude_dirs: Vec<PathBuf> = if agent == VaultAgent::Claude {
        search_dirs
            .iter()
            .filter_map(|root| claude_project_dir(root, cwd))
            .collect()
    } else {
        Vec::new()
    };
    if !sid.is_empty() {
        for dir in &claude_dirs {
            let cand = dir.join(format!("{sid}.jsonl"));
            if cand.is_file() {
                return Some((cand, sid));
            }
        }
    }

    if !sid.is_empty() {
        for root in &search_dirs {
            if let Some(found) = find_transcript_by_id(root, agent, &sid, budget) {
                return Some((found, sid));
            }
        }
    }

    // 이미 session_id가 알려진 세션은 첫 프롬프트 전까지 파일이 없더라도 같은 폴더의 다른 대화를 묶지 않는다.
    if !sid.is_empty() {
        return None;
    }
    if !norm_cwd.is_empty() {
        if let Some((_, path, found_sid)) = claude_dirs
            .iter()
            .filter_map(|dir| newest_claude_in_dir(dir, &norm_cwd, filter))
            .max_by_key(|found| found.0)
        {
            return Some((path, found_sid));
        }
        let mut best: Option<(u64, PathBuf, String)> = None;
        for root in &search_dirs {
            if let Some(cand) =
                find_newest_transcript_by_cwd(root, agent, &norm_cwd, filter, budget)
                && best.as_ref().is_none_or(|b| cand.0 > b.0)
            {
                best = Some(cand);
            }
        }
        if budget.remaining == 0 || Instant::now() >= budget.deadline {
            return None;
        }
        if let Some((_, path, found_sid)) = best {
            return Some((path, found_sid));
        }
    }
    None
}

/// Claude Code's project folder: the cwd with every UTF-16 unit outside `[A-Za-z0-9]` replaced by `-`.
/// Very long paths get a hashed name instead, so a miss falls back to the bounded scan.
fn claude_project_dir(root: &Path, cwd: &str) -> Option<PathBuf> {
    let cwd = cwd.trim().trim_end_matches(['/', '\\']);
    if cwd.is_empty() {
        return None;
    }
    let name: String = cwd
        .encode_utf16()
        .map(|unit| match u8::try_from(unit) {
            Ok(b) if b.is_ascii_alphanumeric() => char::from(b),
            _ => '-',
        })
        .collect();
    let dir = root.join("projects").join(name);
    dir.is_dir().then_some(dir)
}

/// Newest transcript in one Claude project folder whose recorded cwd matches. Only metadata is read for
/// every file; the first lines are peeked newest-first, so one folder with thousands of sessions stays cheap.
fn newest_claude_in_dir(
    dir: &Path,
    norm_cwd: &str,
    filter: &CwdFallbackFilter<'_>,
) -> Option<(u64, PathBuf, String)> {
    let mut files: Vec<(u64, PathBuf)> = fs::read_dir(dir)
        .ok()?
        .take(65_536)
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter_map(|p| Some((file_mtime(&p)?, p)))
        .collect();
    files.sort_unstable_by_key(|f| std::cmp::Reverse(f.0));
    files.into_iter().take(64).find_map(|(mtime, p)| {
        let (sid, file_cwd) = peek_jsonl_id_and_cwd(&p)?;
        (normalize_path(&file_cwd) == norm_cwd && filter.allows(&p, &sid))
            .then_some((mtime, p, sid))
    })
}

fn find_transcript_by_id(
    root: &Path,
    agent: VaultAgent,
    sid: &str,
    budget: &mut ScanBudget,
) -> Option<PathBuf> {
    match agent {
        VaultAgent::Claude => {
            let projects = root.join("projects");
            let file_name = format!("{sid}.jsonl");
            let entries = fs::read_dir(projects).ok()?;
            for entry in entries.take(512).flatten() {
                if !budget.take() {
                    break;
                }
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    let cand = entry.path().join(&file_name);
                    if cand.is_file() {
                        return Some(cand);
                    }
                }
            }
            None
        }
        VaultAgent::Codex => find_codex_by_id(&root.join("sessions"), sid, 4, budget),
        VaultAgent::Agy => {
            for cand in agy_candidate_dirs(root, true) {
                let conv_dir = cand.join("brain").join(sid);
                let logs = conv_dir.join(".system_generated/logs");
                for p in [
                    logs.join("transcript.jsonl"),
                    logs.join("transcript_full.jsonl"),
                    conv_dir.join("transcript.jsonl"),
                    cand.join("sessions").join(format!("{sid}.jsonl")),
                ] {
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
            None
        }
        VaultAgent::OpenCode => {
            for path in super::opencode_sqlite::candidates(root) {
                if super::opencode_sqlite::read(&path, Some(sid), false)
                    .is_ok_and(|batch| !batch.sessions.is_empty())
                {
                    return Some(path);
                }
            }
            for base in opencode_candidate_bases(root) {
                for ext in ["jsonl", "json"] {
                    let direct = base.join(format!("{sid}.{ext}"));
                    if direct.is_file() {
                        return Some(direct);
                    }
                }
                if let Ok(groups) = fs::read_dir(&base) {
                    for group in groups.take(256).flatten() {
                        if !budget.take() {
                            break;
                        }
                        if group.file_type().is_ok_and(|k| k.is_dir()) {
                            for ext in ["jsonl", "json"] {
                                let nested = group.path().join(format!("{sid}.{ext}"));
                                if nested.is_file() {
                                    return Some(nested);
                                }
                            }
                        }
                    }
                }
            }
            None
        }
    }
}

fn find_codex_by_id(
    dir: &Path,
    sid: &str,
    depth: usize,
    budget: &mut ScanBudget,
) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    let mut entries: Vec<_> = entries.take(65_536).flatten().collect();
    entries.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    for entry in entries {
        if !budget.take() {
            break;
        }
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            if let Some(found) = find_codex_by_id(&path, sid, depth - 1, budget) {
                return Some(found);
            }
        } else if ft.is_file() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".jsonl") && name_str.contains(sid) {
                return Some(path);
            }
        }
    }
    None
}

fn find_newest_transcript_by_cwd(
    root: &Path,
    agent: VaultAgent,
    norm_cwd: &str,
    filter: &CwdFallbackFilter<'_>,
    budget: &mut ScanBudget,
) -> Option<(u64, PathBuf, String)> {
    let mut best: Option<(u64, PathBuf, String)> = None;
    match agent {
        VaultAgent::Claude => {
            let Ok(projects) = fs::read_dir(root.join("projects")) else {
                return None;
            };
            for proj in projects.take(256).flatten() {
                if !budget.take() {
                    break;
                }
                if !proj.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let Ok(files) = fs::read_dir(proj.path()) else {
                    continue;
                };
                for file in files.take(256).flatten() {
                    if !budget.take() {
                        break;
                    }
                    let p = file.path();
                    if p.extension().is_none_or(|e| e != "jsonl") {
                        continue;
                    }
                    let Some(mtime) = file_mtime(&p) else {
                        continue;
                    };
                    if best.as_ref().is_some_and(|b| mtime <= b.0) {
                        continue;
                    }
                    if let Some((sid, file_cwd)) = peek_jsonl_id_and_cwd(&p)
                        && normalize_path(&file_cwd) == norm_cwd
                        && filter.allows(&p, &sid)
                    {
                        best = Some((mtime, p, sid));
                    }
                }
            }
        }
        VaultAgent::Codex => {
            walk_codex_for_cwd(
                &root.join("sessions"),
                4,
                Some(""),
                norm_cwd,
                filter,
                &mut best,
                budget,
            );
        }
        VaultAgent::Agy => {
            for cand in agy_candidate_dirs(root, true) {
                scan_agy_brain_for_cwd(&cand, norm_cwd, filter, &mut best, budget);
                if let Ok(files) = fs::read_dir(cand.join("sessions")) {
                    for file in files.take(128).flatten() {
                        if !budget.take() {
                            break;
                        }
                        let p = file.path();
                        if p.extension().is_some_and(|e| e == "jsonl")
                            && let Some(mtime) = file_mtime(&p)
                            && best.as_ref().is_none_or(|b| mtime > b.0)
                            && let Some((sid, file_cwd)) = peek_jsonl_id_and_cwd(&p)
                            && normalize_path(&file_cwd) == norm_cwd
                            && filter.allows(&p, &sid)
                        {
                            best = Some((mtime, p, sid));
                        }
                    }
                }
            }
        }
        VaultAgent::OpenCode => {
            for path in super::opencode_sqlite::candidates(root) {
                if !budget.take() {
                    break;
                }
                if let Ok(batch) = super::opencode_sqlite::read(&path, None, false) {
                    for session in batch.sessions {
                        if normalize_path(&session.cwd) == norm_cwd
                            && !filter.claimed_ids.contains(&session.id)
                            && filter
                                .min_birth_secs
                                .is_none_or(|birth| session.created >= birth.saturating_sub(2))
                            && best.as_ref().is_none_or(|b| session.modified > b.0)
                        {
                            best = Some((session.modified, path.clone(), session.id));
                        }
                    }
                }
            }
            for base in opencode_candidate_bases(root) {
                if let Ok(files) = fs::read_dir(&base) {
                    for file in files.take(128).flatten() {
                        if !budget.take() {
                            break;
                        }
                        let p = file.path();
                        if p.extension().is_some_and(|e| e == "jsonl" || e == "json")
                            && let Some(mtime) = file_mtime(&p)
                            && best.as_ref().is_none_or(|b| mtime > b.0)
                            && let Some((sid, file_cwd)) = peek_jsonl_id_and_cwd(&p)
                            && normalize_path(&file_cwd) == norm_cwd
                            && filter.allows(&p, &sid)
                        {
                            best = Some((mtime, p, sid));
                        }
                    }
                }
            }
        }
    }
    best
}

/// Codex keeps rollouts under `sessions/YYYY/MM/DD`. Walk newest-first: NTFS lists names
/// ascending, so the shared budget used to run out in old days before reaching today.
/// `period` is the canonical date so far (`""`, `YYYY`, `YYYY-MM`); `None` below other names.
fn walk_codex_for_cwd(
    dir: &Path,
    depth: usize,
    period: Option<&str>,
    norm_cwd: &str,
    filter: &CwdFallbackFilter<'_>,
    best: &mut Option<(u64, PathBuf, String)>,
    budget: &mut ScanBudget,
) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.take(65_536).flatten().collect();
    entries.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    for entry in entries {
        if !budget.take() {
            break;
        }
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            continue;
        }
        let p = entry.path();
        if ft.is_dir() {
            let digits = if depth == 4 { 4 } else { 2 };
            let child = entry
                .file_name()
                .to_str()
                .filter(|name| name.len() == digits && name.bytes().all(|b| b.is_ascii_digit()))
                .zip(period)
                .map(|(name, period)| {
                    if period.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{period}-{name}")
                    }
                });
            if child
                .as_deref()
                .is_some_and(|child| filter.predates_codex_period(child))
            {
                continue;
            }
            walk_codex_for_cwd(
                &p,
                depth - 1,
                child.as_deref(),
                norm_cwd,
                filter,
                best,
                budget,
            );
        } else if ft.is_file()
            && p.extension().is_some_and(|e| e == "jsonl")
            && let Some(mtime) = file_mtime(&p)
            && best.as_ref().is_none_or(|b| mtime > b.0)
            && let Some((sid, file_cwd)) = peek_jsonl_id_and_cwd(&p)
            && normalize_path(&file_cwd) == norm_cwd
            && filter.allows(&p, &sid)
        {
            *best = Some((mtime, p, sid));
        }
    }
}

fn scan_agy_brain_for_cwd(
    cand: &Path,
    norm_cwd: &str,
    filter: &CwdFallbackFilter<'_>,
    best: &mut Option<(u64, PathBuf, String)>,
    budget: &mut ScanBudget,
) {
    let Ok(brain_dir) = fs::read_dir(cand.join("brain")) else {
        return;
    };
    let mut transcripts: Vec<(u64, PathBuf, String)> = Vec::new();
    for entry in brain_dir.take(1024).flatten() {
        if !budget.take() {
            break;
        }
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }
        let conv_id = entry.file_name().to_string_lossy().into_owned();
        let conv_dir = entry.path();
        let logs = conv_dir.join(".system_generated/logs");
        let Some(p) = [
            logs.join("transcript.jsonl"),
            logs.join("transcript_full.jsonl"),
            conv_dir.join("transcript.jsonl"),
        ]
        .into_iter()
        .find(|p| p.is_file()) else {
            continue;
        };
        if !filter.allows(&p, &conv_id) {
            continue;
        }
        let Some(mtime) = file_mtime(&p) else {
            continue;
        };
        if best.as_ref().is_some_and(|b| mtime <= b.0) {
            continue;
        }
        transcripts.push((mtime, p, conv_id));
    }
    if transcripts.is_empty() {
        return;
    }
    transcripts.sort_by_key(|a| std::cmp::Reverse(a.0));
    let (by_id, by_display) = read_agy_history_tail(&cand.join("history.jsonl"));
    for (mtime, p, conv_id) in transcripts.into_iter().take(32) {
        if best.as_ref().is_some_and(|b| mtime <= b.0) {
            break;
        }
        if by_id.get(&conv_id).is_some_and(|w| w == norm_cwd)
            || peek_agy_transcript_cwd(&p, &by_display)
                .is_some_and(|w| normalize_path(&w) == norm_cwd)
        {
            *best = Some((mtime, p, conv_id));
            break;
        }
    }
}

fn read_agy_history_tail(path: &Path) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut by_id = BTreeMap::new();
    let mut by_display = BTreeMap::new();
    let Ok(mut file) = File::open(path) else {
        return (by_id, by_display);
    };
    if let Ok(len) = file.metadata().map(|m| m.len())
        && len > 128 * 1024
    {
        file.seek(SeekFrom::Start(len - 128 * 1024)).ok();
    }
    let mut reader = BufReader::with_capacity(8192, file.take(128 * 1024));
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line).ok().unwrap_or(0) > 0 {
        if let Ok(v) = serde_json::from_slice::<Value>(&line)
            && let Some(ws) = v
                .get("workspace")
                .or_else(|| v.get("cwd"))
                .and_then(Value::as_str)
            && !ws.trim().is_empty()
        {
            let norm = normalize_path(ws.trim());
            if let Some(cid) = v
                .get("conversationId")
                .or_else(|| v.get("id"))
                .and_then(Value::as_str)
                && !cid.trim().is_empty()
            {
                by_id.insert(cid.trim().to_owned(), norm.clone());
            }
            if let Some(disp) = v
                .get("display")
                .or_else(|| v.get("title"))
                .and_then(Value::as_str)
                && !disp.trim().is_empty()
            {
                by_display.insert(disp.trim().to_owned(), norm);
            }
        }
        line.clear();
    }
    (by_id, by_display)
}

fn peek_agy_transcript_cwd(path: &Path, by_display: &BTreeMap<String, String>) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::with_capacity(8192, file.take(64 * 1024));
    let mut line = Vec::new();
    for _ in 0..16 {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            break;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if let Some(cwd) = extract_agy_workspace_from_record(&v) {
            return Some(cwd);
        }
        if let Some(content) = v
            .get("content")
            .or_else(|| v.get("text"))
            .and_then(Value::as_str)
        {
            let req = extract_agy_user_request(content);
            if !req.is_empty()
                && let Some(ws) = by_display.get(req.trim())
            {
                return Some(ws.clone());
            }
        }
    }
    None
}

fn peek_jsonl_id_and_cwd(path: &Path) -> Option<(String, String)> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::with_capacity(4096, file.take(32 * 1024));
    let mut line = Vec::new();
    let mut sid = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_owned();
    let mut cwd = String::new();
    for _ in 0..16 {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            break;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        if let Some(s) = v
            .get("sessionId")
            .or_else(|| v.get("conversationId"))
            .or_else(|| v.get("id"))
            .or_else(|| v.get("payload").and_then(|p| p.get("id")))
            .and_then(Value::as_str)
            && !s.trim().is_empty()
        {
            s.trim().clone_into(&mut sid);
        }
        if let Some(c) = v
            .get("cwd")
            .or_else(|| v.get("directory"))
            .or_else(|| v.get("workspace"))
            .or_else(|| v.get("payload").and_then(|p| p.get("cwd")))
            .and_then(Value::as_str)
            && !c.trim().is_empty()
        {
            c.trim().clone_into(&mut cwd);
            break;
        }
        if let Some(extracted) = extract_agy_workspace_from_record(&v) {
            cwd = extracted;
            break;
        }
    }
    if cwd.is_empty() {
        None
    } else {
        Some((sid, cwd))
    }
}

fn file_mtime(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn file_birth_or_mtime(path: &Path) -> Option<u64> {
    let meta = fs::metadata(path).ok()?;
    meta.created()
        .or_else(|_| meta.modified())
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn parse_line_into_items(agent: VaultAgent, v: &Value, out: &mut Vec<ChatItem>) {
    let start = out.len();
    match agent {
        VaultAgent::Claude => parse_claude_line(v, out),
        VaultAgent::Codex => parse_codex_line(v, out),
        VaultAgent::Agy => parse_agy_line(v, out),
        VaultAgent::OpenCode => {
            let mut remaining = MAX_POLL_BYTES;
            parse_opencode_value(v, None, out, &mut remaining);
        }
    }
    let at = record_time(v);
    if !at.is_empty() {
        for item in out.iter_mut().skip(start) {
            item.at.clone_from(&at);
        }
    }
}

/// 한 레코드의 시각. Claude·Codex는 `timestamp`(RFC 3339), `OpenCode`는 `time.created`(epoch ms)다.
fn record_time(v: &Value) -> String {
    if let Some(at) = v.get("timestamp").and_then(Value::as_str) {
        return bounded(at.trim(), 64);
    }
    v.get("time")
        .and_then(|t| t.get("created"))
        .and_then(Value::as_u64)
        .map(|ms| ms.to_string())
        .unwrap_or_default()
}

fn parse_claude_line(v: &Value, out: &mut Vec<ChatItem>) {
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || v.get("isMeta").and_then(Value::as_bool) == Some(true)
    {
        return;
    }
    let rec_type = v
        .get("type")
        .or_else(|| v.get("role"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let msg = v.get("message").unwrap_or(v);
    match rec_type {
        "user" => parse_content_blocks(ChatItemKind::User, msg, out),
        "assistant" => parse_content_blocks(ChatItemKind::Assistant, msg, out),
        "thinking" | "reasoning" => {
            if let Some(t) = v
                .get("thinking")
                .or_else(|| v.get("text"))
                .or_else(|| v.get("content"))
                .and_then(Value::as_str)
            {
                push_item(
                    out,
                    ChatItem {
                        kind: ChatItemKind::Reasoning,
                        text: bounded(t.trim(), MAX_CHAT_TEXT),
                        ..ChatItem::default()
                    },
                );
            }
        }
        _ => {}
    }
}

fn parse_codex_line(v: &Value, out: &mut Vec<ChatItem>) {
    let rec_type = v.get("type").and_then(Value::as_str).unwrap_or("");
    if rec_type == "response_item"
        && let Some(payload) = v.get("payload")
    {
        let p_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
        match p_type {
            "reasoning" => {
                let mut parts = Vec::new();
                if let Some(summary) = payload.get("summary").and_then(Value::as_array) {
                    for s in summary {
                        if let Some(t) = s
                            .get("text")
                            .or_else(|| s.get("summary_text"))
                            .and_then(Value::as_str)
                            && !t.trim().is_empty()
                        {
                            parts.push(t.trim().to_owned());
                        }
                    }
                }
                if parts.is_empty()
                    && let Some(t) = payload.get("text").and_then(Value::as_str)
                    && !t.trim().is_empty()
                {
                    parts.push(t.trim().to_owned());
                }
                if !parts.is_empty() {
                    push_item(
                        out,
                        ChatItem {
                            kind: ChatItemKind::Reasoning,
                            text: bounded(&parts.join("\n"), MAX_CHAT_TEXT),
                            ..ChatItem::default()
                        },
                    );
                }
            }
            "message" => {
                let role = payload
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("user");
                if role == "developer" || role == "system" {
                    return;
                }
                let kind = if role == "assistant" {
                    ChatItemKind::Assistant
                } else {
                    ChatItemKind::User
                };
                parse_content_blocks(kind, payload, out);
            }
            "function_call" | "custom_tool_call" => {
                let name = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let call_id = payload
                    .get("call_id")
                    .or_else(|| payload.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let raw_args = payload.get("arguments").or_else(|| payload.get("input"));
                let parsed_args = match raw_args {
                    Some(Value::String(s)) => serde_json::from_str::<Value>(s)
                        .unwrap_or_else(|_| Value::String(s.clone())),
                    Some(other) => other.clone(),
                    None => Value::Null,
                };
                emit_tool_or_special(name, call_id, &parsed_args, out);
            }
            "function_call_output" | "custom_tool_call_output" => {
                let call_id = payload
                    .get("call_id")
                    .or_else(|| payload.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let output = payload
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let is_error = output.starts_with("Process exited with code ")
                    && !output.starts_with("Process exited with code 0");
                push_item(
                    out,
                    ChatItem {
                        kind: ChatItemKind::ToolResult,
                        tool_id: bounded(call_id, MAX_TEXT),
                        text: bounded(output, MAX_CHAT_TEXT),
                        is_error,
                        ..ChatItem::default()
                    },
                );
            }
            _ => {}
        }
        return;
    }

    if rec_type == "event_msg"
        && let Some(payload) = v.get("payload")
    {
        let p_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
        match p_type {
            "user_message" | "agent_message" => {
                let kind = if p_type == "user_message" {
                    ChatItemKind::User
                } else {
                    ChatItemKind::Assistant
                };
                if let Some(msg) = payload
                    .get("message")
                    .or_else(|| payload.get("text"))
                    .and_then(Value::as_str)
                {
                    let cleaned = clean_user_or_assistant_text(kind, msg);
                    if !cleaned.is_empty() {
                        push_item(
                            out,
                            ChatItem {
                                kind,
                                text: bounded(&cleaned, MAX_CHAT_TEXT),
                                ..ChatItem::default()
                            },
                        );
                    }
                }
            }
            "agent_reasoning" => {
                if let Some(msg) = payload
                    .get("text")
                    .or_else(|| payload.get("message"))
                    .and_then(Value::as_str)
                {
                    push_item(
                        out,
                        ChatItem {
                            kind: ChatItemKind::Reasoning,
                            text: bounded(msg.trim(), MAX_CHAT_TEXT),
                            ..ChatItem::default()
                        },
                    );
                }
            }
            "exec_approval_request" | "apply_patch_approval_request" => {
                let cmd = payload
                    .get("command")
                    .or_else(|| payload.get("reason"))
                    .map(value_summary)
                    .unwrap_or_default();
                push_item(
                    out,
                    ChatItem {
                        kind: ChatItemKind::Permission,
                        title: bounded(p_type, MAX_TEXT),
                        text: bounded(&cmd, MAX_CHAT_TEXT),
                        ..ChatItem::default()
                    },
                );
            }
            _ => {}
        }
        return;
    }

    if let Some(role) = v.get("role").and_then(Value::as_str) {
        if role == "developer" || role == "system" {
            return;
        }
        let kind = if role == "assistant" {
            ChatItemKind::Assistant
        } else {
            ChatItemKind::User
        };
        parse_content_blocks(kind, v, out);
    }
}

fn parse_agy_line(v: &Value, out: &mut Vec<ChatItem>) {
    let source = v.get("source").and_then(Value::as_str).unwrap_or("");
    let rec_type = v.get("type").and_then(Value::as_str).unwrap_or("");
    let role = v.get("role").and_then(Value::as_str).unwrap_or("");
    let content = v
        .get("content")
        .or_else(|| v.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");

    if let Some(thinking) = v
        .get("thinking")
        .or_else(|| v.get("thought"))
        .and_then(Value::as_str)
        && !thinking.trim().is_empty()
    {
        push_item(
            out,
            ChatItem {
                kind: ChatItemKind::Reasoning,
                text: bounded(thinking.trim(), MAX_CHAT_TEXT),
                ..ChatItem::default()
            },
        );
    }

    if matches!(source, "USER_EXPLICIT" | "USER")
        || matches!(rec_type, "USER_INPUT" | "REQUEST")
        || role == "user"
    {
        let req = extract_agy_user_request(content);
        let cleaned = clean_user_or_assistant_text(ChatItemKind::User, &req);
        if !cleaned.is_empty() {
            push_item(
                out,
                ChatItem {
                    kind: ChatItemKind::User,
                    text: bounded(&cleaned, MAX_CHAT_TEXT),
                    ..ChatItem::default()
                },
            );
        }
    } else if matches!(rec_type, "TOOL_RESULT" | "FUNCTION_RESPONSE" | "GENERIC") || role == "tool"
    {
        let tool_id = v
            .get("tool_call_id")
            .or_else(|| v.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let output = v
            .get("output")
            .or_else(|| v.get("content"))
            .or_else(|| v.get("error"))
            .map(value_summary)
            .unwrap_or_default();
        let is_error = v.get("is_error").and_then(Value::as_bool) == Some(true)
            || v.get("status").and_then(Value::as_str) == Some("ERROR")
            || v.get("error").is_some();
        push_item(
            out,
            ChatItem {
                kind: ChatItemKind::ToolResult,
                tool_id: bounded(tool_id, MAX_TEXT),
                text: bounded(&output, MAX_CHAT_TEXT),
                is_error,
                ..ChatItem::default()
            },
        );
    } else if (source == "MODEL" || rec_type == "PLANNER_RESPONSE" || role == "assistant")
        && !content.trim().is_empty()
    {
        push_item(
            out,
            ChatItem {
                kind: ChatItemKind::Assistant,
                text: bounded(content.trim(), MAX_CHAT_TEXT),
                ..ChatItem::default()
            },
        );
    }

    for key in ["tool_calls", "function_calls", "toolCalls"] {
        if let Some(calls) = v.get(key).and_then(Value::as_array) {
            for call in calls {
                let name = call
                    .get("name")
                    .or_else(|| call.get("tool_name"))
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let id = call.get("id").and_then(Value::as_str).unwrap_or("");
                let args = call
                    .get("args")
                    .or_else(|| call.get("arguments"))
                    .or_else(|| call.get("input"))
                    .unwrap_or(&Value::Null);
                emit_tool_or_special(name, id, args, out);
            }
        }
    }
}

fn parse_opencode_value(
    v: &Value,
    storage_root: Option<&Path>,
    out: &mut Vec<ChatItem>,
    remaining: &mut u64,
) {
    if let Some(arr) = v.get("messages").and_then(Value::as_array) {
        for item in arr {
            parse_opencode_message_record(item, storage_root, out, remaining);
        }
        return;
    }
    if v.get("role").is_some() || v.get("parts").is_some() || v.get("content").is_some() {
        parse_opencode_message_record(v, storage_root, out, remaining);
    }
}

fn parse_opencode_message_record(
    v: &Value,
    storage_root: Option<&Path>,
    out: &mut Vec<ChatItem>,
    remaining: &mut u64,
) {
    let role = v.get("role").and_then(Value::as_str).unwrap_or("user");
    if role == "developer" || role == "system" {
        return;
    }
    let default_kind = if role == "assistant" {
        ChatItemKind::Assistant
    } else {
        ChatItemKind::User
    };
    parse_content_blocks(default_kind, v, out);
    if let Some(parts) = v.get("parts").and_then(Value::as_array) {
        for part in parts {
            parse_opencode_part(default_kind, part, out);
        }
    }
    if let (Some(sr), Some(msg_id)) = (storage_root, v.get("id").and_then(Value::as_str))
        && let Ok(entries) = fs::read_dir(sr.join("part").join(msg_id))
    {
        let mut part_files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
            .collect();
        part_files.sort();
        for pf in part_files {
            if let Ok(pv) = read_json_bounded(&pf, remaining) {
                parse_opencode_part(default_kind, &pv, out);
            }
        }
    }
}

fn parse_opencode_part(default_kind: ChatItemKind, part: &Value, out: &mut Vec<ChatItem>) {
    let p_type = part.get("type").and_then(Value::as_str).unwrap_or("text");
    match p_type {
        "reasoning" | "thinking" => {
            if let Some(t) = part
                .get("text")
                .or_else(|| part.get("content"))
                .and_then(Value::as_str)
            {
                push_item(
                    out,
                    ChatItem {
                        kind: ChatItemKind::Reasoning,
                        text: bounded(t.trim(), MAX_CHAT_TEXT),
                        ..ChatItem::default()
                    },
                );
            }
        }
        "tool" | "tool-invocation" | "tool_use" => {
            let name = part
                .get("tool")
                .or_else(|| part.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let id = part
                .get("callID")
                .or_else(|| part.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let state = part.get("state").unwrap_or(part);
            let input = state
                .get("input")
                .or_else(|| part.get("input"))
                .unwrap_or(&Value::Null);
            emit_tool_or_special(name, id, input, out);
            if let Some(output) = state.get("output").and_then(Value::as_str)
                && !output.trim().is_empty()
            {
                push_item(
                    out,
                    ChatItem {
                        kind: ChatItemKind::ToolResult,
                        tool_name: bounded(name, MAX_TEXT),
                        tool_id: bounded(id, MAX_TEXT),
                        text: bounded(output.trim(), MAX_CHAT_TEXT),
                        is_error: state.get("status").and_then(Value::as_str) == Some("error"),
                        ..ChatItem::default()
                    },
                );
            }
        }
        _ => {
            if let Some(t) = part
                .get("text")
                .or_else(|| part.get("content"))
                .and_then(Value::as_str)
            {
                let cleaned = clean_user_or_assistant_text(default_kind, t);
                if !cleaned.is_empty() {
                    push_item(
                        out,
                        ChatItem {
                            kind: default_kind,
                            text: bounded(&cleaned, MAX_CHAT_TEXT),
                            ..ChatItem::default()
                        },
                    );
                }
            }
        }
    }
}

fn parse_content_blocks(default_kind: ChatItemKind, record: &Value, out: &mut Vec<ChatItem>) {
    let Some(content) = record
        .get("content")
        .or_else(|| record.get("text"))
        .or_else(|| record.get("message"))
    else {
        return;
    };
    if let Some(s) = content.as_str() {
        let cleaned = clean_user_or_assistant_text(default_kind, s);
        if !cleaned.is_empty() {
            push_item(
                out,
                ChatItem {
                    kind: default_kind,
                    text: bounded(&cleaned, MAX_CHAT_TEXT),
                    ..ChatItem::default()
                },
            );
        }
        return;
    }
    let Some(arr) = content.as_array() else {
        return;
    };
    for block in arr {
        if let Some(s) = block.as_str() {
            let cleaned = clean_user_or_assistant_text(default_kind, s);
            if !cleaned.is_empty() {
                push_item(
                    out,
                    ChatItem {
                        kind: default_kind,
                        text: bounded(&cleaned, MAX_CHAT_TEXT),
                        ..ChatItem::default()
                    },
                );
            }
            continue;
        }
        let b_type = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        match b_type.as_str() {
            "thinking" | "reasoning" => {
                if let Some(t) = block
                    .get("thinking")
                    .or_else(|| block.get("text"))
                    .or_else(|| block.get("content"))
                    .and_then(Value::as_str)
                {
                    push_item(
                        out,
                        ChatItem {
                            kind: ChatItemKind::Reasoning,
                            text: bounded(t.trim(), MAX_CHAT_TEXT),
                            ..ChatItem::default()
                        },
                    );
                }
            }
            "tool_use" => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                let input = block.get("input").unwrap_or(&Value::Null);
                emit_tool_or_special(name, id, input, out);
            }
            "tool_result" => {
                let id = block
                    .get("tool_use_id")
                    .or_else(|| block.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
                let text = block.get("content").map(value_summary).unwrap_or_default();
                push_item(
                    out,
                    ChatItem {
                        kind: ChatItemKind::ToolResult,
                        tool_id: bounded(id, MAX_TEXT),
                        text: bounded(&text, MAX_CHAT_TEXT),
                        is_error,
                        ..ChatItem::default()
                    },
                );
            }
            _ => {
                if let Some(t) = block
                    .get("text")
                    .or_else(|| block.get("content"))
                    .and_then(Value::as_str)
                {
                    let cleaned = clean_user_or_assistant_text(default_kind, t);
                    if !cleaned.is_empty() {
                        push_item(
                            out,
                            ChatItem {
                                kind: default_kind,
                                text: bounded(&cleaned, MAX_CHAT_TEXT),
                                ..ChatItem::default()
                            },
                        );
                    }
                }
            }
        }
    }
}

fn emit_tool_or_special(name: &str, tool_id: &str, input: &Value, out: &mut Vec<ChatItem>) {
    let lower: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();

    if matches!(
        lower.as_str(),
        "askuserquestion" | "askquestion" | "requestuserinput" | "question"
    ) {
        if !emit_questions(name, tool_id, input, out) {
            let text = value_summary(input);
            push_item(
                out,
                ChatItem {
                    kind: ChatItemKind::Question,
                    tool_name: bounded(name, MAX_TEXT),
                    tool_id: bounded(tool_id, MAX_TEXT),
                    text: bounded(&text, MAX_CHAT_TEXT),
                    ..ChatItem::default()
                },
            );
        }
        return;
    }

    if matches!(
        lower.as_str(),
        "askpermission" | "requestpermission" | "exitplanmode"
    ) || input
        .get("sandbox_permissions")
        .and_then(Value::as_str)
        .is_some_and(|s| s == "require_escalated")
    {
        let reason = input
            .get("Reason")
            .or_else(|| input.get("reason"))
            .or_else(|| input.get("justification"))
            .or_else(|| input.get("plan"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let target = input
            .get("Target")
            .or_else(|| input.get("cmd"))
            .or_else(|| input.get("command"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let summary = if !reason.is_empty() && !target.is_empty() {
            format!("{reason} ({target})")
        } else if !reason.is_empty() {
            reason.to_owned()
        } else if !target.is_empty() {
            target.to_owned()
        } else {
            value_summary(input)
        };
        push_item(
            out,
            ChatItem {
                kind: ChatItemKind::Permission,
                title: bounded(name, MAX_TEXT),
                tool_name: bounded(name, MAX_TEXT),
                tool_id: bounded(tool_id, MAX_TEXT),
                text: bounded(&summary, MAX_CHAT_TEXT),
                ..ChatItem::default()
            },
        );
        return;
    }

    let file_path = extract_tool_file_path(input);
    let title = input
        .get("description")
        .or_else(|| input.get("Description"))
        .or_else(|| input.get("toolSummary"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .trim_matches('"');
    let summary = extract_tool_summary(input, &file_path);
    push_item(
        out,
        ChatItem {
            kind: ChatItemKind::ToolCall,
            title: bounded(title.trim(), MAX_TEXT),
            tool_name: bounded(name, MAX_TEXT),
            tool_id: bounded(tool_id, MAX_TEXT),
            file_path: bounded(&file_path, MAX_TEXT),
            text: bounded(&summary, MAX_CHAT_TEXT),
            ..ChatItem::default()
        },
    );
}

fn emit_questions(name: &str, tool_id: &str, input: &Value, out: &mut Vec<ChatItem>) -> bool {
    let list = input
        .get("questions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![input.clone()]);
    let mut emitted = false;
    for q in list {
        let question_text = q
            .get("question")
            .or_else(|| q.get("prompt"))
            .or_else(|| q.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let header = q
            .get("header")
            .or_else(|| q.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let multi_select = q
            .get("multiSelect")
            .or_else(|| q.get("multi_select"))
            .or_else(|| q.get("is_multi_select"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut options = Vec::new();
        if let Some(opts) = q.get("options").and_then(Value::as_array) {
            for opt in opts.iter().take(MAX_QUESTION_OPTIONS) {
                if let Some(s) = opt.as_str() {
                    if !s.trim().is_empty() {
                        options.push(ChatQuestionOption {
                            label: bounded(s.trim(), MAX_TEXT),
                            description: String::new(),
                        });
                    }
                } else if let Some(label) = opt
                    .get("label")
                    .or_else(|| opt.get("title"))
                    .or_else(|| opt.get("value"))
                    .and_then(Value::as_str)
                    && !label.trim().is_empty()
                {
                    let desc = opt
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    options.push(ChatQuestionOption {
                        label: bounded(label.trim(), MAX_TEXT),
                        description: bounded(desc, MAX_TEXT),
                    });
                }
            }
        }
        if question_text.is_empty() && options.is_empty() {
            continue;
        }
        emitted = true;
        push_item(
            out,
            ChatItem {
                kind: ChatItemKind::Question,
                title: bounded(header, MAX_TEXT),
                tool_name: bounded(name, MAX_TEXT),
                tool_id: bounded(tool_id, MAX_TEXT),
                text: bounded(question_text, MAX_CHAT_TEXT),
                multi_select,
                options,
                ..ChatItem::default()
            },
        );
    }
    emitted
}

fn extract_tool_file_path(input: &Value) -> String {
    for key in [
        "file_path",
        "filePath",
        "path",
        "TargetFile",
        "AbsolutePath",
    ] {
        if let Some(p) = input.get(key).and_then(Value::as_str) {
            let trimmed = p.trim().trim_matches('"').trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }
    let patch_text = input
        .as_str()
        .or_else(|| input.get("patch").and_then(Value::as_str))
        .unwrap_or("");
    for line in patch_text.lines() {
        for prefix in ["*** Update File: ", "*** Add File: ", "*** Delete File: "] {
            if let Some(rest) = line.strip_prefix(prefix)
                && !rest.trim().is_empty()
            {
                return rest.trim().to_owned();
            }
        }
    }
    String::new()
}

fn extract_tool_summary(input: &Value, file_path: &str) -> String {
    for key in [
        "command",
        "cmd",
        "CommandLine",
        "query",
        "Query",
        "pattern",
        "Pattern",
        "DirectoryPath",
        "SearchDirectory",
        "SearchPath",
    ] {
        if let Some(s) = input.get(key).and_then(Value::as_str) {
            let trimmed = s.trim().trim_matches('"').trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }
    if !file_path.is_empty() {
        return file_path.to_owned();
    }
    value_summary(input)
}

fn value_summary(val: &Value) -> String {
    match val {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_owned(),
        Value::Array(arr) => {
            let mut parts = Vec::new();
            for item in arr {
                if let Some(s) = item.as_str() {
                    parts.push(s.trim().to_owned());
                } else if let Some(t) = item
                    .get("text")
                    .or_else(|| item.get("content"))
                    .and_then(Value::as_str)
                {
                    parts.push(t.trim().to_owned());
                }
            }
            if parts.is_empty() {
                val.to_string()
            } else {
                parts.join("\n")
            }
        }
        _ => val.to_string(),
    }
}

fn clean_user_or_assistant_text(kind: ChatItemKind, raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if kind == ChatItemKind::User && is_injected_user_context(trimmed) {
        return String::new();
    }
    trimmed.to_owned()
}

fn push_item(out: &mut Vec<ChatItem>, item: ChatItem) {
    if item.text.is_empty() && item.options.is_empty() && item.tool_name.is_empty() {
        return;
    }
    out.push(item);
}

fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
    bytes.get(start..end).unwrap_or(&[])
}

fn bounded(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut out = String::new();
    for ch in s.chars() {
        if out.len() + ch.len_utf8() > max_bytes {
            break;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use krill_proto::os::ffi::OsStrExt as _;
    use std::fmt::Write as _;
    use std::io::Write as _;

    fn audit_fixture(name: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("krill-chat-audit-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        (dir, path)
    }
    fn claude_record(text: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({"type":"assistant","message":{"content":text}})
        )
    }
    #[test]
    fn mobile_history_consumes_progress_after_the_item_limit() {
        let (dir, path) = audit_fixture("progress");
        let mut text = String::new();
        for i in 0..128 {
            text.push_str(&claude_record(&format!("old {i}")));
        }
        text.push_str("{\"type\":\"progress\"}\n\n");
        let end = u64::try_from(text.len()).unwrap();
        for i in 0..128 {
            text.push_str(&claude_record(&format!("new {i}")));
        }
        fs::write(&path, text).unwrap();
        let resolved = poll_transcript_file(VaultAgent::Claude, "sid", &path, 0);
        let (_, start) = latest_page(resolved.clone(), None).unwrap();
        assert_eq!(start, end);
        let (older, first) = latest_page(resolved.clone(), Some(start)).unwrap();
        assert_eq!(first, 0);
        assert_eq!(older.items.len(), 128);
        assert_eq!(older.next_offset, end);
        assert_eq!(replay_page(resolved, 0, end).unwrap(), older);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn mobile_history_keeps_multicard_records_atomic_even_above_the_page_limit() {
        let (dir, path) = audit_fixture("multicard");
        let prefix = claude_record("old").repeat(127);
        let cards: Vec<_> = (0..140)
            .map(|i| serde_json::json!({"type":"text","text":format!("card {i}")}))
            .collect();
        let multi = format!(
            "{}\n",
            serde_json::json!({"type":"assistant","message":{"content":cards}})
        );
        fs::write(&path, format!("{prefix}{multi}")).unwrap();
        let first = poll_transcript_file(VaultAgent::Claude, "sid", &path, 0);
        assert_eq!(first.items.len(), 127);
        assert_eq!(first.next_offset, u64::try_from(prefix.len()).unwrap());
        let second = poll_transcript_file(VaultAgent::Claude, "sid", &path, first.next_offset);
        assert_eq!(second.items.len(), 140);
        assert_eq!(second.items.last().unwrap().text, "card 139");
        assert_eq!(second.next_offset, fs::metadata(&path).unwrap().len());
        let (latest, start) = latest_page(first, None).unwrap();
        assert_eq!(latest, second);
        assert_eq!(start, u64::try_from(prefix.len()).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn mobile_history_can_pass_a_giant_final_record() {
        let (dir, path) = audit_fixture("giant");
        let old = claude_record("old");
        let giant = claude_record(&"x".repeat(700_000));
        fs::write(&path, format!("{old}{giant}{}", claude_record("last"))).unwrap();
        let resolved = poll_transcript_file(VaultAgent::Claude, "sid", &path, 0);
        let (_, start) = latest_page(resolved.clone(), None).unwrap();
        let (skip, before) = latest_page(resolved.clone(), Some(start)).unwrap();
        assert_eq!(skip.next_offset, start);
        assert_eq!(before, u64::try_from(old.len()).unwrap());
        let (first, zero) = latest_page(resolved, Some(before)).unwrap();
        assert_eq!(zero, 0);
        assert_eq!(first.items[0].text, "old");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn mobile_opencode_nested_and_flat_storage_page_all_sorted_messages_and_parts() {
        for nested in [false, true] {
            let (dir, _) = audit_fixture(if nested { "oc-nested" } else { "oc-flat" });
            let storage = dir.join("storage");
            let session_dir = storage.join(if nested { "session/project" } else { "session" });
            fs::create_dir_all(&session_dir).unwrap();
            let path = session_dir.join("sid.json");
            fs::write(&path, "{}").unwrap();
            fs::create_dir_all(storage.join("message/sid")).unwrap();
            for i in (0..150).rev() {
                let id = format!("msg_{i:04}");
                fs::write(storage.join(format!("message/sid/{id}.json")),
                    serde_json::to_vec(&serde_json::json!({"id":id,"role":"assistant","content":format!("message {i}")})).unwrap()).unwrap();
            }
            let parts = storage.join("part/msg_0149");
            fs::create_dir_all(&parts).unwrap();
            for i in (0..40).rev() {
                fs::write(
                    parts.join(format!("part_{i:04}.json")),
                    serde_json::to_vec(
                        &serde_json::json!({"type":"text","text":format!("part {i}")}),
                    )
                    .unwrap(),
                )
                .unwrap();
            }
            assert_eq!(opencode_storage_root(&path), Some(storage.as_path()));
            let resolved = poll_transcript_file(VaultAgent::OpenCode, "sid", &path, 0);
            let (latest, start) = latest_page(resolved.clone(), None).unwrap();
            assert_eq!(start, 62);
            assert_eq!(latest.items.len(), 128);
            assert_eq!(latest.items.last().unwrap().text, "part 39");
            let (older, zero) = latest_page(resolved.clone(), Some(start)).unwrap();
            assert_eq!(zero, 0);
            assert_eq!(older.items.len(), 62);
            assert_eq!(older.next_offset, start);
            assert_eq!(older.items.first().unwrap().text, "message 0");
            assert_eq!(replay_page(resolved, 0, start).unwrap(), older);
            let forward = poll_transcript_file(VaultAgent::OpenCode, "sid", &path, 128);
            assert_eq!(forward.items.len(), 62);
            assert!(
                poll_transcript_file(VaultAgent::OpenCode, "sid", &path, forward.next_offset)
                    .items
                    .is_empty()
            );
            fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn unsupported_profiles_stay_unbound_and_explicit_ids_never_fall_back() {
        let (dir, path) = audit_fixture("binding");
        fs::write(&path, claude_record("old conversation")).unwrap();
        for profile in [
            "pi", "custom", "shell", "claude", "codex", "agy", "opencode",
        ] {
            let stored = StoredLaunch {
                profile: profile.into(),
                permission: krill_proto::agent::Permission::Manual,
                session_id: Some("not-written-yet".into()),
                transcript_path: None,
                cwd: dir.as_os_str().as_bytes().to_vec(),
                hibernation_blocked: false,
                account: None,
            };
            let roots = VaultRoots {
                claude: vec![dir.clone()],
                codex: vec![dir.clone()],
                agy: vec![dir.clone()],
                opencode: vec![dir.clone()],
            };
            let snapshot = poll_session_in(&roots, Some(&stored), &[], 0);
            assert!(!snapshot.bound, "{profile}");
            if ["pi", "custom", "shell"].contains(&profile) {
                assert!(agent_for_session(&stored).is_none());
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn mobile_latest_window_pages_back_without_gaps_and_leaves_partial_record_for_poll() {
        let dir = std::env::temp_dir().join(format!("chat-latest-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.jsonl");
        let mut text = String::new();
        for index in 0..1050 {
            writeln!(text, "{}", serde_json::json!({"type":"assistant","message":{"role":"assistant","content":format!("message {index} {}", "x".repeat(1024))}})).unwrap();
        }
        let complete_end = u64::try_from(text.len()).unwrap();
        let partial = r#"{"type":"user","message":{"role":"user","content":"next"}}"#;
        text.push_str(partial);
        fs::write(&path, text).unwrap();
        let resolved = poll_transcript_file(VaultAgent::Claude, "sid", &path, 0);
        let (latest, mut start) = latest_page(resolved.clone(), None).unwrap();
        assert_eq!(latest.items.len(), MAX_CHAT_ITEMS);
        assert!(
            latest
                .items
                .last()
                .unwrap()
                .text
                .starts_with("message 1049 ")
        );
        assert_eq!(latest.next_offset, complete_end);
        assert!(start > MAX_POLL_BYTES);
        assert_eq!(
            replay_page(resolved.clone(), start, complete_end).unwrap(),
            latest
        );
        let mut all = latest.items.clone();
        while start > 0 {
            let (earlier, next_start) = latest_page(resolved.clone(), Some(start)).unwrap();
            assert_eq!(earlier.next_offset, start);
            assert!(next_start < start);
            let mut items = earlier.items;
            items.extend(all);
            all = items;
            start = next_start;
        }
        assert_eq!(all.len(), 1050);
        for (index, item) in all.iter().enumerate() {
            assert!(item.text.starts_with(&format!("message {index} ")));
        }
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        let appended = poll_transcript_file(VaultAgent::Claude, "sid", &path, complete_end);
        assert_eq!(appended.items.len(), 1);
        assert_eq!(appended.items[0].text, "next");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn mobile_latest_opencode_json_uses_last_messages() {
        let dir = std::env::temp_dir().join(format!("chat-latest-json-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        let messages: Vec<_> = (0..300)
            .map(|i| serde_json::json!({"role":"assistant","content":format!("message {i}")}))
            .collect();
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"messages":messages})).unwrap(),
        )
        .unwrap();
        let resolved = poll_transcript_file(VaultAgent::OpenCode, "sid", &path, 0);
        let (latest, start) = latest_page(resolved, None).unwrap();
        assert_eq!(start, 172);
        assert_eq!(latest.items.len(), MAX_CHAT_ITEMS);
        assert_eq!(latest.items.last().unwrap().text, "message 299");
        assert!(latest.next_offset < 1_u64 << 53);
        assert_eq!(
            latest.items.first().unwrap().text,
            format!("message {}", 300 - MAX_CHAT_ITEMS)
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn mobile_replay_stops_at_observed_end_even_inside_a_record() {
        let dir = std::env::temp_dir().join(format!("chat-replay-end-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.jsonl");
        let line =
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"same text\"}}\n";
        fs::write(&path, line).unwrap();
        let observed = poll_transcript_file(VaultAgent::Claude, "sid", &path, 0);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(line.as_bytes()).unwrap();
        let current = poll_transcript_file(VaultAgent::Claude, "sid", &path, 0);
        assert_eq!(current.items.len(), 2);
        assert_eq!(current.items[0].text, current.items[1].text);
        let replay = replay_page(current.clone(), 0, observed.next_offset).unwrap();
        assert_eq!(replay, observed);
        let partial = replay_page(current.clone(), 0, observed.next_offset - 1).unwrap();
        assert_eq!(partial.next_offset, 0);
        assert!(partial.items.is_empty());
        assert!(replay_page(current.clone(), 0, 0).is_err());
        assert!(replay_page(current, observed.next_offset, observed.next_offset).is_err());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bounded_history_pages_can_be_replayed_and_generation_survives_append() {
        let dir = std::env::temp_dir().join(format!("chat-pages-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.jsonl");
        let mut lines = String::new();
        for index in 0..300 {
            writeln!(lines, "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"message {index}\"}}}}").unwrap();
        }
        fs::write(&path, lines).unwrap();
        let first = poll_transcript_file(VaultAgent::Claude, "sid", &path, 0);
        assert_eq!(first.items.len(), MAX_CHAT_ITEMS);
        let second = poll_transcript_file(VaultAgent::Claude, "sid", &path, first.next_offset);
        let third = poll_transcript_file(VaultAgent::Claude, "sid", &path, second.next_offset);
        assert_eq!(
            first.items.len() + second.items.len() + third.items.len(),
            300
        );
        assert_eq!(
            first,
            poll_transcript_file(VaultAgent::Claude, "sid", &path, 0)
        );
        assert_eq!(
            second,
            poll_transcript_file(VaultAgent::Claude, "sid", &path, first.next_offset)
        );
        let identity = generation(&first);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            file,
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"new\"}}}}"
        )
        .unwrap();
        assert_eq!(generation(&first), identity);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn claude_incremental_poll_and_truncation_reset_work() {
        let dir = std::env::temp_dir().join(format!("owe945-claude-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let proj = dir.join("projects/demo");
        fs::create_dir_all(&proj).unwrap();
        let path = proj.join("sid-claude-1.jsonl");

        let line1 = r#"{"type":"user","sessionId":"sid-claude-1","cwd":"/tmp/demo","message":{"role":"user","content":"안녕 클로드"}}"#;
        let line2 = r#"{"type":"assistant","sessionId":"sid-claude-1","cwd":"/tmp/demo","message":{"role":"assistant","content":[{"type":"thinking","thinking":"생각 중"},{"type":"text","text":"반가워요"},{"type":"tool_use","id":"toolu_1","name":"Edit","input":{"file_path":"/tmp/demo/src/lib.rs","description":"Update lib"}}]}}"#;
        fs::write(&path, format!("{line1}\n{line2}\n")).unwrap();

        let snap1 = poll_transcript_file(VaultAgent::Claude, "sid-claude-1", &path, 0);
        assert!(snap1.bound);
        assert!(!snap1.reset);
        assert!(snap1.next_offset > 0);
        assert_eq!(snap1.items.len(), 4);
        assert_eq!(snap1.items[0].kind, ChatItemKind::User);
        assert_eq!(snap1.items[0].text, "안녕 클로드");
        assert_eq!(snap1.items[1].kind, ChatItemKind::Reasoning);
        assert_eq!(snap1.items[2].kind, ChatItemKind::Assistant);
        assert_eq!(snap1.items[3].kind, ChatItemKind::ToolCall);
        assert_eq!(snap1.items[3].file_path, "/tmp/demo/src/lib.rs");

        // 같은 오프셋으로 다시 폴링하면 새 아이템이 없다.
        let snap_empty =
            poll_transcript_file(VaultAgent::Claude, "sid-claude-1", &path, snap1.next_offset);
        assert!(snap_empty.items.is_empty());
        assert_eq!(snap_empty.next_offset, snap1.next_offset);

        // tool_result와 AskUserQuestion 추가 후 증분 폴링
        let line3 = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]}}"#;
        let line4 = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_q","name":"AskUserQuestion","input":{"questions":[{"header":"배포","question":"어느 환경으로 배포할까요?","multiSelect":false,"options":[{"label":"Staging","description":"테스트 서버"},{"label":"Prod","description":"운영 서버"}]}]}}]}}"#;
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{line3}\n{line4}").unwrap();
        drop(f);

        let snap2 =
            poll_transcript_file(VaultAgent::Claude, "sid-claude-1", &path, snap1.next_offset);
        assert_eq!(snap2.items.len(), 2);
        assert_eq!(snap2.items[0].kind, ChatItemKind::ToolResult);
        assert_eq!(snap2.items[1].kind, ChatItemKind::Question);
        assert_eq!(snap2.items[1].title, "배포");
        assert_eq!(snap2.items[1].options.len(), 2);
        assert_eq!(snap2.items[1].options[0].label, "Staging");

        // 512 KiB(MAX_POLL_BYTES)를 초과하는 거대 단일 JSONL 줄이 끼어 있어도 오프셋이 고착되지 않고 다음 줄을 읽는다.
        let huge_blob = "A".repeat(600 * 1024);
        let huge_line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_big","content":"{huge_blob}"}}]}}}}"#
        );
        let line_after_huge =
            r#"{"type":"assistant","message":{"role":"assistant","content":"거대 줄 통과 완료"}}"#;
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{huge_line}\n{line_after_huge}").unwrap();
        drop(f);
        let snap_huge =
            poll_transcript_file(VaultAgent::Claude, "sid-claude-1", &path, snap2.next_offset);
        assert!(snap_huge.next_offset > snap2.next_offset + 600 * 1024);
        assert_eq!(snap_huge.items.len(), 1);
        assert_eq!(snap_huge.items[0].kind, ChatItemKind::Assistant);
        assert_eq!(snap_huge.items[0].text, "거대 줄 통과 완료");

        // 파일이 절단(/clear 등)되면 reset=true와 함께 처음부터 다시 읽는다.
        fs::write(&path, format!("{line1}\n")).unwrap();
        let snap_reset = poll_transcript_file(
            VaultAgent::Claude,
            "sid-claude-1",
            &path,
            snap_huge.next_offset,
        );
        assert!(snap_reset.reset);
        assert_eq!(snap_reset.items.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn record_time_is_copied_to_every_item_of_the_record() {
        let mut out = Vec::new();
        let claude: Value = serde_json::from_str(
            r#"{"type":"assistant","timestamp":"2026-10-24T03:48:40.000Z","message":{"role":"assistant","content":[{"type":"text","text":"완료"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
        )
        .unwrap();
        parse_line_into_items(VaultAgent::Claude, &claude, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|item| item.at == "2026-10-24T03:48:40.000Z"));

        let opencode: Value = serde_json::from_str(
            r#"{"role":"user","time":{"created":1792813690000},"content":"안녕"}"#,
        )
        .unwrap();
        parse_line_into_items(VaultAgent::OpenCode, &opencode, &mut out);
        assert_eq!(out.last().unwrap().at, "1792813690000");

        let untimed: Value = serde_json::from_str(
            r#"{"type":"user","message":{"role":"user","content":"시각 없음"}}"#,
        )
        .unwrap();
        parse_line_into_items(VaultAgent::Claude, &untimed, &mut out);
        assert!(out.last().unwrap().at.is_empty());
    }

    #[test]
    fn codex_agy_and_opencode_normalize_into_unified_chat_items() {
        let dir = std::env::temp_dir().join(format!("owe945-multi-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        // Codex JSONL
        let codex_path = dir.join("rollout-codex.jsonl");
        let codex_lines = [
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"버그 고쳐줘"}]}}"#,
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"버그 고쳐줘"}}"#,
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"원인 분석"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"apply_patch","call_id":"call_1","input":"*** Begin Patch\n*** Update File: crates/krilld/src/lib.rs\n*** End Patch"}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"Success"}}"#,
            r#"{"type":"event_msg","payload":{"type":"exec_approval_request","command":"cargo test"}}"#,
        ];
        fs::write(&codex_path, format!("{}\n", codex_lines.join("\n"))).unwrap();
        let codex_snap = poll_transcript_file(VaultAgent::Codex, "codex-1", &codex_path, 0);
        // 중복 user_message는 합쳐져 총 5개 아이템이어야 한다.
        assert_eq!(codex_snap.items.len(), 5);
        assert_eq!(codex_snap.items[0].kind, ChatItemKind::User);
        assert_eq!(codex_snap.items[1].kind, ChatItemKind::Reasoning);
        assert_eq!(codex_snap.items[2].kind, ChatItemKind::ToolCall);
        assert_eq!(codex_snap.items[2].file_path, "crates/krilld/src/lib.rs");
        assert_eq!(codex_snap.items[3].kind, ChatItemKind::ToolResult);
        assert_eq!(codex_snap.items[4].kind, ChatItemKind::Permission);
        fs::write(
            &codex_path,
            format!("{0}\n{1}\n{0}\n{1}\n", codex_lines[0], codex_lines[1]),
        )
        .unwrap();
        let repeated = poll_transcript_file(VaultAgent::Codex, "codex-1", &codex_path, 0);
        assert_eq!(repeated.items.len(), 2);
        assert_eq!(repeated.items[0].text, repeated.items[1].text);

        // Agy JSONL
        let agy_path = dir.join("agy.jsonl");
        let agy_lines = [
            r#"{"source":"USER_EXPLICIT","content":"<USER_REQUEST>UI 개선해줘</USER_REQUEST>"}"#,
            r#"{"source":"MODEL","thinking":"레이아웃 검토","content":"개선안을 준비했습니다.","tool_calls":[{"id":"tc_1","name":"ask_question","args":{"questions":[{"question":"테마를 선택하세요","is_multi_select":true,"options":["Dark","Light"]}]}}]}"#,
        ];
        fs::write(&agy_path, format!("{}\n", agy_lines.join("\n"))).unwrap();
        let agy_snap = poll_transcript_file(VaultAgent::Agy, "agy-1", &agy_path, 0);
        assert_eq!(agy_snap.items.len(), 4);
        assert_eq!(agy_snap.items[0].kind, ChatItemKind::User);
        assert_eq!(agy_snap.items[0].text, "UI 개선해줘");
        assert_eq!(agy_snap.items[1].kind, ChatItemKind::Reasoning);
        assert_eq!(agy_snap.items[2].kind, ChatItemKind::Assistant);
        assert_eq!(agy_snap.items[3].kind, ChatItemKind::Question);
        assert!(agy_snap.items[3].multi_select);
        assert_eq!(agy_snap.items[3].options.len(), 2);

        // OpenCode JSON
        let oc_path = dir.join("ses_oc.json");
        fs::write(
            &oc_path,
            r#"{"id":"ses_oc","directory":"/tmp/demo","messages":[{"role":"user","content":"빌드 실행"},{"role":"assistant","parts":[{"type":"reasoning","text":"빌드 확인"},{"type":"tool","tool":"bash","callID":"c1","state":{"input":{"command":"cargo check"},"output":"Finished","status":"completed"}},{"type":"text","text":"빌드 완료"}]}]}"#,
        )
        .unwrap();
        let oc_snap = poll_transcript_file(VaultAgent::OpenCode, "ses_oc", &oc_path, 0);
        assert_eq!(oc_snap.items.len(), 5);
        assert_eq!(oc_snap.items[0].kind, ChatItemKind::User);
        assert_eq!(oc_snap.items[1].kind, ChatItemKind::Reasoning);
        assert_eq!(oc_snap.items[2].kind, ChatItemKind::ToolCall);
        assert_eq!(oc_snap.items[3].kind, ChatItemKind::ToolResult);
        assert_eq!(oc_snap.items[4].kind, ChatItemKind::Assistant);

        // 동일 offset으로 다시 폴링하면 빈 배열 반환
        let oc_again = poll_transcript_file(
            VaultAgent::OpenCode,
            "ses_oc",
            &oc_path,
            oc_snap.next_offset,
        );
        assert!(oc_again.items.is_empty());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn agy_brain_transcript_binds_by_cwd_and_normalizes_generic_tool_results() {
        let dir = std::env::temp_dir().join(format!("owe945-agy-brain-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let logs = dir.join("brain/conv-agy-77/.system_generated/logs");
        fs::create_dir_all(&logs).unwrap();
        let transcript = logs.join("transcript.jsonl");
        let lines = [
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","content":"<USER_REQUEST>\n모바일 사용성 개선해라\n</USER_REQUEST>"}"#,
            r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","thinking":"원인 파악","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"cargo test\"","Cwd":"\"/tmp/agy-demo\"","toolSummary":"\"Run tests\""}}]}"#,
            r#"{"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","content":"The command exited with code 0."}"#,
            r#"{"step_index":3,"source":"MODEL","type":"PLANNER_RESPONSE","content":"수정을 마쳤습니다."}"#,
        ];
        fs::write(&transcript, format!("{}\n", lines.join("\n"))).unwrap();

        let roots = VaultRoots {
            claude: Vec::new(),
            codex: Vec::new(),
            agy: vec![dir.clone()],
            opencode: Vec::new(),
        };
        let stored = StoredLaunch {
            profile: "Antigravity".to_owned(),
            permission: krill_proto::agent::Permission::Manual,
            session_id: None,
            transcript_path: None,
            cwd: b"/tmp/agy-demo".to_vec(),
            hibernation_blocked: true,
            account: None,
        };
        let snap = poll_session_in(&roots, Some(&stored), b"/tmp/agy-demo", 0);
        assert!(snap.bound);
        assert_eq!(snap.session_id, "conv-agy-77");
        assert_eq!(snap.items.len(), 5);
        assert_eq!(snap.items[0].kind, ChatItemKind::User);
        assert_eq!(snap.items[0].text, "모바일 사용성 개선해라");
        assert_eq!(snap.items[1].kind, ChatItemKind::Reasoning);
        assert_eq!(snap.items[2].kind, ChatItemKind::ToolCall);
        assert_eq!(snap.items[2].title, "Run tests");
        assert_eq!(snap.items[2].text, "cargo test");
        assert_eq!(snap.items[3].kind, ChatItemKind::ToolResult);
        assert_eq!(snap.items[3].text, "The command exited with code 0.");
        assert_eq!(snap.items[4].kind, ChatItemKind::Assistant);
        assert_eq!(snap.items[4].text, "수정을 마쳤습니다.");

        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(all(test, unix))]
mod performance_tests {
    use super::*;
    #[test]
    fn opencode_oversized_json_is_not_read_or_acknowledged_as_complete() {
        let dir = std::env::temp_dir().join(format!("chat-opencode-bound-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_POLL_BYTES + 1).unwrap();
        let result = poll_transcript_file(VaultAgent::OpenCode, "session", &path, 13);
        assert!(!result.bound);
        assert!(result.reset);
        assert_eq!(result.next_offset, 0);
        assert!(result.items.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn opencode_same_size_rewrite_invalidates_synthetic_cursor() {
        let dir =
            std::env::temp_dir().join(format!("chat-opencode-rewrite-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        fs::write(&path, r#"{"messages":[{"role":"user","content":"first"}]}"#).unwrap();
        let first = poll_transcript_file(VaultAgent::OpenCode, "session", &path, 0);
        let unchanged =
            poll_transcript_file(VaultAgent::OpenCode, "session", &path, first.next_offset);
        assert!(unchanged.items.is_empty());
        let replacement = dir.join("replace.json");
        fs::write(
            &replacement,
            r#"{"messages":[{"role":"user","content":"other"}]}"#,
        )
        .unwrap();
        fs::rename(replacement, &path).unwrap();
        let changed =
            poll_transcript_file(VaultAgent::OpenCode, "session", &path, first.next_offset);
        assert!(changed.reset);
        assert_eq!(changed.items[0].text, "other");
        assert_ne!(changed.next_offset, first.next_offset);
        assert!(changed.next_offset < (1_u64 << 53));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn transcript_resolution_cache_is_scoped_to_account_and_stored_binding() {
        let dir = std::env::temp_dir().join(format!("chat-path-scope-{}", std::process::id()));
        let roots = VaultRoots {
            claude: vec![dir.clone()],
            ..VaultRoots::default()
        };
        let mut stored = StoredLaunch {
            profile: "claude".into(),
            permission: krill_proto::agent::Permission::Manual,
            session_id: Some("first".into()),
            transcript_path: None,
            cwd: Vec::new(),
            hibernation_blocked: false,
            account: None,
        };
        let empty_ids = BTreeSet::new();
        let empty_paths = BTreeSet::new();
        let filter = CwdFallbackFilter {
            min_birth_secs: None,
            claimed_ids: &empty_ids,
            claimed_paths: &empty_paths,
        };
        assert!(resolve_transcript(&roots, VaultAgent::Claude, &stored, "", &filter).is_none());
        fs::create_dir_all(dir.join("projects/demo")).unwrap();
        let path = dir.join("projects/demo/other.jsonl");
        fs::write(&path, "{}\n").unwrap();
        stored.session_id = Some("other".into());
        assert_eq!(
            resolve_transcript(&roots, VaultAgent::Claude, &stored, "", &filter),
            Some((path, "other".into()))
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn claude_cwd_fallback_reads_the_encoded_project_folder_past_the_scan_budget() {
        let dir = std::env::temp_dir().join(format!("chat-claude-dir-{}", std::process::id()));
        let roots = VaultRoots {
            claude: vec![dir.clone()],
            ..VaultRoots::default()
        };
        // More decoys than the bounded `projects/` scan visits, so only the direct lookup can find it.
        for index in 0..300 {
            fs::create_dir_all(dir.join(format!("projects/-decoy-{index:03}"))).unwrap();
        }
        let cwd = "/work/한 repo.v2";
        let project = dir.join("projects/-work---repo-v2");
        fs::create_dir_all(&project).unwrap();
        assert_eq!(claude_project_dir(&dir, cwd), Some(project.clone()));
        let line = |sid: &str| format!("{}\n", serde_json::json!({"sessionId":sid,"cwd":cwd}));
        fs::write(project.join("older.jsonl"), line("older")).unwrap();
        fs::write(project.join("newer.jsonl"), line("newer")).unwrap();
        File::options()
            .write(true)
            .open(project.join("older.jsonl"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
            .unwrap();
        let mut stored = StoredLaunch {
            profile: "claude".into(),
            permission: krill_proto::agent::Permission::Manual,
            session_id: None,
            transcript_path: None,
            cwd: Vec::new(),
            hibernation_blocked: false,
            account: None,
        };
        let empty_ids = BTreeSet::new();
        let empty_paths = BTreeSet::new();
        let filter = CwdFallbackFilter {
            min_birth_secs: None,
            claimed_ids: &empty_ids,
            claimed_paths: &empty_paths,
        };
        let mut budget = ScanBudget::new();
        assert_eq!(
            resolve_transcript_uncached(
                &roots,
                VaultAgent::Claude,
                &stored,
                cwd,
                &filter,
                &mut budget
            ),
            Some((project.join("newer.jsonl"), "newer".into()))
        );
        stored.session_id = Some("older".into());
        assert_eq!(
            resolve_transcript_uncached(
                &roots,
                VaultAgent::Claude,
                &stored,
                cwd,
                &filter,
                &mut budget
            ),
            Some((project.join("older.jsonl"), "older".into()))
        );
        // A launched session whose file does not exist yet never borrows another conversation in the folder.
        stored.session_id = Some("not-written-yet".into());
        assert_eq!(
            resolve_transcript_uncached(
                &roots,
                VaultAgent::Claude,
                &stored,
                cwd,
                &filter,
                &mut budget
            ),
            None
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn codex_cwd_fallback_skips_old_date_folders_and_ignores_name_order_within_a_day() {
        use std::os::macos::fs::FileTimesExt as _;
        let dir = std::env::temp_dir().join(format!("chat-codex-newest-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let roots = VaultRoots {
            codex: vec![dir.clone()],
            ..VaultRoots::default()
        };
        // A Windows workspace with Hangul, written as Codex records it in `session_meta`.
        let cwd = r"C:\Users\user\orca\workspaces\onetab.bundle\간단작업";
        let rollout = |day: &Path, time: &str, id: &str| {
            fs::create_dir_all(day).unwrap();
            let path = day.join(format!("rollout-{time}-{id}.jsonl"));
            let meta = serde_json::json!({"type":"session_meta","payload":{"id":id,"cwd":cwd}});
            fs::write(&path, format!("{meta}\n")).unwrap();
            path
        };
        // More same-cwd history than the whole scan budget. Walked first (NTFS lists names
        // ascending), it used to exhaust the budget, and an exhausted scan answers nothing.
        for day in 1..=30 {
            for n in 0..70 {
                rollout(
                    &dir.join(format!("sessions/2025/01/{day:02}")),
                    &format!("2025-01-{day:02}T00-00-{n:02}"),
                    &format!("old-{day}-{n}"),
                );
            }
        }
        // A future folder never ages into pruning. Within it, a copied file sorts after the
        // live rollout but was born before the session: name order must not hide the live one.
        let day = dir.join("sessions/2999/01/01");
        let copied = rollout(&day, "2999-01-01T09-00-00", "copied-before-launch");
        File::options()
            .write(true)
            .open(&copied)
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_created(std::time::SystemTime::now() - Duration::from_secs(3600)),
            )
            .unwrap();
        let live = rollout(&day, "2999-01-01T08-30-00", "live");
        let stored = StoredLaunch {
            profile: "codex".into(),
            permission: krill_proto::agent::Permission::Manual,
            session_id: None,
            transcript_path: None,
            cwd: cwd.as_bytes().to_vec(),
            hibernation_blocked: true,
            account: None,
        };
        let empty_ids = BTreeSet::new();
        let empty_paths = BTreeSet::new();
        let filter = CwdFallbackFilter {
            min_birth_secs: Some(UNIX_EPOCH.elapsed().unwrap().as_secs() - 60),
            claimed_ids: &empty_ids,
            claimed_paths: &empty_paths,
        };
        let mut budget = ScanBudget::new();
        assert_eq!(
            resolve_transcript_uncached(
                &roots,
                VaultAgent::Codex,
                &stored,
                cwd,
                &filter,
                &mut budget
            ),
            Some((live.clone(), "live".into()))
        );
        // 2025 ended long before the session, so its folders were never opened.
        assert!(budget.remaining > 2000);

        let mut stored_by_id = stored;
        stored_by_id.session_id = Some("live".into());
        let mut id_budget = ScanBudget::new();
        assert_eq!(
            resolve_transcript_uncached(
                &roots,
                VaultAgent::Codex,
                &stored_by_id,
                cwd,
                &filter,
                &mut id_budget
            ),
            Some((live, "live".into()))
        );
        assert!(id_budget.remaining > 2000);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn codex_and_claude_system_injected_user_blocks_are_filtered_from_chat_items() {
        let dir = std::env::temp_dir().join(format!("chat-filter-injected-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        let codex_path = dir.join("rollout-codex.jsonl");
        let codex_lines = [
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>\nFilesystem sandboxing...\n</permissions instructions>"}]}}"#,
            r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /Users/macmini/Krill\n\n<INSTRUCTIONS>\n# AGENTS.md — Krill\n</INSTRUCTIONS>"},{"type":"input_text","text":"<environment_context>\n  <cwd>/Users/macmini/Krill</cwd>\n</environment_context>"}]}}"##,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<image name=[Image #1] path=\"/tmp/a.png\">"},{"type":"input_image","image_url":"data:image/png;base64,AAA"},{"type":"input_text","text":"</image>"},{"type":"input_text","text":"실제 유저 질문입니다"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<skill>\n<name>krill</name>\n<path>/tmp/SKILL.md</path>\n</skill>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<subagent_notification>\n{\"agent_id\":\"123\"}\n</subagent_notification>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"답변입니다"}]}}"#,
        ];
        fs::write(&codex_path, format!("{}\n", codex_lines.join("\n"))).unwrap();
        let codex_snap = poll_transcript_file(VaultAgent::Codex, "codex-sys", &codex_path, 0);
        assert_eq!(codex_snap.items.len(), 2);
        assert_eq!(codex_snap.items[0].kind, ChatItemKind::User);
        assert_eq!(codex_snap.items[0].text, "실제 유저 질문입니다");
        assert_eq!(codex_snap.items[1].kind, ChatItemKind::Assistant);
        assert_eq!(codex_snap.items[1].text, "답변입니다");

        let claude_path = dir.join("claude-meta.jsonl");
        let claude_lines = [
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"Base instructions for this skill: ..."}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>a1</task-id>\n</task-notification>"}}"#,
            r#"{"type":"user","message":{"role":"user","content":"클로드 실제 프롬프트"}}"#,
        ];
        fs::write(&claude_path, format!("{}\n", claude_lines.join("\n"))).unwrap();
        let claude_snap = poll_transcript_file(VaultAgent::Claude, "claude-sys", &claude_path, 0);
        assert_eq!(claude_snap.items.len(), 1);
        assert_eq!(claude_snap.items[0].kind, ChatItemKind::User);
        assert_eq!(claude_snap.items[0].text, "클로드 실제 프롬프트");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_agy_session_does_not_bind_older_or_claimed_transcript_in_same_cwd() {
        let dir = std::env::temp_dir().join(format!("chat-agy-isolation-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let logs = dir.join("brain/conv-old-session/.system_generated/logs");
        fs::create_dir_all(&logs).unwrap();
        let transcript = logs.join("transcript.jsonl");
        let lines = [
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","content":"<USER_REQUEST>\n이전 세션 메시지\n</USER_REQUEST>"}"#,
            r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","thinking":"검토","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"ls\"","Cwd":"\"/tmp/agy-iso\""}}]}"#,
        ];
        fs::write(&transcript, format!("{}\n", lines.join("\n"))).unwrap();

        let roots = VaultRoots {
            claude: Vec::new(),
            codex: Vec::new(),
            agy: vec![dir.clone()],
            opencode: Vec::new(),
        };
        let stored = StoredLaunch {
            profile: "agy".to_owned(),
            permission: krill_proto::agent::Permission::Manual,
            session_id: None,
            transcript_path: None,
            cwd: b"/tmp/agy-iso".to_vec(),
            hibernation_blocked: true,
            account: None,
        };

        // 1) 이미 다른 세션이 선점(claimed)한 대화 ID면 바인딩하지 않는다.
        let mut claimed_ids = BTreeSet::new();
        claimed_ids.insert("conv-old-session".to_owned());
        let claimed = (claimed_ids, BTreeSet::new());
        let (snap_claimed, resolved_claimed) =
            poll_session_in_bounded(&roots, Some(&stored), b"/tmp/agy-iso", 0, None, &claimed);
        assert!(!snap_claimed.bound);
        assert!(snap_claimed.items.is_empty());
        assert!(resolved_claimed.is_none());

        // 2) 새 세션 시작 시각(min_birth_secs)이 기존 트랜스크립트 생성 시각보다 미래면 바인딩하지 않는다.
        let future_launch = file_birth_or_mtime(&transcript).unwrap_or(0) + 60;
        let empty_claimed = (BTreeSet::new(), BTreeSet::new());
        let (snap_new, resolved_new) = poll_session_in_bounded(
            &roots,
            Some(&stored),
            b"/tmp/agy-iso",
            0,
            Some(future_launch),
            &empty_claimed,
        );
        assert!(!snap_new.bound);
        assert!(snap_new.items.is_empty());
        assert!(resolved_new.is_none());

        fs::remove_dir_all(dir).unwrap();
    }
}
