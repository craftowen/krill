//! 모바일 JSON 요청을 기존 IPC·M8 원자 저장소로 연결한다. 인증 전에는 호출하지 않는다.
use super::devices::hex;
use super::e2ee::{invalid, random};
use crate::registry::Registry;
use krill_proto::os::{
    ffi::{OsStrExt, OsStringExt},
    fs::{DirBuilderExt, OpenOptionsExt},
    net::UnixStream,
};
use krill_proto::{
    SessionId,
    control::{self, Create, Program, Reply, Request},
    input::{Input, InputAck, InputId, InputResult},
    ipc::{self, ClientRole, ServerHello},
    orchestration::OrchestrationStore,
    screen::{Attach, Msg, Seq, Stream},
    usage::{ProviderKind, UsageReply, UsageRequest},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

mod command_catalog;
pub(super) mod composer;
mod composer_status;
mod drops;
pub(super) mod feed;
mod mention_files;
mod requests;

const SNAPSHOT_MAX_DATA_BYTES: usize = ipc::MAX_FRAME.saturating_sub(256);

fn json_byte_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn truncate_string_value(value: &mut Value, max_chars: usize) {
    if let Value::String(text) = value
        && text.chars().count() > max_chars
    {
        let keep = text
            .char_indices()
            .nth(max_chars)
            .map_or(text.len(), |(idx, _)| idx);
        text.truncate(keep);
    }
}

fn trim_snapshot_card_lines(data: &mut Value, max_lines: usize, max_chars: usize) {
    let Some(cards) = data.get_mut("sessions").and_then(Value::as_array_mut) else {
        return;
    };
    for card in cards {
        let Some(lines) = card.get_mut("lines").and_then(Value::as_array_mut) else {
            continue;
        };
        if max_lines == 0 {
            lines.clear();
            continue;
        }
        if lines.len() > max_lines {
            let drain = lines.len() - max_lines;
            lines.drain(..drain);
        }
        for line in lines {
            truncate_string_value(line, max_chars);
        }
    }
}

fn trim_snapshot_card_fields(data: &mut Value, drop_command_context: bool) {
    let Some(cards) = data.get_mut("sessions").and_then(Value::as_array_mut) else {
        return;
    };
    for card in cards {
        for key in [
            "title",
            "foreground",
            "startup_stall",
            "turn_error",
            "cwd",
            "branch",
            "group_label",
            "group_path",
        ] {
            if let Some(value) = card.get_mut(key) {
                truncate_string_value(value, 200);
            }
        }
        if drop_command_context && let Some(obj) = card.as_object_mut() {
            obj.insert("command_context".into(), Value::Null);
        }
    }
}

fn trim_snapshot_secondary_sections(data: &mut Value) {
    if let Some(orch) = data.get_mut("orchestration").and_then(Value::as_object_mut) {
        for (key, limit) in [
            ("runs", 16),
            ("tasks", 32),
            ("gates", 16),
            ("questions", 16),
        ] {
            if let Some(items) = orch.get_mut(key).and_then(Value::as_array_mut) {
                if items.len() > limit {
                    items.truncate(limit);
                }
                for item in items {
                    if let Some(fields) = item.as_object_mut() {
                        for text_key in ["title", "summary", "body"] {
                            if let Some(val) = fields.get_mut(text_key) {
                                truncate_string_value(val, 240);
                            }
                        }
                    }
                }
            }
        }
    }
    if json_byte_len(data) <= SNAPSHOT_MAX_DATA_BYTES {
        return;
    }
    if let Some(qc) = data.get_mut("quick_commands").and_then(Value::as_array_mut) {
        qc.clear();
    }
}

pub(super) fn bound_snapshot_payload(data: &mut Value) {
    if json_byte_len(data) <= SNAPSHOT_MAX_DATA_BYTES {
        return;
    }
    trim_snapshot_card_lines(data, 1, 160);
    if json_byte_len(data) <= SNAPSHOT_MAX_DATA_BYTES {
        return;
    }
    trim_snapshot_card_lines(data, 0, 0);
    if json_byte_len(data) <= SNAPSHOT_MAX_DATA_BYTES {
        return;
    }
    trim_snapshot_card_fields(data, false);
    if json_byte_len(data) <= SNAPSHOT_MAX_DATA_BYTES {
        return;
    }
    trim_snapshot_card_fields(data, true);
    if json_byte_len(data) <= SNAPSHOT_MAX_DATA_BYTES {
        return;
    }
    trim_snapshot_secondary_sections(data);
    if json_byte_len(data) <= SNAPSHOT_MAX_DATA_BYTES {
        return;
    }
    if let Some(cards) = data.get_mut("sessions").and_then(Value::as_array_mut)
        && !cards.is_empty()
    {
        while !cards.is_empty()
            && json_byte_len(&Value::Array(cards.clone())) > SNAPSHOT_MAX_DATA_BYTES / 2
        {
            cards.pop();
        }
        if let Some(obj) = data.as_object_mut() {
            obj.insert("truncated".into(), Value::Bool(true));
        }
    }
    while json_byte_len(data) > SNAPSHOT_MAX_DATA_BYTES {
        let Some(cards) = data.get_mut("sessions").and_then(Value::as_array_mut) else {
            break;
        };
        if cards.pop().is_none() {
            break;
        }
        if let Some(obj) = data.as_object_mut() {
            obj.insert("truncated".into(), Value::Bool(true));
        }
    }
    if json_byte_len(data) > SNAPSHOT_MAX_DATA_BYTES
        && let Some(obj) = data.as_object_mut()
    {
        obj.insert(
            "orchestration".into(),
            json!({"runs":[],"tasks":[],"gates":[],"questions":[]}),
        );
        for key in ["workspaces", "repositories", "agents"] {
            if json_byte_len(&Value::Object(obj.clone())) <= SNAPSHOT_MAX_DATA_BYTES {
                break;
            }
            if let Some(arr) = obj.get_mut(key).and_then(Value::as_array_mut) {
                arr.clear();
            }
        }
    }
}

const BOT_PAGE_DEFAULT_LIMIT: usize = 50;
const BOT_PAGE_MAX_LIMIT: usize = 100;
const BOT_TEXT_DOC_MAX_BYTES: usize = 256 * 1024;
const MOBILE_FRAME_HEADROOM: usize = 32;

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn validate_bot_token(raw: &str, max_bytes: usize) -> io::Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > max_bytes || trimmed.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(trimmed.to_owned())
}

fn validate_opt_bot_token(raw: Option<&str>, max_bytes: usize) -> io::Result<Option<String>> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(value) => validate_bot_token(value, max_bytes).map(Some),
    }
}

fn validate_bot_text(
    raw: &str,
    max_bytes: usize,
    allow_newlines: bool,
    allow_empty: bool,
) -> io::Result<String> {
    if raw.len() > max_bytes {
        return Err(invalid());
    }
    let has_bad_ctrl = raw
        .chars()
        .any(|ch| ch.is_control() && !(allow_newlines && matches!(ch, '\n' | '\r' | '\t')));
    if has_bad_ctrl || (!allow_empty && raw.trim().is_empty()) {
        return Err(invalid());
    }
    Ok(raw.to_owned())
}

fn validate_opt_bot_text(
    raw: Option<&str>,
    max_bytes: usize,
    allow_newlines: bool,
    allow_empty: bool,
) -> io::Result<Option<String>> {
    raw.map(|text| validate_bot_text(text, max_bytes, allow_newlines, allow_empty))
        .transpose()
}

fn validate_bot_item_link(link: &krill_proto::bots::BotItemLink) -> io::Result<()> {
    let value = match link {
        krill_proto::bots::BotItemLink::Dispatch(v)
        | krill_proto::bots::BotItemLink::Pr(v)
        | krill_proto::bots::BotItemLink::Url(v)
        | krill_proto::bots::BotItemLink::File(v) => v,
    };
    validate_bot_token(value, 2048).map(|_| ())
}

fn validate_bot_decisions(decisions: &[krill_proto::bots::BotItemDecision]) -> io::Result<()> {
    if decisions.len() > 16 {
        return Err(invalid());
    }
    for decision in decisions {
        validate_bot_text(&decision.question, 1024, true, false)?;
        if decision.options.len() > 16 {
            return Err(invalid());
        }
        for option in &decision.options {
            validate_bot_text(option, 256, false, false)?;
        }
        if let Some(recommended) = decision.recommended
            && recommended >= decision.options.len()
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn parse_bot_page(limit: Option<usize>, cursor: Option<&str>) -> io::Result<(usize, usize)> {
    let bounded_limit = match limit {
        Some(0) => return Err(invalid()),
        Some(n) => n.min(BOT_PAGE_MAX_LIMIT),
        None => BOT_PAGE_DEFAULT_LIMIT,
    };
    let offset = match validate_opt_bot_token(cursor, 64)? {
        None => 0,
        Some(raw) => raw.parse::<usize>().map_err(|_| invalid())?,
    };
    Ok((offset, bounded_limit))
}

fn wrapped_ok_len(data: &Value) -> usize {
    serde_json::to_vec(&json!({"ok": true, "data": data})).map_or(usize::MAX, |bytes| bytes.len())
}

fn shrink_bot_list_entry(entry: &mut Value) -> bool {
    let Some(obj) = entry.as_object_mut() else {
        return false;
    };
    if let Some(events) = obj.get_mut("events").and_then(Value::as_array_mut)
        && !events.is_empty()
    {
        events.clear();
        return true;
    }
    for field in ["description", "done_means", "close_note", "body"] {
        if let Some(val) = obj.get_mut(field)
            && let Some(s) = val.as_str()
            && s.chars().count() > 256
        {
            truncate_string_value(val, 256);
            return true;
        }
    }
    false
}

fn build_bot_list_value(
    list_key: &str,
    items: &[Value],
    truncated: bool,
    next_cursor: Option<&str>,
    total_count: usize,
    extra_fields: &[(&str, Value)],
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert(list_key.to_owned(), Value::Array(items.to_vec()));
    obj.insert("truncated".to_owned(), Value::Bool(truncated));
    obj.insert(
        "next_cursor".to_owned(),
        next_cursor.map_or(Value::Null, |c| Value::String(c.to_owned())),
    );
    obj.insert("total_count".to_owned(), json!(total_count));
    for (k, v) in extra_fields {
        obj.insert((*k).to_owned(), v.clone());
    }
    Value::Object(obj)
}

fn item_cursor_from_value(item: &Value) -> Option<String> {
    let ts = item.get("last_activity_at_ms")?.as_u64()?;
    let uid = item.get("uid")?.as_str()?;
    Some(format!("{ts}|{uid}"))
}

fn bound_bot_list_payload(
    items: &[Value],
    list_key: &str,
    offset: usize,
    limit: usize,
    total_count: usize,
    upstream_next_cursor: Option<String>,
    extra_fields: &[(&str, Value)],
) -> io::Result<Value> {
    let max_wrapped = ipc::MAX_FRAME.saturating_sub(MOBILE_FRAME_HEADROOM);
    let slice_start = offset.min(items.len());
    let mut end = slice_start.saturating_add(limit).min(items.len());
    let mut page_items: Vec<Value> = items
        .get(slice_start..end)
        .map(<[Value]>::to_vec)
        .unwrap_or_default();
    let mut truncated = upstream_next_cursor.is_some() || end < items.len();
    let mut next_cursor = upstream_next_cursor.or_else(|| {
        if end < items.len() {
            if list_key == "items" {
                page_items
                    .last()
                    .and_then(item_cursor_from_value)
                    .or_else(|| Some(end.to_string()))
            } else {
                Some(end.to_string())
            }
        } else {
            None
        }
    });

    let mut data = build_bot_list_value(
        list_key,
        &page_items,
        truncated,
        next_cursor.as_deref(),
        total_count,
        extra_fields,
    );
    while wrapped_ok_len(&data) >= max_wrapped {
        if page_items.len() > 1 {
            page_items.pop();
            end = slice_start.saturating_add(page_items.len());
            truncated = true;
            next_cursor = (list_key == "items")
                .then(|| page_items.last().and_then(item_cursor_from_value))
                .flatten()
                .or_else(|| Some(end.to_string()));
            data = build_bot_list_value(
                list_key,
                &page_items,
                truncated,
                next_cursor.as_deref(),
                total_count,
                extra_fields,
            );
            continue;
        }
        if let Some(first) = page_items.first_mut()
            && shrink_bot_list_entry(first)
        {
            truncated = true;
            data = build_bot_list_value(
                list_key,
                &page_items,
                truncated,
                next_cursor.as_deref(),
                total_count,
                extra_fields,
            );
            continue;
        }
        if !page_items.is_empty() {
            page_items.clear();
            truncated = true;
            next_cursor = if list_key == "items" {
                None
            } else {
                Some(slice_start.to_string())
            };
            data = build_bot_list_value(
                list_key,
                &page_items,
                truncated,
                next_cursor.as_deref(),
                total_count,
                extra_fields,
            );
            continue;
        }
        return Err(io::Error::other("response exceeds mobile frame limit"));
    }
    Ok(data)
}

fn bound_group_view_payload(mut data: Value) -> io::Result<Value> {
    let max_wrapped = ipc::MAX_FRAME.saturating_sub(MOBILE_FRAME_HEADROOM);
    if let Some(obj) = data.as_object_mut() {
        obj.insert("truncated".to_owned(), Value::Bool(false));
    }
    while wrapped_ok_len(&data) >= max_wrapped {
        let drained = data
            .get_mut("group")
            .and_then(|g| g.get_mut("messages"))
            .and_then(Value::as_array_mut)
            .is_some_and(|msgs| {
                if msgs.len() > 1 {
                    msgs.remove(0);
                    true
                } else if let Some(first) = msgs.first_mut() {
                    shrink_bot_list_entry(first)
                } else {
                    false
                }
            });
        if !drained {
            return Err(io::Error::other("response exceeds mobile frame limit"));
        }
        if let Some(obj) = data.as_object_mut() {
            obj.insert("truncated".to_owned(), Value::Bool(true));
        }
    }
    Ok(data)
}

fn bound_bot_doc_payload(bot_id: &str, mut content: String) -> io::Result<Value> {
    let max_wrapped = ipc::MAX_FRAME.saturating_sub(MOBILE_FRAME_HEADROOM);
    let mut truncated = false;
    loop {
        let candidate = json!({
            "bot_id": bot_id,
            "content": content,
            "truncated": truncated,
        });
        if wrapped_ok_len(&candidate) < max_wrapped {
            return Ok(candidate);
        }
        if content.is_empty() {
            return Err(io::Error::other("response exceeds mobile frame limit"));
        }
        truncated = true;
        let new_len = content.len() / 2;
        let mut boundary = new_len;
        while boundary > 0 && !content.is_char_boundary(boundary) {
            boundary -= 1;
        }
        content.truncate(boundary);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct QuickCommandEntry {
    name: String,
    text: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct QuickCommandsFile {
    #[serde(default)]
    quick_commands: Vec<QuickCommandEntry>,
}

fn normalize_quick_command(mut entry: QuickCommandEntry) -> QuickCommandEntry {
    entry.name = entry.name.trim().to_owned();
    entry
        .text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_matches('\n')
        .clone_into(&mut entry.text);
    entry
}

fn load_quick_commands(dir: Option<&Path>) -> Vec<QuickCommandEntry> {
    dir.and_then(|dir| fs::read_to_string(dir.join("settings.toml")).ok())
        .and_then(|content| toml::from_str::<QuickCommandsFile>(&content).ok())
        .unwrap_or_default()
        .quick_commands
        .into_iter()
        .filter(|entry| !entry.name.trim().is_empty() && !entry.text.trim().is_empty())
        .map(normalize_quick_command)
        .take(128)
        .collect()
}

fn quick_commands_json(dir: Option<&Path>) -> Value {
    load_quick_commands(dir)
        .into_iter()
        .map(|entry| json!({"name":entry.name,"text":entry.text}))
        .collect()
}

/// `create_terminal`이 받는 에이전트 프로필(`registry` create와 같은 출처) id·이름, 표준 순서. 설정을 못 읽으면 빈 목록.
/// 미설치 CLI도 싣는다 — 띄우면 로그인 셸 preflight가 설치 안내를 보여 준다.
fn format_agents_json(agents: Option<&krill_proto::agent::Agents>) -> Value {
    let Some(agents) = agents else {
        return json!([]);
    };
    let mut entries: Vec<_> = agents.profiles.iter().collect();
    entries.sort_by(|(a, _), (b, _)| {
        (krill_proto::agent::profile_rank(a), *a).cmp(&(krill_proto::agent::profile_rank(b), *b))
    });
    entries
        .into_iter()
        .map(|(id, p)| json!({"id":id,"name":p.name}))
        .collect()
}

pub(super) struct Backend {
    registry: Weak<Registry>,
    dir: Option<PathBuf>,
    hello: ServerHello,
    host: Value,
    /// 카드 git 요약(branch·added·deleted) cwd별 캐시. 실패도 `Null`로 캐시한다.
    git_cache: Arc<Mutex<HashMap<PathBuf, (Instant, Value)>>>,
    git_inflight: Arc<Mutex<HashSet<PathBuf>>>,
    git_revision: Arc<AtomicU64>,
    /// 프로젝트 아이콘 비동기 SWR 캐시(스냅샷 응답 0ms 유지 + 콘텐츠 해시 배치 서빙).
    repo_icons: Arc<super::project_icons::RepoIconCache>,
    snapshot_cache: Mutex<Option<DashboardSnapshot>>,
    /// 스냅샷을 새로 만들 때마다 늘어난다. 늦게 도착한 옛 스냅샷이 새 것을 덮지 않게 한다.
    snapshot_rev: AtomicU64,
    catalog_cache: Mutex<Option<CatalogSnapshot>>,
    settings_lock: Mutex<()>,
    /// `~/Downloads/Krill Drop`, where finished phone uploads land.
    drop_inbox: Option<PathBuf>,
    // ponytail: global drop lock, per-upload locks if concurrent uploads matter
    drop_lock: Mutex<()>,
}
type FileStamp = Option<crate::agent::control::cache::Stamp>;
type SnapshotDependencies = (Vec<PathBuf>, Vec<FileStamp>);

struct DashboardSnapshot {
    revision: u64,
    paths: Vec<PathBuf>,
    stamps: Vec<FileStamp>,
    at: Instant,
    data: Value,
    etag: String,
    rev: u64,
}

struct CatalogSnapshot {
    repositories: Vec<control::RepositoryTrees>,
    paths: Vec<PathBuf>,
    stamps: Vec<FileStamp>,
    at: Instant,
    catalog: Arc<super::workspaces::Catalog>,
}

fn file_stamps(paths: &[PathBuf]) -> Vec<FileStamp> {
    paths
        .iter()
        .map(|path| crate::agent::control::cache::stamp(path))
        .collect()
}

/// 스냅샷 폴링이 같은 cwd마다 git 자식을 다시 띄우지 않게 하는 카드 요약 수명. 상세 git 화면은 캐시를 안 쓴다.
const GIT_TTL: Duration = Duration::from_secs(5);
impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").finish_non_exhaustive()
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PromptMode {
    #[default]
    Normal,
    Steer,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    /// `since`가 직전 응답의 `etag`와 같으면 본문 없이 `{"unchanged":true}`만 보낸다(폴링 대역폭·복호화·재렌더 절약).
    Snapshot {
        #[serde(default)]
        since: Option<String>,
        /// 변경 피드 간격(ms, 0=일시정지). `capabilities.change_feed`를 본 클라이언트만 보낸다.
        #[serde(default)]
        feed: Option<u64>,
    },
    #[serde(alias = "set_quick_commands")]
    SaveQuickCommands {
        commands: Vec<QuickCommandEntry>,
    },
    CreateTerminal {
        #[serde(default)]
        cwd_session: Option<u64>,
        #[serde(default)]
        workspace: Option<String>,
        #[serde(default)]
        agent: Option<String>,
    },
    CreateWorkspace {
        repository: String,
        name: String,
        #[serde(default)]
        base: Option<String>,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        setup_hash: Option<String>,
        #[serde(default)]
        skip_setup: bool,
    },
    CloseTerminal {
        session: u64,
    },
    Prompt {
        session: u64,
        text: String,
        #[serde(default)]
        photo: Option<PreparedPhoto>,
        #[serde(default)]
        photos: Vec<PreparedPhoto>,
        #[serde(default)]
        mode: PromptMode,
    },
    LivePromptReply {
        session: u64,
        expected_prompt: String,
        #[serde(default)]
        yes: Option<bool>,
        #[serde(default)]
        choice: Option<usize>,
        #[serde(default)]
        multi_selected: Option<Vec<usize>>,
        #[serde(default)]
        custom_text: Option<String>,
    },
    Interrupt {
        session: u64,
    },
    #[expect(
        dead_code,
        reason = "Legacy approval fields are decoded for compatibility but cannot authorize PTY input"
    )]
    Approve {
        session: u64,
        value: String,
    },
    Photo {
        session: u64,
        mime: String,
        data: String,
        #[serde(default)]
        prompt: Option<String>,
    },
    ChatPoll {
        session: u64,
        #[serde(default)]
        offset: u64,
        /// Open at the tail, or page backwards from `end_offset`.
        #[serde(default)]
        latest: bool,
        /// Display-only replay boundary from a previously observed page; never approval authority.
        #[serde(default)]
        end_offset: Option<u64>,
    },
    #[expect(
        dead_code,
        reason = "Legacy question fields are decoded for compatibility but cannot authorize PTY input"
    )]
    AnswerQuestion {
        session: u64,
        #[serde(default)]
        selected: Vec<usize>,
        #[serde(default)]
        option_count: usize,
        #[serde(default)]
        multi_select: bool,
        #[serde(default)]
        custom_text: Option<String>,
    },
    Accounts,
    SwitchAccount {
        provider: String,
        account_id: String,
    },
    Orchestration,
    GateReply {
        id: String,
        decision: String,
    },
    QuestionReply {
        id: String,
        body: String,
    },
    Handoff {
        session: u64,
        agent: String,
    },
    Git {
        session: u64,
        path: Option<String>,
    },
    /// Selected session CLI skills and custom command metadata.
    CommandCatalog {
        session: u64,
    },
    MentionFiles {
        session: u64,
        query: String,
    },
    ChatComposer {
        session: u64,
    },
    ComposerModels {
        session: u64,
    },
    SetModel {
        session: u64,
        model: String,
        #[serde(default)]
        effort: Option<String>,
    },
    SetEffort {
        session: u64,
        effort: String,
    },
    DropPut {
        upload: String,
        name: String,
        size: u64,
        offset: u64,
        data: String,
    },
    DropList,
    DropGet {
        id: String,
        offset: u64,
    },
    DropRemove {
        id: String,
    },
    #[serde(rename = "bots.list")]
    BotsList {
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        cursor: Option<String>,
    },
    #[serde(rename = "bots.templates")]
    BotsTemplates {},
    #[serde(rename = "bots.models")]
    BotsModels {
        agent_id: String,
    },
    #[serde(rename = "bots.upcoming")]
    BotsUpcoming {
        bot_id: String,
    },
    #[serde(rename = "bots.sessions.list")]
    BotsSessionsList {
        #[serde(default)]
        bot_id: Option<String>,
    },
    #[serde(rename = "bots.sessions.stop")]
    BotsSessionStop {
        bot_id: String,
        session_id: u64,
    },
    #[serde(rename = "bots.schedule.run")]
    BotsScheduleRun {
        bot_id: String,
        schedule_id: String,
    },
    #[serde(rename = "bots.schedule.toggle")]
    BotsScheduleToggle {
        bot_id: String,
        schedule_id: String,
        enabled: bool,
    },
    #[serde(rename = "bots.update")]
    BotsUpdate {
        bot_id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        agent_id: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        clear_model: bool,
        #[serde(default)]
        effort: Option<String>,
        #[serde(default)]
        clear_effort: bool,
        #[serde(default)]
        permission_mode: Option<krill_proto::agent::Permission>,
        #[serde(default)]
        paused: Option<bool>,
    },
    #[serde(rename = "bots.create")]
    BotsCreate {
        name: String,
        agent_id: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        emoji: Option<String>,
        #[serde(default)]
        slug: Option<String>,
        #[serde(default)]
        avatar_color: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        effort: Option<String>,
        #[serde(default)]
        permission_mode: Option<krill_proto::agent::Permission>,
        #[serde(default)]
        workspace: Option<String>,
        #[serde(default)]
        template_id: Option<String>,
        #[serde(default)]
        initial_soul: Option<String>,
        #[serde(default)]
        skills: Vec<String>,
    },
    #[serde(rename = "bots.chat.open")]
    BotsChatOpen {
        bot_id: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        prompt: Option<String>,
    },
    #[serde(rename = "bots.items")]
    BotsItems {
        #[serde(default)]
        bot_id: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        include_closed: bool,
        #[serde(default)]
        include_snoozed: bool,
        #[serde(default)]
        limit: Option<u32>,
        #[serde(default)]
        cursor: Option<String>,
    },
    #[serde(rename = "bots.item.create")]
    BotsItemCreate {
        bot_id: String,
        title: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        done_means: Option<String>,
        #[serde(default)]
        status: Option<krill_proto::bots::BotItemStatus>,
        #[serde(default)]
        links: Vec<krill_proto::bots::BotItemLink>,
    },
    #[serde(rename = "bots.item.update")]
    BotsItemUpdate {
        item_id: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        done_means: Option<String>,
        #[serde(default)]
        status: Option<krill_proto::bots::BotItemStatus>,
        #[serde(default)]
        comment: Option<String>,
        #[serde(default)]
        decisions: Vec<krill_proto::bots::BotItemDecision>,
        #[serde(default)]
        close_note: Option<String>,
        #[serde(default)]
        snoozed_until_ms: krill_proto::bots::FieldPatch<u64>,
    },
    #[serde(rename = "bots.item.answer")]
    BotsItemAnswer {
        item_id: String,
        #[serde(default)]
        answer_text: Option<String>,
        #[serde(default)]
        option_index: Option<u32>,
        #[serde(default)]
        decision_id: Option<String>,
        #[serde(default)]
        request_id: Option<String>,
    },
    #[serde(rename = "bots.item.link")]
    BotsItemLink {
        item_id: String,
        link: krill_proto::bots::BotItemLink,
    },
    #[serde(rename = "bots.access.list")]
    BotsAccessList {
        #[serde(default)]
        bot_id: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        cursor: Option<String>,
    },
    #[serde(rename = "bots.access.decide")]
    BotsAccessDecide {
        token: String,
        approve: bool,
    },
    #[serde(rename = "bots.access.redeem")]
    BotsAccessRedeem {
        code: String,
    },
    #[serde(rename = "bots.reply")]
    BotsReply {
        bot_id: String,
        #[serde(default)]
        link_id: Option<String>,
        text: String,
        #[serde(default)]
        reaction: Option<String>,
    },
    #[serde(rename = "bots.connectors.status")]
    BotsConnectorsStatus {
        #[serde(default)]
        bot_id: Option<String>,
    },
    ChatSubscribe {
        session: u64,
        #[serde(default)]
        offset: u64,
    },
    ChatUnsubscribe {
        session: u64,
    },
    #[serde(rename = "bots.groups.list")]
    BotsGroupsList {
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        cursor: Option<String>,
    },
    #[serde(rename = "bots.groups.create")]
    BotsGroupsCreate {
        name: String,
        bot_ids: Vec<String>,
        #[serde(default)]
        description: String,
    },
    #[serde(rename = "bots.groups.read")]
    BotsGroupsRead {
        group_id: String,
        #[serde(default)]
        thread_id: Option<String>,
        #[serde(default)]
        limit: Option<u32>,
    },
    #[serde(rename = "bots.groups.send")]
    BotsGroupsSend {
        group_id: String,
        #[serde(default)]
        thread_id: Option<String>,
        #[serde(default)]
        sender: Option<String>,
        text: String,
        #[serde(default)]
        reply_mode: Option<krill_proto::bots::BotGroupReplyMode>,
    },
    #[serde(rename = "bots.groups.new_thread")]
    BotsGroupsNewThread {
        group_id: String,
        #[serde(default)]
        title: Option<String>,
    },
    #[serde(rename = "bots.groups.stop")]
    BotsGroupsStop {
        group_id: String,
        #[serde(default)]
        thread_id: Option<String>,
    },
    #[serde(rename = "bots.look_around")]
    BotsLookAround {
        bot_id: String,
    },
    #[serde(rename = "bots.suggestions.list")]
    BotsSuggestionsList {
        #[serde(default)]
        bot_id: Option<String>,
        #[serde(default)]
        include_dismissed: bool,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        cursor: Option<String>,
    },
    #[serde(rename = "bots.suggestions.dismiss")]
    BotsSuggestionsDismiss {
        bot_id: String,
        suggestion_id: String,
    },
    #[serde(rename = "bots.suggestions.start")]
    BotsSuggestionsStart {
        bot_id: String,
        suggestion_id: String,
    },
    #[serde(rename = "repo_icons.read")]
    RepoIconsRead {
        keys: Vec<String>,
    },
    #[serde(rename = "bots.avatar.read")]
    BotsAvatarRead {
        bot_id: String,
    },
    #[serde(rename = "bots.soul.get")]
    BotsSoulGet {
        bot_id: String,
    },
    #[serde(rename = "bots.soul.set")]
    BotsSoulSet {
        bot_id: String,
        content: String,
    },
    #[serde(rename = "bots.memory.get")]
    BotsMemoryGet {
        bot_id: String,
    },
    #[serde(rename = "bots.memory.update")]
    BotsMemoryUpdate {
        bot_id: String,
        content: String,
        #[serde(default)]
        allow_deletions: bool,
    },
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedPhoto {
    mime: String,
    data: String,
}

fn chat_page_value(
    snapshot: krill_proto::agent_history::ChatPollSnapshot,
    offset: u64,
) -> io::Result<Value> {
    let identity = crate::agent_history::chat::generation(&snapshot);
    let generation = hex(ring::digest::digest(&ring::digest::SHA256, identity.as_bytes()).as_ref());
    let start = if snapshot.reset { 0 } else { offset };
    // `at`은 postcard 와이어에서 빠지는 필드라 직렬화 뒤 같은 순서의 항목에 붙인다.
    let times: Vec<String> = snapshot.items.iter().map(|item| item.at.clone()).collect();
    let mut value = to_value(snapshot)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("generation".into(), generation.into());
        object.insert("page_start".into(), start.into());
        object.insert("reply_mode".into(), "review_in_term".into());
        if let Some(items) = object.get_mut("items").and_then(Value::as_array_mut) {
            // These are display IDs for an observed [start, next_offset) page. Historical
            // replay must keep that byte end so later appends cannot change the partition.
            for (index, item) in items.iter_mut().enumerate() {
                if let Some(fields) = item.as_object_mut() {
                    fields.insert("id".into(), format!("{start}:{index}").into());
                    if let Some(at) = times.get(index).filter(|at| !at.is_empty()) {
                        fields.insert("at".into(), at.clone().into());
                    }
                }
            }
        }
    }
    Ok(value)
}

fn review_in_term() -> io::Error {
    io::Error::other(
        "This historical request cannot be verified against the current question. Confirm in Term.",
    )
}

const RPC_GENERIC_ERROR: &str = "Request failed. Check the desktop connection and try again.";

fn contains_session_prefix(lower: &str) -> bool {
    let mut rest = lower;
    while let Some((_, after)) = rest.split_once("session ") {
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after.as_bytes().get(digits) == Some(&b':') {
            return true;
        }
        rest = after;
    }
    false
}

fn contains_http_status(lower: &str) -> bool {
    let mut rest = lower;
    while let Some((_, after)) = rest.split_once("http ") {
        if let Some(code) = after.as_bytes().get(..3)
            && code.iter().all(u8::is_ascii_digit)
        {
            return true;
        }
        rest = after;
    }
    false
}

fn sanitize_mobile_rpc_error(err: &io::Error) -> String {
    let raw = err.to_string();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return RPC_GENERIC_ERROR.to_string();
    }
    if trimmed.eq_ignore_ascii_case("invalid mobile authentication or frame")
        || trimmed.contains("Confirm in Term")
    {
        return trimmed.to_string();
    }
    let lower = trimmed.to_ascii_lowercase();
    if err.kind() == io::ErrorKind::TimedOut {
        return "Request timed out. Try again.".to_string();
    }
    if err.kind() == io::ErrorKind::PermissionDenied
        && (lower == "permission denied" || lower.contains("os error"))
    {
        return "Permission denied on desktop.".to_string();
    }
    if matches!(
        err.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::UnexpectedEof
    ) || lower.contains("os error")
        || lower.contains("broken pipe")
        || lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("connection aborted")
        || lower.contains("unexpected control reply")
        || lower.contains("unexpected daemon reply")
        || lower.contains("bad request")
        || lower.contains("entity not found")
        || lower.contains("no such file or directory")
        || lower.contains("usage api")
        || lower.contains("/users/")
        || lower.contains("/home/")
        || lower.contains("/var/")
        || lower.contains("/tmp/")
        || lower.contains("/volumes/")
        || lower.contains(".sock")
        || lower.contains(":\\")
        || trimmed.starts_with("\\\\")
        || trimmed.contains("\\\\")
        || lower.contains("sqlite")
        || lower.contains("rusqlite")
        || lower.contains("database is locked")
        || lower.contains("no such table")
        || lower.contains("constraint failed")
        || lower.contains("sql logic error")
        || lower.contains("vault")
        || lower.contains("master key")
        || lower.contains("secrets.enc")
        || lower.contains("secrets.key")
        || lower.contains("dpapi")
        || lower.contains("keychain")
        || lower.contains("cryptprotect")
        || contains_session_prefix(&lower)
        || contains_http_status(&lower)
    {
        return RPC_GENERIC_ERROR.to_string();
    }
    trimmed.to_string()
}

