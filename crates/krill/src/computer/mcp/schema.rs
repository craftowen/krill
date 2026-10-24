//! The proxy accepts only the native operations it understands and the worker advertises.

use serde_json::{Value, json};

use super::transport::MAX_FRAME;

pub(super) const VERSION: &str = "2025-11-25";
pub(super) const CONTRACT: &str = "krill-computer-worker/1";

#[derive(Debug)]
pub(super) struct Hello {
    pub raw: Value,
    pub epoch: String,
    pub context: String,
    pub capabilities: Vec<String>,
    pub max_deadline: u64,
    pub max_gap: u64,
    pub max_text: usize,
    pub max_nodes: u64,
    pub max_frame: usize,
    pub max_batch: Option<usize>,
}

impl Hello {
    pub fn parse(raw: Value) -> Result<Self, String> {
        if raw["type"] != "hello" || raw["contract"] != CONTRACT || raw["supported"] != true {
            return Err(
                "worker contract is incompatible or native execution is unsupported".into(),
            );
        }
        let identity = |key: &str| {
            raw[key]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 128 && !s.contains('/'))
                .map(str::to_owned)
                .ok_or_else(|| format!("worker hello has invalid {key}"))
        };
        let epoch = identity("epoch")?;
        let context = identity("context")?;
        if raw["pid"].as_u64().is_none_or(|pid| pid == 0) {
            return Err("worker hello has invalid pid".into());
        }
        let capabilities = string_list(&raw["capabilities"])?;
        let unsupported = string_list(&raw["unsupported"])?;
        if !capabilities.iter().any(|kind| kind == "capabilities")
            || capabilities.iter().any(|kind| unsupported.contains(kind))
        {
            return Err("worker capability advertisement is incompatible".into());
        }
        let limit = |key: &str, max: u64| {
            raw["limits"][key]
                .as_u64()
                .filter(|n| *n > 0 && *n <= max)
                .ok_or_else(|| format!("worker hello has incompatible limit {key}"))
        };
        let max_frame = usize::try_from(limit("maxFrameBytes", MAX_FRAME as u64)?)
            .map_err(|_| "worker frame limit overflows")?;
        let max_deadline = limit("maxDeadlineMs", 30_000)?;
        let max_gap = limit("maxSeqGap", 1024)?;
        let max_text = usize::try_from(limit("maxTextUtf16", 4096)?)
            .map_err(|_| "worker text limit overflows")?;
        let max_nodes = limit("maxObserveNodes", 2000)?;
        let max_batch = if capabilities.iter().any(|kind| kind == "batch") {
            Some(usize::try_from(limit("maxBatchSteps", 4)?).map_err(|_| "batch limit overflows")?)
        } else {
            None
        };
        limit("queueCapacity", 64)?;
        limit("ledgerCapacity", 1024)?;
        Ok(Self {
            raw,
            epoch,
            context,
            capabilities,
            max_deadline,
            max_gap,
            max_text,
            max_nodes,
            max_frame,
            max_batch,
        })
    }

    pub fn validate_op(&self, op: &Value) -> Result<(), String> {
        let kind = op["kind"].as_str().ok_or("op.kind is required")?;
        if !self
            .capabilities
            .iter()
            .any(|capability| capability == kind)
        {
            return Err("operation is not advertised by this native worker".into());
        }
        match kind {
            "capabilities" | "permissions" | "apps" => fields(op, &["kind"]),
            "observe" => {
                fields(op, &["kind", "pid", "window", "limit"])?;
                if op["pid"]
                    .as_i64()
                    .is_none_or(|pid| pid <= 0 || pid > i64::from(i32::MAX))
                {
                    return Err("observe.pid must be a positive i32".into());
                }
                if op
                    .get("window")
                    .is_some_and(|v| !v.is_null() && v.as_u64().is_none())
                {
                    return Err("window must be a nonnegative native index".into());
                }
                optional_positive(op, "limit", self.max_nodes)
            }
            "action" | "click" | "set_value" | "type_text" => {
                let allowed: &[&str] = match kind {
                    "action" => &["kind", "target", "name"],
                    "click" => &["kind", "target"],
                    "set_value" => &["kind", "target", "text", "verify"],
                    _ => &["kind", "target", "text"],
                };
                fields(op, allowed)?;
                self.target(&op["target"])?;
                if kind == "action"
                    && op["name"]
                        .as_str()
                        .is_none_or(|s| s.is_empty() || s.len() > 64)
                {
                    return Err("action.name must be 1..64 bytes".into());
                }
                if matches!(kind, "set_value" | "type_text") {
                    let text = op["text"].as_str().ok_or("op.text must be a string")?;
                    if text.encode_utf16().count() > self.max_text
                        || (kind == "type_text" && text.is_empty())
                    {
                        return Err("op.text exceeds native limits or is empty".into());
                    }
                }
                if op.get("verify").is_some_and(|v| !v.is_boolean()) {
                    return Err("op.verify must be boolean".into());
                }
                Ok(())
            }
            "batch" => {
                fields(op, &["kind", "steps"])?;
                let max = self
                    .max_batch
                    .ok_or("batch is not advertised with known limits")?;
                let steps = op["steps"]
                    .as_array()
                    .filter(|steps| !steps.is_empty() && steps.len() <= max)
                    .ok_or("batch.steps must contain 1..maxBatchSteps mutations")?;
                for step in steps {
                    if !mutation_kind(step["kind"].as_str().unwrap_or("")) {
                        return Err(
                            "batch steps must be mutations; reads/nesting are unsupported".into(),
                        );
                    }
                    self.validate_op(step)?;
                }
                Ok(())
            }
            _ => Err("operation is not supported by this experimental proxy".into()),
        }
    }

    fn target(&self, target: &Value) -> Result<(), String> {
        fields(target, &["snapshot", "element"])?;
        let snapshot = target["snapshot"]
            .as_str()
            .ok_or("mutation requires a native snapshot")?;
        let mut parts = snapshot.split('/');
        if parts.next() != Some(self.epoch.as_str())
            || parts.next() != Some(self.context.as_str())
            || parts
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .is_none_or(|n| n == 0)
            || parts.next().is_some()
        {
            return Err("snapshot belongs to another native epoch/context; observe again".into());
        }
        let element = target["element"].as_str().and_then(|s| s.strip_prefix('e'));
        if element.is_none_or(|digits| {
            digits.is_empty()
                || !digits.bytes().all(|b| b.is_ascii_digit())
                || (digits.len() > 1 && digits.starts_with('0'))
                || digits.parse::<usize>().is_err()
        }) {
            return Err("target.element must be a native eN reference".into());
        }
        Ok(())
    }

    pub fn tools(&self) -> Value {
        let seq = json!({"type":"integer","minimum":1});
        let object = |properties, required| {
            json!({"type":"object", "properties":properties,
            "required":required,"additionalProperties":false})
        };
        let mut ops = Vec::new();
        let mut mutations = Vec::new();
        for kind in &self.capabilities {
            let mut properties = json!({"kind":{"const":kind}});
            let mut required = vec!["kind"];
            match kind.as_str() {
                "capabilities" | "permissions" | "apps" => {}
                "observe" => {
                    properties["pid"] = json!({"type":"integer","minimum":1,"maximum":i32::MAX});
                    properties["window"] = json!({"type":"integer","minimum":0});
                    properties["limit"] =
                        json!({"type":"integer","minimum":1,"maximum":self.max_nodes});
                    required.push("pid");
                }
                "action" | "click" | "set_value" | "type_text" => {
                    properties["target"] = object(
                        json!({"snapshot":{"type":"string"},
                        "element":{"type":"string","pattern":"^e(0|[1-9][0-9]*)$"}}),
                        json!(["snapshot", "element"]),
                    );
                    required.push("target");
                    if kind == "action" {
                        properties["name"] = json!({"type":"string","minLength":1,"maxLength":64});
                        required.push("name");
                    }
                    if matches!(kind.as_str(), "set_value" | "type_text") {
                        properties["text"] = json!({"type":"string","maxLength":self.max_text});
                        required.push("text");
                    }
                    if kind == "set_value" {
                        properties["verify"] = json!({"type":"boolean","default":false});
                    }
                }
                _ => continue,
            }
            let schema = object(properties, json!(required));
            if mutation_kind(kind) {
                mutations.push(schema.clone());
            }
            ops.push(schema);
        }
        if let Some(max) = self.max_batch.filter(|_| !mutations.is_empty()) {
            ops.push(object(
                json!({"kind":{"const":"batch"},"steps":{"type":"array","minItems":1,
                "maxItems":max,"items":{"oneOf":mutations}}}),
                json!(["kind", "steps"]),
            ));
        }
        let mut execute = object(
            json!({"seq":seq,"deadlineMs":{"type":"integer","minimum":1,"maximum":self.max_deadline},
            "op":{"oneOf":ops},"format":{"type":"string","enum":["compact","full","delta"],"default":"compact"},
            "baseSnapshot":{"type":"string","minLength":5,"maxLength":280}}),
            json!(["seq", "deadlineMs", "op"]),
        );
        execute["oneOf"] = json!([
            {"properties":{"format":{"enum":["compact","full"]}},"not":{"required":["baseSnapshot"]}},
            {"properties":{"format":{"const":"delta"},"op":{"properties":{"kind":{"const":"observe"}}}},"required":["format","baseSnapshot"]}
        ]);
        json!({"tools":[
            {"name":"computer_session","description":"Experimental native session hello: epoch, context, actual capabilities and limits. No desktop input.",
                "inputSchema":object(json!({}),json!([])),"annotations":{"readOnlyHint":true}},
            {"name":"computer_execute","description":"Execute one native operation. Supply a positive monotonic seq; retain it on retries. Mutations require this session's native snapshot target. Submitted is not verified; Unknown closes the session. No automatic retry or fallback.",
                "inputSchema":execute},
            {"name":"computer_status","description":"Query the original native seq without issuing desktop input. A missing result never permits replay.",
                "inputSchema":object(json!({"seq":seq}),json!(["seq"])),"annotations":{"readOnlyHint":true}},
            {"name":"computer_cancel","description":"Best effort cancellation before native dispatch. too_late does not undo posted input.",
                "inputSchema":object(json!({"seq":seq}),json!(["seq"]))}
        ]})
    }
}

