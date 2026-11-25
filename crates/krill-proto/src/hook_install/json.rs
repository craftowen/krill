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
                    // A user added a matcher or another hook inside our group. Remove only the
                    // unchanged Krill handler(s) from `hooks`, preserve sibling group fields
                    // (such as `matcher`), and drop the group if no handlers remain.
                    let remove_group = if let Some(group) = now.get_mut(i)
                        && let Some(item_hooks) = item.get("hooks")
                        && let Some(group_hooks) = group.get_mut("hooks")
                    {
                        undo(group_hooks, None, item_hooks);
                        group_hooks.as_array().is_some_and(Vec::is_empty)
                    } else {
                        false
                    };
                    if remove_group {
                        now.remove(i);
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

    #[test]
    fn uninstall_preserves_matcher_on_user_extended_agy_group_and_drops_empty_claude_group() {
        // Antigravity: user appends a custom handler inside Krill's `PreToolUse` group (`matcher: "*"`).
        let agy_after = install(None, Agent::Agy, "krill hook agy").unwrap();
        let mut agy_current: Value = serde_json::from_str(&agy_after).unwrap();
        let user_handler = json!({"type": "command", "command": "user-pre-tool", "timeout": 5});
        agy_current["krill"]["PreToolUse"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(user_handler.clone());
        let agy_removed = uninstall(&agy_current.to_string(), None, &agy_after).unwrap();
        let agy_final = parse(Some(&agy_removed)).unwrap();
        assert_eq!(
            agy_final["krill"]["PreToolUse"],
            json!([{"matcher": "*", "hooks": [user_handler]}])
        );

        // Claude: user adds `matcher: "Bash"` to Krill's group with no custom handler ->
        // group must be removed instead of leaving a matcher-only object without `hooks`.
        let claude_after =
            crate::agent::hooks::install_claude_json(None, "krill hook claude").unwrap();
        let mut claude_matcher_only: Value = serde_json::from_str(&claude_after).unwrap();
        claude_matcher_only["hooks"]["PreToolUse"][0]
            .as_object_mut()
            .unwrap()
            .insert("matcher".to_owned(), json!("Bash"));
        let claude_removed =
            uninstall(&claude_matcher_only.to_string(), None, &claude_after).unwrap();
        assert_eq!(parse(Some(&claude_removed)).unwrap(), json!({}));

        // Claude: user adds both `matcher: "Bash"` and a custom handler -> both survive.
        let mut claude_extended: Value = serde_json::from_str(&claude_after).unwrap();
        let group = claude_extended["hooks"]["PreToolUse"][0]
            .as_object_mut()
            .unwrap();
        group.insert("matcher".to_owned(), json!("Bash"));
        let custom_claude = json!({"type": "command", "command": "user-bash-hook"});
        group["hooks"]
            .as_array_mut()
            .unwrap()
            .push(custom_claude.clone());
        let claude_kept = uninstall(&claude_extended.to_string(), None, &claude_after).unwrap();
        let claude_final = parse(Some(&claude_kept)).unwrap();
        assert_eq!(
            claude_final["hooks"]["PreToolUse"],
            json!([{"matcher": "Bash", "hooks": [custom_claude]}])
        );
    }
}