pub(crate) fn sanitize_tap_reason(error: &io::Error) -> &'static str {
    if error.kind() == io::ErrorKind::NotFound {
        return "Session is no longer active.";
    }
    match error.to_string().trim() {
        "Wait for the running agent to become idle" => "Wait for the running agent to become idle",
        "Wait for an idle Claude Code 2.1.311 or later chat with an empty native composer" => {
            "Wait for an idle Claude Code 2.1.311 or later chat with an empty native composer"
        }
        "Wait for an idle Claude Code chat with an empty native composer" => {
            "Wait for an idle Claude Code chat with an empty native composer"
        }
        "Wait for an idle Codex 0.164.0 or later chat with an empty native composer" => {
            "Wait for an idle Codex 0.164.0 or later chat with an empty native composer"
        }
        _ => "Could not connect to the live agent session.",
    }
}

/// Carries a structured [`krill_proto::bots::BotSessionRefusal`] through `io::Error` so
/// `Backend::request` emits `data.refusal` instead of collapsing it into a generic error
/// (Orca `e1779f39` / `#46892`, `5fba3d89` / `#46862`).
#[derive(Debug)]
struct BotRefusedIoError(krill_proto::bots::BotSessionRefusal);

impl std::fmt::Display for BotRefusedIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.message_en(Some("this computer")))
    }
}

impl std::error::Error for BotRefusedIoError {}