fn mutation_kind(kind: &str) -> bool {
    matches!(kind, "action" | "click" | "set_value" | "type_text")
}

fn string_list(value: &Value) -> Result<Vec<String>, String> {
    let values = value
        .as_array()
        .filter(|a| a.len() <= 64)
        .ok_or("invalid worker capability list")?;
    let mut seen = std::collections::BTreeSet::new();
    values
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty() && s.len() <= 64)
                .filter(|s| seen.insert(*s))
                .map(str::to_owned)
                .ok_or_else(|| "invalid worker capability".into())
        })
        .collect()
}

pub(super) fn fields(value: &Value, allowed: &[&str]) -> Result<(), String> {
    let object = value.as_object().ok_or("expected an object")?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unexpected field; caller cannot supply epoch or context".into());
    }
    Ok(())
}

fn optional_positive(value: &Value, key: &str, max: u64) -> Result<(), String> {
    if value
        .get(key)
        .is_some_and(|v| !v.is_null() && v.as_u64().is_none_or(|n| n == 0 || n > max))
    {
        return Err(format!("{key} must be a positive bounded integer"));
    }
    Ok(())
}

pub(super) fn is_error(value: &Value) -> bool {
    value.get("error").is_some_and(|e| !e.is_null())
        || matches!(
            value["state"].as_str(),
            Some("unknown" | "not_dispatched" | "expired" | "unseen")
        )
        || matches!(
            value["delivery"]["state"].as_str(),
            Some("partial" | "unknown" | "not_dispatched")
        )
        || value["observation"]["state"] == "failed"
        || matches!(
            value["verification"]["state"].as_str(),
            Some("unsatisfied" | "unknown")
        )
        || value["cleanup"]["state"] == "incomplete"
        || matches!(value["type"].as_str(), Some("rejected" | "protocol_error"))
}

