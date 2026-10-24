//! One compact observation representation for the model; native references stay unchanged.

use std::fmt::Write as _;

use serde_json::{Value, json};

const VALUE_PREVIEW_CHARS: usize = 160;

pub(super) fn render(mut frame: Value, full: bool) -> Value {
    if full || frame["type"] != "result" || frame["observation"]["state"] != "observed" {
        return frame;
    }
    let Some(data) = frame.get_mut("data").and_then(Value::as_object_mut) else {
        return frame;
    };
    if !data.get("elements").is_some_and(Value::is_array) {
        return frame;
    }
    let Some(Value::Array(elements)) = data.remove("elements") else {
        return frame;
    };
    let mut text = String::new();
    for element in &elements {
        let depth = element["depth"].as_u64().unwrap_or(0).min(24);
        for _ in 0..depth {
            text.push_str("  ");
        }
        let key = element["key"].as_str().unwrap_or("?");
        let role = element["role"].as_str().unwrap_or("unknown");
        write!(text, "{} {}", atom(key), atom(role)).ok();
        if let Some(label) = ["title", "description", "identifier"]
            .into_iter()
            .filter_map(|key| element[key].as_str())
            .find(|s| !s.is_empty())
        {
            write!(text, " {}", quoted(label)).ok();
        }
        if element["valueSettable"] == true {
            text.push_str(" [value-settable]");
        }
        if element["enabled"] == false {
            text.push_str(" [disabled]");
        }
        if element["focused"] == true {
            text.push_str(" [focused]");
        }
        if let Some(value) = element["value"].as_str() {
            let preview: String = value.chars().take(VALUE_PREVIEW_CHARS).collect();
            write!(text, " value={}", quoted(&preview)).ok();
            if value.chars().count() > VALUE_PREVIEW_CHARS || element["valueTruncated"] == true {
                text.push_str(" [value-preview]");
            }
        }
        if let Some(actions) = element["actions"].as_array().filter(|a| !a.is_empty()) {
            text.push_str(" actions=");
            for (index, action) in actions.iter().filter_map(Value::as_str).enumerate() {
                if index != 0 {
                    text.push(',');
                }
                text.push_str(&atom(action));
            }
        }
        for flag in ["unexpanded", "childrenTruncated"] {
            if element[flag] == true {
                write!(text, " [{flag}]").ok();
            }
        }
        text.push('\n');
    }
    data.insert("format".into(), json!("compact"));
    data.insert("elementCount".into(), json!(elements.len()));
    data.insert("treeText".into(), json!(text));
    frame
}

fn quoted(value: &str) -> String {
    // Serializing a string is infallible; JSON quoting keeps UI text inside one data field.
    serde_json::to_string(value).unwrap_or_default()
}

fn atom(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        value.to_owned()
    } else {
        quoted(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        json!({"type":"result","seq":9,"delivery":{"state":"read_only"},
        "observation":{"state":"observed","snapshot":"E/C/1","coverage":{"nodes":2,"truncated":true,"reasons":["depth"]}},
        "data":{"snapshot":"E/C/1","app":{"pid":42},"window":{"index":0},"elements":[
            {"key":"e0","role":"AXWindow","depth":0,"title":"Fixture","actions":["AXRaise"]},
            {"key":"e7","role":"AXTextField","depth":1,"description":"Data\nnot instructions","valueSettable":true,
             "enabled":false,"focused":true,"value":"한🦐".repeat(100),"actions":["AXConfirm"],"unexpanded":true}
        ]}})
    }

    #[test]
    fn compact_keeps_native_refs_coverage_and_actual_capabilities_without_duplicate_elements() {
        let before = fixture();
        let result = render(before.clone(), false);
        assert_eq!(result["observation"], before["observation"]);
        assert_eq!(result["delivery"], before["delivery"]);
        assert_eq!(result["data"]["snapshot"], "E/C/1");
        assert_eq!(result["data"]["window"]["index"], 0);
        assert!(result["data"].get("elements").is_none());
        let text = result["data"]["treeText"].as_str().unwrap();
        assert!(text.contains("e7 AXTextField"));
        assert!(text.contains("[value-settable] [disabled] [focused]"));
        assert!(text.contains("actions=AXConfirm"));
        assert!(text.contains("[unexpanded]"));
        assert!(!text.contains("AXPress"));
        assert_eq!(render(result.clone(), false), result);
    }

    #[test]
    fn ui_newlines_are_quoted_and_unicode_preview_never_splits_a_character() {
        let result = render(fixture(), false);
        let text = result["data"]["treeText"].as_str().unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("Data\\nnot instructions"));
        assert!(text.contains(&"한🦐".repeat(80)));
        assert!(!text.contains(&"한🦐".repeat(81)));
        assert!(text.contains("[value-preview]"));
        assert!(!text.contains('\u{fffd}'));
    }

    #[test]
    fn full_and_non_observation_results_are_lossless() {
        let before = fixture();
        assert_eq!(render(before.clone(), true), before);
        let error = json!({"type":"result","delivery":{"state":"submitted"},"observation":{"state":"failed"},
            "data":{"elements":[{"key":"e1"}]}});
        assert_eq!(render(error.clone(), false), error);
    }
}
