//! Zero-State 증분 트랜스크립트 리더(OWE-945 / MK-242).
//!
//! 상주 DB나 워커 스레드 없이 요청 시 `offset` 바이트부터 트랜스크립트 파일을 읽어
//! 4대 에이전트(`claude`·`codex`·`agy`·`opencode`)의 대화를 단일 `ChatItem` 스키마
//! (`User`·`Assistant`·`Reasoning`·`ToolCall`·`ToolResult`·`Question`·`Permission`)로 정규화한다.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use krill_proto::agent_history::{
    ChatItem, ChatItemKind, ChatPollSnapshot, ChatQuestionOption, MAX_CHAT_ITEMS, MAX_CHAT_TEXT,
    MAX_QUESTION_OPTIONS, MAX_TEXT, VaultAgent,
};
use serde_json::Value;

use crate::agent::control::StoredLaunch;

use super::vault::{
    VaultRoots, agy_candidate_dirs, collect_roots, extract_agy_user_request,
    extract_agy_workspace_from_record, normalize_path, opencode_candidate_bases,
};

/// 폴링 1회당 읽는 최대 바이트(512 KiB).
const MAX_POLL_BYTES: u64 = 512 * 1024;

/// 세션의 `StoredLaunch`와 작업 디렉터리를 바탕으로 트랜스크립트를 증분 폴링한다.
pub(crate) fn poll_session(
    state_dir: Option<&Path>,
    stored: Option<&StoredLaunch>,
    fallback_cwd: &[u8],
    offset: u64,
) -> ChatPollSnapshot {
    let roots = collect_roots(state_dir);
    poll_session_in(&roots, stored, fallback_cwd, offset)
}

