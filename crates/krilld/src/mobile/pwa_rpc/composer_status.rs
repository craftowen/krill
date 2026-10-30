//! Display metadata from the selected transcript and its launch account's model catalog.
use super::composer::Context;
use super::{Path, Value, fs, io, json};
use krill_proto::agent_history::ChatPollSnapshot;
use std::io::{Read as _, Seek as _, SeekFrom};

const MAX_BYTES: u64 = 2 * 1024 * 1024;

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

pub(super) fn read(
    snapshot: &ChatPollSnapshot,
    context: &Context,
    state_dir: Option<&Path>,
) -> io::Result<Value> {
    let agent = context.agent.as_str();
    let mut state =
        json!({"agent":agent,"model":null,"effort":null,"context":null,"models":[],"efforts":[]});
    if snapshot.bound
        && krill_proto::agent::canonical_id(&snapshot.agent) == agent
        && let Ok(mut file) = fs::File::open(&snapshot.transcript_path)
    {
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
    }
    let profile = context
        .config
        .clone()
        .unwrap_or_else(|| context.home.join(".claude"));
    if agent == "claude"
        && krill_proto::agent::canonical_id(&snapshot.agent) == agent
        && snapshot.bound
        && let Some(observed) = state_dir.and_then(|dir| {
            krill_proto::statusline::context::read(dir, &profile, &snapshot.session_id)
        })
        && (state.get("model").is_none_or(Value::is_null)
            || state.get("model").and_then(Value::as_str) == observed.model.as_deref())
    {
        if let Some(model) = observed.model {
            set(&mut state, "model", model.into());
        }
        if let Some(effort) = observed.effort {
            set(&mut state, "effort", effort.into());
        }
        if let Some(percent) = observed
            .percent
            .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
        {
            set(&mut state, "context", json!({"percent":percent}));
        }
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
}