/// A known partial batch result still permits observations/new explicit requests. Unknown or
/// incomplete cleanup requires process termination; killing cannot undo already posted input.
pub(super) fn must_close(value: &Value) -> bool {
    value["state"] == "unknown"
        || value["delivery"]["state"] == "unknown"
        || value["cleanup"]["state"] == "incomplete"
        || value["quiescing"] == true
        || value["data"]["quiescing"] == true
        || matches!(
            value["error"]["code"].as_str(),
            Some("quiescing" | "executor_quiescing")
        )
}

pub(super) fn unknown(seq: u64, reason: &str) -> Value {
    json!({"type":"result","seq":seq,"state":"unknown",
        "delivery":{"state":"unknown","reason":reason},
        "observation":{"state":"failed","reason":reason},
        "verification":{"state":"unknown","reason":reason},
        "cleanup":{"state":"unknown","reason":"worker termination cannot undo posted effects"},
        "error":{"code":"unknown","message":reason}})
}

/// One structured copy of native data, with a small human summary. Never repeat a giant tree as text.
pub(super) fn tool_result(mut data: Value) -> Value {
    // Reserve room for the MCP envelope, bounded request ID and summary. Trim only observation
    // data; submitted delivery and the other result axes must survive framing limits.
    if serde_json::to_vec(&data).is_ok_and(|bytes| bytes.len() > MAX_FRAME.saturating_sub(2048))
        && let Some(object) = data.as_object_mut()
    {
        object.remove("data");
        data["observation"] = json!({"state":"failed","reason":"output_too_large"});
        data["error"] =
            json!({"code":"output_too_large","message":"native data omitted to fit MCP envelope"});
    }
    let failed = is_error(&data);
    let summary = if failed {
        "Native operation reported an error or uncertain outcome; inspect structuredContent axes."
    } else {
        "Native result; inspect structuredContent. Submitted input is not proof of completion."
    };
    json!({"content":[{"type":"text","text":summary}],"structuredContent":data,"isError":failed})
}