pub(crate) fn poll_session_in(
    roots: &VaultRoots,
    stored: Option<&StoredLaunch>,
    fallback_cwd: &[u8],
    offset: u64,
) -> ChatPollSnapshot {
    let Some(stored) = stored else {
        return ChatPollSnapshot::default();
    };
    let agent = VaultAgent::parse(&stored.profile).unwrap_or(VaultAgent::Claude);
    let cwd_bytes = if stored.cwd.is_empty() {
        fallback_cwd
    } else {
        stored.cwd.as_slice()
    };
    let cwd_str = String::from_utf8_lossy(cwd_bytes);
    let resolved = resolve_transcript(roots, agent, stored, &cwd_str);
    let Some((path, session_id)) = resolved else {
        return ChatPollSnapshot {
            agent: bounded(agent.profile(), MAX_TEXT),
            session_id: bounded(stored.session_id.as_deref().unwrap_or(""), MAX_TEXT),
            transcript_path: bounded(stored.transcript_path.as_deref().unwrap_or(""), MAX_TEXT),
            next_offset: 0,
            reset: offset > 0,
            bound: false,
            items: Vec::new(),
        };
    };
    poll_transcript_file(agent, &session_id, &path, offset)
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
    if agent == VaultAgent::OpenCode && path.extension().is_some_and(|e| e == "json") {
        // OpenCode's JSON offset is synthetic, not a JSONL byte boundary.
        return if snapshot.next_offset == end_offset {
            Ok(snapshot)
        } else {
            Err(std::io::Error::other("chat page changed; return to latest"))
        };
    }
    Ok(poll_transcript_file_until(
        agent,
        &snapshot.session_id,
        path,
        offset,
        Some(end_offset),
    ))
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

    if agent == VaultAgent::OpenCode && path.extension().is_some_and(|e| e == "json") {
        return poll_opencode_json_file(&agent_str, &sid_str, path, &path_str, offset);
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
    let mut items = Vec::new();
    let mut previous_codex_record = None;
    let max_line = usize::try_from(MAX_POLL_BYTES).unwrap_or(512 * 1024);

    while items.len() < MAX_CHAT_ITEMS && batch_bytes < MAX_POLL_BYTES {
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
            while skipped < 64 * 1024 * 1024 {
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
        consumed = consumed.saturating_add(n_u64);
        batch_bytes = batch_bytes.saturating_add(n_u64);
        let trimmed = trim_ascii_whitespace(&line);
        if trimmed.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_slice::<Value>(trimmed) else {
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
        let before = items.len();
        parse_line_into_items(agent, &val, &mut items);
        // Only Codex's two representations of the same adjacent event may merge.
        // Separate records with the same text must remain separate messages.
        if previous_codex_record
            .zip(codex_record)
            .is_some_and(|(a, b)| a != b)
            && let Some(previous) = before.checked_sub(1).and_then(|i| items.get(i))
            && let Some(current) = items.get(before)
            && matches!(
                current.kind,
                ChatItemKind::User | ChatItemKind::Assistant | ChatItemKind::Reasoning
            )
            && previous.kind == current.kind
            && previous.text == current.text
        {
            items.remove(before);
            previous_codex_record = None;
        } else {
            previous_codex_record = codex_record;
        }
    }

    if items.len() > MAX_CHAT_ITEMS {
        items.truncate(MAX_CHAT_ITEMS);
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

fn poll_opencode_json_file(
    agent_str: &str,
    sid_str: &str,
    path: &Path,
    path_str: &str,
    offset: u64,
) -> ChatPollSnapshot {
    let Ok(bytes) = fs::read(path) else {
        return ChatPollSnapshot {
            agent: agent_str.to_owned(),
            session_id: sid_str.to_owned(),
            transcript_path: path_str.to_owned(),
            next_offset: 0,
            reset: offset > 0,
            bound: false,
            items: Vec::new(),
        };
    };
    let storage_root = path.parent().and_then(Path::parent);
    let mut synthetic_len = u64::try_from(bytes.len()).unwrap_or(0);
    let resolved_sid = if sid_str.is_empty() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned()
    } else {
        sid_str.to_owned()
    };
    let mut msg_files = Vec::new();
    if let Some(sr) = storage_root
        && !resolved_sid.is_empty()
        && let Ok(entries) = fs::read_dir(sr.join("message").join(&resolved_sid))
    {
        for entry in entries.take(128).flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "json")
                && let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                synthetic_len = synthetic_len.saturating_add(meta.len());
                msg_files.push(p);
            }
        }
        msg_files.sort();
    }

    if offset == synthetic_len && offset > 0 {
        return ChatPollSnapshot {
            agent: agent_str.to_owned(),
            session_id: bounded(&resolved_sid, MAX_TEXT),
            transcript_path: path_str.to_owned(),
            next_offset: synthetic_len,
            reset: false,
            bound: true,
            items: Vec::new(),
        };
    }

    let mut items = Vec::new();
    if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
        parse_opencode_value(&v, storage_root, &mut items);
    }
    for msg_file in msg_files {
        if items.len() >= MAX_CHAT_ITEMS {
            break;
        }
        if let Ok(text) = fs::read_to_string(&msg_file)
            && let Ok(v) = serde_json::from_str::<Value>(&text)
        {
            parse_opencode_message_record(&v, storage_root, &mut items);
        }
    }
    if items.len() > MAX_CHAT_ITEMS {
        items.truncate(MAX_CHAT_ITEMS);
    }
    ChatPollSnapshot {
        agent: agent_str.to_owned(),
        session_id: bounded(&resolved_sid, MAX_TEXT),
        transcript_path: path_str.to_owned(),
        next_offset: synthetic_len,
        reset: offset > 0,
        bound: true,
        items,
    }
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

fn resolve_transcript(
    roots: &VaultRoots,
    agent: VaultAgent,
    stored: &StoredLaunch,
    cwd: &str,
) -> Option<(PathBuf, String)> {
    let sid = stored.session_id.as_deref().unwrap_or("").trim();
    if let Some(tp) = stored.transcript_path.as_deref()
        && crate::agent::control::valid_transcript_path(tp)
    {
        let p = Path::new(tp);
        if p.is_file() {
            let inferred_sid = if sid.is_empty() {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_owned()
            } else {
                sid.to_owned()
            };
            return Some((p.to_path_buf(), inferred_sid));
        }
    }

    let mut search_dirs: Vec<PathBuf> = Vec::new();
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

    if !sid.is_empty() {
        for root in &search_dirs {
            if let Some(found) = find_transcript_by_id(root, agent, sid) {
                return Some((found, sid.to_owned()));
            }
        }
    }

    let norm_cwd = normalize_path(cwd);
    if !norm_cwd.is_empty() {
        let mut best: Option<(u64, PathBuf, String)> = None;
        for root in &search_dirs {
            if let Some(cand) = find_newest_transcript_by_cwd(root, agent, &norm_cwd)
                && best.as_ref().is_none_or(|b| cand.0 > b.0)
            {
                best = Some(cand);
            }
        }
        if let Some((_, path, found_sid)) = best {
            return Some((path, found_sid));
        }
    }
    None
}

fn find_transcript_by_id(root: &Path, agent: VaultAgent, sid: &str) -> Option<PathBuf> {
    match agent {
        VaultAgent::Claude => {
            let projects = root.join("projects");
            let file_name = format!("{sid}.jsonl");
            let entries = fs::read_dir(projects).ok()?;
            for entry in entries.take(512).flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    let cand = entry.path().join(&file_name);
                    if cand.is_file() {
                        return Some(cand);
                    }
                }
            }
            None
        }
        VaultAgent::Codex => find_codex_by_id(&root.join("sessions"), sid, 4),
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
            for base in opencode_candidate_bases(root) {
                for ext in ["jsonl", "json"] {
                    let direct = base.join(format!("{sid}.{ext}"));
                    if direct.is_file() {
                        return Some(direct);
                    }
                }
                if let Ok(groups) = fs::read_dir(&base) {
                    for group in groups.take(256).flatten() {
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

fn find_codex_by_id(dir: &Path, sid: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.take(256).flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            if let Some(found) = find_codex_by_id(&path, sid, depth - 1) {
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
) -> Option<(u64, PathBuf, String)> {
    let mut best: Option<(u64, PathBuf, String)> = None;
    match agent {
        VaultAgent::Claude => {
            let Ok(projects) = fs::read_dir(root.join("projects")) else {
                return None;
            };
            for proj in projects.take(256).flatten() {
                if !proj.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let Ok(files) = fs::read_dir(proj.path()) else {
                    continue;
                };
                for file in files.take(256).flatten() {
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
                    {
                        best = Some((mtime, p, sid));
                    }
                }
            }
        }
        VaultAgent::Codex => {
            walk_codex_for_cwd(&root.join("sessions"), 4, norm_cwd, &mut best);
        }
        VaultAgent::Agy => {
            for cand in agy_candidate_dirs(root, true) {
                scan_agy_brain_for_cwd(&cand, norm_cwd, &mut best);
                if let Ok(files) = fs::read_dir(cand.join("sessions")) {
                    for file in files.take(128).flatten() {
                        let p = file.path();
                        if p.extension().is_some_and(|e| e == "jsonl")
                            && let Some(mtime) = file_mtime(&p)
                            && best.as_ref().is_none_or(|b| mtime > b.0)
                            && let Some((sid, file_cwd)) = peek_jsonl_id_and_cwd(&p)
                            && normalize_path(&file_cwd) == norm_cwd
                        {
                            best = Some((mtime, p, sid));
                        }
                    }
                }
            }
        }
        VaultAgent::OpenCode => {
            for base in opencode_candidate_bases(root) {
                if let Ok(files) = fs::read_dir(&base) {
                    for file in files.take(128).flatten() {
                        let p = file.path();
                        if p.extension().is_some_and(|e| e == "jsonl" || e == "json")
                            && let Some(mtime) = file_mtime(&p)
                            && best.as_ref().is_none_or(|b| mtime > b.0)
                            && let Some((sid, file_cwd)) = peek_jsonl_id_and_cwd(&p)
                            && normalize_path(&file_cwd) == norm_cwd
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

fn walk_codex_for_cwd(
    dir: &Path,
    depth: usize,
    norm_cwd: &str,
    best: &mut Option<(u64, PathBuf, String)>,
) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.take(256).flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            continue;
        }
        let p = entry.path();
        if ft.is_dir() {
            walk_codex_for_cwd(&p, depth - 1, norm_cwd, best);
        } else if ft.is_file()
            && p.extension().is_some_and(|e| e == "jsonl")
            && let Some(mtime) = file_mtime(&p)
            && best.as_ref().is_none_or(|b| mtime > b.0)
            && let Some((sid, file_cwd)) = peek_jsonl_id_and_cwd(&p)
            && normalize_path(&file_cwd) == norm_cwd
        {
            *best = Some((mtime, p, sid));
        }
    }
}

fn scan_agy_brain_for_cwd(cand: &Path, norm_cwd: &str, best: &mut Option<(u64, PathBuf, String)>) {
    let Ok(brain_dir) = fs::read_dir(cand.join("brain")) else {
        return;
    };
    let mut transcripts: Vec<(u64, PathBuf, String)> = Vec::new();
    for entry in brain_dir.take(1024).flatten() {
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

fn parse_line_into_items(agent: VaultAgent, v: &Value, out: &mut Vec<ChatItem>) {
    match agent {
        VaultAgent::Claude => parse_claude_line(v, out),
        VaultAgent::Codex => parse_codex_line(v, out),
        VaultAgent::Agy => parse_agy_line(v, out),
        VaultAgent::OpenCode => parse_opencode_value(v, None, out),
    }
}

fn parse_claude_line(v: &Value, out: &mut Vec<ChatItem>) {
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
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
                    push_item(
                        out,
                        ChatItem {
                            kind,
                            text: bounded(msg.trim(), MAX_CHAT_TEXT),
                            ..ChatItem::default()
                        },
                    );
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
        if !req.trim().is_empty() {
            push_item(
                out,
                ChatItem {
                    kind: ChatItemKind::User,
                    text: bounded(req.trim(), MAX_CHAT_TEXT),
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

fn parse_opencode_value(v: &Value, storage_root: Option<&Path>, out: &mut Vec<ChatItem>) {
    if let Some(arr) = v.get("messages").and_then(Value::as_array) {
        for item in arr {
            parse_opencode_message_record(item, storage_root, out);
        }
        return;
    }
    if v.get("role").is_some() || v.get("parts").is_some() || v.get("content").is_some() {
        parse_opencode_message_record(v, storage_root, out);
    }
}

fn parse_opencode_message_record(v: &Value, storage_root: Option<&Path>, out: &mut Vec<ChatItem>) {
    let role = v.get("role").and_then(Value::as_str).unwrap_or("user");
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
            .take(32)
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
            .collect();
        part_files.sort();
        for pf in part_files {
            if let Ok(pt) = fs::read_to_string(pf)
                && let Ok(pv) = serde_json::from_str::<Value>(&pt)
            {
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
                push_item(
                    out,
                    ChatItem {
                        kind: default_kind,
                        text: bounded(t.trim(), MAX_CHAT_TEXT),
                        ..ChatItem::default()
                    },
                );
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
    if kind == ChatItemKind::User
        && (trimmed.starts_with("<environment_context>")
            || trimmed.starts_with("<permissions instructions>")
            || trimmed.starts_with("<collaboration_mode>")
            || trimmed.starts_with("<local-command-caveat>"))
    {
        return String::new();
    }
    trimmed.to_owned()
}

fn push_item(out: &mut Vec<ChatItem>, item: ChatItem) {
    if item.text.is_empty() && item.options.is_empty() && item.tool_name.is_empty() {
        return;
    }
    if out.len() < MAX_CHAT_ITEMS {
        out.push(item);
    }
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
    use std::fmt::Write as _;
    use std::io::Write as _;

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
