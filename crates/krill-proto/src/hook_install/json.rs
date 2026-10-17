//! Add groups alongside user hooks; undo only the saved additions, never restore an old whole document.

use std::io;

use crate::agent::hooks::Agent;
use serde_json::Value;

use super::store::error;

pub fn encode(value: &Value) -> io::Result<String> {
    serde_json::to_string_pretty(value)
        .map(|s| s + "\n")
        .map_err(io::Error::other)
}

pub fn parse(text: Option<&str>) -> io::Result<Value> {
    let value: Value = serde_json::from_str(text.unwrap_or("{}"))?;
    if !value.is_object() {
        return Err(error("hook settings must be a JSON object"));
    }
    Ok(value)
}

pub fn install(before: Option<&str>, agent: Agent, command: &str) -> io::Result<String> {
    // GUI(관리자 에이전트 탭)와 같은 공용 머지. 모양이 바뀌면 양쪽 테스트가 함께 잡는다.
    crate::agent::hooks::install_agent_json(before, agent, command)
}

/// Undo a three-way additive patch. Scalars changed by users are left alone, arrays use saved
/// element equality (not command substrings), and newly created empty containers disappear.
pub fn undo(current: &mut Value, before: Option<&Value>, installed: &Value) {
    if before == Some(installed) {
        return;
    }
    match (current, installed) {
        (Value::Object(now), Value::Object(after)) => {
            for (key, added) in after {
                let old = before.and_then(|v| v.get(key));
                if old == Some(added) {
                    continue;
                }
                if old.is_none() && now.get(key) == Some(added) {
                    now.remove(key);
                    continue;
                }
                if let Some(value) = now.get_mut(key) {
                    undo(value, old, added);
                    if old.is_none()
                        && (value.as_object().is_some_and(serde_json::Map::is_empty)
                            || value.as_array().is_some_and(Vec::is_empty))
                    {
                        now.remove(key);
                    }
                }
            }
        }
        (Value::Array(now), Value::Array(after)) => {
            let mut original = before
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for item in after {
                if let Some(i) = original.iter().position(|v| v == item) {
                    original.remove(i);
                } else if let Some(i) = now.iter().position(|v| v == item) {
                    now.remove(i);
                } else if let Some(owned) = item.get("hooks").and_then(Value::as_array)
                    && !before.and_then(Value::as_array).is_some_and(|original| {
                        original.iter().any(|group| {
                            group
                                .get("hooks")
                                .and_then(Value::as_array)
                                .is_some_and(|hooks| owned.iter().any(|hook| hooks.contains(hook)))
                        })
                    })
                    && let Some(i) = now.iter().position(|v| {
                        v.get("hooks")
                            .and_then(Value::as_array)
                            .is_some_and(|hooks| owned.iter().any(|h| hooks.contains(h)))
                    })
                {
                    // A user added a matcher or another hook inside our group. Remove the
                    // unchanged handler and preserve the user's group fields and handlers.
                    if let Some(group) = now.get_mut(i) {
                        undo(group, None, item);
                    }
                }
            }
        }
        (now, after) if now == after => {
            if let Some(old) = before {
                *now = old.clone();
            }
        }
        _ => {}
    }
}

pub fn uninstall(current: &str, before: Option<&str>, installed: &str) -> io::Result<String> {
    let mut now = parse(Some(current))?;
    let old = parse(before)?;
    let after = parse(Some(installed))?;
    undo(&mut now, Some(&old), &after);
    // Restore byte-for-byte when only formatting or ordering changed since installation.
    if now == old {
        return Ok(before.unwrap_or("{}\n").to_owned());
    }
    encode(&now)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    proptest::proptest! {
        #[test]
        fn additive_undo_preserves_arbitrary_user_values(user in ".{0,256}", later in ".{0,256}") {
            let before = json!({"user": user, "hooks":{"AfterAgent":[{"hooks":[{"command":"user-hook"}]}]}});
            let after = install(Some(&before.to_string()), Agent::Agy, "managed command").unwrap();
            let mut current: Value = serde_json::from_str(&after).unwrap();
            current.as_object_mut().unwrap().insert("later".to_owned(), json!(later));
            let removed = uninstall(&current.to_string(), Some(&before.to_string()), &after).unwrap();
            let mut expected = before;
            expected.as_object_mut().unwrap().insert("later".to_owned(), json!(later));
            proptest::prop_assert_eq!(parse(Some(&removed)).unwrap(), expected);
        }
    }
}
