//! Display-only deltas over exact fresh native observations. No AX cache or input authority.

use std::collections::{BTreeMap, VecDeque};

use serde_json::{Value, json};

use super::{schema::Hello, transport::MAX_FRAME, view};

pub(super) const MAX_BASES: usize = 4;
pub(super) const MAX_BASE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    Compact,
    Full,
    Delta,
}

pub(super) fn format(args: &Value, hello: &Hello) -> Result<Format, String> {
    let format = match args.get("format") {
        None => Format::Compact,
        Some(Value::String(s)) if s == "compact" => Format::Compact,
        Some(Value::String(s)) if s == "full" => Format::Full,
        Some(Value::String(s)) if s == "delta" => Format::Delta,
        _ => return Err("format must be compact, full or delta".into()),
    };
    match (format, args.get("baseSnapshot")) {
        (Format::Delta, Some(Value::String(base))) if args["op"]["kind"] == "observe" => {
            generation(base, hello)?;
        }
        (Format::Delta, _) => return Err("delta requires observe and baseSnapshot".into()),
        (_, Some(_)) => return Err("baseSnapshot is only valid with format delta".into()),
        _ => {}
    }
    Ok(format)
}

fn generation(snapshot: &str, hello: &Hello) -> Result<u64, String> {
    let mut parts = snapshot.split('/');
    if parts.next() != Some(hello.epoch.as_str()) || parts.next() != Some(hello.context.as_str()) {
        return Err("snapshot belongs to another native epoch/context".into());
    }
    let number = parts.next().ok_or("missing snapshot generation")?;
    let parsed = number.parse::<u64>().ok().filter(|n| *n > 0);
    if parts.next().is_some()
        || parsed.is_none()
        || number.starts_with('0')
        || !number.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("invalid snapshot generation".into());
    }
    parsed.ok_or_else(|| "invalid snapshot generation".into())
}