impl Backend {
    pub(super) fn new(registry: Weak<Registry>, dir: Option<&Path>, hello: ServerHello) -> Self {
        Self {
            registry,
            dir: dir.map(Path::to_path_buf),
            hello,
            host: host_metadata(),
            git_cache: Arc::default(),
            git_inflight: Arc::default(),
            git_revision: Arc::default(),
            repo_icons: Arc::default(),
            snapshot_cache: Mutex::default(),
            snapshot_rev: AtomicU64::default(),
            catalog_cache: Mutex::default(),
            settings_lock: Mutex::default(),
            drop_inbox: krill_proto::os::home_dir().map(|home| krill_proto::drop::inbox_dir(&home)),
            drop_lock: Mutex::default(),
        }
    }
    pub(super) fn request(&self, request: &str) -> String {
        let result = (|| {
            if request.len() > 750_000 {
                return Err(invalid());
            }
            let action: Action = serde_json::from_str(request).map_err(|_| invalid())?;
            self.handle(action)
        })();
        let value = match result {
            Ok(data) => json!({"ok":true,"data":data}),
            Err(e) => {
                if let Some(refused) = e
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<BotRefusedIoError>())
                {
                    json!({
                        "ok": false,
                        "error": refused.0.message_en(Some("this computer")),
                        "data": { "refusal": &refused.0 }
                    })
                } else {
                    json!({"ok":false,"error":sanitize_mobile_rpc_error(&e)})
                }
            }
        };
        let out = serde_json::to_string(&value)
            .ok()
            .filter(|text| text.len() < ipc::MAX_FRAME.saturating_sub(32))
            .unwrap_or_else(|| "{\"ok\":false,\"error\":\"response unavailable\"}".into());
        crate::alloc::purge();
        out
    }
    fn handle(&self, action: Action) -> io::Result<Value> {
        match action {
            Action::Snapshot { since, feed } => {
                let (mut value, rev) = self.snapshot_versioned(since.as_deref())?;
                // 응답은 언제나 피드에 반영한다. rev·현재 간격은 feed를 보낸(새) 클라이언트에만 덧붙여
                // 옛 클라이언트의 응답은 그대로 둔다(etag 밖이라 본문 동일성에도 영향 없음).
                if let Some(etag) = value.get("etag").and_then(Value::as_str).map(str::to_owned)
                    && let Some(armed) = feed::observe(feed, &etag, rev)
                    && feed.is_some()
                    && let Some(object) = value.as_object_mut()
                {
                    object.insert("rev".into(), rev.into());
                    object.insert("feed".into(), armed.into());
                }
                Ok(value)
            }
            Action::SaveQuickCommands { commands } => self.save_quick_commands(commands),
            Action::CreateTerminal {
                cwd_session,
                workspace,
                agent,
            } => {
                let program = match agent.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    None => Program::Login,
                    Some(id) if crate::agent::control::valid_session_id(id) => {
                        Program::Agent { id: id.to_owned() }
                    }
                    Some(_) => return Err(invalid()),
                };
                let cwd = match (cwd_session, workspace) {
                    (Some(session), None) => self.cwd(session)?,
                    (None, Some(key)) => {
                        let repositories = self
                            .registry
                            .upgrade()
                            .ok_or_else(invalid)?
                            .mobile_repositories()?;
                        self.catalog(&repositories).cwd(&key).ok_or_else(invalid)?
                    }
                    _ => return Err(invalid()),
                };
                match self.control(&Request::Create(Create {
                    program,
                    cwd: cwd.into_os_string().into_vec(),
                    cols: 80,
                    rows: 24,
                    account: None,
                }))? {
                    Reply::Created { session, .. } => {
                        self.prewarm_composer(session.0);
                        Ok(json!({"session":session.0}))
                    }
                    Reply::Failed(error) => Err(io::Error::other(error.to_string())),
                    _ => Err(invalid()),
                }
            }
            Action::CreateWorkspace {
                repository,
                name,
                base,
                agent,
                setup_hash,
                skip_setup,
            } => {
                let program = match agent.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    None => Program::Login,
                    Some(id) if crate::agent::control::valid_session_id(id) => {
                        Program::Agent { id: id.to_owned() }
                    }
                    Some(_) => return Err(invalid()),
                };
                let name = name.trim();
                if name.is_empty()
                    || name.len() > 256
                    || name.chars().any(char::is_control)
                    || base
                        .as_ref()
                        .is_some_and(|b| b.len() > 256 || b.chars().any(char::is_control))
                    || skip_setup && setup_hash.is_some()
                {
                    return Err(invalid());
                }
                let repositories = self
                    .registry
                    .upgrade()
                    .ok_or_else(invalid)?
                    .mobile_repositories()?;
                let repo = self
                    .catalog(&repositories)
                    .repository(&repository)
                    .ok_or_else(invalid)?;
                let setup = if skip_setup {
                    control::Setup::Skip
                } else if let Some(hash) = setup_hash {
                    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(invalid());
                    }
                    let bytes = hash
                        .as_bytes()
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| {
                            std::str::from_utf8(pair)
                                .map_err(|_| invalid())
                                .and_then(|s| u8::from_str_radix(s, 16).map_err(|_| invalid()))
                        })
                        .collect::<io::Result<Vec<_>>>()?;
                    control::Setup::Approve(bytes)
                } else {
                    control::Setup::Ask
                };
                match self.control(&Request::NewWorktree(control::NewWorktree {
                    repo,
                    name: name.to_owned(),
                    base: base.filter(|b| !b.trim().is_empty()),
                    setup,
                }))? {
                    Reply::SetupUntrusted { script, hash } => {
                        Ok(json!({"needs_setup":true,"script":script,"hash":hex(&hash)}))
                    }
                    Reply::WorktreeCreated(mut tree) => {
                        let session = if let Ok(Reply::Created { session, .. }) =
                            self.control(&Request::Create(Create {
                                program,
                                cwd: tree.path.clone(),
                                cols: 80,
                                rows: 24,
                                account: None,
                            })) {
                            if let Some(dir) = self.dir.as_deref()
                                && !krill_proto::git::is_default_branch_or_worktree_name(name)
                            {
                                krill_proto::git::record_session_auto_title(dir, session, name)
                                    .ok();
                            }
                            self.prewarm_composer(session.0);
                            Some(session.0)
                        } else {
                            tree.warnings
                                .push("Workspace created; terminal unavailable".to_string());
                            None
                        };
                        let path = PathBuf::from(std::ffi::OsStr::from_bytes(&tree.path));
                        let path = fs::canonicalize(&path).unwrap_or(path);
                        Ok(json!({"workspace":hex(path.as_os_str().as_bytes()),
                            "path":path.to_string_lossy(),"branch":tree.branch,"session":session,
                            "setup_session":tree.setup.map(|s| s.0),"warnings":tree.warnings}))
                    }
                    Reply::WorktreeFailed(error) => Err(io::Error::other(error)),
                    Reply::Failed(error) => Err(io::Error::other(error.to_string())),
                    _ => Err(invalid()),
                }
            }
            Action::CloseTerminal { session } => {
                match self.control(&Request::Close {
                    session: SessionId(session),
                })? {
                    Reply::Closed => Ok(json!({"closed":true})),
                    Reply::Failed(error) => Err(io::Error::other(error.to_string())),
                    _ => Err(invalid()),
                }
            }
            Action::Prompt {
                session,
                text,
                photo,
                photos,
                mode,
            } => {
                if text.len() >= krill_proto::input::MAX_INPUT {
                    return Err(invalid());
                }
                let all_photos: Vec<PreparedPhoto> = photo.into_iter().chain(photos).collect();
                if all_photos.len() > 4 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Maximum 4 photos per message.",
                    ));
                }
                if let Some(reg) = self.registry.upgrade()
                    && let Some(status) = reg.mobile_session_status(SessionId(session))
                    && matches!(
                        status.state,
                        control::AgentState::Waiting | control::AgentState::Blocked
                    )
                {
                    let has_live_prompt =
                        reg.session_capture(SessionId(session))
                            .is_some_and(|screen| {
                                krill_proto::screen::prompt::detect(&screen.rows).is_some()
                            });
                    if mode == PromptMode::Steer || has_live_prompt {
                        return Err(io::Error::other(
                            "Wait for the active prompt before sending a new message",
                        ));
                    }
                }
                // Prepare images before sending anything, then submit image paths and text in order.
                let mut saved_paths = Vec::with_capacity(all_photos.len());
                for item in all_photos {
                    match save_photo(&item.mime, &item.data) {
                        Ok(path) => saved_paths.push(path),
                        Err(err) => {
                            for saved in saved_paths {
                                fs::remove_file(saved).ok();
                            }
                            return Err(err);
                        }
                    }
                }
                let combined = if saved_paths.is_empty() {
                    text
                } else {
                    let quoted: Vec<String> = saved_paths
                        .iter()
                        .map(|path| krill_proto::agent::hooks::shell_quote(&path.to_string_lossy()))
                        .collect();
                    format!("{} {text}", quoted.join(" "))
                };
                let agy_steer_baseline = if mode == PromptMode::Steer {
                    self.registry.upgrade().and_then(|reg| {
                        let is_agy = reg
                            .mobile_session_status(SessionId(session))
                            .is_some_and(|s| krill_proto::agent::canonical_id(&s.agent) == "agy");
                        if !is_agy {
                            return None;
                        }
                        let baseline =
                            reg.session_capture(SessionId(session)).map_or(0, |screen| {
                                krill_proto::screen::agy::count_queued_matches(&screen, &combined)
                                    .count
                            });
                        Some(baseline)
                    })
                } else {
                    None
                };
                let result = self.send_prompt(session, &combined);
                if result.is_err() {
                    for path in saved_paths {
                        fs::remove_file(path).ok();
                    }
                }
                result?;
                if let Some(baseline) = agy_steer_baseline {
                    self.promote_agy_steer_if_queued(session, &combined, baseline);
                }
                if let Some(reg) = self.registry.upgrade()
                    && let Some(bots) = reg.bots_if_initialized()
                {
                    bots.on_owner_prompt_submitted(SessionId(session), &combined);
                }
                Ok(json!({"sent":true}))
            }
            Action::Interrupt { session } => {
                let registry = self.registry.upgrade().ok_or_else(invalid)?;
                let interrupted = registry.mobile_interrupt(SessionId(session))?;
                Ok(json!({"interrupted": interrupted}))
            }
            Action::Approve { .. } => {
                // Transcript IDs and visible text cannot atomically prove the current PTY question.
                // Fail closed, including requests from older clients without a card identity.
                Err(review_in_term())
            }
            Action::Photo {
                session,
                mime,
                data,
                prompt,
            } => {
                let path = save_photo(&mime, &data)?;
                let quoted = krill_proto::agent::hooks::shell_quote(&path.to_string_lossy());
                let trimmed_prompt = prompt.as_deref().map(str::trim).filter(|s| !s.is_empty());
                let combined = match trimmed_prompt {
                    Some(p) => {
                        if p.len() >= krill_proto::input::MAX_INPUT {
                            fs::remove_file(&path).ok();
                            return Err(invalid());
                        }
                        format!("{quoted} {p}")
                    }
                    None => format!("{quoted} "),
                };
                let bracketed =
                    self.session_bracketed_paste(session) || combined.contains(['\n', '\r']);
                let payload = if bracketed {
                    format_bracketed_paste(&combined)
                } else {
                    sanitize_plain_input(&combined)
                };
                if payload.len() > krill_proto::input::MAX_INPUT {
                    fs::remove_file(&path).ok();
                    return Err(invalid());
                }
                let send_result = if trimmed_prompt.is_some() {
                    self.input_paced(
                        session,
                        &[payload, b"\r".to_vec()],
                        Duration::from_millis(60),
                    )
                } else {
                    self.input(session, payload)
                };
                if let Err(error) = send_result {
                    fs::remove_file(&path).ok();
                    return Err(error);
                }
                Ok(json!({"attached":true,"path":path.to_string_lossy()}))
            }
            Action::ChatPoll {
                session,
                offset,
                latest,
                end_offset,
            } => {
                let registry = self.registry.upgrade().ok_or_else(invalid)?;
                let snapshot = registry.chat_poll(SessionId(session), offset);
                if latest {
                    let (snapshot, start) =
                        crate::agent_history::chat::latest_page(snapshot, end_offset)?;
                    let mut value = chat_page_value(snapshot, start)?;
                    if let Some(object) = value.as_object_mut() {
                        object.insert("has_earlier".into(), (start > 0).into());
                    }
                    return Ok(value);
                }
                let snapshot = if let Some(end) = end_offset {
                    crate::agent_history::chat::replay_page(snapshot, offset, end)?
                } else {
                    snapshot
                };
                chat_page_value(snapshot, offset)
            }
            Action::AnswerQuestion { .. } => Err(review_in_term()),
            Action::Accounts => to_value(self.accounts(&UsageRequest::ListAccounts)?),
            Action::SwitchAccount {
                provider,
                account_id,
            } => {
                let provider = ProviderKind::parse(&provider).ok_or_else(invalid)?;
                to_value(self.accounts(&UsageRequest::SwitchAccount {
                    provider,
                    account_id,
                })?)
            }
            Action::Orchestration => self.orchestration(),
            Action::GateReply { id, decision } => {
                let dir = self.dir.as_deref().ok_or_else(invalid)?;
                let out = to_value(OrchestrationStore::mutate(dir, |store| {
                    if !store
                        .gates
                        .iter()
                        .any(|g| g.id == id && requests::gate_pending(store, g))
                    {
                        return Err(io::Error::other("This request is no longer pending."));
                    }
                    store.resolve_gate(&id, &decision, None, "mobile")
                })?)?;
                if let Some(reg) = self.registry.upgrade() {
                    reg.notify_orch();
                }
                *crate::session::lock(&self.snapshot_cache) = None;
                Ok(out)
            }
            Action::QuestionReply { id, body } => {
                if body.len() > 16 * 1024 {
                    return Err(invalid());
                }
                let dir = self.dir.as_deref().ok_or_else(invalid)?;
                let out = to_value(OrchestrationStore::mutate(dir, |store| {
                    if body.trim().is_empty() {
                        return Err(invalid());
                    }
                    if !store
                        .messages
                        .iter()
                        .any(|m| m.id == id && requests::question_pending(store, m))
                    {
                        return Err(io::Error::other("This request is no longer pending."));
                    }
                    store.reply_message(&id, "mobile", &body)
                })?)?;
                if let Some(reg) = self.registry.upgrade() {
                    reg.notify_orch();
                }
                *crate::session::lock(&self.snapshot_cache) = None;
                Ok(out)
            }
            Action::Handoff { session, agent } => {
                let agent = krill_proto::agent::canonical_id(&agent);
                if !["codex", "claude", "agy"].contains(&agent) {
                    return Err(invalid());
                }
                let cwd = self.cwd(session)?;
                let binary = std::env::current_exe()?
                    .parent()
                    .ok_or_else(invalid)?
                    .join("krill");
                let mut command = krill_proto::os::background_command(binary);
                command
                    .args([
                        "handoff",
                        "--from",
                        &session.to_string(),
                        "--to-agent",
                        agent,
                        "--lines",
                        "3",
                        "--json",
                    ])
                    .current_dir(cwd)
                    .stdin(Stdio::null());
                if let Some(dir) = &self.dir {
                    command.env("KRILL_STATE_DIR", dir);
                }
                let output = bounded_command(&mut command, Duration::from_secs(15), 128 * 1024)?;
                if output.is_empty() {
                    return Err(io::Error::other(
                        "agent handoff failed; check the desktop agent configuration",
                    ));
                }
                serde_json::from_slice(&output).map_err(io::Error::other)
            }
            Action::Git { session, path } => self.git(session, path.as_deref()),
            Action::CommandCatalog { session } => {
                let context = self.composer_context(session)?;
                Ok(command_catalog::load(
                    &context.home,
                    &context.cwd,
                    &context.agent,
                    context.config.as_deref(),
                ))
            }
            Action::MentionFiles { session, query } => {
                mention_files::load(&self.cwd(session)?, &query)
            }
            Action::LivePromptReply {
                session,
                expected_prompt,
                yes,
                choice,
                multi_selected,
                custom_text,
            } => self.live_prompt_reply(
                session,
                &expected_prompt,
                yes,
                choice,
                multi_selected,
                custom_text,
            ),
            Action::ChatComposer { session } => self.chat_composer_status(session),
            Action::ComposerModels { session } => self.composer_models(session),
            Action::SetModel {
                session,
                model,
                effort,
            } => {
                composer_status::invalidate_all();
                self.set_composer_model(session, &model, effort.as_deref())
            }
            Action::SetEffort { session, effort } => {
                composer_status::invalidate_all();
                self.set_composer_effort(session, &effort)
            }
            Action::DropPut {
                upload,
                name,
                size,
                offset,
                data,
            } => {
                let (state, inbox) = self.drop_dirs()?;
                let _guard = crate::session::lock(&self.drop_lock);
                drops::put(
                    state,
                    inbox,
                    &drops::Chunk {
                        upload: &upload,
                        name: &name,
                        size,
                        offset,
                        data: &data,
                    },
                )
            }
            Action::DropList => {
                let (state, inbox) = self.drop_dirs()?;
                let _guard = crate::session::lock(&self.drop_lock);
                drops::list(state, inbox)
            }
            Action::DropGet { id, offset } => drops::get(self.drop_dirs()?.0, &id, offset),
            Action::DropRemove { id } => {
                let (state, _) = self.drop_dirs()?;
                let _guard = crate::session::lock(&self.drop_lock);
                drops::remove(state, &id)
            }
            Action::BotsList { limit, cursor } => {
                let (offset, limit) = parse_bot_page(limit, cursor.as_deref())?;
                if self.bots_inactive()? {
                    return bound_bot_list_payload(&[], "bots", 0, limit, 0, None, &[]);
                }
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::ListBots)? {
                    krill_proto::bots::BotRpcResponse::Bots(bots) => {
                        let total = bots.len();
                        let values = bots
                            .into_iter()
                            .map(serde_json::to_value)
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|_| invalid())?;
                        bound_bot_list_payload(&values, "bots", offset, limit, total, None, &[])
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsTemplates {} => {
                let templates: Vec<Value> = krill_proto::bots::BUILTIN_BOT_TEMPLATES
                    .iter()
                    .map(|tpl| {
                        json!({
                            "id": tpl.id,
                            "category": tpl.category,
                            "name": tpl.name,
                            "description": tpl.description,
                            "mascot": tpl.mascot,
                            "needs_setup": tpl.needs_setup,
                        })
                    })
                    .collect();
                Ok(json!({"templates": templates}))
            }
            Action::BotsModels { agent_id } => {
                let agent_id = validate_bot_token(&agent_id, 64)?;
                let canon = krill_proto::agent::canonical_id(&agent_id).to_owned();
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::ListAgentModels {
                    agent_id: canon,
                })? {
                    krill_proto::bots::BotRpcResponse::AgentModels(found) => Ok(to_value(found)?),
                    _ => Err(invalid()),
                }
            }
            Action::BotsUpcoming { bot_id } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::GetBotUpcoming { bot_id })? {
                    krill_proto::bots::BotRpcResponse::Upcoming(up) => Ok(to_value(up)?),
                    _ => Err(invalid()),
                }
            }
            Action::BotsSessionsList { bot_id } => {
                let bot_id = parse_opt_bot_id(bot_id.as_deref())?;
                if self.bots_inactive()? {
                    return Ok(json!({"sessions": []}));
                }
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::ListBotSessions { bot_id })? {
                    krill_proto::bots::BotRpcResponse::BotSessions(sessions) => {
                        Ok(json!({"sessions": to_value(sessions)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsSessionStop { bot_id, session_id } => {
                self.ensure_bots_active()?;
                if session_id == 0 {
                    return Err(invalid());
                }
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::StopBotSession {
                    bot_id,
                    session_id: SessionId(session_id),
                })? {
                    krill_proto::bots::BotRpcResponse::Ok => Ok(json!({"stopped": true})),
                    _ => Err(invalid()),
                }
            }
            Action::BotsScheduleRun {
                bot_id,
                schedule_id,
            } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let schedule_id = validate_bot_token(&schedule_id, 128)?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::RunScheduleNow {
                    bot_id,
                    schedule_id,
                })? {
                    krill_proto::bots::BotRpcResponse::Ok => Ok(json!({"started": true})),
                    _ => Err(invalid()),
                }
            }
            Action::BotsScheduleToggle {
                bot_id,
                schedule_id,
                enabled,
            } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let schedule_id = validate_bot_token(&schedule_id, 128)?;
                let mut bot = match self.bot_rpc(krill_proto::bots::BotRpcRequest::GetBot {
                    bot_id: bot_id.clone(),
                })? {
                    krill_proto::bots::BotRpcResponse::Bot(bot) => *bot,
                    _ => return Err(invalid()),
                };
                let mut found = false;
                for sched in &mut bot.schedules {
                    if sched.id == schedule_id {
                        sched.enabled = enabled;
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err(io::Error::other(format!(
                        "schedule not found: {schedule_id}"
                    )));
                }
                let params = krill_proto::bots::UpdateBotParams {
                    bot_id,
                    name: None,
                    description: None,
                    emoji: krill_proto::bots::FieldPatch::Unchanged,
                    avatar: krill_proto::bots::FieldPatch::Unchanged,
                    agent_id: None,
                    model: krill_proto::bots::FieldPatch::Unchanged,
                    effort: krill_proto::bots::FieldPatch::Unchanged,
                    permission_mode: None,
                    workspace: krill_proto::bots::FieldPatch::Unchanged,
                    paused: None,
                    first_run_completed: None,
                    notifications: None,
                    schedules: Some(bot.schedules),
                    look_around: None,
                    memory_tidy: None,
                    mcp_tools: None,
                };
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::UpdateBot(Box::new(
                    params,
                )))? {
                    krill_proto::bots::BotRpcResponse::Bot(updated) => {
                        Ok(json!({"bot": to_value(*updated)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsUpdate {
                bot_id,
                name,
                description,
                agent_id,
                model,
                clear_model,
                effort,
                clear_effort,
                permission_mode,
                paused,
            } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let name = name
                    .map(|n| krill_proto::bots::validate_bot_name(&n).map_err(|_| invalid()))
                    .transpose()?;
                let description = validate_opt_bot_text(
                    description.as_deref(),
                    krill_proto::bots::BOT_DESCRIPTION_MAX_BYTES,
                    true,
                    true,
                )?;
                let agent_id = validate_opt_bot_token(agent_id.as_deref(), 64)?
                    .map(|a| krill_proto::agent::canonical_id(&a).to_owned());
                let model = if clear_model {
                    krill_proto::bots::FieldPatch::Clear
                } else {
                    match validate_opt_bot_token(model.as_deref(), 128)? {
                        Some(m) => krill_proto::bots::FieldPatch::Set(m),
                        None => krill_proto::bots::FieldPatch::Unchanged,
                    }
                };
                let effort = if clear_effort {
                    krill_proto::bots::FieldPatch::Clear
                } else {
                    match validate_opt_bot_token(effort.as_deref(), 64)? {
                        Some(e) => krill_proto::bots::FieldPatch::Set(e),
                        None => krill_proto::bots::FieldPatch::Unchanged,
                    }
                };
                let params = krill_proto::bots::UpdateBotParams {
                    bot_id,
                    name,
                    description,
                    emoji: krill_proto::bots::FieldPatch::Unchanged,
                    avatar: krill_proto::bots::FieldPatch::Unchanged,
                    agent_id,
                    model,
                    effort,
                    permission_mode,
                    workspace: krill_proto::bots::FieldPatch::Unchanged,
                    paused,
                    first_run_completed: None,
                    notifications: None,
                    schedules: None,
                    look_around: None,
                    memory_tidy: None,
                    mcp_tools: None,
                };
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::UpdateBot(Box::new(
                    params,
                )))? {
                    krill_proto::bots::BotRpcResponse::Bot(bot) => {
                        Ok(json!({"bot": to_value(*bot)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsCreate {
                name,
                agent_id,
                description,
                emoji,
                slug,
                avatar_color,
                model,
                effort,
                permission_mode,
                workspace,
                template_id,
                initial_soul,
                skills,
            } => {
                let name = krill_proto::bots::validate_bot_name(&name).map_err(|_| invalid())?;
                let agent_id = validate_bot_token(&agent_id, 64)?;
                let description = validate_bot_text(
                    &description,
                    krill_proto::bots::BOT_DESCRIPTION_MAX_BYTES,
                    true,
                    true,
                )?;
                let slug = match validate_opt_bot_token(
                    slug.as_deref(),
                    krill_proto::bots::BOT_SLUG_MAX_BYTES,
                )? {
                    Some(s) => Some(krill_proto::bots::BotSlug::new(&s).map_err(|_| invalid())?),
                    None => None,
                };
                let avatar_color = validate_opt_bot_token(avatar_color.as_deref(), 32)?;
                let emoji = validate_opt_bot_token(emoji.as_deref(), 32)?;
                let model = validate_opt_bot_token(model.as_deref(), 128)?;
                let effort = validate_opt_bot_token(effort.as_deref(), 64)?;
                let workspace = validate_opt_bot_token(workspace.as_deref(), 1024)?;
                let template_id = validate_opt_bot_token(template_id.as_deref(), 128)?;
                let initial_soul = validate_opt_bot_text(
                    initial_soul.as_deref(),
                    BOT_TEXT_DOC_MAX_BYTES,
                    true,
                    true,
                )?;
                if skills.len() > 64 {
                    return Err(invalid());
                }
                let mut clean_skills = Vec::with_capacity(skills.len());
                for skill in &skills {
                    clean_skills.push(validate_bot_token(skill, 128)?);
                }
                let params = krill_proto::bots::CreateBotV2Params {
                    base: krill_proto::bots::CreateBotParams {
                        name,
                        agent_id,
                        description,
                        emoji,
                        avatar: None,
                        model,
                        effort,
                        permission_mode,
                        workspace,
                        template_id,
                        initial_soul,
                    },
                    slug,
                    avatar_color,
                    skills: clean_skills,
                    events: Vec::new(),
                };
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::CreateBotV2(Box::new(
                    params,
                )))? {
                    krill_proto::bots::BotRpcResponse::Bot(bot) => {
                        Ok(json!({"bot": to_value(*bot)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsChatOpen {
                bot_id,
                cwd,
                prompt,
            } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let cwd = validate_opt_bot_token(cwd.as_deref(), 4096)?;
                let prompt = validate_opt_bot_text(
                    prompt.as_deref(),
                    krill_proto::bots::BOT_ITEM_DESCRIPTION_MAX_BYTES,
                    true,
                    false,
                )?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::StartBotSession {
                    bot_id,
                    cwd,
                    prompt,
                })? {
                    krill_proto::bots::BotRpcResponse::SessionStarted {
                        bot_id,
                        session_id,
                        pid,
                    } => Ok(json!({
                        "bot_id": bot_id.as_str(),
                        "session_id": session_id.0,
                        "pid": pid,
                    })),
                    _ => Err(invalid()),
                }
            }
            Action::BotsItems {
                bot_id,
                status,
                include_closed,
                include_snoozed,
                limit,
                cursor,
            } => {
                let bot_id = parse_opt_bot_id(bot_id.as_deref())?;
                let status_filter = match status.as_deref().map(str::trim).filter(|s| !s.is_empty())
                {
                    Some(raw) => Some(
                        crate::bots::items::BotItemsStatusFilter::parse(raw).ok_or_else(invalid)?,
                    ),
                    None => None,
                };
                let bounded_limit = match limit {
                    Some(0) => return Err(invalid()),
                    Some(n) => usize::try_from(n)
                        .unwrap_or(BOT_PAGE_MAX_LIMIT)
                        .min(BOT_PAGE_MAX_LIMIT),
                    None => BOT_PAGE_DEFAULT_LIMIT,
                };
                let validated_cursor = validate_opt_bot_token(cursor.as_deref(), 128)?;
                if self.bots_inactive()? {
                    return bound_bot_list_payload(
                        &[],
                        "items",
                        0,
                        bounded_limit,
                        0,
                        None,
                        &[("unseen_count", json!(0))],
                    );
                }
                let req_limit = u32::try_from(bounded_limit).unwrap_or(100);
                let page = if let Some(filter) = status_filter {
                    let registry = self.registry.upgrade().ok_or_else(invalid)?;
                    let mgr = registry.bots().map_err(io::Error::other)?;
                    mgr.items().list_items_page_filtered(
                        bot_id.as_ref(),
                        Some(filter),
                        include_closed,
                        include_snoozed,
                        Some(req_limit),
                        validated_cursor.as_deref(),
                        unix_ms(),
                    )?
                } else {
                    match self.bot_rpc(krill_proto::bots::BotRpcRequest::ListItemsPage {
                        bot_id,
                        include_closed,
                        include_snoozed,
                        limit: Some(req_limit),
                        cursor: validated_cursor.clone(),
                    })? {
                        krill_proto::bots::BotRpcResponse::ItemsPage(page) => page,
                        _ => return Err(invalid()),
                    }
                };
                let total = page.total_count;
                let unseen = page.unseen_count;
                let next = page.next_cursor;
                let values = page
                    .items
                    .into_iter()
                    .map(serde_json::to_value)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| invalid())?;
                let page_len = values.len().max(1);
                bound_bot_list_payload(
                    &values,
                    "items",
                    0,
                    page_len,
                    total,
                    next,
                    &[("unseen_count", json!(unseen))],
                )
            }
            Action::BotsItemCreate {
                bot_id,
                title,
                description,
                done_means,
                status,
                links,
            } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let title = validate_bot_text(
                    title.trim(),
                    krill_proto::bots::BOT_ITEM_TITLE_MAX_BYTES,
                    false,
                    false,
                )?;
                let description = validate_opt_bot_text(
                    description.as_deref(),
                    krill_proto::bots::BOT_ITEM_DESCRIPTION_MAX_BYTES,
                    true,
                    true,
                )?;
                let done_means = validate_opt_bot_text(
                    done_means.as_deref(),
                    krill_proto::bots::BOT_ITEM_DESCRIPTION_MAX_BYTES,
                    true,
                    true,
                )?;
                if links.len() > 32 {
                    return Err(invalid());
                }
                for link in &links {
                    validate_bot_item_link(link)?;
                }
                let params = krill_proto::bots::CreateBotItemParams {
                    bot_id,
                    title,
                    description,
                    done_means,
                    status,
                    links,
                };
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::CreateItem(Box::new(
                    params,
                )))? {
                    krill_proto::bots::BotRpcResponse::Item(item) => {
                        Ok(json!({"item": to_value(*item)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsItemUpdate {
                item_id,
                title,
                description,
                done_means,
                status,
                comment,
                decisions,
                close_note,
                snoozed_until_ms,
            } => {
                self.ensure_bots_active()?;
                let trimmed_id = validate_bot_token(&item_id, 128)?;
                let title = match title.as_deref() {
                    Some(raw) => Some(validate_bot_text(
                        raw.trim(),
                        krill_proto::bots::BOT_ITEM_TITLE_MAX_BYTES,
                        false,
                        false,
                    )?),
                    None => None,
                };
                let description = validate_opt_bot_text(
                    description.as_deref(),
                    krill_proto::bots::BOT_ITEM_DESCRIPTION_MAX_BYTES,
                    true,
                    true,
                )?;
                let done_means = validate_opt_bot_text(
                    done_means.as_deref(),
                    krill_proto::bots::BOT_ITEM_DESCRIPTION_MAX_BYTES,
                    true,
                    true,
                )?;
                let comment = validate_opt_bot_text(
                    comment.as_deref(),
                    krill_proto::bots::BOT_ITEM_COMMENT_MAX_BYTES,
                    true,
                    false,
                )?;
                let close_note = validate_opt_bot_text(
                    close_note.as_deref(),
                    krill_proto::bots::BOT_ITEM_DESCRIPTION_MAX_BYTES,
                    true,
                    true,
                )?;
                validate_bot_decisions(&decisions)?;
                let params = krill_proto::bots::UpdateBotItemParams {
                    item_id: trimmed_id,
                    title,
                    description,
                    done_means,
                    status,
                    comment,
                    decisions,
                    close_note,
                    snoozed_until_ms,
                };
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::UpdateItem(Box::new(
                    params,
                )))? {
                    krill_proto::bots::BotRpcResponse::Item(item) => {
                        Ok(json!({"item": to_value(*item)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsItemAnswer {
                item_id,
                answer_text,
                option_index,
                decision_id,
                request_id,
            } => {
                self.ensure_bots_active()?;
                let item_id = validate_bot_token(&item_id, 128)?;
                let answer_text = validate_opt_bot_text(
                    answer_text.as_deref(),
                    krill_proto::bots::BOT_ITEM_COMMENT_MAX_BYTES,
                    true,
                    false,
                )?;
                if answer_text.is_none() && option_index.is_none() {
                    return Err(invalid());
                }
                let decision_id = validate_opt_bot_token(decision_id.as_deref(), 128)?;
                let request_id = validate_opt_bot_token(request_id.as_deref(), 128)?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::AnswerBotItem {
                    item_id,
                    answer_text,
                    option_index,
                    decision_id,
                    request_id,
                })? {
                    krill_proto::bots::BotRpcResponse::Item(item) => {
                        Ok(json!({"item": to_value(*item)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsItemLink { item_id, link } => {
                self.ensure_bots_active()?;
                let item_id = validate_bot_token(&item_id, 128)?;
                validate_bot_item_link(&link)?;
                match self
                    .bot_rpc(krill_proto::bots::BotRpcRequest::AddItemLink { item_id, link })?
                {
                    krill_proto::bots::BotRpcResponse::Item(item) => {
                        Ok(json!({"item": to_value(*item)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsAccessList {
                bot_id,
                limit,
                cursor,
            } => {
                let bot_id = parse_opt_bot_id(bot_id.as_deref())?;
                let (offset, limit) = parse_bot_page(limit, cursor.as_deref())?;
                if self.bots_inactive()? {
                    return bound_bot_list_payload(&[], "requests", 0, limit, 0, None, &[]);
                }
                match self
                    .bot_rpc(krill_proto::bots::BotRpcRequest::ListAccessRequests { bot_id })?
                {
                    krill_proto::bots::BotRpcResponse::AccessRequests(requests) => {
                        let total = requests.len();
                        let values = requests
                            .into_iter()
                            .map(serde_json::to_value)
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|_| invalid())?;
                        bound_bot_list_payload(&values, "requests", offset, limit, total, None, &[])
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsAccessDecide { token, approve } => {
                self.ensure_bots_active()?;
                let trimmed = validate_bot_token(&token, 128)?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::DecideAccessRequest {
                    token: trimmed,
                    approve,
                })? {
                    krill_proto::bots::BotRpcResponse::AccessRequestDecided(request) => {
                        Ok(json!({"request": to_value(request)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsAccessRedeem { code } => {
                self.ensure_bots_active()?;
                let code = validate_bot_token(&code, 128)?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::RedeemPairingCode { code })? {
                    krill_proto::bots::BotRpcResponse::AccessRequestDecided(request) => {
                        Ok(json!({"request": to_value(request)?}))
                    }
                    krill_proto::bots::BotRpcResponse::Bot(bot) => {
                        Ok(json!({"bot": to_value(*bot)?}))
                    }
                    krill_proto::bots::BotRpcResponse::Ok => Ok(json!({"redeemed": true})),
                    _ => Err(invalid()),
                }
            }
            Action::BotsReply {
                bot_id,
                link_id,
                text,
                reaction,
            } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let link_id = validate_opt_bot_token(link_id.as_deref(), 256)?;
                let reaction = validate_opt_bot_token(reaction.as_deref(), 64)?;
                let text = validate_bot_text(&text, 32 * 1024, true, true)?;
                if text.trim().is_empty() && reaction.is_none() {
                    return Err(invalid());
                }
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::PostBotReply {
                    bot_id,
                    link_id,
                    text,
                    files: Vec::new(),
                    reaction,
                })? {
                    krill_proto::bots::BotRpcResponse::ReplyPosted { external_ref } => {
                        let delivered = external_ref
                            .as_deref()
                            .map(str::trim)
                            .is_some_and(|s| !s.is_empty());
                        if !delivered {
                            return Err(io::Error::new(
                                io::ErrorKind::NotFound,
                                "no external conversation link available for reply",
                            ));
                        }
                        Ok(json!({"posted": true, "external_ref": external_ref}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsConnectorsStatus { bot_id } => {
                let bot_id = parse_opt_bot_id(bot_id.as_deref())?;
                if self.bots_inactive()? {
                    return Ok(json!({"statuses": []}));
                }
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::ConnectorStatus { bot_id })? {
                    krill_proto::bots::BotRpcResponse::ConnectorStatuses(statuses) => {
                        Ok(json!({"statuses": to_value(statuses)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::ChatSubscribe { session, offset } => {
                let registry = self.registry.upgrade().ok_or_else(invalid)?;
                let snapshot = registry.chat_poll(SessionId(session), offset);
                let watch_path = (!snapshot.transcript_path.is_empty())
                    .then(|| PathBuf::from(&snapshot.transcript_path));
                let push = feed::subscribe_chat(session, offset, watch_path).unwrap_or(false);
                Ok(json!({
                    "subscribed": true,
                    "session": session,
                    "offset": offset,
                    "push": push,
                }))
            }
            Action::ChatUnsubscribe { session } => {
                feed::unsubscribe_chat(session);
                Ok(json!({"unsubscribed": true}))
            }
            Action::BotsGroupsList { limit, cursor } => {
                let (offset, limit) = parse_bot_page(limit, cursor.as_deref())?;
                if self.bots_inactive()? {
                    return bound_bot_list_payload(&[], "groups", 0, limit, 0, None, &[]);
                }
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::ListGroups)? {
                    krill_proto::bots::BotRpcResponse::Groups(groups) => {
                        let total = groups.len();
                        let values = groups
                            .into_iter()
                            .map(serde_json::to_value)
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|_| invalid())?;
                        bound_bot_list_payload(&values, "groups", offset, limit, total, None, &[])
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsGroupsCreate {
                name,
                bot_ids,
                description,
            } => {
                self.ensure_bots_active()?;
                let name = validate_bot_text(
                    name.trim(),
                    krill_proto::bots::BOT_NAME_MAX_BYTES,
                    false,
                    false,
                )?;
                let description = validate_bot_text(
                    &description,
                    krill_proto::bots::BOT_DESCRIPTION_MAX_BYTES,
                    true,
                    true,
                )?;
                if bot_ids.is_empty() || bot_ids.len() > 32 {
                    return Err(invalid());
                }
                let mut ids = Vec::with_capacity(bot_ids.len());
                for id in &bot_ids {
                    ids.push(krill_proto::bots::BotId::new(id).map_err(|_| invalid())?);
                }
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::CreateGroup {
                    name,
                    bot_ids: ids,
                    description,
                })? {
                    krill_proto::bots::BotRpcResponse::Group(group) => {
                        Ok(json!({"group": to_value(group)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsGroupsRead {
                group_id,
                thread_id,
                limit,
            } => {
                let group_id = validate_bot_token(&group_id, 128)?;
                let thread_id = validate_opt_bot_token(thread_id.as_deref(), 128)?;
                let limit = match limit {
                    Some(0) => return Err(invalid()),
                    Some(n) => Some(n.min(200)),
                    None => None,
                };
                self.ensure_bots_active()?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::ReadGroup {
                    group_id,
                    thread_id,
                    limit,
                })? {
                    krill_proto::bots::BotRpcResponse::GroupView(view) => {
                        bound_group_view_payload(json!({"group": to_value(view)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsGroupsSend {
                group_id,
                thread_id,
                sender,
                text,
                reply_mode,
            } => {
                self.ensure_bots_active()?;
                let group_id = validate_bot_token(&group_id, 128)?;
                let thread_id = validate_opt_bot_token(thread_id.as_deref(), 128)?;
                let sender = validate_opt_bot_token(sender.as_deref(), 64)?
                    .unwrap_or_else(|| "owner".to_owned());
                let text = validate_bot_text(
                    &text,
                    krill_proto::bots::BOT_ITEM_DESCRIPTION_MAX_BYTES,
                    true,
                    false,
                )?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::SendGroupMessage {
                    group_id,
                    thread_id,
                    sender,
                    text,
                    reply_mode,
                })? {
                    krill_proto::bots::BotRpcResponse::GroupMessagePosted {
                        message,
                        dispatched_bots,
                    } => Ok(json!({
                        "message": to_value(message)?,
                        "dispatched_bots": to_value(dispatched_bots)?,
                    })),
                    _ => Err(invalid()),
                }
            }
            Action::BotsGroupsNewThread { group_id, title } => {
                self.ensure_bots_active()?;
                let group_id = validate_bot_token(&group_id, 128)?;
                let title = validate_opt_bot_text(
                    title.as_deref().map(str::trim),
                    krill_proto::bots::BOT_ITEM_TITLE_MAX_BYTES,
                    false,
                    false,
                )?;
                match self
                    .bot_rpc(krill_proto::bots::BotRpcRequest::NewGroupThread { group_id, title })?
                {
                    krill_proto::bots::BotRpcResponse::GroupView(view) => {
                        bound_group_view_payload(json!({"group": to_value(view)?}))
                    }
                    krill_proto::bots::BotRpcResponse::Ok => Ok(json!({"created": true})),
                    _ => Err(invalid()),
                }
            }
            Action::BotsGroupsStop {
                group_id,
                thread_id,
            } => {
                self.ensure_bots_active()?;
                let group_id = validate_bot_token(&group_id, 128)?;
                let thread_id = validate_opt_bot_token(thread_id.as_deref(), 128)?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::StopGroup {
                    group_id,
                    thread_id,
                })? {
                    krill_proto::bots::BotRpcResponse::Ok => Ok(json!({"stopped": true})),
                    krill_proto::bots::BotRpcResponse::GroupView(view) => {
                        bound_group_view_payload(json!({"stopped": true, "group": to_value(view)?}))
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsLookAround { bot_id } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::RunLookAround { bot_id })? {
                    krill_proto::bots::BotRpcResponse::LookAroundResult {
                        bot_id,
                        suggestions,
                    } => {
                        let total = suggestions.len();
                        let values = suggestions
                            .into_iter()
                            .map(serde_json::to_value)
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|_| invalid())?;
                        bound_bot_list_payload(
                            &values,
                            "suggestions",
                            0,
                            BOT_PAGE_MAX_LIMIT,
                            total,
                            None,
                            &[("bot_id", json!(bot_id.as_str()))],
                        )
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsSuggestionsList {
                bot_id,
                include_dismissed,
                limit,
                cursor,
            } => {
                let bot_id = parse_opt_bot_id(bot_id.as_deref())?;
                let (offset, limit) = parse_bot_page(limit, cursor.as_deref())?;
                if self.bots_inactive()? {
                    return bound_bot_list_payload(&[], "suggestions", 0, limit, 0, None, &[]);
                }
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::ListSuggestions {
                    bot_id,
                    include_dismissed,
                })? {
                    krill_proto::bots::BotRpcResponse::Suggestions(suggestions) => {
                        let total = suggestions.len();
                        let values = suggestions
                            .into_iter()
                            .map(serde_json::to_value)
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|_| invalid())?;
                        bound_bot_list_payload(
                            &values,
                            "suggestions",
                            offset,
                            limit,
                            total,
                            None,
                            &[],
                        )
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsSuggestionsDismiss {
                bot_id,
                suggestion_id,
            } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let suggestion_id = validate_bot_token(&suggestion_id, 128)?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::DismissSuggestion {
                    bot_id,
                    suggestion_id,
                })? {
                    krill_proto::bots::BotRpcResponse::Ok => Ok(json!({"dismissed": true})),
                    _ => Err(invalid()),
                }
            }
            Action::BotsSuggestionsStart {
                bot_id,
                suggestion_id,
            } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let suggestion_id = validate_bot_token(&suggestion_id, 128)?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::StartSuggestion {
                    bot_id,
                    suggestion_id,
                })? {
                    krill_proto::bots::BotRpcResponse::SessionStarted {
                        bot_id,
                        session_id,
                        pid,
                    } => Ok(json!({
                        "bot_id": bot_id.as_str(),
                        "session_id": session_id.0,
                        "pid": pid,
                    })),
                    krill_proto::bots::BotRpcResponse::Item(item) => {
                        Ok(json!({"item": to_value(*item)?}))
                    }
                    krill_proto::bots::BotRpcResponse::Ok => Ok(json!({"started": true})),
                    _ => Err(invalid()),
                }
            }
            Action::RepoIconsRead { keys } => {
                let registry = self.registry.upgrade().ok_or_else(invalid)?;
                let dashboard = registry.mobile_dashboard_inputs_swr()?;
                let catalog = self.catalog(&dashboard.repositories);
                self.repo_icons
                    .read_batch(&catalog, &keys, self.dir.as_deref())
            }
            Action::BotsAvatarRead { bot_id } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let id_str = bot_id.as_str().to_owned();
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::GetBotAvatar {
                    bot_id: bot_id.clone(),
                }) {
                    Ok(krill_proto::bots::BotRpcResponse::BotAvatar(avatar)) => {
                        if avatar.png_bytes.is_empty() {
                            return Ok(json!({
                                "bot_id": avatar.bot_id.as_str(),
                                "mime": null,
                                "data_base64": null,
                            }));
                        }
                        let encoded = super::ws::base64(&avatar.png_bytes, false);
                        let data = json!({
                            "bot_id": avatar.bot_id.as_str(),
                            "mime": avatar.mime,
                            "data_base64": encoded,
                        });
                        if wrapped_ok_len(&data)
                            >= ipc::MAX_FRAME.saturating_sub(MOBILE_FRAME_HEADROOM)
                        {
                            return Err(io::Error::other("avatar exceeds mobile frame limit"));
                        }
                        Ok(data)
                    }
                    Ok(_) => Err(invalid()),
                    Err(err) => {
                        let lower = err.to_string().to_ascii_lowercase();
                        if !lower.contains("bot not found")
                            && (lower.contains("no such file")
                                || lower.contains("cannot find the file")
                                || lower.contains("os error 2"))
                            && matches!(
                                self.bot_rpc(krill_proto::bots::BotRpcRequest::GetBot { bot_id }),
                                Ok(krill_proto::bots::BotRpcResponse::Bot(_))
                            )
                        {
                            return Ok(json!({
                                "bot_id": id_str,
                                "mime": null,
                                "data_base64": null,
                            }));
                        }
                        Err(err)
                    }
                }
            }
            Action::BotsSoulGet { bot_id } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let id_str = bot_id.as_str().to_owned();
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::GetSoul { bot_id })? {
                    krill_proto::bots::BotRpcResponse::Text(content) => {
                        bound_bot_doc_payload(&id_str, content)
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsSoulSet { bot_id, content } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let content = validate_bot_text(&content, BOT_TEXT_DOC_MAX_BYTES, true, true)?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::SetSoul { bot_id, content })? {
                    krill_proto::bots::BotRpcResponse::Ok => Ok(json!({"saved": true})),
                    _ => Err(invalid()),
                }
            }
            Action::BotsMemoryGet { bot_id } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let id_str = bot_id.as_str().to_owned();
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::GetMemory { bot_id })? {
                    krill_proto::bots::BotRpcResponse::Text(content) => {
                        bound_bot_doc_payload(&id_str, content)
                    }
                    _ => Err(invalid()),
                }
            }
            Action::BotsMemoryUpdate {
                bot_id,
                content,
                allow_deletions,
            } => {
                self.ensure_bots_active()?;
                let bot_id = krill_proto::bots::BotId::new(&bot_id).map_err(|_| invalid())?;
                let content = validate_bot_text(&content, BOT_TEXT_DOC_MAX_BYTES, true, true)?;
                match self.bot_rpc(krill_proto::bots::BotRpcRequest::UpdateMemory {
                    bot_id,
                    content,
                    allow_deletions,
                })? {
                    krill_proto::bots::BotRpcResponse::MemoryUpdated {
                        content,
                        restored_facts,
                        removed_count,
                    } => Ok(json!({
                        "content": content,
                        "restored_facts": restored_facts,
                        "removed_count": removed_count,
                    })),
                    _ => Err(invalid()),
                }
            }
        }
    }
    fn ensure_bots_active(&self) -> io::Result<()> {
        if self.bots_inactive()? {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no bots configured",
            ));
        }
        Ok(())
    }
    fn snapshot_bots_summary(&self) -> io::Result<Value> {
        if self.bots_inactive()? {
            return Ok(json!({
                "count": 0,
                "needs_you": 0,
                "pending_access": 0,
            }));
        }
        let count = match self.bot_rpc(krill_proto::bots::BotRpcRequest::ListBots)? {
            krill_proto::bots::BotRpcResponse::Bots(bots) => bots.len(),
            _ => return Err(invalid()),
        };
        let needs_you = match self.bot_rpc(krill_proto::bots::BotRpcRequest::ListItems {
            bot_id: None,
            include_closed: false,
        })? {
            krill_proto::bots::BotRpcResponse::Items(items) => {
                let now_ms = unix_ms();
                items
                    .iter()
                    .filter(|item| {
                        item.status == krill_proto::bots::BotItemStatus::NeedsYou
                            && item.snoozed_until_ms.is_none_or(|until| until <= now_ms)
                    })
                    .count()
            }
            _ => return Err(invalid()),
        };
        let pending_access = match self
            .bot_rpc(krill_proto::bots::BotRpcRequest::ListAccessRequests { bot_id: None })?
        {
            krill_proto::bots::BotRpcResponse::AccessRequests(requests) => requests
                .iter()
                .filter(|req| req.status == krill_proto::bots::BotAccessRequestStatus::Pending)
                .count(),
            _ => return Err(invalid()),
        };
        Ok(json!({
            "count": count,
            "needs_you": needs_you,
            "pending_access": pending_access,
        }))
    }
    fn bots_inactive(&self) -> io::Result<bool> {
        let registry = self.registry.upgrade().ok_or_else(invalid)?;
        if registry.bots_if_initialized().is_some() {
            return Ok(false);
        }
        Ok(self
            .dir
            .as_deref()
            .is_none_or(|dir| !crate::bots::store::BotStore::index_exists_at(dir)))
    }
    fn bot_rpc(
        &self,
        req: krill_proto::bots::BotRpcRequest,
    ) -> io::Result<krill_proto::bots::BotRpcResponse> {
        let mutates = !req.is_read_only()
            && !matches!(
                req,
                krill_proto::bots::BotRpcRequest::ListAccessRequests { .. }
            );
        let registry = self.registry.upgrade().ok_or_else(invalid)?;
        let mgr = registry.bots().map_err(io::Error::other)?;
        match mgr.handle_rpc(&registry, req) {
            krill_proto::bots::BotRpcResponse::Error(msg) => Err(io::Error::other(msg)),
            krill_proto::bots::BotRpcResponse::SessionRefused(refusal) => {
                Err(io::Error::other(BotRefusedIoError(*refusal)))
            }
            resp => {
                if mutates {
                    *crate::session::lock(&self.snapshot_cache) = None;
                }
                Ok(resp)
            }
        }
    }
    fn drop_dirs(&self) -> io::Result<(&Path, &Path)> {
        Ok((
            self.dir.as_deref().ok_or_else(invalid)?,
            self.drop_inbox.as_deref().ok_or_else(invalid)?,
        ))
    }
    fn session_bracketed_paste(&self, session: u64) -> bool {
        self.registry
            .upgrade()
            .is_some_and(|reg| reg.session_bracketed_paste(SessionId(session)))
    }
    fn send_prompt(&self, session: u64, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return self.input(session, b"\r".to_vec());
        }
        let bracketed = self.session_bracketed_paste(session) || text.contains(['\n', '\r']);
        let payload = if bracketed {
            format_bracketed_paste(text)
        } else {
            sanitize_plain_input(text)
        };
        if payload.len() > krill_proto::input::MAX_INPUT {
            return Err(invalid());
        }
        if let Some(reg) = self.registry.upgrade()
            && let Some(tap) = reg.session_tap(SessionId(session))
        {
            let before_end = tap.pulse().end;
            if !payload.is_empty() {
                tap.write_input(&payload, None)?;
                crate::registry::Registry::wait_for_paste_submit_settle(&tap, before_end);
            }
            return tap.write_input(b"\r", None);
        }
        let delay = if bracketed {
            krill_proto::input::PASTE_SUBMIT_DELAY
        } else {
            Duration::from_millis(60)
        };
        self.input_paced(session, &[payload, b"\r".to_vec()], delay)
    }

    fn promote_agy_steer_if_queued(&self, session: u64, combined: &str, baseline: usize) {
        let Some(reg) = self.registry.upgrade() else {
            return;
        };
        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline {
            if let Some(screen) = reg.session_capture(SessionId(session))
                && let Some(qm) = krill_proto::screen::agy::queued_submission_confirmed(
                    &screen, combined, baseline,
                )
            {
                if qm.send_immediately_hint {
                    self.input(
                        session,
                        krill_proto::screen::agy::SEND_IMMEDIATELY_KEY.to_vec(),
                    )
                    .ok();
                }
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn chat_composer_status(&self, session: u64) -> io::Result<Value> {
        let mut value = self.composer_status(session)?;
        if let Some(registry) = self.registry.upgrade()
            && let Some(obj) = value.as_object_mut()
        {
            let id = SessionId(session);
            let rate_limited = registry.agent_control().is_rate_limited(id);
            obj.insert("rate_limited".into(), Value::Bool(rate_limited));
            let live_prompt = registry
                .mobile_session_status(id)
                .filter(|status| {
                    matches!(
                        status.state,
                        control::AgentState::Waiting | control::AgentState::Blocked
                    )
                })
                .and_then(|_| registry.session_capture(id))
                .and_then(|screen| krill_proto::screen::prompt::detect(&screen.rows))
                .and_then(|prompt| serde_json::to_value(prompt).ok());
            obj.insert("live_prompt".into(), live_prompt.unwrap_or(Value::Null));
        }
        Ok(value)
    }

    fn live_prompt_reply(
        &self,
        session: u64,
        expected_prompt: &str,
        yes: Option<bool>,
        choice: Option<usize>,
        multi_selected: Option<Vec<usize>>,
        custom_text: Option<String>,
    ) -> io::Result<Value> {
        let expected = expected_prompt.trim();
        if expected.chars().count() < 3 || expected.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Expected prompt text is required.",
            ));
        }
        let registry = self.registry.upgrade().ok_or_else(|| {
            io::Error::other("Confirm in Term — live session verification is unavailable.")
        })?;
        let id = SessionId(session);
        let status = registry
            .mobile_session_status(id)
            .ok_or_else(|| io::Error::other("Session is no longer active."))?;
        if status.state == control::AgentState::Exited
            || registry.agent_control().account_binding_retiring(id)
        {
            return Err(io::Error::other("Session is no longer active."));
        }
        let screen = registry
            .session_capture(id)
            .ok_or_else(|| io::Error::other("Live terminal screen is unavailable."))?;
        let waiting_or_blocked = matches!(
            status.state,
            control::AgentState::Waiting | control::AgentState::Blocked
        );
        if !waiting_or_blocked
            && (expected.chars().count() < 8
                || krill_proto::screen::prompt::detect(&screen.rows).is_none())
        {
            return Err(io::Error::other(
                "Wait for an active prompt before replying.",
            ));
        }
        let reply = if let Some(text) = custom_text {
            let option_index = match choice {
                Some(idx) => idx,
                None => match krill_proto::screen::prompt::detect(&screen.rows) {
                    Some(krill_proto::screen::prompt::PromptKind::NumberedMenu {
                        options, ..
                    }) => options.iter().position(|o| o.is_other).ok_or_else(|| {
                        io::Error::other("No custom input option is visible on screen.")
                    })?,
                    _ => {
                        return Err(io::Error::other(
                            "No custom input option is visible on screen.",
                        ));
                    }
                },
            };
            krill_proto::screen::prompt::PromptReply::Other { option_index, text }
        } else if let Some(yes) = yes {
            krill_proto::screen::prompt::PromptReply::YesNo(yes)
        } else if let Some(selected) = multi_selected {
            krill_proto::screen::prompt::PromptReply::MultiSelect(selected)
        } else if let Some(idx) = choice {
            krill_proto::screen::prompt::PromptReply::Choice(idx)
        } else {
            return Err(invalid());
        };
        let bracketed = registry.session_bracketed_paste(id);
        let steps =
            krill_proto::screen::prompt::verify_and_plan(&screen.rows, expected, &reply, bracketed)
                .map_err(|_| io::Error::other("Live screen prompt changed; confirm in Term."))?;
        self.input_paced(session, &steps, Duration::from_millis(40))?;
        Ok(json!({"sent":true}))
    }
    fn wire(&self) -> io::Result<Wire> {
        let registry = self.registry.upgrade().ok_or_else(invalid)?;
        let hello = self.hello.clone();
        let (mut socket, server) = UnixStream::pair()?;
        socket.set_read_timeout(Some(Duration::from_secs(20)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        let worker = thread::Builder::new()
            .name("mobile-rpc".into())
            .stack_size(super::gateway::RPC_STACK)
            .spawn(move || {
                crate::alloc::mobile_thread().ok();
                let out = registry.serve_mobile(server, &hello).ok();
                crate::alloc::purge();
                out
            })?;
        let mut wire = Wire {
            socket: socket.try_clone()?,
            worker: Some(worker),
        };
        let (kind, body) = ipc::read_frame(&mut socket)?.ok_or_else(invalid)?;
        ServerHello::decode(kind, &body)?;
        socket.write_all(
            &krill_proto::daemon::client_hello(ClientRole::App, "mobile-pwa").encode()?,
        )?;
        // wire는 위 clone과 같은 스트림이며 Drop에서 반드시 양쪽 작업을 끝낸다.
        std::mem::swap(&mut wire.socket, &mut socket);
        Ok(wire)
    }
    pub(super) fn poll_chat_for_feed(
        &self,
        session: u64,
        offset: u64,
    ) -> io::Result<feed::ChatFeedPoll> {
        let registry = self.registry.upgrade().ok_or_else(invalid)?;
        let snapshot = registry.chat_poll(SessionId(session), offset);
        let next_offset = snapshot.next_offset;
        let watch_path = (!snapshot.transcript_path.is_empty())
            .then(|| PathBuf::from(&snapshot.transcript_path));
        if !snapshot.reset && snapshot.items.is_empty() && next_offset == offset {
            return Ok(feed::ChatFeedPoll {
                delta: None,
                next_offset,
                watch_path,
            });
        }
        let delta = chat_page_value(snapshot, offset)?;
        Ok(feed::ChatFeedPoll {
            delta: Some(delta),
            next_offset,
            watch_path,
        })
    }
    fn control(&self, request: &Request) -> io::Result<Reply> {
        Ok(self
            .registry
            .upgrade()
            .ok_or_else(invalid)?
            .handle_control_mobile(request.clone()))
    }
    fn accounts(&self, request: &UsageRequest) -> io::Result<krill_proto::usage::AccountsSnapshot> {
        match self
            .registry
            .upgrade()
            .ok_or_else(invalid)?
            .handle_usage_mobile(request.clone())
        {
            UsageReply::Accounts(data) => Ok(data),
            UsageReply::Failed(e) => Err(io::Error::other(e.to_string())),
            _ => Err(invalid()),
        }
    }
    fn input(&self, session: u64, bytes: Vec<u8>) -> io::Result<()> {
        self.input_paced(session, &[bytes], Duration::ZERO)
    }
    fn input_paced(&self, session: u64, chunks: &[Vec<u8>], delay: Duration) -> io::Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let mut wire = self.wire()?;
        wire.socket.write_all(
            &Attach {
                session: SessionId(session),
                // 입력 전용 attach는 로그를 재생하지 않는다. 범위 밖 seq는 현재 끝으로 이동한다.
                stream: Stream::Raw {
                    from: Seq(u64::MAX),
                },
            }
            .encode()?,
        )?;
        loop {
            let (kind, _) = ipc::read_frame(&mut wire.socket)?.ok_or_else(invalid)?;
            if kind == krill_proto::screen::kind::ATTACHED {
                break;
            }
            if kind == krill_proto::screen::kind::REFUSED {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        for (idx, bytes) in chunks.iter().enumerate() {
            if idx > 0 && !delay.is_zero() {
                thread::sleep(delay);
            }
            let input_id = u64::try_from(idx).unwrap_or(0).saturating_add(1);
            wire.socket.write_all(
                &Input {
                    id: InputId(input_id),
                    bytes: bytes.clone(),
                }
                .encode()?,
            )?;
            loop {
                let (kind, body) = ipc::read_frame(&mut wire.socket)?.ok_or_else(invalid)?;
                if kind == krill_proto::input::kind::INPUT_ACK {
                    let ack = InputAck::decode(kind, &body)?;
                    if ack.result != InputResult::Applied {
                        return Err(io::Error::other("input rejected"));
                    }
                    break;
                }
            }
        }
        Ok(())
    }
    fn cwd(&self, session: u64) -> io::Result<PathBuf> {
        self.registry
            .upgrade()
            .ok_or_else(invalid)?
            .mobile_session_cwd(SessionId(session))
    }
    fn save_quick_commands(&self, commands: Vec<QuickCommandEntry>) -> io::Result<Value> {
        if commands.len() > 128 {
            return Err(invalid());
        }
        let normalized: Vec<_> = commands.into_iter().map(normalize_quick_command).collect();
        for entry in &normalized {
            if entry.name.is_empty()
                || entry.name.chars().count() > 120
                || entry.name.len() > 480
                || entry.name.chars().any(char::is_control)
                || entry.text.trim().is_empty()
                || entry.text.len() > 16 * 1024
                || entry
                    .text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
            {
                return Err(invalid());
            }
        }
        let _guard = crate::session::lock(&self.settings_lock);
        let dir = self.dir.as_deref().ok_or_else(invalid)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let path = dir.join("settings.toml");
        let _document_lock = krill_proto::config_document::lock_toml(&path)?;
        let content = krill_proto::config_document::save_toml(
            &path,
            &QuickCommandsFile {
                quick_commands: normalized.clone(),
            },
        )
        .map_err(io::Error::other)?;
        let tmp = dir.join(format!(".settings.{}.tmp", hex(&random()?)));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            fs::rename(&tmp, &path)
        })();
        if let Err(error) = result {
            fs::remove_file(&tmp).ok();
            return Err(error);
        }
        *crate::session::lock(&self.snapshot_cache) = None;
        Ok(json!({"quick_commands":normalized}))
    }

    fn effective_revision(&self, registry: &Registry) -> u64 {
        registry.dashboard_revision()
            ^ self
                .git_revision
                .load(Ordering::Relaxed)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ self
                .repo_icons
                .revision()
                .wrapping_mul(0x517C_C1B7_2722_0A95)
    }

    #[cfg(test)]
    fn snapshot_since(&self, since: Option<&str>) -> io::Result<Value> {
        self.snapshot_versioned(since).map(|(value, _)| value)
    }

    fn snapshot_versioned(&self, since: Option<&str>) -> io::Result<(Value, u64)> {
        let registry = self.registry.upgrade().ok_or_else(invalid)?;
        // This mutex is a snapshot-only singleflight, independent of registry/session IO.
        let mut cached = crate::session::lock(&self.snapshot_cache);
        let revision = self.effective_revision(&registry);
        if let Some(snapshot) = cached.as_ref()
            && snapshot.revision == revision
            && snapshot.at.elapsed() < GIT_TTL
            && snapshot.stamps == file_stamps(&snapshot.paths)
        {
            return Ok(if since == Some(snapshot.etag.as_str()) {
                (json!({"unchanged":true,"etag":snapshot.etag}), snapshot.rev)
            } else {
                (snapshot.data.clone(), snapshot.rev)
            });
        }
        // Drop stale terminal bodies before rebuilding (including a viewport clear).
        *cached = None;
        let base_paths = self.snapshot_paths(&Value::Null);
        let before = file_stamps(&base_paths);
        let mut catalog_used = None;
        let mut data = self.snapshot(&mut catalog_used)?;
        let rev = self.snapshot_rev.fetch_add(1, Ordering::Relaxed) + 1;
        let bytes = serde_json::to_vec(&data).map_err(io::Error::other)?;
        let etag: String = hex(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref())
            .chars()
            .take(16)
            .collect();
        drop(bytes);
        if let Some(object) = data.as_object_mut() {
            object.insert("etag".into(), etag.clone().into());
        }
        // Validate the dependency identity actually used by this body. A newer
        // catalog or a fresh post-build stamp cannot authenticate an older body.
        if let Some((paths, stamps)) = self.snapshot_dependencies(&data, catalog_used.as_ref())
            && before == file_stamps(&base_paths)
        {
            *cached = Some(DashboardSnapshot {
                revision,
                paths,
                stamps,
                at: Instant::now(),
                data: data.clone(),
                etag: etag.clone(),
                rev,
            });
        }
        Ok(if since == Some(etag.as_str()) {
            (json!({"unchanged":true,"etag":etag}), rev)
        } else {
            (data, rev)
        })
    }

    fn snapshot_dependencies(
        &self,
        data: &Value,
        used: Option<&Arc<super::workspaces::Catalog>>,
    ) -> Option<SnapshotDependencies> {
        let used = used?;
        let (catalog_paths, catalog_stamps) = crate::session::lock(&self.catalog_cache)
            .as_ref()
            .filter(|cached| Arc::ptr_eq(&cached.catalog, used))
            .map(|cached| (cached.paths.clone(), cached.stamps.clone()))?;
        let mut paths = self.snapshot_paths(data);
        paths.extend(catalog_paths);
        let stamps = file_stamps(&paths);
        stamps.ends_with(&catalog_stamps).then_some((paths, stamps))
    }

    fn snapshot_paths(&self, data: &Value) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(dir) = &self.dir {
            for name in [
                "projects.toml",
                "settings.toml",
                "sidebar.bin",
                "worktree-comments.toml",
                "session-titles.json",
                "orchestration.json",
                "agents.toml",
                "agent-launches",
                "bots",
                "bots/bots.json",
                "bots/bot-items.db",
                "bots/bot-items.db-wal",
                "bots/access_requests.json",
            ] {
                paths.push(dir.join(name));
            }
            // Use the actual signed rules names as well as user/profile metadata.
            paths.push(dir.join(krill_proto::agent::rules::PUBLIC_KEY_FILE_NAME));
            paths.push(dir.join(krill_proto::agent::rules::RULES_FILE_NAME));
            if let Some(sessions) = data.get("sessions").and_then(Value::as_array) {
                for id in sessions
                    .iter()
                    .filter_map(|session| session.get("id").and_then(Value::as_u64))
                {
                    paths.push(dir.join("agent-launches").join(format!("{id}.json")));
                }
            }
        }
        paths
    }

    fn catalog(
        &self,
        repositories: &[control::RepositoryTrees],
    ) -> Arc<super::workspaces::Catalog> {
        let mut cached = crate::session::lock(&self.catalog_cache);
        if let Some(old) = cached.as_ref()
            && old.repositories == repositories
            && old.at.elapsed() < GIT_TTL
            && old.stamps == file_stamps(&old.paths)
        {
            return Arc::clone(&old.catalog);
        }
        *cached = None;
        let mut paths = self.snapshot_paths(&Value::Null);
        for tree in repositories.iter().flat_map(|repo| &repo.worktrees) {
            let tree = PathBuf::from(std::ffi::OsString::from_vec(tree.clone()));
            let dot = tree.join(".git");
            paths.push(dot.clone());
            let git = if dot.is_dir() {
                dot
            } else {
                fs::read_to_string(&dot)
                    .ok()
                    .and_then(|text| {
                        text.strip_prefix("gitdir:")
                            .map(|git| tree.join(git.trim()))
                    })
                    .unwrap_or(dot)
            };
            paths.push(git.join("HEAD"));
            paths.push(git.join("commondir"));
        }
        let before = file_stamps(&paths);
        let catalog = Arc::new(super::workspaces::Catalog::load(
            self.dir.as_deref(),
            repositories,
        ));
        if before == file_stamps(&paths) {
            *cached = Some(CatalogSnapshot {
                repositories: repositories.to_vec(),
                paths,
                stamps: before,
                at: Instant::now(),
                catalog: Arc::clone(&catalog),
            });
        }
        catalog
    }

    fn snapshot(
        &self,
        catalog_used: &mut Option<Arc<super::workspaces::Catalog>>,
    ) -> io::Result<Value> {
        let registry = self.registry.upgrade().ok_or_else(invalid)?;
        let dashboard = registry.mobile_dashboard_inputs_swr()?;
        let catalog = self.catalog(&dashboard.repositories);
        *catalog_used = Some(Arc::clone(&catalog));
        let truncated = dashboard.sessions.len() > 64;
        let rows: Vec<_> = dashboard
            .sessions
            .into_iter()
            .take(64)
            .map(|row| {
                let group = session_group(row.info.session, &row.cwd, &catalog.repositories);
                (row.info, row.cwd, row.status, group)
            })
            .collect();
        let git = self.git_summaries_swr(
            rows.iter()
                .filter_map(|(_, cwd, _, group)| git_cwd(cwd, group)),
        );
        let mut cards = Vec::new();
        let mut workspace_titles = std::collections::BTreeMap::new();
        let launch_metadata = registry.agent_control();
        let loaded_agents = launch_metadata.agents().ok();
        let agents_value = format_agents_json(loaded_agents.as_ref());
        let agent_profiles = loaded_agents.unwrap_or_default();
        // 데스크톱 스마트 정렬과 같은 워크트리 순위(승인대기 0 → 작업 중 2 → 완료 3 → 유휴 4).
        let mut ranks: std::collections::BTreeMap<PathBuf, u8> = std::collections::BTreeMap::new();
        // 데스크톱 "잠든 작업 공간 숨기기"의 깨어 있음 판정: 작업 중·대기·완료이거나 에이전트가 살아 있는 세션.
        let mut awake = std::collections::BTreeSet::new();
        for (_, _, status, group) in &rows {
            let Some(path) = group.path.as_deref() else {
                continue;
            };
            if matches!(
                status.state,
                control::AgentState::Working
                    | control::AgentState::Waiting
                    | control::AgentState::Blocked
                    | control::AgentState::Done
            ) || !status.agent.is_empty() && status.state != control::AgentState::Exited
            {
                awake.insert(PathBuf::from(path));
            }
            let rank = match status.state {
                control::AgentState::Waiting | control::AgentState::Blocked => 0,
                control::AgentState::Working => 2,
                control::AgentState::Done => 3,
                _ => 4,
            };
            ranks
                .entry(PathBuf::from(path))
                .and_modify(|r| *r = (*r).min(rank))
                .or_insert(rank);
        }
        let auto_titles = self
            .dir
            .as_deref()
            .map(krill_proto::git::load_session_auto_titles)
            .unwrap_or_default();
        for (info, cwd_bytes, state, group) in rows {
            let stored_launch = launch_metadata.stored(info.session).ok().flatten();
            if let Some(title) = auto_titles.get(&info.session)
                && let Some(path) = &group.path
            {
                workspace_titles
                    .entry(PathBuf::from(path))
                    .or_insert_with(|| title.clone());
            }
            let agent = if state.agent.is_empty() {
                stored_launch
                    .as_ref()
                    .map(|launch| launch.profile.clone())
                    .unwrap_or_default()
            } else {
                state.agent.clone()
            };
            let title = state
                .title
                .as_deref()
                .filter(|title| {
                    !krill_proto::git::is_default_branch_or_worktree_name(title)
                        && !title.eq_ignore_ascii_case("claude code")
                })
                .or_else(|| auto_titles.get(&info.session).map(String::as_str))
                .or(state.title.as_deref());
            let cwd = String::from_utf8_lossy(&cwd_bytes);
            let lines = registry.dashboard_tail(info.session).unwrap_or_default();
            let git = git_cwd(&cwd_bytes, &group)
                .and_then(|path| git.get(&path))
                .unwrap_or(&Value::Null);
            cards.push(json!({"id":info.session.0,"state":card_state(state.state,info.status.is_some()),"agent":agent,"title":title,"foreground":state.foreground,"startup_stall":state.startup_stall,"turn_error":state.turn_error,"cwd":cwd,"lines":lines,"exited":info.status.is_some(),"branch":git.get("branch"),"added":git.get("added"),"deleted":git.get("deleted"),
                "profile_id":stored_launch.as_ref().map(|s|s.profile.as_str()),
                "account_id":stored_launch.as_ref().and_then(|s|s.account.as_ref()).map(|a|a.id.as_str()),
                "command_context":composer::command_context(stored_launch.as_ref(), &agent_profiles, &agent),
                "repo_key":group.repo_key,"worktree_key":group.worktree_key,
                "group_label":group.label,"group_path":group.path,
                "state_source":state.source,"state_confidence":state.confidence,
                "state_since_unix_ns":state.since_unix_ns.to_string()}));
        }
        let repo_icons = self
            .repo_icons
            .resolve_swr(&catalog.icon_targets(), self.dir.as_deref());
        let mut data = json!({"sessions":cards,"orchestration":self.orchestration()?,
            "bots":self.snapshot_bots_summary()?,
            "repositories":catalog.repositories_json_with_icons(&repo_icons),"workspaces":catalog.workspaces_json_with_icons(&workspace_titles, &ranks, &awake, &repo_icons),"truncated":truncated,
            "capabilities":{"snapshot_groups":true,"session_create":true,"session_close":true,"workspace_create":true,"worktree_create":true,"hybrid_chat":true,"chat_tail":true,"chat_push":true,"chat_interrupt":true,"live_prompt_reply":true,"prompt_steer":true,"quick_commands":true,"command_catalog":true,"mention_files":true,"composer_models":true,"chat_composer":true,"model_set":cfg!(unix),"change_feed":true,"drop":true,"bots":true,"repo_icons":true},
            "quick_commands":quick_commands_json(self.dir.as_deref()),
            "host":self.host,"agents":agents_value});
        bound_snapshot_payload(&mut data);
        Ok(data)
    }
    fn orchestration(&self) -> io::Result<Value> {
        let store = self
            .registry
            .upgrade()
            .ok_or_else(invalid)?
            .orchestration_snapshot()?;
        Ok(requests::snapshot(&store))
    }
    /// 미캐시(cold) cwd는 `Null`을, 만료된 cwd는 직전 값을 즉시(`0ms`) 반환하고 cwd별로 하나의 백그라운드 조회만 예약한다.
    fn git_summaries_swr(
        &self,
        cwds: impl IntoIterator<Item = PathBuf>,
    ) -> HashMap<PathBuf, Value> {
        let mut found = HashMap::new();
        let mut cold = Vec::new();
        let mut stale = Vec::new();
        {
            let cache = crate::session::lock(&self.git_cache);
            for cwd in cwds {
                if found.contains_key(&cwd) {
                    continue;
                }
                let value = if let Some((at, value)) = cache.get(&cwd) {
                    if at.elapsed() >= GIT_TTL {
                        stale.push(cwd.clone());
                    }
                    value.clone()
                } else {
                    cold.push(cwd.clone());
                    Value::Null
                };
                found.insert(cwd, value);
            }
        }
        cold.extend(stale);
        {
            let mut inflight = crate::session::lock(&self.git_inflight);
            cold.retain(|cwd| inflight.insert(cwd.clone()));
        }
        if !cold.is_empty() {
            let cache = Arc::clone(&self.git_cache);
            let inflight = Arc::clone(&self.git_inflight);
            let revision = Arc::clone(&self.git_revision);
            let scheduled = cold.clone();
            let spawned = thread::Builder::new()
                .name("mobile-git-swr".into())
                .stack_size(128 * 1024)
                .spawn(move || {
                    crate::alloc::mobile_thread().ok();
                    let queue = crate::git::queue().ok();
                    let pending: Vec<_> = cold
                        .into_iter()
                        .map(|cwd| {
                            let rx = queue.map(|q| {
                                crate::git::changes::query_with_branch(
                                    q,
                                    cwd.clone(),
                                    Arc::new(AtomicBool::new(false)),
                                )
                            });
                            (cwd, rx)
                        })
                        .collect();
                    for (cwd, rx) in pending {
                        let value = rx
                            .and_then(|rx| rx.recv().ok())
                            .and_then(Result::ok)
                            .map_or(Value::Null, |(changes, branch)| {
                                let (added, deleted) = totals(&changes);
                                json!({"branch":branch,"added":added,"deleted":deleted})
                            });
                        cache_git_summary(&cache, cwd.clone(), value);
                        crate::session::lock(&inflight).remove(&cwd);
                        revision.fetch_add(1, Ordering::Relaxed);
                    }
                    crate::alloc::purge();
                });
            if spawned.is_err() {
                let mut inflight = crate::session::lock(&self.git_inflight);
                for cwd in scheduled {
                    inflight.remove(&cwd);
                }
            }
        }
        found
    }
    /// 카드용 git 요약을 cwd별로 모은다. 캐시에 없는 cwd는 큐에 전부 먼저 넣고 나서 받는다
    /// (큐 워커 [`crate::git::MAX_CONCURRENT`]개가 서로 다른 cwd를 동시에 돈다. 같은 키만 직렬).
    #[cfg(all(test, unix))]
    fn git_summaries(&self, cwds: impl IntoIterator<Item = PathBuf>) -> HashMap<PathBuf, Value> {
        let mut found = HashMap::new();
        let mut missing = Vec::new();
        {
            let mut cache = crate::session::lock(&self.git_cache);
            cache.retain(|_, (at, _)| at.elapsed() < GIT_TTL);
            for cwd in cwds {
                if let Some((_, value)) = cache.get(&cwd) {
                    found.insert(cwd, value.clone());
                } else if !missing.contains(&cwd) {
                    missing.push(cwd);
                }
            }
        }
        let queue = crate::git::queue().ok();
        let pending: Vec<_> = missing
            .into_iter()
            .map(|cwd| {
                let rx = queue.map(|q| {
                    crate::git::changes::query_with_branch(
                        q,
                        cwd.clone(),
                        Arc::new(AtomicBool::new(false)),
                    )
                });
                (cwd, rx)
            })
            .collect();
        for (cwd, rx) in pending {
            let value = rx
                .and_then(|rx| rx.recv().ok())
                .and_then(Result::ok)
                .map_or(Value::Null, |(changes, branch)| {
                    let (added, deleted) = totals(&changes);
                    json!({"branch":branch,"added":added,"deleted":deleted})
                });
            cache_git_summary(&self.git_cache, cwd.clone(), value.clone());
            found.insert(cwd, value);
        }
        found
    }
    fn git(&self, session: u64, path: Option<&str>) -> io::Result<Value> {
        let cwd = self.cwd(session)?;
        let root = git_root(&cwd)?;
        let queue = crate::git::queue().map_err(io::Error::other)?;
        let (changes, branch) = crate::git::changes::query_with_branch(
            queue,
            cwd.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .recv()
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
        let (added, deleted) = totals(&changes);
        let files: Vec<_> = changes
            .files
            .iter()
            .map(|file| String::from_utf8_lossy(&file.path).into_owned())
            .collect();
        let patch = if let Some(raw_path) = path {
            let Some(rel_path) = git_relative_path(&root, &cwd, raw_path, &files) else {
                return Ok(
                    json!({"diff_status":"outside_scope","patch":"","files":files,"limited":changes.limited}),
                );
            };
            if !changes.limited && !files.iter().any(|p| p == &rel_path) {
                return Ok(
                    json!({"diff_status":"no_current_diff","patch":"","files":files,"limited":changes.limited}),
                );
            }
            read_patch(&root, &rel_path)?
        } else {
            String::new()
        };
        Ok(
            json!({"branch":branch,"added":added,"deleted":deleted,"files":files,"patch":patch,"limited":changes.limited}),
        )
    }
}
fn parse_opt_bot_id(raw: Option<&str>) -> io::Result<Option<krill_proto::bots::BotId>> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(id) => krill_proto::bots::BotId::new(id)
            .map(Some)
            .map_err(|_| invalid()),
    }
}

/// 표시용 lossy UTF-8과 분리해 OS 경로 바이트를 그대로 식별한다.
#[derive(Debug)]
struct SessionGroup {
    repo_key: Option<String>,
    worktree_key: String,
    label: Option<String>,
    path: Option<String>,
}
fn session_group(
    session: SessionId,
    cwd: &[u8],
    repositories: &[control::RepositoryTrees],
) -> SessionGroup {
    if cwd.is_empty() {
        return SessionGroup {
            repo_key: None,
            worktree_key: format!("unknown:{}", session.0),
            label: None,
            path: None,
        };
    }
    let cwd_path = Path::new(std::ffi::OsStr::from_bytes(cwd));
    let matched = repositories
        .iter()
        .flat_map(|repo| {
            repo.worktrees
                .iter()
                .filter(|tree| !tree.is_empty())
                .map(move |tree| (repo, tree))
        })
        .filter_map(|(repo, tree)| {
            let path = Path::new(std::ffi::OsStr::from_bytes(tree));
            cwd_path
                .starts_with(path)
                .then(|| (repo, tree, path.components().count()))
        })
        .max_by_key(|(_, _, depth)| *depth);
    let (repo_key, bytes) = matched.map_or((None, cwd), |(repo, tree, _)| {
        (Some(hex(&repo.repo)), tree.as_slice())
    });
    SessionGroup {
        repo_key,
        worktree_key: hex(bytes),
        label: Some(path_label(bytes)),
        path: Some(String::from_utf8_lossy(bytes).into_owned()),
    }
}
/// 카드 git 요약을 물을 cwd. 저장소 밖(`repo_key` 없음)·cwd 미상은 git을 부르지 않는다(값은 `null`).
fn git_cwd(cwd: &[u8], group: &SessionGroup) -> Option<PathBuf> {
    group
        .repo_key
        .as_ref()
        .map(|_| PathBuf::from(std::ffi::OsStr::from_bytes(cwd)))
}
fn totals(changes: &krill_proto::git::Changes) -> (u64, u64) {
    changes
        .files
        .iter()
        .flat_map(|file| [file.staged, file.unstaged])
        .flatten()
        .fold((0_u64, 0_u64), |(added, deleted), stat| {
            (
                added.saturating_add(stat.added.unwrap_or(0)),
                deleted.saturating_add(stat.deleted.unwrap_or(0)),
            )
        })
}
fn path_label(bytes: &[u8]) -> String {
    let path = Path::new(std::ffi::OsStr::from_bytes(bytes));
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
fn host_metadata() -> Value {
    #[cfg(windows)]
    let display_name = crate::sys::hostname().ok();
    #[cfg(unix)]
    let display_name = {
        let mut command = Command::new("/bin/hostname");
        bounded_command(&mut command, Duration::from_secs(1), 1024)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    };
    let platform = match std::env::consts::OS {
        "macos" => Some("macos"),
        "linux" => Some("linux"),
        "windows" => Some("windows"),
        _ => None,
    };
    json!({"display_name":display_name,"platform":platform})
}
struct Wire {
    socket: UnixStream,
    worker: Option<JoinHandle<Option<()>>>,
}
impl Drop for Wire {
    fn drop(&mut self) {
        self.socket.shutdown(Shutdown::Both).ok();
        if let Some(worker) = self.worker.take() {
            worker.join().ok();
        }
    }
}
fn to_value(v: impl serde::Serialize) -> io::Result<Value> {
    serde_json::to_value(v).map_err(io::Error::other)
}

fn save_photo(mime: &str, data: &str) -> io::Result<PathBuf> {
    let extension = match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        _ => return Err(invalid()),
    };
    let bytes = decode_base64(data)?;
    if bytes.is_empty() || bytes.len() > 512 * 1024 {
        return Err(invalid());
    }
    let dir = std::env::temp_dir().join("krill-paste");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || !crate::os::private_to_me(&meta, 0o077) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let path = dir.join(format!("mobile-{}.{extension}", hex(&random()?)));
    let mut file = crate::os::open_no_follow(
        OpenOptions::new().create_new(true).write(true).mode(0o600),
        &path,
    )?;
    file.write_all(&bytes)?;
    Ok(path)
}
fn decode_base64(data: &str) -> io::Result<Vec<u8>> {
    if data.len() > 700_000 {
        return Err(invalid());
    }
    let mut bits = 0_u32;
    let mut count = 0;
    let mut out = Vec::new();
    let mut padded = false;
    for byte in data.bytes() {
        if byte == b'=' {
            padded = true;
            continue;
        }
        if padded {
            return Err(invalid());
        }
        let n = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return Err(invalid()),
        };
        bits = (bits << 6) | u32::from(n);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push(u8::try_from((bits >> count) & 255).map_err(|_| invalid())?);
        }
    }
    if count >= 6 || (bits & ((1 << count) - 1)) != 0 {
        return Err(invalid());
    }
    Ok(out)
}
fn cache_git_summary(
    cache: &Mutex<HashMap<PathBuf, (Instant, Value)>>,
    cwd: PathBuf,
    value: Value,
) {
    let mut cache = crate::session::lock(cache);
    cache.insert(cwd, (Instant::now(), value));
    while cache.len() > 64 {
        let oldest = cache
            .iter()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(path, _)| path.clone());
        if let Some(path) = oldest {
            cache.remove(&path);
        } else {
            break;
        }
    }
}

fn mobile_git_command(cwd: &Path) -> Command {
    let mut command = krill_proto::os::background_command("git");
    command
        .current_dir(krill_proto::os::shell_path(cwd))
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
    ] {
        command.env_remove(key);
    }
    command.arg("--literal-pathspecs");
    command
}

fn git_root(cwd: &Path) -> io::Result<PathBuf> {
    let bytes = bounded_command(
        mobile_git_command(cwd).args(["rev-parse", "--show-toplevel"]),
        Duration::from_secs(5),
        16 * 1024,
    )
    .map_err(|_| io::Error::other("Not a Git repository"))?;
    // Git terminates this path with a newline; spaces in a repository name are significant.
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    #[cfg(windows)]
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    fs::canonicalize(PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec())))
}

fn canonical_missing(path: &Path) -> Option<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Some(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let name = path.file_name()?;
            Some(canonical_missing(path.parent()?)?.join(name))
        }
        Err(_) => None,
    }
}

fn git_relative_path(root: &Path, cwd: &Path, raw: &str, files: &[String]) -> Option<String> {
    use std::path::Component;
    let input = Path::new(raw);
    let normalize = |path: &Path| {
        let mut out = PathBuf::new();
        for part in path.components() {
            match part {
                Component::CurDir => (),
                Component::ParentDir => {
                    if !out.pop() {
                        return None;
                    }
                }
                other => out.push(other.as_os_str()),
            }
        }
        Some(out)
    };
    let normalized = normalize(input)?;
    let candidate = if input.is_absolute() {
        normalized
    } else if files.iter().any(|p| Path::new(p) == normalized) {
        root.join(normalized)
    } else {
        let local = normalize(&cwd.join(input))?;
        if local.exists() || !root.join(&normalized).exists() {
            local
        } else {
            root.join(normalized)
        }
    };
    // Resolve parent aliases without following a final symlink: Git diffs the link
    // itself, and untracked_patch must still be able to reject that link.
    let canonical = canonical_missing(candidate.parent()?)?.join(candidate.file_name()?);
    let relative = canonical.strip_prefix(root).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    relative.to_str().map(|path| {
        if cfg!(windows) {
            path.replace('\\', "/")
        } else {
            path.to_owned()
        }
    })
}

fn read_patch(cwd: &Path, path: &str) -> io::Result<String> {
    let has_head = bounded_command(
        mobile_git_command(cwd).args(["rev-parse", "--verify", "HEAD"]),
        Duration::from_secs(5),
        1024,
    )
    .is_ok();
    let base = if has_head {
        "HEAD"
    } else {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    };
    let patch = bounded_command(
        mobile_git_command(cwd).args([
            "-c",
            "diff.maxFiles=1",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            base,
            "--",
            path,
        ]),
        Duration::from_secs(5),
        128 * 1024,
    )?;
    if !patch.is_empty() {
        return Ok(String::from_utf8_lossy(&patch).into_owned());
    }
    if bounded_command(
        mobile_git_command(cwd).args(["ls-files", "--error-unmatch", "--", path]),
        Duration::from_secs(5),
        16 * 1024,
    )
    .is_ok()
    {
        return Ok(String::new());
    }
    untracked_patch(cwd, path)
}

fn untracked_patch(root: &Path, path: &str) -> io::Result<String> {
    const LIMIT: u64 = 128 * 1024;
    let target = root.join(path);
    let meta = match fs::symlink_metadata(&target) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e),
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Ok(String::new());
    }
    if meta.len() > LIMIT {
        return Err(io::Error::other("File exceeds 128 KiB diff limit"));
    }
    let canonical_root = fs::canonicalize(root)?;
    if !fs::canonicalize(&target)?.starts_with(&canonical_root) {
        return Err(io::Error::other("File is outside the repository"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(&target)?;
    if !file.metadata()?.is_file() {
        return Ok(String::new());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > LIMIT {
        return Err(io::Error::other("File exceeds 128 KiB diff limit"));
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(format!("Binary files /dev/null and b/{path} differ\n"));
    };
    if bytes.contains(&0) {
        return Ok(format!("Binary files /dev/null and b/{path} differ\n"));
    }
    let lines = text.split_inclusive('\n').count();
    let filename = format!("b/{path}");
    let filename = if path.chars().any(char::is_control) {
        serde_json::to_string(&filename).map_err(io::Error::other)?
    } else {
        filename
    };
    let mut patch = format!("--- /dev/null\n+++ {filename}\n@@ -0,0 +1,{lines} @@\n");
    for line in text.split_inclusive('\n') {
        patch.push('+');
        patch.push_str(line);
        if !line.ends_with('\n') {
            patch.push_str("\n\\ No newline at end of file\n");
        }
    }
    if u64::try_from(patch.len()).unwrap_or(u64::MAX) > LIMIT {
        return Err(io::Error::other("mobile command output exceeds limit"));
    }
    Ok(patch)
}
fn bounded_command(command: &mut Command, timeout: Duration, limit: usize) -> io::Result<Vec<u8>> {
    let mut child = crate::child::spawn(
        crate::child::own_group(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )?;
    let result = (|| {
        let mut pipe = crate::child::PipeReader::new(child.stdout.take().ok_or_else(invalid)?)?;
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    out.extend(buffer.iter().take(n).copied());
                    if out.len() > limit {
                        return Err(io::Error::other("mobile command output exceeds limit"));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e),
            }
        }
        loop {
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(out)
                } else {
                    Err(io::Error::other("mobile command failed"))
                };
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    drop(child); // Process owns the child group/Job, including descendants on timeout.
    result
}

/// Bracketed Paste(`\x1b[200~...\x1b[201~`)로 감싸고 내부 ESC(`0x1b`)·ETX(`0x03`) 등 제어 바이트를 제거한다(OWE-944 / MK-241).
fn format_bracketed_paste(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len().saturating_add(12));
    out.extend_from_slice(b"\x1b[200~");
    for &b in text.as_bytes() {
        if matches!(b, b'\n' | b'\r' | b'\t') || (b >= 0x20 && b != 0x7f) {
            out.push(b);
        }
    }
    out.extend_from_slice(b"\x1b[201~");
    out
}

fn sanitize_plain_input(text: &str) -> Vec<u8> {
    text.as_bytes()
        .iter()
        .copied()
        .filter(|&b| b == b'\t' || (b >= 0x20 && b != 0x7f))
        .collect()
}

/// 터미널 화면 텍스트에서 현재 선택 포인터(`❯`·`›`·`▸`·`>`)가 위치한 질문 옵션 인덱스(0-based)를 추정한다.
/// 감지할 수 없으면 기본값 `0`을 반환한다.
#[cfg(test)]
fn detect_question_cursor_index(screen_text: &str, option_count: usize) -> usize {
    let max_idx = option_count.max(16);
    let tail: Vec<&str> = screen_text
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(40)
        .collect();
    for line in &tail {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed
            .strip_prefix('❯')
            .or_else(|| trimmed.strip_prefix('›'))
            .or_else(|| trimmed.strip_prefix('▸'))
            .or_else(|| trimmed.strip_prefix('>'))
        else {
            continue;
        };
        let rest = rest.trim_start();
        let rest = rest
            .strip_prefix("[ ]")
            .or_else(|| rest.strip_prefix("[x]"))
            .or_else(|| rest.strip_prefix("[X]"))
            .or_else(|| rest.strip_prefix("[✓]"))
            .or_else(|| rest.strip_prefix("( )"))
            .or_else(|| rest.strip_prefix("(*)"))
            .unwrap_or(rest)
            .trim_start();
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty()
            && let Some(after) = rest.strip_prefix(&digits)
            && (after.starts_with('.') || after.starts_with(')'))
            && let Ok(num) = digits.parse::<usize>()
            && num >= 1
            && num <= max_idx.saturating_add(1)
        {
            return num - 1;
        }
    }
    0
}

#[cfg(test)]
fn step_cursor(steps: &mut Vec<Vec<u8>>, cursor: &mut usize, target: usize) {
    while *cursor < target {
        steps.push(b"\x1b[B".to_vec());
        *cursor += 1;
    }
    while *cursor > target {
        steps.push(b"\x1b[A".to_vec());
        *cursor -= 1;
    }
}

#[cfg(test)]
fn plan_question_keystrokes(
    selected: &[usize],
    option_count: usize,
    multi_select: bool,
    custom_text: Option<&str>,
    bracketed: bool,
) -> io::Result<Vec<Vec<u8>>> {
    plan_question_keystrokes_from(
        selected,
        option_count,
        multi_select,
        custom_text,
        bracketed,
        0,
    )
}

/// 인터랙티브 질문 카드(`AskUserQuestion` 등)의 응답을 현재 TUI 커서 위치(`initial_cursor`) 기준
/// 60ms 간격 키스트로크 시퀀스로 계획한다(OWE-946 / MK-243).
#[cfg(test)]
fn plan_question_keystrokes_from(
    selected: &[usize],
    option_count: usize,
    multi_select: bool,
    custom_text: Option<&str>,
    bracketed: bool,
    initial_cursor: usize,
) -> io::Result<Vec<Vec<u8>>> {
    let capped_options = option_count.min(16);
    let custom = custom_text.map(str::trim).filter(|s| !s.is_empty());
    if let Some(t) = custom
        && t.len() > 4096
    {
        return Err(invalid());
    }
    let mut steps: Vec<Vec<u8>> = Vec::new();
    let mut cursor = initial_cursor.min(capped_options.max(16));

    if multi_select {
        let mut sorted: Vec<usize> = selected
            .iter()
            .copied()
            .filter(|&idx| idx < capped_options.max(16))
            .collect();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.is_empty() && custom.is_none() {
            return Err(invalid());
        }
        for target in sorted {
            step_cursor(&mut steps, &mut cursor, target);
            steps.push(b" ".to_vec());
        }
        if let Some(text) = custom {
            step_cursor(&mut steps, &mut cursor, capped_options);
            steps.push(b" ".to_vec());
            let payload = if bracketed || text.contains(['\n', '\r']) {
                format_bracketed_paste(text)
            } else {
                sanitize_plain_input(text)
            };
            steps.push(payload);
            steps.push(b"\r".to_vec());
        } else {
            steps.push(b"\r".to_vec());
        }
        return Ok(steps);
    }

    if let Some(text) = custom {
        step_cursor(&mut steps, &mut cursor, capped_options);
        if capped_options > 0 {
            steps.push(b"\r".to_vec());
        }
        let payload = if bracketed || text.contains(['\n', '\r']) {
            format_bracketed_paste(text)
        } else {
            sanitize_plain_input(text)
        };
        steps.push(payload);
        steps.push(b"\r".to_vec());
        return Ok(steps);
    }

    let Some(&choice) = selected.first() else {
        return Err(invalid());
    };
    if choice >= 16 || (capped_options > 0 && choice >= capped_options) {
        return Err(invalid());
    }
    step_cursor(&mut steps, &mut cursor, choice);
    steps.push(b"\r".to_vec());
    Ok(steps)
}

/// An agent that exits and leaves its shell behind is a live shell session, not a closed one.
fn card_state(state: control::AgentState, exited: bool) -> control::AgentState {
    if state == control::AgentState::Exited && !exited {
        control::AgentState::Unknown
    } else {
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick_commands_test_dir() -> PathBuf {
        std::env::temp_dir().join(format!("pwa-quick-commands-{}", hex(&random().unwrap())))
    }

    fn rpc(backend: &Backend, request: &Value) -> Value {
        serde_json::from_str(&backend.request(&request.to_string())).unwrap()
    }

    #[test]
    fn exited_agent_with_a_live_shell_stays_listed() {
        use control::AgentState::{Exited, Unknown, Working};
        assert_eq!(card_state(Exited, false), Unknown);
        assert_eq!(card_state(Exited, true), Exited);
        assert_eq!(card_state(Working, false), Working);
    }

    #[test]
    #[cfg(unix)]
    fn save_quick_commands_preserves_settings_and_creates_private_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = quick_commands_test_dir();
        let mut backend = test_backend();
        backend.dir = Some(dir.clone());
        let commands = json!([{"name":"  orchestration  ","text":"\r\n/krill-orchestration \r\n"}]);
        let expected = json!([{"name":"orchestration","text":"/krill-orchestration "}]);
        let saved = rpc(
            &backend,
            &json!({"action":"save_quick_commands","commands":commands}),
        );
        assert_eq!(saved["ok"], true);
        assert_eq!(saved["data"]["quick_commands"], expected);
        let path = dir.join("settings.toml");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(quick_commands_json(Some(&dir)), expected);

        let preserved = "# user preferences\nterminal_font_size = 15 # keep font\nfuture = { enabled = true }\n\n[[webhooks]]\n# keep webhook\nurl = 'https://example.test/hook'\nevents = ['done']\n";
        fs::write(&path, format!("{preserved}\n[[quick_commands]]\nname = 'old'\ntext = 'old text'\nfuture_command = 42\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let saved = rpc(
            &backend,
            &json!({"action":"save_quick_commands","commands":[{"name":"new","text":"\n  prompt\r\n\tline\rtail \n"}]}),
        );
        assert_eq!(saved["ok"], true);
        assert_eq!(
            saved["data"]["quick_commands"],
            json!([{"name":"new","text":"  prompt\n\tline\ntail "}])
        );
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.starts_with(preserved));
        assert!(content.contains("future_command = 42"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            quick_commands_json(Some(&dir)),
            saved["data"]["quick_commands"]
        );
        let cleared = rpc(
            &backend,
            &json!({"action":"save_quick_commands","commands":[]}),
        );
        assert_eq!(cleared["ok"], true);
        assert_eq!(quick_commands_json(Some(&dir)), json!([]));
        assert!(fs::read_to_string(&path).unwrap().starts_with(preserved));
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["settings.toml", "settings.toml.lock"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn snapshot_quick_commands_refresh_after_mobile_save_and_desktop_edit() {
        let dir = quick_commands_test_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("settings.toml"),
            "[[quick_commands]]\nname = 'desktop'\ntext = '/desktop '\n",
        )
        .unwrap();
        let registry = Arc::new(Registry::new(None, None));
        let mut backend = test_backend();
        backend.dir = Some(dir.clone());
        backend.registry = Arc::downgrade(&registry);
        let first = rpc(&backend, &json!({"action":"snapshot"}));
        assert_eq!(first["ok"], true);
        assert_eq!(first["data"]["capabilities"]["quick_commands"], true);
        assert_eq!(
            first["data"]["quick_commands"],
            json!([{"name":"desktop","text":"/desktop "}])
        );
        assert!(crate::session::lock(&backend.snapshot_cache).is_some());
        let etag = first["data"]["etag"].as_str().unwrap();
        assert_eq!(
            backend.snapshot_since(Some(etag)).unwrap()["unchanged"],
            true
        );

        let saved = rpc(
            &backend,
            &json!({"action":"set_quick_commands","commands":[{"name":"mobile","text":"/mobile "}]}),
        );
        assert_eq!(saved["ok"], true);
        assert!(crate::session::lock(&backend.snapshot_cache).is_none());
        let refreshed = backend.snapshot_since(Some(etag)).unwrap();
        assert_ne!(refreshed["etag"], first["data"]["etag"]);
        assert_eq!(refreshed["quick_commands"], saved["data"]["quick_commands"]);

        let replacement = dir.join("settings.new");
        fs::write(
            &replacement,
            "[[quick_commands]]\nname = 'desktop edit'\ntext = '/desktop-updated '\n",
        )
        .unwrap();
        fs::rename(replacement, dir.join("settings.toml")).unwrap();
        let desktop = backend.snapshot_since(refreshed["etag"].as_str()).unwrap();
        assert_ne!(desktop["etag"], refreshed["etag"]);
        assert_eq!(
            desktop["quick_commands"],
            json!([{"name":"desktop edit","text":"/desktop-updated "}])
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn quick_commands_wait_for_an_external_settings_writer_and_preserve_its_changes() {
        let dir = quick_commands_test_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        fs::write(&path, "terminal_font_size = 15\n").unwrap();
        let guard = krill_proto::config_document::lock_toml(&path).unwrap();
        let mut backend = test_backend();
        backend.dir = Some(dir.clone());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = backend.save_quick_commands(vec![QuickCommandEntry {
                name: "mobile".into(),
                text: "/skill ".into(),
            }]);
            done_tx.send(result).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        fs::write(&path, "terminal_font_size = 19\nfuture_flag = true\n").unwrap();
        drop(guard);
        done_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
        let saved: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["terminal_font_size"].as_integer(), Some(19));
        assert_eq!(saved["future_flag"].as_bool(), Some(true));
        assert_eq!(saved["quick_commands"][0]["text"].as_str(), Some("/skill "));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_quick_commands_rejects_invalid_requests_without_changing_settings() {
        let dir = quick_commands_test_dir();
        fs::create_dir_all(&dir).unwrap();
        let original = "terminal_font_size = 15\n";
        let path = dir.join("settings.toml");
        fs::write(&path, original).unwrap();
        let mut backend = test_backend();
        backend.dir = Some(dir.clone());
        for commands in [
            json!([{"name":" \t ","text":"valid"}]),
            json!([{"name":"valid","text":" \r\n\t"}]),
            json!([{"name":"invalid\nname","text":"valid"}]),
            json!([{"name":"invalid\u{7f}name","text":"valid"}]),
            json!([{"name":"valid","text":"invalid\u{1b}text"}]),
            json!([{"name":"valid","text":"invalid\u{0}text"}]),
            json!([{"name":"x".repeat(121),"text":"valid"}]),
            json!([{"name":"한".repeat(121),"text":"valid"}]),
            json!([{"name":"valid","text":"x".repeat(16 * 1024 + 1)}]),
            json!(vec![json!({"name":"valid","text":"valid"}); 129]),
            json!([{"name":"missing text"}]),
        ] {
            let response = rpc(
                &backend,
                &json!({"action":"save_quick_commands","commands":commands}),
            );
            assert_eq!(response["ok"], false, "{response}");
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
        let maximum = json!([{"name":"x".repeat(120),"text":"x".repeat(16 * 1024)}]);
        assert_eq!(
            rpc(
                &backend,
                &json!({"action":"save_quick_commands","commands":maximum})
            )["ok"],
            true
        );
        assert_eq!(
            rpc(
                &backend,
                &json!({"action":"set_quick_commands","commands":[{"name":"한".repeat(120),"text":"cargo check"}]})
            )["ok"],
            true
        );
        let maximum_count = json!(vec![json!({"name":"valid","text":"valid"}); 128]);
        assert_eq!(
            rpc(
                &backend,
                &json!({"action":"save_quick_commands","commands":maximum_count})
            )["ok"],
            true
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_quick_commands_normalizes_filters_caps_and_tolerates_missing_or_invalid_files() {
        assert!(load_quick_commands(None).is_empty());
        let dir = quick_commands_test_dir();
        assert!(load_quick_commands(Some(&dir)).is_empty());
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        fs::write(&path, "invalid [").unwrap();
        assert!(load_quick_commands(Some(&dir)).is_empty());
        fs::write(&path, "terminal_font_size = 15\n").unwrap();
        assert!(load_quick_commands(Some(&dir)).is_empty());
        let mut commands = vec![
            QuickCommandEntry {
                name: "  ".into(),
                text: "valid".into(),
            },
            QuickCommandEntry {
                name: "valid".into(),
                text: " \r\n\t".into(),
            },
        ];
        commands.extend((0..130).map(|i| QuickCommandEntry {
            name: format!(" command {i} "),
            text: "\r\n  /skill \r\nnext\rtail \r\n".into(),
        }));
        fs::write(
            &path,
            toml::to_string(&QuickCommandsFile {
                quick_commands: commands,
            })
            .unwrap(),
        )
        .unwrap();
        let loaded = load_quick_commands(Some(&dir));
        assert_eq!(loaded.len(), 128);
        assert_eq!(loaded.first().unwrap().name, "command 0");
        assert_eq!(loaded.first().unwrap().text, "  /skill \nnext\ntail ");
        assert_eq!(loaded.last().unwrap().name, "command 127");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn historical_approval_and_question_requests_fail_closed_before_input() {
        let backend = test_backend();
        // No Registry is present: these errors prove no attach or PTY write was attempted.
        for request in [
            r#"{"action":"approve","session":7,"value":"y"}"#,
            r#"{"action":"approve","session":7,"value":"n"}"#,
            r#"{"action":"answer_question","session":7,"selected":[0],"option_count":2}"#,
            r#"{"action":"answer_question","session":7,"selected":[1],"option_count":2,"multi_select":true,"custom_text":"Other"}"#,
        ] {
            for _ in 0..2 {
                let response: Value = serde_json::from_str(&backend.request(request)).unwrap();
                assert_eq!(response["ok"], false);
                assert!(
                    response["error"]
                        .as_str()
                        .unwrap()
                        .contains("Confirm in Term")
                );
            }
        }
    }

    #[test]
    fn prompt_accepts_one_prepared_photo_and_rejects_bad_preparation_before_input() {
        assert!(
            serde_json::from_str::<Action>(r#"{"action":"prompt","session":7,"text":"plain"}"#)
                .is_ok()
        );
        let request = r#"{"action":"prompt","session":7,"text":"image then text","photo":{"mime":"image/jpeg","data":"Zg=="}}"#;
        let Action::Prompt {
            text,
            photo: Some(photo),
            ..
        } = serde_json::from_str::<Action>(request).unwrap()
        else {
            panic!("prepared photo must survive JSON decoding");
        };
        assert_eq!(text, "image then text");
        assert_eq!(photo.mime, "image/jpeg");
        assert_eq!(photo.data, "Zg==");
        let backend = test_backend();
        for photo in [
            json!({"mime":"text/html","data":"Zg=="}),
            json!({"mime":"image/jpeg","data":"invalid"}),
        ] {
            let response: Value = serde_json::from_str(
                &backend.request(
                    &json!({"action":"prompt","session":7,"text":"keep draft","photo":photo})
                        .to_string(),
                ),
            )
            .unwrap();
            assert_eq!(response["ok"], false);
        }
    }

    #[test]
    fn mobile_chat_small_append_pages_replay_with_original_display_ids() {
        use crate::agent_history::chat::{poll_transcript_file, replay_page};
        use krill_proto::agent_history::VaultAgent;
        let dir = std::env::temp_dir().join(format!("mobile-replay-pages-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        let mut offset = 0;
        let mut observed = Vec::new();
        for index in 0..300 {
            let text = if index == 48 || index == 49 {
                "same text".to_owned()
            } else {
                format!("message {index}")
            };
            writeln!(
                file,
                "{}",
                json!({"type":"user","message":{"role":"user","content":text}})
            )
            .unwrap();
            let snapshot = poll_transcript_file(VaultAgent::Claude, "sid", &path, offset);
            assert_eq!(snapshot.items.len(), 1);
            let end = snapshot.next_offset;
            observed.push((offset, end, chat_page_value(snapshot, offset).unwrap()));
            offset = end;
        }
        for (start, end, original) in &observed {
            // Reading after all appends would return up to 128 records; replay retains
            // the original one-record partition and ID rather than comparing text.
            let current = poll_transcript_file(VaultAgent::Claude, "sid", &path, *start);
            let replay = replay_page(current, *start, *end).unwrap();
            assert_eq!(chat_page_value(replay, *start).unwrap(), *original);
        }
        let a = observed.get(48).unwrap();
        let b = observed.get(49).unwrap();
        assert_eq!(a.2["items"][0]["text"], b.2["items"][0]["text"]);
        assert_ne!(a.2["items"][0]["id"], b.2["items"][0]["id"]);
        let action: Action = serde_json::from_value(json!({
            "action":"chat_poll","session":7,"offset":a.0,"end_offset":a.1
        }))
        .unwrap();
        assert!(matches!(action, Action::ChatPoll { end_offset: Some(end), .. } if end == a.1));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn mobile_chat_page_identity_is_additive_and_replays_without_changing_legacy_snapshot() {
        use krill_proto::agent_history::{ChatItem, ChatPollSnapshot};
        let snapshot = ChatPollSnapshot {
            items: vec![ChatItem {
                text: "stable".into(),
                ..ChatItem::default()
            }],
            next_offset: 64,
            bound: true,
            ..ChatPollSnapshot::default()
        };
        let legacy = serde_json::to_value(&snapshot).unwrap();
        assert!(legacy["items"][0].get("id").is_none());
        let page = chat_page_value(snapshot.clone(), 32).unwrap();
        assert_eq!(page, chat_page_value(snapshot.clone(), 32).unwrap());
        assert_eq!(page["items"][0]["id"], "32:0");
        assert_eq!(page["reply_mode"], "review_in_term");
        let reset = chat_page_value(
            ChatPollSnapshot {
                reset: true,
                ..snapshot
            },
            512,
        )
        .unwrap();
        assert_eq!(reset["page_start"], 0);
        assert_eq!(reset["items"][0]["id"], "0:0");
    }

    #[test]
    fn bracketed_paste_and_question_keystroke_planner_work() {
        assert_eq!(
            format_bracketed_paste("hi\x1b[201~\nthere"),
            b"\x1b[200~hi[201~\nthere\x1b[201~"
        );
        // 단일 선택 2번째(인덱스 1): Down 1번 + Enter
        let single = plan_question_keystrokes(&[1], 3, false, None, true).unwrap();
        assert_eq!(single, vec![b"\x1b[B".to_vec(), b"\r".to_vec()]);

        // TUI에서 이미 2번째(인덱스 1)로 커서가 내려간 상태에서 1번째(인덱스 0)를 고르면 Up 1번 + Enter
        let screen = "Which option?\n  1. First\n❯ 2. Second\n  3. Third\n";
        let detected = detect_question_cursor_index(screen, 3);
        assert_eq!(detected, 1);
        let relative_up =
            plan_question_keystrokes_from(&[0], 3, false, None, true, detected).unwrap();
        assert_eq!(relative_up, vec![b"\x1b[A".to_vec(), b"\r".to_vec()]);

        // 다중 선택 [0, 2]: Space, Down, Down, Space, Enter
        let multi = plan_question_keystrokes(&[2, 0], 3, true, None, true).unwrap();
        assert_eq!(
            multi,
            vec![
                b" ".to_vec(),
                b"\x1b[B".to_vec(),
                b"\x1b[B".to_vec(),
                b" ".to_vec(),
                b"\r".to_vec()
            ]
        );

        // 커스텀 텍스트 입력(옵션 2개 뒤 Other): Down, Down, Enter, Paste, Enter
        let custom = plan_question_keystrokes(&[], 2, false, Some("직접 입력"), true).unwrap();
        assert_eq!(
            custom,
            vec![
                b"\x1b[B".to_vec(),
                b"\x1b[B".to_vec(),
                b"\r".to_vec(),
                b"\x1b[200~\xec\xa7\x81\xec\xa0\x91 \xec\x9e\x85\xeb\xa0\xa5\x1b[201~".to_vec(),
                b"\r".to_vec(),
            ]
        );
    }

    #[test]
    fn snapshot_groups_use_deepest_path_components_and_keep_empty_repositories() {
        let repositories = vec![
            control::RepositoryTrees {
                repo: b"/repo".to_vec(),
                worktrees: vec![b"/repo/a".to_vec(), b"/repo/a/nested".to_vec()],
            },
            control::RepositoryTrees {
                repo: b"/empty".to_vec(),
                worktrees: vec![],
            },
        ];
        let group = session_group(SessionId(1), b"/repo/a/nested/src", &repositories);
        assert_eq!(group.repo_key, Some(hex(b"/repo")));
        assert_eq!(group.worktree_key, hex(b"/repo/a/nested"));
        assert_eq!(group.label.as_deref(), Some("nested"));
        assert_eq!(group.path.as_deref(), Some("/repo/a/nested"));
        let group = session_group(SessionId(2), b"/repo/abc", &repositories);
        assert_eq!(group.repo_key, None);
        assert_eq!(group.worktree_key, hex(b"/repo/abc"));
        let outside = session_group(SessionId(3), b"/outside/sub", &repositories);
        assert_eq!(outside.repo_key, None);
        assert_eq!(outside.path.as_deref(), Some("/outside/sub"));
        let listed =
            super::super::workspaces::Catalog::load(None, &repositories).repositories_json();
        let empty = listed
            .iter()
            .find(|repo| repo["key"] == hex(b"/empty"))
            .unwrap();
        assert_eq!(empty["worktree_keys"], json!([]));
    }

    #[test]
    fn snapshot_group_keys_preserve_non_utf8_bytes_and_unknown_sessions() {
        let first = session_group(SessionId(1), b"/tmp/\xff", &[]);
        let second = session_group(SessionId(2), b"/tmp/\xfe", &[]);
        assert_eq!(first.path, second.path);
        assert_ne!(first.worktree_key, second.worktree_key);
        let first = session_group(SessionId(1), &[], &[]);
        let second = session_group(SessionId(2), &[], &[]);
        assert_ne!(first.worktree_key, second.worktree_key);
        assert!(first.repo_key.is_none());
        assert!(first.path.is_none());
        assert!(first.label.is_none());
    }

    #[test]
    fn snapshot_agents_follow_registry_profiles_in_standard_order() {
        let agents_json = |dir: Option<&Path>| -> Value {
            format_agents_json(
                crate::agent::control::Control::new(dir)
                    .agents()
                    .ok()
                    .as_ref(),
            )
        };
        let ids = |v: Value| -> Vec<String> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a["id"].as_str().map(str::to_owned))
                .collect()
        };
        let builtin = agents_json(None);
        assert_eq!(builtin[0], json!({"id":"claude","name":"Claude Code"}));
        assert_eq!(
            ids(builtin),
            ["claude", "codex", "agy", "cursor", "opencode"]
        );
        let dir = std::env::temp_dir().join(format!("mk-pwa-agents-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(krill_proto::agent::FILE_NAME),
            "[agent.my-bot]\nname = \"My Bot\"\nlaunch = [\"my-bot\"]\n",
        )
        .unwrap();
        let user = agents_json(Some(&dir));
        assert_eq!(
            user.as_array().unwrap().last(),
            Some(&json!({"id":"my-bot","name":"My Bot"}))
        );
        fs::write(dir.join(krill_proto::agent::FILE_NAME), "not toml [").unwrap();
        assert_eq!(agents_json(Some(&dir)), json!([]));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mobile_git_paths_untracked_and_unborn_head_have_real_patches() {
        let dir = std::env::temp_dir().join(format!("mk-mobile-git-audit-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(dir.join("sub")).unwrap();
        let root = fs::canonicalize(&dir).unwrap();
        let cwd = root.join("sub");
        assert_eq!(
            git_root(&cwd).unwrap_err().to_string(),
            "Not a Git repository"
        );
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        fs::write(cwd.join("a.txt"), "one\ntwo").unwrap();
        assert_eq!(git_root(&cwd).unwrap(), root);
        let files = vec!["sub/a.txt".to_owned()];
        for raw in [
            "sub/a.txt".to_owned(),
            "a.txt".into(),
            "./a.txt".into(),
            cwd.join("a.txt").to_string_lossy().into_owned(),
            dir.join("sub/a.txt").to_string_lossy().into_owned(),
        ] {
            assert_eq!(
                git_relative_path(&root, &cwd, &raw, &files).as_deref(),
                Some("sub/a.txt"),
                "{raw}"
            );
        }
        assert!(git_relative_path(&root, &cwd, "../../escape", &files).is_none());
        // A truncated status list still permits a valid cwd-relative path.
        assert_eq!(
            git_relative_path(&root, &cwd, "./a.txt", &[]).as_deref(),
            Some("sub/a.txt")
        );
        let patch = read_patch(&root, "sub/a.txt").unwrap();
        assert!(
            patch.starts_with("--- /dev/null\n+++ b/sub/a.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n"),
            "{patch}"
        );
        assert!(patch.contains("\\ No newline at end of file"));
        assert!(
            Command::new("git")
                .args(["add", "sub/a.txt"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        let staged = read_patch(&root, "sub/a.txt").unwrap();
        assert!(staged.contains("+one"), "{staged}");
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.email=fixture@example.com",
                    "-c",
                    "user.name=Fixture",
                    "commit",
                    "-qm",
                    "fixture"
                ])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        fs::write(cwd.join("a.txt"), "changed\n").unwrap();
        assert!(read_patch(&root, "sub/a.txt").unwrap().contains("+changed"));
        fs::remove_file(cwd.join("a.txt")).unwrap();
        assert_eq!(
            git_relative_path(&root, &cwd, "./a.txt", &files).as_deref(),
            Some("sub/a.txt")
        );
        assert!(
            read_patch(&root, "sub/a.txt")
                .unwrap()
                .contains("--- a/sub/a.txt")
        );
        fs::write(root.join("binary"), b"a\0b").unwrap();
        assert!(
            read_patch(&root, "binary")
                .unwrap()
                .contains("Binary files")
        );
        fs::write(root.join("large"), vec![b'x'; 128 * 1024 + 1]).unwrap();
        assert!(read_patch(&root, "large").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("binary"), root.join("link")).unwrap();
            assert_eq!(
                git_relative_path(&root, &cwd, root.join("link").to_str().unwrap(), &[]).as_deref(),
                Some("link")
            );
            assert!(read_patch(&root, "link").unwrap().is_empty());
            std::os::unix::fs::symlink(&root, dir.with_extension("alias")).unwrap();
            assert_eq!(
                git_relative_path(
                    &root,
                    &cwd,
                    dir.with_extension("alias")
                        .join("sub/a.txt")
                        .to_str()
                        .unwrap(),
                    &files
                )
                .as_deref(),
                Some("sub/a.txt")
            );
            fs::remove_file(dir.with_extension("alias")).unwrap();
        }
        fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn mobile_git_cache_stays_bounded_even_for_late_background_results() {
        let backend = test_backend();
        for i in 0..100 {
            cache_git_summary(
                &backend.git_cache,
                PathBuf::from(format!("/fixture/{i}")),
                json!(i),
            );
        }
        let cache = crate::session::lock(&backend.git_cache);
        assert_eq!(cache.len(), 64);
        assert!(cache.contains_key(Path::new("/fixture/99")));
        assert!(!cache.contains_key(Path::new("/fixture/0")));
    }
    fn test_backend() -> Backend {
        Backend::new(
            Weak::new(),
            None,
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        )
    }

    #[test]
    #[cfg(unix)] // git CLI
    fn card_git_skips_non_repositories_dedups_cwds_and_expires_cache() {
        let outside = session_group(SessionId(1), b"/tmp/not-a-repo", &[]);
        assert_eq!(git_cwd(b"/tmp/not-a-repo", &outside), None);
        assert_eq!(git_cwd(&[], &session_group(SessionId(2), &[], &[])), None);

        let dir = std::env::temp_dir().join(format!("mk-pwa-git-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        };
        git(&["init", "-q", "-b", "card"]);
        fs::write(dir.join("f"), "a\n").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-qm", "f"]);
        fs::write(dir.join("f"), "a\nb\n").unwrap();
        let repositories = vec![control::RepositoryTrees {
            repo: dir.as_os_str().as_bytes().to_vec(),
            worktrees: vec![dir.as_os_str().as_bytes().to_vec()],
        }];
        let inside = session_group(SessionId(3), dir.as_os_str().as_bytes(), &repositories);
        assert_eq!(
            git_cwd(dir.as_os_str().as_bytes(), &inside),
            Some(dir.clone())
        );

        let backend = test_backend();
        let expected = json!({"branch":"card","added":1,"deleted":0});
        let first = backend.git_summaries([dir.clone(), dir.clone()]);
        assert_eq!(first.len(), 1, "same cwd shares one query");
        assert_eq!(first[&dir], expected);
        fs::write(dir.join("f"), "a\nb\nc\n").unwrap();
        assert_eq!(
            backend.git_summaries([dir.clone()])[&dir],
            expected,
            "cached"
        );
        for (at, _) in crate::session::lock(&backend.git_cache).values_mut() {
            *at = Instant::now().checked_sub(GIT_TTL).unwrap();
        }
        assert_eq!(
            backend.git_summaries([dir.clone()])[&dir],
            json!({"branch":"card","added":2,"deleted":0}),
            "expired entry is queried again"
        );
        let missing = dir.join("missing");
        assert_eq!(
            backend.git_summaries([missing.clone()])[&missing],
            Value::Null
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(unix)] // git CLI shell alias gates the shared queue for this cwd.
    fn card_git_swr_returns_cold_and_expired_values_before_background_refresh() {
        let dir = std::env::temp_dir().join(format!("mk-pwa-git-swr-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let output = Command::new("git")
            .args(["init", "-q", "-b", "swr"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let backend = test_backend();
        let expected = json!({"branch":"swr","added":0,"deleted":0});
        let stale = json!({"branch":"old","added":7,"deleted":3});
        for generation in 1..=2 {
            if generation == 2 {
                crate::session::lock(&backend.git_cache).insert(
                    dir.clone(),
                    (Instant::now().checked_sub(GIT_TTL).unwrap(), stale.clone()),
                );
            }
            // Keep the real Git query queued until both immediate responses are checked.
            let gate = crate::git::queue().unwrap().submit(dir.clone(), [
                "-c", "alias.swr-gate=!touch .git/swr-started; while test ! -e .git/swr-release; do sleep 0.01; done",
                "swr-gate",
            ]);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !dir.join(".git/swr-started").exists() {
                assert!(Instant::now() < deadline, "queue gate did not start");
                thread::sleep(Duration::from_millis(5));
            }
            let started = Instant::now();
            let found = backend.git_summaries_swr([dir.clone(), dir.clone()]);
            assert!(started.elapsed() < Duration::from_millis(500));
            assert_eq!(found.len(), 1);
            let immediate = if generation == 1 {
                Value::Null
            } else {
                stale.clone()
            };
            assert_eq!(found[&dir], immediate);
            assert_eq!(backend.git_summaries_swr([dir.clone()])[&dir], immediate);
            assert_eq!(crate::session::lock(&backend.git_inflight).len(), 1);
            assert_eq!(backend.git_revision.load(Ordering::Relaxed), generation - 1);
            fs::remove_file(dir.join(".git/swr-started")).unwrap();
            fs::write(dir.join(".git/swr-release"), "").unwrap();
            assert!(
                gate.recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap()
                    .status
                    .success()
            );
            fs::remove_file(dir.join(".git/swr-release")).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while backend.git_revision.load(Ordering::Relaxed) < generation {
                assert!(
                    Instant::now() < deadline,
                    "background refresh did not complete"
                );
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(backend.git_revision.load(Ordering::Relaxed), generation);
            assert!(crate::session::lock(&backend.git_inflight).is_empty());
            assert_eq!(crate::session::lock(&backend.git_cache)[&dir].1, expected);
            assert_eq!(backend.git_summaries_swr([dir.clone()])[&dir], expected);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn host_metadata_reports_only_actual_hostname_and_platform() {
        let host = host_metadata();
        let expected = Command::new(if cfg!(windows) {
            "hostname.exe"
        } else {
            "/bin/hostname"
        })
        .output()
        .unwrap();
        assert_eq!(
            host["display_name"],
            String::from_utf8(expected.stdout).unwrap().trim()
        );
        assert_eq!(host["platform"], std::env::consts::OS);
    }

    #[test]
    fn terminal_actions_reject_phone_supplied_paths_and_programs() {
        for request in [
            r#"{"action":"create_terminal","cwd_session":1,"cwd":"/tmp"}"#,
            r#"{"action":"create_terminal","cwd_session":1,"program":"/bin/sh"}"#,
            r#"{"action":"close_terminal","session":1,"extra":true}"#,
        ] {
            assert!(
                serde_json::from_str::<Action>(request).is_err(),
                "{request}"
            );
        }
        // 생성 출처는 기존 세션 또는 등록 워크스페이스 중 하나다.
        for request in [
            r#"{"action":"create_terminal","cwd_session":1}"#,
            r#"{"action":"create_terminal","workspace":"registered-key"}"#,
        ] {
            assert!(serde_json::from_str::<Action>(request).is_ok(), "{request}");
        }
        for request in [
            r#"{"action":"create_terminal"}"#,
            r#"{"action":"create_terminal","cwd_session":1,"workspace":"key"}"#,
        ] {
            let reply: Value = serde_json::from_str(&test_backend().request(request)).unwrap();
            assert_eq!(reply["ok"], false, "{request}");
        }
    }

    #[test]
    fn terminal_creation_requires_one_registered_workspace_or_session() {
        let backend = test_backend();
        for request in [
            r#"{"action":"create_terminal"}"#,
            r#"{"action":"create_terminal","cwd_session":1,"workspace":"key"}"#,
            r#"{"action":"create_workspace","repository":"key","name":" "}"#,
        ] {
            let reply: Value = serde_json::from_str(&backend.request(request)).unwrap();
            assert_eq!(reply["ok"], false, "{request}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn create_workspace_on_non_git_folder_project_spawns_session_and_records_title() {
        use crate::holder::{Holder, link::Link};
        use crate::os::{AsFd as _, OwnedFd};
        use krill_proto::holder::{FromHolder, ToHolder, VERSION};

        let dir = std::env::temp_dir().join(format!("mk-pwa-folder-ws-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let state_dir = dir.join("state");
        let folder = dir.join("구글드라이브").join("내 드라이브");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&folder).unwrap();
        let canonical_folder = fs::canonicalize(&folder).unwrap();
        fs::write(
            state_dir.join("projects.toml"),
            format!(
                "[[projects]]\nid = \"gdrive\"\nname = \"구글드라이브\"\npath = {:?}\nkind = \"folder\"\n",
                canonical_folder.to_string_lossy()
            ),
        )
        .unwrap();

        let (daemon_end, holder_end) = UnixStream::pair().unwrap();
        let mut holder_link = Link::new(OwnedFd::from(holder_end));
        for message in [
            FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            },
            FromHolder::Ready,
        ] {
            holder_link.send(&message.encode().unwrap(), &[]).unwrap();
        }
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let registry = Arc::new(Registry::new(Some((holder, vec![])), Some(&state_dir)));
        let reader_reg = Arc::clone(&registry);
        let reader = std::thread::spawn(move || reader_reg.run_holder(link));
        let holder_worker = std::thread::spawn(move || {
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Spawn { req, id, cwd, .. } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!("expected Spawn");
            };
            let (master, _pty) = UnixStream::pair().unwrap();
            holder_link
                .send(
                    &FromHolder::Spawned {
                        req,
                        id,
                        pid: std::process::id(),
                    }
                    .encode()
                    .unwrap(),
                    &[master.as_fd()],
                )
                .unwrap();
            cwd
        });

        let backend = Backend::new(
            Arc::downgrade(&registry),
            Some(&state_dir),
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        let repo_key = hex(canonical_folder.as_os_str().as_bytes());
        let request = json!({
            "action": "create_workspace",
            "repository": repo_key,
            "name": "보험",
        })
        .to_string();
        let reply: Value = serde_json::from_str(&backend.request(&request)).unwrap();
        let spawned_cwd = holder_worker.join().unwrap();
        reader.join().unwrap();

        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["data"]["workspace"], repo_key);
        assert_eq!(
            reply["data"]["path"],
            canonical_folder.to_string_lossy().as_ref()
        );
        assert_eq!(reply["data"]["branch"], "보험");
        assert_eq!(spawned_cwd, canonical_folder.as_os_str().as_bytes());
        let session_id = reply["data"]["session"].as_u64().expect("session spawned");
        let titles = krill_proto::git::load_session_auto_titles(&state_dir);
        assert_eq!(
            titles.get(&SessionId(session_id)).map(String::as_str),
            Some("보험")
        );

        drop(backend);
        drop(registry);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn snapshot_and_session_reads_populate_from_live_registry_in_process() {
        use crate::holder::{Holder, link::Link};
        use crate::os::{AsFd as _, OwnedFd};
        use krill_proto::holder::{FromHolder, ToHolder, VERSION};

        let dir = std::env::temp_dir().join(format!("mk-pwa-live-snap-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let state_dir = dir.join("state");
        let repo_dir = dir.join("repo");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&repo_dir).unwrap();
        let canonical_repo = fs::canonicalize(&repo_dir).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .current_dir(&canonical_repo)
                .status()
                .unwrap()
                .success()
        );
        fs::write(
            state_dir.join("projects.toml"),
            format!(
                "[[projects]]\nid = \"repo\"\nname = \"Repo\"\npath = {:?}\n",
                canonical_repo.to_string_lossy()
            ),
        )
        .unwrap();
        fs::write(
            state_dir.join(krill_proto::agent::FILE_NAME),
            "[agent.my-bot]\nname = \"My Bot\"\nlaunch = [\"my-bot\"]\n",
        )
        .unwrap();

        let mut child = Command::new("/bin/sleep")
            .arg("60")
            .current_dir(&canonical_repo)
            .spawn()
            .unwrap();
        let child_pid = child.id();
        let (daemon_end, holder_end) = UnixStream::pair().unwrap();
        let mut holder_link = Link::new(OwnedFd::from(holder_end));
        for message in [
            FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            },
            FromHolder::Ready,
        ] {
            holder_link.send(&message.encode().unwrap(), &[]).unwrap();
        }
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let registry = Arc::new(Registry::new(Some((holder, vec![])), Some(&state_dir)));
        let reader_reg = Arc::clone(&registry);
        let reader = std::thread::spawn(move || reader_reg.run_holder(link));
        let holder_worker = std::thread::spawn(move || {
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Spawn { req, id, cwd, .. } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!("expected Spawn");
            };
            let (master, pty) = UnixStream::pair().unwrap();
            holder_link
                .send(
                    &FromHolder::Spawned {
                        req,
                        id,
                        pid: child_pid,
                    }
                    .encode()
                    .unwrap(),
                    &[master.as_fd()],
                )
                .unwrap();
            (cwd, pty)
        });

        let backend = Backend::new(
            Arc::downgrade(&registry),
            Some(&state_dir),
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        let repo_key = hex(canonical_repo.as_os_str().as_bytes());
        let create_reply: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "create_terminal",
                    "workspace": repo_key,
                    "agent": "my-bot",
                })
                .to_string(),
            ),
        )
        .unwrap();
        let (spawned_cwd, _pty) = holder_worker.join().unwrap();
        reader.join().unwrap();

        assert_eq!(create_reply["ok"], true, "{create_reply}");
        assert_eq!(spawned_cwd, canonical_repo.as_os_str().as_bytes());
        let session_id = create_reply["data"]["session"]
            .as_u64()
            .expect("session id");

        let snap: Value =
            serde_json::from_str(&backend.request(r#"{"action":"snapshot"}"#)).unwrap();
        assert_eq!(snap["ok"], true, "{snap}");
        let sessions = snap["data"]["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["id"], session_id);
        assert_eq!(sessions[0]["exited"], false);
        assert_eq!(sessions[0]["agent"], "my-bot");
        assert_eq!(sessions[0]["profile_id"], "my-bot");
        assert_eq!(
            sessions[0]["cwd"],
            canonical_repo.to_string_lossy().as_ref()
        );
        assert_eq!(sessions[0]["repo_key"], repo_key);

        let repositories = snap["data"]["repositories"].as_array().unwrap();
        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0]["key"], repo_key);

        let workspaces = snap["data"]["workspaces"].as_array().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(
            workspaces[0]["path"],
            canonical_repo.to_string_lossy().as_ref()
        );

        let agents = snap["data"]["agents"].as_array().unwrap();
        assert_eq!(
            agents.first(),
            Some(&json!({"id":"claude","name":"Claude Code"}))
        );
        assert_eq!(agents.last(), Some(&json!({"id":"my-bot","name":"My Bot"})));

        assert_eq!(backend.cwd(session_id).unwrap(), canonical_repo);
        assert_eq!(
            backend.cwd(session_id + 999).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let context = backend.composer_context(session_id).unwrap();
        assert_eq!(context.cwd, canonical_repo);
        assert_eq!(context.agent, "my-bot");

        let deadline = Instant::now() + Duration::from_secs(5);
        while !crate::session::lock(&backend.git_inflight).is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        drop(backend);
        drop(registry);
        child.kill().ok();
        child.wait().ok();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn mobile_interrupt_writes_ctrl_c_only_while_session_is_working() {
        use crate::holder::{Holder, link::Link};
        use crate::os::{AsFd as _, OwnedFd};
        use krill_proto::holder::{FromHolder, ToHolder, VERSION};
        use krill_proto::hook::{EventId, HookEvent, Input};
        use std::io::Read as _;

        struct Cleanup {
            child: Option<std::process::Child>,
            dir: std::path::PathBuf,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(child) = self.child.as_mut() {
                    child.kill().ok();
                    child.wait().ok();
                }
                fs::remove_dir_all(&self.dir).ok();
            }
        }

        let dir = std::env::temp_dir().join(format!("mk-pwa-interrupt-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let mut cleanup = Cleanup {
            child: None,
            dir: dir.clone(),
        };
        let state_dir = dir.join("state");
        let repo_dir = dir.join("repo");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&repo_dir).unwrap();
        let canonical_repo = fs::canonicalize(&repo_dir).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .current_dir(&canonical_repo)
                .status()
                .unwrap()
                .success()
        );
        fs::write(
            state_dir.join("projects.toml"),
            format!(
                "[[projects]]\nid = \"repo\"\nname = \"Repo\"\npath = {:?}\n",
                canonical_repo.to_string_lossy()
            ),
        )
        .unwrap();

        let child = Command::new("/bin/sleep")
            .arg("60")
            .current_dir(&canonical_repo)
            .spawn()
            .unwrap();
        let child_pid = child.id();
        cleanup.child = Some(child);
        let (daemon_end, holder_end) = UnixStream::pair().unwrap();
        let mut holder_link = Link::new(OwnedFd::from(holder_end));
        for message in [
            FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            },
            FromHolder::Ready,
        ] {
            holder_link.send(&message.encode().unwrap(), &[]).unwrap();
        }
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let registry = Arc::new(Registry::new(Some((holder, vec![])), Some(&state_dir)));
        let reader_reg = Arc::clone(&registry);
        let reader = std::thread::spawn(move || reader_reg.run_holder(link));
        let holder_worker = std::thread::spawn(move || {
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Spawn { req, id, .. } = ToHolder::decode(packet.frame()).unwrap() else {
                panic!("expected Spawn");
            };
            let (master, pty) = UnixStream::pair().unwrap();
            holder_link
                .send(
                    &FromHolder::Spawned {
                        req,
                        id,
                        pid: child_pid,
                    }
                    .encode()
                    .unwrap(),
                    &[master.as_fd()],
                )
                .unwrap();
            (holder_link, master, pty)
        });

        let backend = Backend::new(
            Arc::downgrade(&registry),
            Some(&state_dir),
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        let repo_key = hex(canonical_repo.as_os_str().as_bytes());
        let create_reply: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "create_terminal",
                    "workspace": repo_key,
                })
                .to_string(),
            ),
        )
        .unwrap();
        let (holder_link, master, mut pty) = holder_worker.join().unwrap();
        drop(master);
        pty.set_nonblocking(true).unwrap();

        let session_id = create_reply["data"]["session"]
            .as_u64()
            .expect("session id");
        let snap: Value =
            serde_json::from_str(&backend.request(r#"{"action":"snapshot"}"#)).unwrap();
        assert_eq!(snap["data"]["capabilities"]["chat_interrupt"], true);

        // Unknown / idle sessions must not receive Ctrl+C.
        let mut buf = [0u8; 16];
        let idle_reply: Value = serde_json::from_str(
            &backend.request(&json!({"action": "interrupt", "session": session_id}).to_string()),
        )
        .unwrap();
        assert_eq!(
            idle_reply,
            json!({"ok": true, "data": {"interrupted": false}})
        );
        assert_eq!(
            pty.read(&mut buf).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        let missing_reply: Value = serde_json::from_str(
            &backend
                .request(&json!({"action": "interrupt", "session": session_id + 999}).to_string()),
        )
        .unwrap();
        assert_eq!(
            missing_reply,
            json!({"ok": true, "data": {"interrupted": false}})
        );

        // Working session receives exactly b"\x03" and returns {"interrupted": true}.
        registry
            .record_test_hook(
                SessionId(session_id),
                &HookEvent {
                    id: EventId {
                        pid: child_pid,
                        unix_ns: 1_000,
                    },
                    agent: "claude".into(),
                    ppid: child_pid,
                    input: Input::Complete,
                    payload: br#"{"hook_event_name":"UserPromptSubmit"}"#.to_vec(),
                },
            )
            .unwrap();
        let working_reply: Value = serde_json::from_str(
            &backend.request(&json!({"action": "interrupt", "session": session_id}).to_string()),
        )
        .unwrap();
        assert_eq!(
            working_reply,
            json!({"ok": true, "data": {"interrupted": true}})
        );
        let n = pty.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"\x03");

        // Late interrupt arriving after Working finished (Done) writes 0 bytes and returns false.
        registry
            .record_test_hook(
                SessionId(session_id),
                &HookEvent {
                    id: EventId {
                        pid: child_pid,
                        unix_ns: 2_000,
                    },
                    agent: "claude".into(),
                    ppid: child_pid,
                    input: Input::Complete,
                    payload: br#"{"hook_event_name":"Stop"}"#.to_vec(),
                },
            )
            .unwrap();
        let late_reply: Value = serde_json::from_str(
            &backend.request(&json!({"action": "interrupt", "session": session_id}).to_string()),
        )
        .unwrap();
        assert_eq!(
            late_reply,
            json!({"ok": true, "data": {"interrupted": false}})
        );
        assert_eq!(
            pty.read(&mut buf).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        while !crate::session::lock(&backend.git_inflight).is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        drop(holder_link);
        reader.join().unwrap();
        drop(backend);
        drop(registry);
    }

    #[test]
    fn photo_decoder_bounds_and_malformed_data() {
        for (encoded, plain) in [
            ("", ""),
            ("Zg==", "f"),
            ("Zm8=", "fo"),
            ("Zm9v", "foo"),
            ("8J+MjQ==", "🌍"),
        ] {
            assert_eq!(decode_base64(encoded).unwrap(), plain.as_bytes());
        }
        for encoded in ["Z", "Zh==", "Zm9=", "Z g==", "Zg==a", "☀"] {
            assert!(decode_base64(encoded).is_err(), "{encoded}");
        }
        assert!(decode_base64(&"A".repeat(700_001)).is_err());
        assert!(save_photo("text/html", "Zg==").is_err());
        assert!(save_photo("image/png", "").is_err());
        assert!(save_photo("image/png", &"A".repeat(699_996)).is_err());
    }

    #[test]
    fn workflow_errors_are_bounded_and_json() {
        let backend = test_backend();
        for request in [
            "",
            "{}",
            "{\"action\":\"unknown\"}",
            "{\"action\":\"snapshot\",\"extra\":true}",
        ] {
            let result: Value = serde_json::from_str(&backend.request(request)).unwrap();
            assert_eq!(result["ok"], false);
        }
        assert!(backend.request(&" ".repeat(750_001)).len() < 200);
    }

    #[test]
    #[cfg(unix)] // /bin/sh
    fn subprocess_output_and_time_are_bounded() {
        let mut good = Command::new("/bin/sh");
        good.args(["-c", "printf hello"]);
        assert_eq!(
            bounded_command(&mut good, Duration::from_secs(1), 5).unwrap(),
            b"hello"
        );
        let mut large = Command::new("/bin/sh");
        large.args(["-c", "printf hello"]);
        assert!(bounded_command(&mut large, Duration::from_secs(1), 4).is_err());
        let mut slow = Command::new("/bin/sh");
        slow.args(["-c", "exec sleep 1"]);
        assert_eq!(
            bounded_command(&mut slow, Duration::from_millis(10), 10)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_subprocess_output_and_time_are_bounded() {
        use std::os::windows::process::CommandExt as _;

        let command = |script: &str| {
            let mut command = Command::new("cmd.exe");
            command.args(["/D", "/C"]).raw_arg(script);
            command
        };
        assert_eq!(
            bounded_command(&mut command("echo hello"), Duration::from_secs(5), 7).unwrap(),
            b"hello\r\n"
        );
        assert!(bounded_command(&mut command("echo hello"), Duration::from_secs(5), 6).is_err());
        assert_eq!(
            bounded_command(
                &mut command("for /L %i in (0,0,1) do @rem waiting"),
                Duration::from_millis(30),
                10
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
    }

    proptest::proptest! {
        #[test]
        fn photo_decoder_never_panics(text in ".{0,2048}") {
            decode_base64(&text).ok();
        }
    }
}

#[cfg(test)]
mod revision_tests {
    use super::*;
    #[test]
    fn git_refresh_changes_the_effective_dashboard_revision() {
        let registry = Registry::new(None, None);
        let backend = Backend::new(
            Weak::new(),
            None,
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        let before = backend.effective_revision(&registry);
        assert_eq!(before, registry.dashboard_revision());
        backend.git_revision.fetch_add(1, Ordering::Relaxed);
        assert_ne!(backend.effective_revision(&registry), before);
        assert_eq!(
            backend.effective_revision(&registry),
            registry.dashboard_revision() ^ 0x9E37_79B9_7F4A_7C15
        );
    }

    #[test]
    fn near_limit_workflow_snapshot_roundtrips_on_rpc_stack() {
        let worker = std::thread::Builder::new().stack_size(super::super::gateway::RPC_STACK).spawn(|| {
            let registry = Arc::new(Registry::new(None, None));
            let backend = Backend::new(Arc::downgrade(&registry), None, ServerHello {
                proto_min: 1, proto_max: 1, features: ipc::FEATURES, build: "stack-fixture".into(), instance_id: 1,
            });
            let sessions: Vec<_> = (1..=64).map(|id| json!({"id":id,"title":"fixture","state":"Working","lines":["one","two","three"]})).collect();
            let data = json!({"sessions":sessions,"padding":"x".repeat(ipc::MAX_FRAME - 20_000),"etag":"stack"});
            *crate::session::lock(&backend.snapshot_cache) = Some(DashboardSnapshot {
                revision: backend.effective_revision(&registry), paths: Vec::new(), stamps: Vec::new(), at: Instant::now(), data, etag: "stack".into(), rev: 1,
            });
            let reply = backend.request("{\"action\":\"snapshot\"}");
            assert!(reply.len() > ipc::MAX_FRAME - 20_000);
            assert!(reply.len() < ipc::MAX_FRAME - 32);
            let value: Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(value["ok"], true);
            assert_eq!(value["data"]["sessions"].as_array().unwrap().len(), 64);
            let encoded = krill_proto::mobile::MobileReply::Workflow(reply.clone()).encode().unwrap();
            let (kind, body) = ipc::read_frame(&mut encoded.as_slice()).unwrap().unwrap();
            assert_eq!(krill_proto::mobile::MobileReply::decode(kind, &body).unwrap(), krill_proto::mobile::MobileReply::Workflow(reply));
            // A large request is rejected against this isolated empty registry;
            // it never reaches a PTY or an external service.
            let request = json!({"action":"prompt","session":1,"text":"x".repeat(749_000)}).to_string();
            assert!(request.len() < 750_000);
            let failure: Value = serde_json::from_str(&backend.request(&request)).unwrap();
            assert_eq!(failure["ok"], false);
        }).unwrap();
        worker.join().unwrap();
    }
    #[test]
    fn unchanged_snapshot_returns_before_opening_any_control_socket() {
        let registry = Arc::new(Registry::new(None, None));
        let backend = Backend::new(
            Arc::downgrade(&registry),
            None,
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        backend.git_revision.store(7, Ordering::Relaxed);
        *crate::session::lock(&backend.snapshot_cache) = Some(DashboardSnapshot {
            revision: backend.effective_revision(&registry),
            paths: Vec::new(),
            stamps: Vec::new(),
            at: Instant::now(),
            data: json!({"sessions":[],"etag":"same"}),
            etag: "same".into(),
            rev: 1,
        });
        assert_eq!(
            backend.snapshot_since(Some("same")).unwrap(),
            json!({"unchanged":true,"etag":"same"})
        );
    }

    #[test]
    fn catalog_stamp_reuses_unchanged_source_and_observes_atomic_preferences_edit() {
        let dir = std::env::temp_dir().join(format!("catalog-stamp-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("projects.toml"), "").unwrap();
        let backend = Backend::new(
            Weak::new(),
            Some(&dir),
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        let first = backend.catalog(&[]);
        let same = backend.catalog(&[]);
        assert!(Arc::ptr_eq(&first, &same));
        assert!(
            backend
                .snapshot_dependencies(&Value::Null, Some(&first))
                .is_some()
        );
        let replacement = dir.join("projects.new");
        fs::write(&replacement, "\n").unwrap();
        fs::rename(replacement, dir.join("projects.toml")).unwrap();
        // A body built with the old catalog must not adopt the new file stamps.
        assert!(
            backend
                .snapshot_dependencies(&Value::Null, Some(&first))
                .is_none()
        );
        assert!(!Arc::ptr_eq(&first, &backend.catalog(&[])));
        assert!(
            backend
                .snapshot_dependencies(&Value::Null, Some(&first))
                .is_none()
        );
        *crate::session::lock(&backend.catalog_cache) = None;
        assert!(
            backend
                .snapshot_dependencies(&Value::Null, Some(&same))
                .is_none()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sanitize_mobile_rpc_error_preserves_safe_messages_and_hides_internals() {
        for safe in [
            "This historical request cannot be verified against the current question. Confirm in Term.",
            "invalid mobile authentication or frame",
            "Not a Git repository",
            "File exceeds 128 KiB diff limit",
            "This file is no longer on the PC",
            "Session not found",
            "File is larger than 2 GiB",
            "Invalid file name",
            "This request is no longer pending.",
        ] {
            assert_eq!(sanitize_mobile_rpc_error(&io::Error::other(safe)), safe);
        }

        for leaked in [
            io::Error::new(io::ErrorKind::BrokenPipe, "Broken pipe (os error 32)"),
            io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "Connection refused (os error 61)",
            ),
            io::Error::other("unexpected control reply: Denied"),
            io::Error::other("unexpected daemon reply"),
            io::Error::other("bad request"),
            io::Error::other("failed connecting /Users/owen/.krill/daemon.sock"),
            io::Error::other("Session 41205: could not resume with selected account"),
            io::Error::other("Usage API rejected the sign-in (HTTP 401)"),
            io::Error::other(r"\\server\share\krill\bots.db"),
            io::Error::other("sqlite failure: no such table: bot_items"),
            io::Error::other("vault master key decrypt failed (secrets.enc)"),
        ] {
            assert_eq!(sanitize_mobile_rpc_error(&leaked), RPC_GENERIC_ERROR);
        }
    }

    #[test]
    fn sanitize_tap_reason_hides_raw_io_and_preserves_wait_guidance() {
        assert_eq!(
            sanitize_tap_reason(&io::Error::new(io::ErrorKind::NotFound, "no /Users/foo")),
            "Session is no longer active."
        );
        assert_eq!(
            sanitize_tap_reason(&io::Error::other(
                "Wait for the running agent to become idle"
            )),
            "Wait for the running agent to become idle"
        );
        assert_eq!(
            sanitize_tap_reason(&io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Broken pipe (os error 32)"
            )),
            "Could not connect to the live agent session."
        );
    }

    #[test]
    fn snapshot_bounds_oversized_payload_below_ipc_and_e2ee_limits() {
        use super::super::e2ee::{Channel, Endpoint, MAX_PLAINTEXT};

        let huge_line = "x".repeat(8192);
        let huge_title = "t".repeat(4096);
        let mut sessions: Vec<Value> = (1..=64)
            .map(|id| {
                json!({
                    "id": id,
                    "state": "working",
                    "agent": "claude",
                    "title": huge_title,
                    "foreground": "claude",
                    "startup_stall": null,
                    "cwd": "/tmp/repo",
                    "lines": [huge_line, huge_line, huge_line],
                    "exited": false,
                    "branch": "main",
                    "added": 1,
                    "deleted": 0,
                    "profile_id": "claude",
                    "account_id": null,
                    "command_context": {"commands": vec![huge_line.clone(); 4]},
                    "repo_key": "repo",
                    "worktree_key": "wt",
                    "group_label": "repo",
                    "group_path": "/tmp/repo",
                    "state_source": "hook",
                    "state_confidence": "high",
                    "state_since_unix_ns": "0",
                })
            })
            .collect();
        let mut data = json!({
            "sessions": sessions,
            "orchestration": {"runs":[],"tasks":[],"gates":[],"questions":[]},
            "repositories": [],
            "workspaces": [],
            "truncated": false,
            "capabilities": {"change_feed": true},
            "quick_commands": [],
            "host": "test",
            "agents": [],
        });
        assert!(json_byte_len(&data) > ipc::MAX_FRAME);

        bound_snapshot_payload(&mut data);
        let wrapped = json!({"ok": true, "data": data});
        let raw = serde_json::to_vec(&wrapped).unwrap();
        assert!(
            raw.len() < ipc::MAX_FRAME.saturating_sub(32),
            "bounded snapshot rpc len {} exceeded IPC budget",
            raw.len()
        );
        let ipc_frame = ipc::encode_frame(6, &raw).unwrap();
        assert!(ipc_frame.len() <= MAX_PLAINTEXT);
        let mut daemon = Channel::new(&[1u8; 32], &[2u8; 64], Endpoint::Daemon).unwrap();
        let mut client = Channel::new(&[1u8; 32], &[2u8; 64], Endpoint::Client).unwrap();
        let sealed = daemon.seal(&ipc_frame).unwrap();
        assert_eq!(client.open(&sealed).unwrap(), ipc_frame);

        // Even when cards remain >1MB without lines/context, cards are trimmed and `truncated` flips true.
        sessions = (1..=64)
            .map(|id| {
                json!({
                    "id": id,
                    "blob": "b".repeat(32 * 1024),
                    "lines": [],
                })
            })
            .collect();
        let mut hard = json!({
            "sessions": sessions,
            "orchestration": {"runs":[],"tasks":[],"gates":[],"questions":[]},
            "repositories": [],
            "workspaces": [],
            "truncated": false,
            "quick_commands": [],
            "agents": [],
        });
        bound_snapshot_payload(&mut hard);
        assert_eq!(hard["truncated"], true);
        let wrapped_hard = serde_json::to_vec(&json!({"ok": true, "data": hard})).unwrap();
        assert!(wrapped_hard.len() < ipc::MAX_FRAME.saturating_sub(32));
    }

    #[test]
    #[cfg(unix)]
    fn snapshot_swr_returns_without_blocking_on_cold_agy_session() {
        use crate::holder::{Holder, link::Link};
        use crate::os::OwnedFd;
        use krill_proto::holder::{FromHolder, ToHolder, VERSION};
        use std::os::fd::AsFd as _;
        use std::os::unix::ffi::OsStrExt as _;

        struct Cleanup {
            dir: PathBuf,
            child: Option<std::process::Child>,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(mut c) = self.child.take() {
                    c.kill().ok();
                    c.wait().ok();
                }
                fs::remove_dir_all(&self.dir).ok();
            }
        }
        let dir = std::env::temp_dir().join(format!("pwa-agy-swr-{}", hex(&random().unwrap())));
        let state_dir = dir.join("state");
        let repo = dir.join("repo");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let repo = fs::canonicalize(&repo).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        fs::write(
            state_dir.join("projects.toml"),
            format!(
                "[[projects]]\nid = \"repo\"\nname = \"Repo\"\npath = {:?}\n",
                repo.to_string_lossy()
            ),
        )
        .unwrap();
        let mut cleanup = Cleanup { dir, child: None };
        let child = Command::new("/bin/sleep")
            .arg("60")
            .current_dir(&repo)
            .spawn()
            .unwrap();
        let child_pid = child.id();
        cleanup.child = Some(child);

        let (daemon_end, holder_end) = UnixStream::pair().unwrap();
        let mut holder_link = Link::new(OwnedFd::from(holder_end));
        for message in [
            FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            },
            FromHolder::Ready,
        ] {
            holder_link.send(&message.encode().unwrap(), &[]).unwrap();
        }
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let registry = Arc::new(Registry::new(Some((holder, vec![])), Some(&state_dir)));
        let reader_reg = Arc::clone(&registry);
        let reader = thread::spawn(move || reader_reg.run_holder(link));
        let holder_worker = thread::spawn(move || {
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Spawn { req, id, .. } = ToHolder::decode(packet.frame()).unwrap() else {
                panic!("expected Spawn");
            };
            let (master, pty) = UnixStream::pair().unwrap();
            holder_link
                .send(
                    &FromHolder::Spawned {
                        req,
                        id,
                        pid: child_pid,
                    }
                    .encode()
                    .unwrap(),
                    &[master.as_fd()],
                )
                .unwrap();
            (holder_link, master, pty)
        });

        let backend = Backend::new(
            Arc::downgrade(&registry),
            Some(&state_dir),
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        let repo_key = hex(repo.as_os_str().as_bytes());
        let create_reply: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "create_terminal",
                    "workspace": repo_key,
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(
            create_reply["ok"], true,
            "create_terminal failed: {create_reply}"
        );
        let (holder_link, master, _pty) = holder_worker.join().unwrap();
        drop(master);
        let session_id = create_reply["data"]["session"].as_u64().unwrap();

        registry
            .agent_control()
            .remember_session(
                SessionId(session_id),
                &control::AgentLaunch {
                    profile: "agy".into(),
                    permission: krill_proto::agent::Permission::Yolo,
                },
                None,
                repo.as_os_str().as_bytes().to_vec(),
                None,
            )
            .unwrap();

        let rev_before = registry.dashboard_revision();
        let started = Instant::now();
        let snap: Value =
            serde_json::from_str(&backend.request(r#"{"action":"snapshot"}"#)).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "snapshot SWR blocked for {:?}",
            started.elapsed()
        );
        assert_eq!(snap["ok"], true);
        assert_eq!(snap["data"]["sessions"][0]["agent"], "agy");

        // When background SWR discovery changes title/binding, dashboard_revision must advance.
        crate::agent_history::vault::bump_agy_swr_revision();
        assert_ne!(registry.dashboard_revision(), rev_before);

        let deadline = Instant::now() + Duration::from_secs(5);
        while !crate::session::lock(&backend.git_inflight).is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        drop(holder_link);
        reader.join().ok();
        drop(backend);
        drop(registry);
    }

    #[test]
    fn mobile_pwa_bot_workflow_rpcs_are_lazy_safe_and_preserve_fail_closed_approvals() {
        use krill_proto::bots::{
            BotConnectorConfig, BotItemStatus, BotRpcRequest, BotRpcResponse, CreateBotItemParams,
            CreateBotParams, ExternalSource,
        };

        let dir = std::env::temp_dir().join(format!("mk-pwa-bots-rpc-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        let registry = Arc::new(Registry::new(None, Some(&dir)));
        registry.bind_self_arc();
        let backend = Backend::new(
            Arc::downgrade(&registry),
            Some(&dir),
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );

        // Strictly lazy when no bots exist: read-only workflow actions and snapshot() do NOT initialize BotsManager,
        // and write actions fail fast with NotFound without initializing BotsManager.
        let snap_empty: Value =
            serde_json::from_str(&backend.request(r#"{"action":"snapshot"}"#)).unwrap();
        assert_eq!(snap_empty["ok"], true, "{snap_empty}");
        assert_eq!(
            snap_empty["data"]["bots"],
            json!({"count": 0, "needs_you": 0, "pending_access": 0})
        );
        let empty_bots: Value =
            serde_json::from_str(&backend.request(r#"{"action":"bots.list"}"#)).unwrap();
        assert_eq!(empty_bots["ok"], true);
        assert_eq!(empty_bots["data"]["bots"], json!([]));
        assert_eq!(empty_bots["data"]["truncated"], false);
        assert_eq!(empty_bots["data"]["next_cursor"], Value::Null);
        let empty_items: Value =
            serde_json::from_str(&backend.request(r#"{"action":"bots.items"}"#)).unwrap();
        assert_eq!(empty_items["ok"], true);
        assert_eq!(empty_items["data"]["items"], json!([]));
        let empty_groups: Value =
            serde_json::from_str(&backend.request(r#"{"action":"bots.groups.list"}"#)).unwrap();
        assert_eq!(empty_groups["ok"], true);
        assert_eq!(empty_groups["data"]["groups"], json!([]));
        let empty_suggestions: Value =
            serde_json::from_str(&backend.request(r#"{"action":"bots.suggestions.list"}"#))
                .unwrap();
        assert_eq!(empty_suggestions["ok"], true);
        assert_eq!(empty_suggestions["data"]["suggestions"], json!([]));
        let empty_statuses: Value =
            serde_json::from_str(&backend.request(r#"{"action":"bots.connectors.status"}"#))
                .unwrap();
        assert_eq!(
            empty_statuses,
            json!({"ok": true, "data": {"statuses": []}})
        );
        let templates_resp: Value =
            serde_json::from_str(&backend.request(r#"{"action":"bots.templates"}"#)).unwrap();
        assert_eq!(templates_resp["ok"], true);
        assert!(
            !templates_resp["data"]["templates"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        for inactive_write in [
            r#"{"action":"bots.chat.open","bot_id":"b1"}"#,
            r#"{"action":"bots.item.create","bot_id":"b1","title":"Task"}"#,
            r#"{"action":"bots.item.update","item_id":"item-1","status":"done"}"#,
            r#"{"action":"bots.item.answer","item_id":"item-1","answer_text":"yes"}"#,
            r#"{"action":"bots.item.link","item_id":"item-1","link":{"url":"https://example.com"}}"#,
            r#"{"action":"bots.access.decide","token":"tok-1","approve":true}"#,
            r#"{"action":"bots.access.redeem","code":"PAIR-1"}"#,
            r#"{"action":"bots.reply","bot_id":"b1","text":"hi"}"#,
            r#"{"action":"bots.groups.create","name":"Team","bot_ids":["b1"]}"#,
            r#"{"action":"bots.groups.send","group_id":"grp-1","text":"hi"}"#,
            r#"{"action":"bots.groups.new_thread","group_id":"grp-1"}"#,
            r#"{"action":"bots.groups.stop","group_id":"grp-1"}"#,
            r#"{"action":"bots.look_around","bot_id":"b1"}"#,
            r#"{"action":"bots.suggestions.dismiss","bot_id":"b1","suggestion_id":"sug-1"}"#,
            r#"{"action":"bots.suggestions.start","bot_id":"b1","suggestion_id":"sug-1"}"#,
            r#"{"action":"bots.soul.set","bot_id":"b1","content":"hello"}"#,
            r#"{"action":"bots.memory.update","bot_id":"b1","content":"fact"}"#,
        ] {
            let res: Value = serde_json::from_str(&backend.request(inactive_write)).unwrap();
            assert_eq!(res["ok"], false, "{inactive_write}");
            assert_eq!(
                res["error"], "no bots configured",
                "{inactive_write}: {res}"
            );
        }
        assert!(registry.bots_if_initialized().is_none());

        // Seed a bot, an item, a pending access request, and an enabled connector.
        let mgr = registry.bots().unwrap();
        let created = match mgr.handle_rpc(
            &registry,
            BotRpcRequest::CreateBot(Box::new(CreateBotParams {
                name: "Mobile Triage Bot".to_owned(),
                agent_id: "claude".to_owned(),
                description: "Triages PRs from phone".to_owned(),
                emoji: Some("M".to_owned()),
                avatar: None,
                model: None,
                effort: None,
                permission_mode: None,
                workspace: None,
                template_id: None,
                initial_soul: None,
            })),
        ) {
            BotRpcResponse::Bot(b) => *b,
            other => panic!("unexpected create bot reply: {other:?}"),
        };

        let mut connectors = BotConnectorConfig::default();
        connectors.telegram.enabled = true;
        assert!(matches!(
            mgr.handle_rpc(
                &registry,
                BotRpcRequest::SetConnectorConfig {
                    bot_id: created.id.clone(),
                    connectors: Box::new(connectors),
                }
            ),
            BotRpcResponse::Bot(_)
        ));

        let item = match mgr.handle_rpc(
            &registry,
            BotRpcRequest::CreateItem(Box::new(CreateBotItemParams {
                bot_id: created.id.clone(),
                title: "Verify release checksum".to_owned(),
                description: None,
                done_means: None,
                status: Some(BotItemStatus::NeedsYou),
                links: Vec::new(),
            })),
        ) {
            BotRpcResponse::Item(i) => *i,
            other => panic!("unexpected create item reply: {other:?}"),
        };

        let decision = mgr
            .store()
            .check_or_request_access(
                &created.id,
                ExternalSource::Telegram,
                "777",
                "Bob",
                "chat-777",
                1_500,
            )
            .unwrap();
        let access_token = match decision {
            crate::bots::connectors::AccessDecision::Pending(req) => req.token,
            other => panic!("expected pending access request, got {other:?}"),
        };

        // 0. snapshot() reports live bot summary
        let snap_active: Value =
            serde_json::from_str(&backend.request(r#"{"action":"snapshot"}"#)).unwrap();
        assert_eq!(snap_active["ok"], true, "{snap_active}");
        assert_eq!(
            snap_active["data"]["bots"],
            json!({"count": 1, "needs_you": 1, "pending_access": 1})
        );

        // 1. bots.list
        let list_resp: Value =
            serde_json::from_str(&backend.request(r#"{"action":"bots.list"}"#)).unwrap();
        assert_eq!(list_resp["ok"], true, "{list_resp}");
        assert_eq!(list_resp["data"]["bots"].as_array().unwrap().len(), 1);
        assert_eq!(list_resp["data"]["bots"][0]["name"], "Mobile Triage Bot");

        // 2. bots.items + bots.item.create + bots.item.link
        let items_resp: Value = serde_json::from_str(
            &backend.request(
                &json!({"action":"bots.items","bot_id":created.id.as_str(),"include_closed":false})
                    .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(items_resp["ok"], true, "{items_resp}");
        assert_eq!(items_resp["data"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(items_resp["data"]["items"][0]["uid"], item.uid);

        let created_item_resp: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.item.create",
                    "bot_id": created.id.as_str(),
                    "title": "Follow up on mobile UX",
                    "status": "planned"
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(created_item_resp["ok"], true, "{created_item_resp}");
        let item2_uid = created_item_resp["data"]["item"]["uid"]
            .as_str()
            .unwrap()
            .to_owned();

        let linked_resp: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.item.link",
                    "item_id": item2_uid,
                    "link": {"url": "https://example.com/pr/42"}
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(linked_resp["ok"], true, "{linked_resp}");
        assert_eq!(
            linked_resp["data"]["item"]["links"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        // 3. bots.item.update
        let upd_resp: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.item.update",
                    "item_id": item.uid,
                    "status": "done",
                    "comment": "Verified on mobile",
                    "close_note": "SHA-256 matches"
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(upd_resp["ok"], true, "{upd_resp}");
        assert_eq!(upd_resp["data"]["item"]["status"], "done");

        // Open items list shows only item2; include_closed=true returns both.
        let open_items: Value = serde_json::from_str(
            &backend.request(
                &json!({"action":"bots.items","bot_id":created.id.as_str(),"include_closed":false})
                    .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(open_items["data"]["items"].as_array().unwrap().len(), 1);
        let all_items: Value = serde_json::from_str(
            &backend.request(
                &json!({"action":"bots.items","bot_id":created.id.as_str(),"include_closed":true})
                    .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(all_items["data"]["items"].as_array().unwrap().len(), 2);

        // 4. bots.access.list + bots.access.decide
        let access_list: Value = serde_json::from_str(&backend.request(
            &json!({"action":"bots.access.list","bot_id":created.id.as_str()}).to_string(),
        ))
        .unwrap();
        assert_eq!(access_list["ok"], true, "{access_list}");
        assert_eq!(access_list["data"]["requests"].as_array().unwrap().len(), 1);

        let decided: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.access.decide",
                    "token": access_token,
                    "approve": true
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(decided["ok"], true, "{decided}");
        assert_eq!(decided["data"]["request"]["status"], "approved");

        // 5. bots.reply fails closed when bot has no external conversation link.
        let reply_resp: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.reply",
                    "bot_id": created.id.as_str(),
                    "text": "Ack from phone"
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(reply_resp["ok"], false, "{reply_resp}");
        assert!(
            reply_resp["error"]
                .as_str()
                .unwrap()
                .contains("no external conversation link"),
            "{reply_resp}"
        );

        // 5b. bots.groups.{create,list} + bots.soul.{get,set} + bots.memory.{get,update}
        let grp_create: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.groups.create",
                    "name": "Mobile Sync",
                    "bot_ids": [created.id.as_str()],
                    "description": "Coordinate releases"
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(grp_create["ok"], true, "{grp_create}");
        assert_eq!(grp_create["data"]["group"]["name"], "Mobile Sync");

        let grp_list: Value =
            serde_json::from_str(&backend.request(r#"{"action":"bots.groups.list"}"#)).unwrap();
        assert_eq!(grp_list["ok"], true, "{grp_list}");
        assert_eq!(grp_list["data"]["groups"].as_array().unwrap().len(), 1);

        let soul_set: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.soul.set",
                    "bot_id": created.id.as_str(),
                    "content": "# Soul\nBe concise.\n"
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(soul_set["ok"], true, "{soul_set}");
        let soul_get: Value = serde_json::from_str(&backend.request(
            &json!({"action": "bots.soul.get", "bot_id": created.id.as_str()}).to_string(),
        ))
        .unwrap();
        assert_eq!(soul_get["ok"], true, "{soul_get}");
        assert!(
            soul_get["data"]["content"]
                .as_str()
                .unwrap()
                .contains("Be concise.")
        );

        let mem_upd: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.memory.update",
                    "bot_id": created.id.as_str(),
                    "content": "- Mobile PWA verified\n",
                    "allow_deletions": false
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(mem_upd["ok"], true, "{mem_upd}");
        let mem_get: Value = serde_json::from_str(&backend.request(
            &json!({"action": "bots.memory.get", "bot_id": created.id.as_str()}).to_string(),
        ))
        .unwrap();
        assert_eq!(mem_get["ok"], true, "{mem_get}");
        assert!(
            mem_get["data"]["content"]
                .as_str()
                .unwrap()
                .contains("Mobile PWA verified")
        );

        // 5c. Validation limits aligned with krill-proto constants.
        for bad_req in [
            json!({
                "action": "bots.item.update",
                "item_id": item2_uid,
                "comment": "x".repeat(krill_proto::bots::BOT_ITEM_COMMENT_MAX_BYTES + 1)
            }),
            json!({
                "action": "bots.item.create",
                "bot_id": created.id.as_str(),
                "title": "t".repeat(krill_proto::bots::BOT_ITEM_TITLE_MAX_BYTES + 1)
            }),
            json!({
                "action": "bots.item.create",
                "bot_id": created.id.as_str(),
                "title": "Valid",
                "description": "d".repeat(krill_proto::bots::BOT_ITEM_DESCRIPTION_MAX_BYTES + 1)
            }),
            json!({
                "action": "bots.item.answer",
                "item_id": item2_uid
            }),
            json!({
                "action": "bots.groups.create",
                "name": "",
                "bot_ids": [created.id.as_str()]
            }),
            json!({
                "action": "bots.access.redeem",
                "code": "bad\ncode"
            }),
        ] {
            let res: Value = serde_json::from_str(&backend.request(&bad_req.to_string())).unwrap();
            assert_eq!(res["ok"], false, "{bad_req}");
        }

        // 6. bots.connectors.status (missing token reported cleanly as error/not_set_up)
        let status_resp: Value = serde_json::from_str(&backend.request(
            &json!({"action":"bots.connectors.status","bot_id":created.id.as_str()}).to_string(),
        ))
        .unwrap();
        assert_eq!(status_resp["ok"], true, "{status_resp}");
        let statuses = status_resp["data"]["statuses"].as_array().unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["source"], "telegram");
        assert_eq!(statuses[0]["error_category"], "not_set_up");

        // 7. Fail-closed invariants for legacy approve / answer_question remain strictly preserved.
        for legacy in [
            r#"{"action":"approve","session":1,"value":"y"}"#,
            r#"{"action":"answer_question","session":1,"selected":[0]}"#,
        ] {
            let res: Value = serde_json::from_str(&backend.request(legacy)).unwrap();
            assert_eq!(res["ok"], false);
            assert!(
                res["error"].as_str().unwrap().contains("Confirm in Term"),
                "{res}"
            );
        }

        // Unknown fields rejected cleanly.
        let bad: Value =
            serde_json::from_str(&backend.request(r#"{"action":"bots.list","extra":123}"#))
                .unwrap();
        assert_eq!(bad["ok"], false);

        // 8. End-to-end v2 BotsManager flows: CreateBotV2, ListItemsPage, AnswerBotItem, Group threads/send/stop, Avatar & Suggestions.
        let v2_created: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.create",
                    "name": "Release Guard",
                    "agent_id": "claude",
                    "slug": "release-guard",
                    "avatar_color": "#0ea5e9",
                    "skills": ["verify-krill"]
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(v2_created["ok"], true, "{v2_created}");
        let v2_id = v2_created["data"]["bot"]["id"].as_str().unwrap().to_owned();
        assert_eq!(v2_created["data"]["bot"]["slug"], "release-guard");

        let av_empty: Value = serde_json::from_str(
            &backend.request(&json!({"action": "bots.avatar.read", "bot_id": v2_id}).to_string()),
        )
        .unwrap();
        assert_eq!(av_empty["ok"], true, "{av_empty}");
        assert_eq!(av_empty["data"]["data_base64"], Value::Null);
        let av_missing_bot: Value = serde_json::from_str(
            &backend.request(r#"{"action":"bots.avatar.read","bot_id":"no_such_bot"}"#),
        )
        .unwrap();
        assert_eq!(av_missing_bot["ok"], false, "{av_missing_bot}");

        let icon_png = include_bytes!("assets/icon-192.png").to_vec();
        assert!(matches!(
            mgr.handle_rpc(
                &registry,
                BotRpcRequest::UploadBotAvatar {
                    bot_id: krill_proto::bots::BotId::new(&v2_id).unwrap(),
                    png_bytes: icon_png,
                }
            ),
            BotRpcResponse::BotAvatar(_)
        ));
        let av_present: Value = serde_json::from_str(
            &backend.request(&json!({"action": "bots.avatar.read", "bot_id": v2_id}).to_string()),
        )
        .unwrap();
        assert_eq!(av_present["ok"], true, "{av_present}");
        assert_eq!(av_present["data"]["mime"], "image/png");
        assert!(
            av_present["data"]["data_base64"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        );

        let item_a: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.item.create",
                    "bot_id": v2_id,
                    "title": "Approve release tag",
                    "status": "needs_you"
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(item_a["ok"], true, "{item_a}");
        let item_a_uid = item_a["data"]["item"]["uid"].as_str().unwrap().to_owned();
        let item_b: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.item.create",
                    "bot_id": v2_id,
                    "title": "Publish checksums",
                    "status": "planned"
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(item_b["ok"], true, "{item_b}");

        let page1: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.items",
                    "bot_id": v2_id,
                    "include_closed": false,
                    "limit": 1
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(page1["ok"], true, "{page1}");
        assert_eq!(page1["data"]["items"].as_array().unwrap().len(), 1);
        let cursor1 = page1["data"]["next_cursor"].as_str().unwrap().to_owned();
        let page2: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.items",
                    "bot_id": v2_id,
                    "include_closed": false,
                    "limit": 1,
                    "cursor": cursor1
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(page2["ok"], true, "{page2}");
        assert_eq!(page2["data"]["items"].as_array().unwrap().len(), 1);
        assert_ne!(
            page1["data"]["items"][0]["uid"],
            page2["data"]["items"][0]["uid"]
        );

        // D-10 + M-07: bots.items supports optional status filtering ("needs_you" / "planned").
        let planned_only: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.items",
                    "bot_id": v2_id,
                    "status": "planned"
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(planned_only["ok"], true, "{planned_only}");
        let planned_items = planned_only["data"]["items"].as_array().unwrap();
        assert_eq!(planned_items.len(), 1);
        assert_eq!(planned_items[0]["status"], "planned");

        let ask_upd: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.item.update",
                    "item_id": item_a_uid,
                    "decisions": [{"question": "Ship now?", "options": ["Ship", "Hold"], "recommended": 0}]
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(ask_upd["ok"], true, "{ask_upd}");
        let answered: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.item.answer",
                    "item_id": item_a_uid,
                    "option_index": 0,
                    "answer_text": "Ship"
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(answered["ok"], true, "{answered}");

        let grp_id = grp_create["data"]["group"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let new_th: Value = serde_json::from_str(
            &backend.request(
                &json!({"action": "bots.groups.new_thread", "group_id": grp_id, "title": "Standup"})
                    .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(new_th["ok"], true, "{new_th}");
        let th_id = new_th["data"]["group"]["threads"][0]["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let grp_send: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.groups.send",
                    "group_id": grp_id,
                    "thread_id": th_id,
                    "text": "Status check",
                    "reply_mode": "mentions"
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(grp_send["ok"], true, "{grp_send}");
        let grp_read: Value = serde_json::from_str(
            &backend.request(
                &json!({"action": "bots.groups.read", "group_id": grp_id, "thread_id": th_id, "limit": 10})
                    .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(grp_read["ok"], true, "{grp_read}");
        assert_eq!(
            grp_read["data"]["group"]["messages"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let grp_stop: Value = serde_json::from_str(
            &backend.request(
                &json!({"action": "bots.groups.stop", "group_id": grp_id, "thread_id": th_id})
                    .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(grp_stop["ok"], true, "{grp_stop}");

        let sugs: Value = serde_json::from_str(
            &backend
                .request(&json!({"action": "bots.suggestions.list", "bot_id": v2_id}).to_string()),
        )
        .unwrap();
        assert_eq!(sugs["ok"], true, "{sugs}");

        // Live bot sessions list + invalid stop rejection
        let bot_sessions: Value = serde_json::from_str(
            &backend.request(&json!({"action": "bots.sessions.list", "bot_id": v2_id}).to_string()),
        )
        .unwrap();
        assert_eq!(bot_sessions["ok"], true, "{bot_sessions}");
        assert!(bot_sessions["data"]["sessions"].is_array());
        let bad_stop: Value = serde_json::from_str(&backend.request(
            &json!({"action": "bots.sessions.stop", "bot_id": v2_id, "session_id": 0}).to_string(),
        ))
        .unwrap();
        assert_eq!(bad_stop["ok"], false, "{bad_stop}");

        // Schedule toggle + paused RunScheduleNow structured refusal
        let sched = krill_proto::bots::BotScheduleConfig {
            id: "nightly".into(),
            name: "Nightly check".into(),
            cron: "0 9 * * *".into(),
            prompt: "Check status".into(),
            enabled: true,
            timezone: None,
        };
        let add_sched = mgr.handle_rpc(
            &registry,
            krill_proto::bots::BotRpcRequest::UpdateBot(Box::new(
                krill_proto::bots::UpdateBotParams {
                    bot_id: krill_proto::bots::BotId::new(&v2_id).unwrap(),
                    name: None,
                    description: None,
                    emoji: krill_proto::bots::FieldPatch::Unchanged,
                    avatar: krill_proto::bots::FieldPatch::Unchanged,
                    agent_id: None,
                    model: krill_proto::bots::FieldPatch::Unchanged,
                    effort: krill_proto::bots::FieldPatch::Unchanged,
                    permission_mode: None,
                    workspace: krill_proto::bots::FieldPatch::Unchanged,
                    paused: Some(true),
                    first_run_completed: None,
                    notifications: None,
                    schedules: Some(vec![sched]),
                    look_around: None,
                    memory_tidy: None,
                    mcp_tools: None,
                },
            )),
        );
        assert!(matches!(
            add_sched,
            krill_proto::bots::BotRpcResponse::Bot(_)
        ));
        let toggled: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.schedule.toggle",
                    "bot_id": v2_id,
                    "schedule_id": "nightly",
                    "enabled": false
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(toggled["ok"], true, "{toggled}");
        assert_eq!(toggled["data"]["bot"]["schedules"][0]["enabled"], false);
        let run_refused: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "bots.schedule.run",
                    "bot_id": v2_id,
                    "schedule_id": "nightly"
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(run_refused["ok"], false, "{run_refused}");
        assert_eq!(run_refused["data"]["refusal"]["code"], "bot_paused");

        mgr.shutdown();
        drop(backend);
        drop(registry);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn live_prompt_reply_and_steer_mode_validation_contracts() {
        let steer: Action = serde_json::from_str(
            r#"{"action":"prompt","session":7,"text":"focus","mode":"steer"}"#,
        )
        .unwrap();
        assert!(matches!(
            steer,
            Action::Prompt {
                session: 7,
                mode: PromptMode::Steer,
                ..
            }
        ));

        let reply: Action = serde_json::from_str(
            r#"{"action":"live_prompt_reply","session":7,"expected_prompt":"Deploy target","choice":1}"#,
        )
        .unwrap();
        assert!(matches!(
            reply,
            Action::LivePromptReply {
                session: 7,
                choice: Some(1),
                ..
            }
        ));

        let backend = Backend::new(
            Weak::new(),
            None,
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        let res: Value = serde_json::from_str(&backend.request(
            r#"{"action":"live_prompt_reply","session":7,"expected_prompt":"Deploy","choice":0}"#,
        ))
        .unwrap();
        assert_eq!(res["ok"], false);
        assert!(
            res["error"].as_str().unwrap().contains("Confirm in Term"),
            "{res}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn chat_composer_and_live_prompt_reply_verify_real_pty_screen_and_guard_prompt() {
        use crate::holder::{Holder, link::Link};
        use crate::os::{AsFd as _, OwnedFd};
        use krill_proto::holder::{FromHolder, ToHolder, VERSION};
        use krill_proto::hook::{EventId, HookEvent, Input};

        struct Cleanup {
            child: Option<std::process::Child>,
            dir: std::path::PathBuf,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(child) = self.child.as_mut() {
                    child.kill().ok();
                    child.wait().ok();
                }
                fs::remove_dir_all(&self.dir).ok();
            }
        }

        let dir = std::env::temp_dir().join(format!("mk-pwa-live-prompt-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let mut cleanup = Cleanup {
            child: None,
            dir: dir.clone(),
        };
        let state_dir = dir.join("state");
        let repo_dir = dir.join("repo");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&repo_dir).unwrap();
        let canonical_repo = fs::canonicalize(&repo_dir).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .current_dir(&canonical_repo)
                .status()
                .unwrap()
                .success()
        );
        fs::write(
            state_dir.join("projects.toml"),
            format!(
                "[[projects]]\nid = \"repo\"\nname = \"Repo\"\npath = {:?}\n",
                canonical_repo.to_string_lossy()
            ),
        )
        .unwrap();

        let child = Command::new("/bin/sleep")
            .arg("60")
            .current_dir(&canonical_repo)
            .spawn()
            .unwrap();
        let child_pid = child.id();
        cleanup.child = Some(child);
        let (daemon_end, holder_end) = UnixStream::pair().unwrap();
        let mut holder_link = Link::new(OwnedFd::from(holder_end));
        for message in [
            FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            },
            FromHolder::Ready,
        ] {
            holder_link.send(&message.encode().unwrap(), &[]).unwrap();
        }
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let registry = Arc::new(Registry::new(Some((holder, vec![])), Some(&state_dir)));
        let reader_reg = Arc::clone(&registry);
        let reader = std::thread::spawn(move || reader_reg.run_holder(link));
        let holder_worker = std::thread::spawn(move || {
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Spawn { req, id, .. } = ToHolder::decode(packet.frame()).unwrap() else {
                panic!("expected Spawn");
            };
            let open_pty = rustix_openpty::openpty(None, None).unwrap();
            let mut mode = rustix::termios::tcgetattr(&open_pty.user).unwrap();
            mode.make_raw();
            rustix::termios::tcsetattr(
                &open_pty.user,
                rustix::termios::OptionalActions::Now,
                &mode,
            )
            .unwrap();
            rustix::fs::fcntl_setfl(
                &open_pty.user,
                rustix::fs::fcntl_getfl(&open_pty.user).unwrap() | rustix::fs::OFlags::NONBLOCK,
            )
            .unwrap();
            holder_link
                .send(
                    &FromHolder::Spawned {
                        req,
                        id,
                        pid: child_pid,
                    }
                    .encode()
                    .unwrap(),
                    &[open_pty.controller.as_fd()],
                )
                .unwrap();
            let pty = fs::File::from(open_pty.user);
            (holder_link, open_pty.controller, pty)
        });

        let backend = Backend::new(
            Arc::downgrade(&registry),
            Some(&state_dir),
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        let repo_key = hex(canonical_repo.as_os_str().as_bytes());
        let create_reply: Value = serde_json::from_str(
            &backend
                .request(&json!({"action": "create_terminal", "workspace": repo_key}).to_string()),
        )
        .unwrap();
        let (holder_link, master, mut pty) = holder_worker.join().unwrap();
        drop(master);

        let session_id = create_reply["data"]["session"]
            .as_u64()
            .expect("session id");
        let snap: Value =
            serde_json::from_str(&backend.request(r#"{"action":"snapshot"}"#)).unwrap();
        assert_eq!(snap["data"]["capabilities"]["live_prompt_reply"], true);
        assert_eq!(snap["data"]["capabilities"]["prompt_steer"], true);

        registry
            .record_test_hook(
                SessionId(session_id),
                &HookEvent {
                    id: EventId {
                        pid: child_pid,
                        unix_ns: 1_000,
                    },
                    agent: "claude".into(),
                    ppid: child_pid,
                    input: Input::Complete,
                    payload: br#"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#
                        .to_vec(),
                },
            )
            .unwrap();

        pty.write_all(b"Allow Bash: cargo check?\r\n\xe2\x9d\xaf 1. Yes\r\n  2. No\r\n")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut live_prompt_seen = false;
        while Instant::now() < deadline {
            if let Some(screen) = registry.session_capture(SessionId(session_id))
                && krill_proto::screen::prompt::detect(&screen.rows).is_some()
            {
                live_prompt_seen = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(live_prompt_seen, "PTY menu did not reach screen buffer");

        registry
            .agent_control()
            .mark_rate_limited(SessionId(session_id));
        let composer: Value = serde_json::from_str(
            &backend.request(&json!({"action":"chat_composer","session":session_id}).to_string()),
        )
        .unwrap();
        assert_eq!(composer["ok"], true, "{composer}");
        assert_eq!(composer["data"]["rate_limited"], true);
        assert_eq!(composer["data"]["live_prompt"]["kind"], "numbered_menu");
        assert_eq!(composer["data"]["live_prompt"]["options"][1]["label"], "No");

        // Sending a chat prompt while Waiting/Blocked must fail closed.
        let blocked: Value = serde_json::from_str(&backend.request(
            &json!({"action":"prompt","session":session_id,"text":"new prompt"}).to_string(),
        ))
        .unwrap();
        assert_eq!(blocked["ok"], false);
        assert!(
            blocked["error"].as_str().unwrap().contains("active prompt"),
            "{blocked}"
        );

        // Mismatched expected_prompt fails closed without writing to PTY.
        let mismatch: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "live_prompt_reply",
                    "session": session_id,
                    "expected_prompt": "something else entirely",
                    "choice": 1
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(mismatch["ok"], false);
        let mut pty_buf = [0u8; 64];
        assert_eq!(
            pty.read(&mut pty_buf).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        // Matching expected_prompt writes Down Arrow + CR to PTY.
        let matched: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "live_prompt_reply",
                    "session": session_id,
                    "expected_prompt": "Allow Bash: cargo check?",
                    "choice": 1
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(matched["ok"], true, "{matched}");
        let mut sent = Vec::new();
        let read_deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < read_deadline {
            match pty.read(&mut pty_buf) {
                Ok(n) if n > 0 => sent.extend_from_slice(&pty_buf[..n]),
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if !sent.is_empty() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("unexpected PTY read error: {e}"),
            }
        }
        assert_eq!(sent, b"\x1b[B\r");

        // Transition to Working -> steer prompt succeeds.
        registry
            .record_test_hook(
                SessionId(session_id),
                &HookEvent {
                    id: EventId {
                        pid: child_pid,
                        unix_ns: 2_000,
                    },
                    agent: "claude".into(),
                    ppid: child_pid,
                    input: Input::Complete,
                    payload: br#"{"hook_event_name":"UserPromptSubmit"}"#.to_vec(),
                },
            )
            .unwrap();
        let steer_res: Value = serde_json::from_str(
            &backend.request(
                &json!({"action":"prompt","session":session_id,"text":"keep tests minimal","mode":"steer"})
                    .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(steer_res["ok"], true, "{steer_res}");

        let read_pty = |pty: &mut fs::File| -> Vec<u8> {
            let mut out = Vec::new();
            let mut buf = [0u8; 256];
            let deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < deadline {
                match pty.read(&mut buf) {
                    Ok(n) if n > 0 => out.extend_from_slice(&buf[..n]),
                    Ok(_) => break,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        if !out.is_empty() {
                            break;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("unexpected PTY read error: {e}"),
                }
            }
            out
        };
        assert_eq!(read_pty(&mut pty), b"keep tests minimal\r");

        // Multiline mobile prompts on a non-bracketed live PTY must preserve newlines inside
        // bracketed paste framing (never flatten with " | ") and still strip control bytes.
        let multiline_res: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "prompt",
                    "session": session_id,
                    "text": "line1\nline2\x1b[31m\x03\nline3"
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(multiline_res["ok"], true, "{multiline_res}");
        assert_eq!(
            read_pty(&mut pty),
            b"\x1b[200~line1\nline2[31m\nline3\x1b[201~\r"
        );

        let inflight_deadline = Instant::now() + Duration::from_secs(5);
        while !crate::session::lock(&backend.git_inflight).is_empty()
            && Instant::now() < inflight_deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        drop(holder_link);
        reader.join().ok();
        drop(backend);
        drop(registry);
    }

    #[test]
    fn live_prompt_reply_action_deserializes_exact_client_payloads() {
        let yes_no: Action = serde_json::from_str(
            &json!({
                "action": "live_prompt_reply",
                "session": 1,
                "expected_prompt": "Allow?",
                "yes": true
            })
            .to_string(),
        )
        .unwrap();
        assert!(matches!(
            yes_no,
            Action::LivePromptReply {
                session: 1,
                yes: Some(true),
                choice: None,
                custom_text: None,
                ..
            }
        ));

        let other: Action = serde_json::from_str(
            &json!({
                "action": "live_prompt_reply",
                "session": 1,
                "expected_prompt": "Pick deployment mode",
                "choice": 1,
                "custom_text": "Canary 10%"
            })
            .to_string(),
        )
        .unwrap();
        assert!(matches!(
            other,
            Action::LivePromptReply {
                session: 1,
                yes: None,
                choice: Some(1),
                custom_text: Some(ref text),
                ..
            } if text == "Canary 10%"
        ));

        let sub: Action = serde_json::from_str(
            &json!({"action": "chat_subscribe", "session": 42, "offset": 128}).to_string(),
        )
        .unwrap();
        assert!(matches!(
            sub,
            Action::ChatSubscribe {
                session: 42,
                offset: 128
            }
        ));

        let unsub: Action =
            serde_json::from_str(&json!({"action": "chat_unsubscribe", "session": 42}).to_string())
                .unwrap();
        assert!(matches!(unsub, Action::ChatUnsubscribe { session: 42 }));
    }

    #[test]
    fn prompt_accepts_up_to_four_photos_and_rejects_five() {
        let one = |i: u8| {
            json!({
                "mime": "image/png",
                "data": hex(&[i]),
            })
        };
        let valid: Action = serde_json::from_str(
            &json!({
                "action": "prompt",
                "session": 1,
                "text": "check screenshots",
                "photos": [one(1), one(2), one(3), one(4)]
            })
            .to_string(),
        )
        .unwrap();
        assert!(matches!(
            valid,
            Action::Prompt {
                session: 1,
                photo: None,
                ref photos,
                ..
            } if photos.len() == 4
        ));

        let registry = Arc::new(Registry::new(None, None));
        let backend = Backend::new(
            Arc::downgrade(&registry),
            None,
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        let too_many: Value = serde_json::from_str(
            &backend.request(
                &json!({
                    "action": "prompt",
                    "session": 1,
                    "text": "too many",
                    "photos": [one(1), one(2), one(3), one(4), one(5)]
                })
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(too_many["ok"], false);
        assert!(
            too_many["error"]
                .as_str()
                .is_some_and(|e| e.contains("Maximum 4 photos"))
        );
    }

    #[test]
    fn mobile_pwa_bot_pagination_and_v2_action_parsing() {
        let oversized_items: Vec<Value> = (0..120)
            .map(|idx| {
                json!({
                    "uid": format!("item-{idx}"),
                    "title": format!("Item {idx}"),
                    "description": "x".repeat(16 * 1024),
                    "events": [{"id": idx, "text": "e".repeat(4096)}],
                })
            })
            .collect();
        let bounded = bound_bot_list_payload(
            &oversized_items,
            "items",
            0,
            100,
            120,
            None,
            &[("unseen_count", json!(3))],
        )
        .unwrap();
        assert_eq!(bounded["truncated"], true);
        assert!(bounded["next_cursor"].as_str().is_some());
        let wrapped = serde_json::to_vec(&json!({"ok": true, "data": bounded})).unwrap();
        assert!(wrapped.len() < ipc::MAX_FRAME.saturating_sub(32));

        // Verify serde parsing of all new v2 Action variants.
        for payload in [
            r#"{"action":"bots.create","name":"Triage","agent_id":"claude","slug":"triage","skills":["rust"]}"#,
            r#"{"action":"bots.models","agent_id":"antigravity"}"#,
            r#"{"action":"bots.sessions.list","bot_id":"b1"}"#,
            r#"{"action":"bots.sessions.stop","bot_id":"b1","session_id":7}"#,
            r#"{"action":"bots.schedule.run","bot_id":"b1","schedule_id":"nightly"}"#,
            r#"{"action":"bots.schedule.toggle","bot_id":"b1","schedule_id":"nightly","enabled":false}"#,
            r#"{"action":"bots.update","bot_id":"b1","agent_id":"antigravity","model":"gemini-3.8-flash-low","clear_effort":true}"#,
            r#"{"action":"handoff","session":1,"agent":"antigravity"}"#,
            r#"{"action":"bots.chat.open","bot_id":"b1","prompt":"check inbox"}"#,
            r#"{"action":"bots.items","limit":25,"cursor":"10","include_snoozed":true}"#,
            r#"{"action":"bots.items","status":"needs_you","limit":25}"#,
            r#"{"action":"bots.item.answer","item_id":"item-1","option_index":0,"answer_text":"Go"}"#,
            r#"{"action":"bots.groups.read","group_id":"grp-1","thread_id":"thr-1","limit":20}"#,
            r#"{"action":"bots.groups.send","group_id":"grp-1","text":"@b1 hello","reply_mode":"mentions"}"#,
            r#"{"action":"bots.groups.new_thread","group_id":"grp-1","title":"Triage"}"#,
            r#"{"action":"bots.groups.stop","group_id":"grp-1"}"#,
            r#"{"action":"bots.look_around","bot_id":"b1"}"#,
            r#"{"action":"bots.suggestions.list","bot_id":"b1","include_dismissed":false}"#,
            r#"{"action":"bots.suggestions.dismiss","bot_id":"b1","suggestion_id":"sug-1"}"#,
            r#"{"action":"bots.suggestions.start","bot_id":"b1","suggestion_id":"sug-1"}"#,
            r#"{"action":"bots.avatar.read","bot_id":"b1"}"#,
            r#"{"action":"repo_icons.read","keys":["616263"]}"#,
            r#"{"action":"bots.access.redeem","code":"KRILL-1234"}"#,
        ] {
            let parsed: Result<Action, _> = serde_json::from_str(payload);
            assert!(parsed.is_ok(), "failed parsing {payload}: {parsed:?}");
        }
        assert!(
            serde_json::from_str::<Action>(
                r#"{"action":"bots.item.answer","item_id":"i","unknown":1}"#
            )
            .is_err()
        );

        // D-33(c): bound_group_view_payload enforces budget WITH "truncated" field present.
        let huge_msgs: Vec<Value> = (0..60)
            .map(|i| json!({"message_id": format!("m-{i}"), "body": "x".repeat(20_000)}))
            .collect();
        let bounded_grp = bound_group_view_payload(json!({
            "group": { "messages": huge_msgs }
        }))
        .unwrap();
        assert_eq!(bounded_grp["truncated"], true);
        assert!(
            wrapped_ok_len(&bounded_grp) < ipc::MAX_FRAME.saturating_sub(MOBILE_FRAME_HEADROOM)
        );
    }

    #[test]
    fn snapshot_and_repo_icons_read_resolve_icons_without_blocking_or_leaking_paths() {
        let base = std::env::temp_dir().join(format!("mk-pwa-repo-icons-{}", std::process::id()));
        fs::remove_dir_all(&base).ok();
        let state_dir = base.join("state");
        let repo = base.join("krill-demo");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(repo.join("public")).unwrap();
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect width="16" height="16"/></svg>"#;
        let fav = repo.join("public/favicon.svg");
        fs::write(&fav, svg).unwrap();
        fs::write(
            state_dir.join("projects.toml"),
            format!(
                r#"[[projects]]
id = "demo"
path = "{}"
display_name = "krill-demo"
kind = "git"
[projects.repo_icon]
kind = "image"
value = "{}"
"#,
                repo.display().to_string().replace('\\', "\\\\"),
                fav.display().to_string().replace('\\', "\\\\"),
            ),
        )
        .unwrap();

        let registry = Arc::new(Registry::new(None, Some(&state_dir)));
        let backend = Backend::new(
            Arc::downgrade(&registry),
            Some(&state_dir),
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );

        let rev_before = backend.effective_revision(&registry);
        // 1. Cold snapshot returns immediately with capability advertised and never leaks local icon path.
        let first_raw = backend.request(r#"{"action":"snapshot"}"#);
        let first: Value = serde_json::from_str(&first_raw).unwrap();
        assert_eq!(first["ok"], true);
        assert_eq!(first["data"]["capabilities"]["repo_icons"], true);
        let first_repo = &first["data"]["repositories"][0];
        assert!(
            first_repo["icon"].is_null(),
            "cold snapshot must not block or emit raw local file paths: {first_repo}"
        );
        let repo_key = first_repo["key"].as_str().unwrap().to_owned();

        // 2. Background SWR probe resolves the favicon and bumps effective_revision.
        let deadline = Instant::now() + Duration::from_secs(5);
        while (!backend.repo_icons.is_idle() || backend.repo_icons.revision() == 0)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(backend.repo_icons.is_idle());
        assert_ne!(backend.effective_revision(&registry), rev_before);

        // 3. Warm snapshot emits compact content hash (no raw path, no inline base64).
        let second_raw = backend.request(r#"{"action":"snapshot"}"#);
        let second: Value = serde_json::from_str(&second_raw).unwrap();
        assert_eq!(second["ok"], true);
        let second_icon = &second["data"]["repositories"][0]["icon"];
        assert_eq!(second_icon["kind"], "image");
        let hash = second_icon["hash"].as_str().unwrap();
        assert_eq!(hash.len(), 16);
        assert!(second_icon.get("value").is_none());
        assert!(!second_raw.contains("favicon.svg"));

        // 4. Batch RPC serves the SVG payload for the registered key and ignores unknown keys.
        let read_raw = backend
            .request(&json!({"action": "repo_icons.read", "keys": [repo_key, "0000"]}).to_string());
        let read: Value = serde_json::from_str(&read_raw).unwrap();
        assert_eq!(read["ok"], true);
        assert_eq!(read["data"]["truncated"], false);
        let entry = &read["data"]["icons"][&repo_key];
        assert_eq!(entry["hash"], hash);
        assert_eq!(entry["mime"], "image/svg+xml");
        assert_eq!(
            entry["data_base64"].as_str().unwrap(),
            crate::mobile::ws::base64(svg, false)
        );
        assert!(read["data"]["icons"].get("0000").is_none());

        fs::remove_dir_all(&base).ok();
    }
}
