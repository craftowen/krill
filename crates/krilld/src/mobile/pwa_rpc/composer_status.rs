//! Display metadata from the selected transcript and its launch account's model catalog.
use super::composer::Context;
use super::{Value, fs, io, json};
use krill_proto::agent_history::ChatPollSnapshot;
use std::collections::HashMap;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

const MAX_BYTES: u64 = 2 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(300);
const CACHE_CAP: usize = 64;

type FileStamp = Option<(Option<SystemTime>, u64)>;
type StatusKey = (String, String, String);
type StatusCache = Mutex<HashMap<StatusKey, CachedStatus>>;

#[derive(Clone)]
struct CachedStatus {
    stamp: FileStamp,
    at: Instant,
    value: Value,
}

fn status_cache() -> &'static StatusCache {
    static CACHE: OnceLock<StatusCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn invalidate_all() {
    crate::session::lock(status_cache()).clear();
}

fn file_stamp(file: &fs::File) -> FileStamp {
    let meta = file.metadata().ok()?;
    Some((meta.modified().ok(), meta.len()))
}

fn command_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:/@[]".contains(&b))
}

fn set(state: &mut Value, key: &str, value: Value) {
    if let Some(fields) = state.as_object_mut() {
        fields.insert(key.to_owned(), value);
    }
}

fn observe(record: &Value, state: &mut Value) {
    let payload = record.get("payload").unwrap_or(record);
    let message = record.get("message").unwrap_or(record);
    if record.get("type").and_then(Value::as_str) == Some("turn_context") {
        if payload
            .get("model")
            .is_some_and(|model| state.get("model").is_some_and(|old| old != model))
        {
            set(state, "context", Value::Null);
        }
        for (source, target) in [
            ("model", "model"),
            ("effort", "effort"),
            ("reasoning_effort", "effort"),
        ] {
            if let Some(value) = payload
                .get(source)
                .and_then(Value::as_str)
                .filter(|v| command_value(v))
            {
                set(state, target, value.into());
            }
        }
    }
    if let Some(model) = message
        .get("model")
        .and_then(Value::as_str)
        .filter(|v| command_value(v) && !v.starts_with('<'))
    {
        set(state, "model", model.into());
    }
    if let Some(info) = payload.get("info") {
        let used = info
            .pointer("/last_token_usage/total_tokens")
            .and_then(Value::as_u64);
        let limit = info.get("model_context_window").and_then(Value::as_u64);
        if let (Some(used), Some(limit)) = (used, limit.filter(|n| *n > 0))
            && let (Ok(numerator), Ok(denominator)) = (u32::try_from(used), u32::try_from(limit))
        {
            set(
                state,
                "context",
                json!({"used":used,"limit":limit,"percent":(f64::from(numerator) / f64::from(denominator) * 100.0).clamp(0.0,100.0)}),
            );
        }
    }
}

pub(super) fn read(snapshot: &ChatPollSnapshot, context: &Context) -> io::Result<Value> {
    let agent = context.agent.as_str();
    let cache_key = (
        snapshot.session_id.clone(),
        agent.to_owned(),
        snapshot.transcript_path.clone(),
    );
    let mut state =
        json!({"agent":agent,"model":null,"effort":null,"context":null,"models":[],"efforts":[]});
    if snapshot.bound
        && krill_proto::agent::canonical_id(&snapshot.agent) == agent
        && let Ok(mut file) = fs::File::open(&snapshot.transcript_path)
    {
        let stamp = file_stamp(&file);
        {
            let guard = crate::session::lock(status_cache());
            if let Some(entry) = guard.get(&cache_key)
                && entry.at.elapsed() < CACHE_TTL
                && entry.stamp == stamp
            {
                return Ok(entry.value.clone());
            }
        }
        let len = file.metadata()?.len();
        let start = len.saturating_sub(MAX_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES).read_to_end(&mut bytes)?;
        for line in String::from_utf8_lossy(&bytes)
            .lines()
            .skip(usize::from(start > 0))
        {
            if let Ok(record) = serde_json::from_str::<Value>(line) {
                observe(&record, &mut state);
            }
        }
        let mut guard = crate::session::lock(status_cache());
        if guard.len() >= CACHE_CAP {
            guard.retain(|_, v| v.at.elapsed() < CACHE_TTL);
            if guard.len() >= CACHE_CAP
                && let Some(oldest_key) = guard
                    .iter()
                    .min_by_key(|(_, v)| v.at)
                    .map(|(k, _)| k.clone())
            {
                guard.remove(&oldest_key);
            }
        }
        guard.insert(
            cache_key,
            CachedStatus {
                stamp,
                at: Instant::now(),
                value: state.clone(),
            },
        );
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observed_codex_context_uses_current_turn_instead_of_lifetime_totals() {
        let mut state = json!({"model":null,"effort":null,"context":null});
        observe(
            &json!({"type":"turn_context","payload":{"model":"observed-model","effort":"high"}}),
            &mut state,
        );
        observe(
            &json!({"type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":1000,"total_token_usage":{"total_tokens":9000},"last_token_usage":{"total_tokens":250}}}}),
            &mut state,
        );
        assert_eq!(state["model"], "observed-model");
        assert_eq!(state["effort"], "high");
        assert_eq!(state["context"]["percent"], 25.0);
        observe(&json!({"message":{"model":"<synthetic>"}}), &mut state);
        assert_eq!(state["model"], "observed-model");
    }
    #[test]
    fn missing_context_window_does_not_invent_a_percentage() {
        let mut state = json!({"context":null});
        observe(
            &json!({"payload":{"info":{"last_token_usage":{"total_tokens":250}}}}),
            &mut state,
        );
        assert!(state["context"].is_null());
    }

    #[test]
    fn transcript_cache_reuses_unchanged_file_and_invalidates_on_change() {
        let dir = std::env::temp_dir().join(format!("krill-cs-ttl-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("claude.jsonl");
        fs::write(&transcript, "{\"message\":{\"model\":\"claude-alpha\"}}\n").unwrap();

        let ctx = Context::test_context(dir.clone(), dir.clone(), "claude");
        let mut snap = ChatPollSnapshot {
            bound: true,
            agent: "claude".into(),
            session_id: "ttl-s1".into(),
            transcript_path: transcript.to_string_lossy().into_owned(),
            ..ChatPollSnapshot::default()
        };

        invalidate_all();
        let first = read(&snap, &ctx).unwrap();
        assert_eq!(first["model"], "claude-alpha");

        // Second read with identical metadata hits cache even before changing content.
        let cached = read(&snap, &ctx).unwrap();
        assert_eq!(cached["model"], "claude-alpha");

        // Changing file length invalidates cache immediately without waiting 300s.
        fs::write(
            &transcript,
            "{\"message\":{\"model\":\"claude-alpha\"}}\n{\"message\":{\"model\":\"claude-beta-newer\"}}\n",
        )
        .unwrap();
        let refreshed = read(&snap, &ctx).unwrap();
        assert_eq!(refreshed["model"], "claude-beta-newer");
        snap.bound = false;
        assert!(read(&snap, &ctx).unwrap()["model"].is_null());
        fs::remove_dir_all(&dir).ok();
    }
}