struct Base {
    snapshot: String,
    data: Option<Value>,
    complete: bool,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct Bases {
    entries: VecDeque<Base>,
    bytes: usize,
    generation: u64,
}

impl Bases {
    pub fn render(&mut self, frame: Value, args: &Value, hello: &Hello) -> Result<Value, String> {
        let requested = format(args, hello)?;
        if args["op"]["kind"] != "observe" || frame["observation"]["state"] != "observed" {
            return Ok(view::render(frame, requested == Format::Full));
        }
        let snapshot = frame["data"]["snapshot"]
            .as_str()
            .ok_or("observation missing snapshot")?;
        let current_generation = generation(snapshot, hello)?;
        if frame["observation"]["snapshot"] != snapshot || current_generation <= self.generation {
            return Err("native observation identity mismatched or reused".into());
        }
        if frame["data"]["app"]["pid"] != args["op"]["pid"] {
            return Err("native observation process mismatched request".into());
        }
        self.generation = current_generation;
        let elements = frame["data"]["elements"]
            .as_array()
            .ok_or("observation missing elements")?;
        if elements.len() > usize::try_from(hello.max_nodes).unwrap_or(0) {
            return Err("native observation exceeds node limit".into());
        }
        let current = nodes(elements).ok_or("native observation has invalid display keys")?;
        let coverage = &frame["observation"]["coverage"];
        let complete = coverage["truncated"] == false
            && coverage["nodes"].as_u64() == u64::try_from(elements.len()).ok()
            && coverage["reasons"].as_array().is_some_and(Vec::is_empty)
            && !elements
                .iter()
                .any(|e| e["unexpanded"] == true || e["childrenTruncated"] == true);
        if requested == Format::Compact {
            self.retain(snapshot.to_owned(), None, complete)?;
            return Ok(view::render(frame, false));
        }
        let mut rendered = match requested {
            Format::Full => frame.clone(),
            Format::Compact => view::render(frame.clone(), false),
            Format::Delta => {
                let base = self
                    .entries
                    .iter()
                    .find(|base| Some(base.snapshot.as_str()) == args["baseSnapshot"].as_str());
                let reason = match base {
                    None => Some("base_missing_or_evicted"),
                    Some(base) if !base.complete => Some("base_partial"),
                    Some(_) if !complete => Some("current_partial"),
                    Some(base) if base.data.is_none() => Some("base_requires_full_observation"),
                    Some(base)
                        if base.data.as_ref().is_some_and(|data| {
                            !scope_known(&frame["data"]) || !scope_known(data)
                        }) =>
                    {
                        Some("observation_scope_unknown")
                    }
                    Some(base)
                        if base.data.as_ref().is_some_and(|data| {
                            data["app"] != frame["data"]["app"]
                                || data["window"] != frame["data"]["window"]
                        }) =>
                    {
                        Some("observation_scope_changed")
                    }
                    Some(_) => None,
                };
                if let Some(reason) = reason {
                    let mut fallback = view::render(frame.clone(), false);
                    fallback["data"]["deltaFallback"] =
                        json!({"reason":reason,"baseSnapshot":args["baseSnapshot"]});
                    fallback
                } else if let Some(base) = base {
                    let data = base
                        .data
                        .as_ref()
                        .ok_or("delta base is not reconstructable")?;
                    let previous = data["elements"].as_array().and_then(|a| nodes(a));
                    let previous = previous.ok_or("cached observation has invalid display keys")?;
                    let upsert: Vec<_> = elements
                        .iter()
                        .filter(|node| {
                            node["key"]
                                .as_str()
                                .is_some_and(|key| previous.get(key).copied() != Some(*node))
                        })
                        .cloned()
                        .collect();
                    let removed: Vec<_> = previous
                        .keys()
                        .filter(|key| !current.contains_key(**key))
                        .copied()
                        .collect();
                    let order: Vec<_> = elements.iter().map(|e| e["key"].clone()).collect();
                    let mut result = frame.clone();
                    if let Some(data) = result["data"].as_object_mut() {
                        data.remove("elements");
                    }
                    result["data"]["format"] = json!("delta");
                    result["data"]["baseSnapshot"] = args["baseSnapshot"].clone();
                    result["data"]["elementCount"] = json!(elements.len());
                    result["data"]["upsert"] = json!(upsert);
                    result["data"]["removed"] = json!(removed);
                    result["data"]["order"] = json!(order);
                    result
                } else {
                    return Err("missing validated delta base".into());
                }
            }
        };
        if rendered["data"]["format"] == "delta" {
            let delta_size = serde_json::to_vec(&rendered)
                .map_err(|_| "cannot encode observation delta")?
                .len();
            // A change-heavy delta can exceed the complete representation. Preserve every
            // fresh node using full output when it is cheaper, including the fallback metadata.
            let mut full = frame.clone();
            full["data"]["format"] = json!("full");
            full["data"]["deltaFallback"] = json!({"reason":"delta_not_smaller_than_full"});
            let full_size = serde_json::to_vec(&full)
                .map_err(|_| "cannot encode full observation")?
                .len();
            if full_size < delta_size && full_size <= MAX_FRAME.saturating_sub(2048) {
                rendered = full;
            }
        }
        if rendered["data"]["format"] == "delta"
            && serde_json::to_vec(&rendered).is_ok_and(|b| b.len() > MAX_FRAME.saturating_sub(2048))
        {
            rendered = view::render(frame.clone(), false);
            rendered["data"]["deltaFallback"] =
                json!({"reason":"delta_output_too_large","baseSnapshot":args["baseSnapshot"]});
        }
        // tool_result may omit oversized data. Such an observation cannot be a client base.
        let reconstructable = complete
            && (requested == Format::Full
                || matches!(rendered["data"]["format"].as_str(), Some("delta" | "full")))
            && serde_json::to_vec(&rendered)
                .is_ok_and(|b| b.len() <= MAX_FRAME.saturating_sub(2048));
        self.retain(
            snapshot.to_owned(),
            reconstructable.then(|| frame["data"].clone()),
            complete,
        )?;
        Ok(rendered)
    }

    fn retain(
        &mut self,
        snapshot: String,
        data: Option<Value>,
        complete: bool,
    ) -> Result<(), String> {
        // Compact and incomplete results cannot be reconstructed by the client. Keep only a
        // tiny diagnostic marker so the default path never retains an unused full tree.
        let bytes = if let Some(data) = &data {
            serde_json::to_vec(data)
        } else {
            serde_json::to_vec(
                &json!({"snapshot":snapshot,"complete":complete,"reconstructable":false}),
            )
        }
        .map_err(|_| "cannot encode native base")?
        .len();
        if bytes > MAX_BASE_BYTES {
            return Ok(());
        }
        while self.entries.len() >= MAX_BASES || self.bytes.saturating_add(bytes) > MAX_BASE_BYTES {
            if let Some(old) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.bytes);
            } else {
                break;
            }
        }
        self.bytes += bytes;
        self.entries.push_back(Base {
            snapshot,
            data,
            complete,
            bytes,
        });
        Ok(())
    }
}

fn scope_known(data: &Value) -> bool {
    data["app"]["pid"].as_i64().is_some_and(|pid| pid > 0)
        && data["app"]["startWitness"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        && data["window"]["index"].as_u64().is_some()
}

fn nodes(elements: &[Value]) -> Option<BTreeMap<&str, &Value>> {
    let mut nodes = BTreeMap::new();
    for node in elements {
        let key = node["key"].as_str()?;
        let digits = key.strip_prefix('e')?;
        if digits.is_empty()
            || (digits.len() > 1 && digits.starts_with('0'))
            || !digits.bytes().all(|b| b.is_ascii_digit())
            || digits.parse::<usize>().is_err()
            || nodes.insert(key, node).is_some()
        {
            return None;
        }
    }
    Some(nodes)
}

#[cfg(test)]
mod tests;
