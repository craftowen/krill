//! 세션마다 지금 돌고 있는 서브에이전트 목록(사이드바 표시용). 메모리에만 두며 krilld가 재기동하면 비어 시작한다.
//!
//! - 부모의 `PreToolUse`(Agent·Task 도구): `description`·`subagent_type`·`run_in_background`를 대기열에 둔다.
//! - `SubagentStart`: 같은 `agent_type`의 대기열 첫 항목과 짝지어 목록에 올린다. `SubagentStop`: 내린다.
//! - 부모 `Stop`: 포그라운드 서브에이전트는 턴과 함께 끝났으므로 내리고, 백그라운드만 남긴다.
//! - 부모 `SessionStart`·`SessionEnd`·세션 종료: 비운다.

use krill_proto::control::{MAX_AGENT_TEXT, MAX_SUBAGENTS, MAX_TITLE_TEXT, Subagent};
use serde_json::Value;

#[derive(Debug, Default)]
pub(super) struct Roster {
    /// 아직 `SubagentStart`를 못 받은 Agent 도구 호출: (`tool_use_id`, 설명 묶음, 백그라운드 여부).
    pending: Vec<(String, Subagent, bool)>,
    /// (`agent_id`, 표시 정보, 백그라운드 여부).
    running: Vec<(String, Subagent, bool)>,
}

impl Roster {
    pub(super) fn list(&self) -> Vec<Subagent> {
        self.running.iter().map(|(_, a, _)| a.clone()).collect()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.running.is_empty()
    }

    /// hook 하나를 반영한다. `from_parent`는 부모 세션 자신의 이벤트(서브에이전트·외래 hook이 아님)인지다.
    /// 목록이 바뀌었으면 참.
    pub(super) fn observe(&mut self, payload: &[u8], ns: u64, from_parent: bool) -> bool {
        let Ok(p) = serde_json::from_slice::<Value>(payload) else {
            return false;
        };
        let event = text(&p, "hook_event_name");
        let agent_id = text(&p, "agent_id");
        match event {
            "SubagentStart" if !agent_id.is_empty() => {
                if self.running.len() >= MAX_SUBAGENTS
                    || self.running.iter().any(|(id, ..)| id == agent_id)
                {
                    return false;
                }
                let kind = clip(text(&p, "agent_type"), MAX_AGENT_TEXT);
                let at = self
                    .pending
                    .iter()
                    .position(|(_, a, _)| a.kind == kind)
                    .or_else(|| (!self.pending.is_empty()).then_some(0));
                let (mut agent, background) = at.map_or_else(
                    || {
                        let agent = Subagent {
                            kind: kind.clone(),
                            description: String::new(),
                            since_unix_ns: 0,
                        };
                        (agent, false)
                    },
                    |i| {
                        let (_, a, bg) = self.pending.remove(i);
                        (a, bg)
                    },
                );
                if !kind.is_empty() {
                    agent.kind = kind;
                }
                agent.since_unix_ns = ns;
                self.running.push((agent_id.to_owned(), agent, background));
                true
            }
            "SubagentStop" if !agent_id.is_empty() => {
                let before = self.running.len();
                self.running.retain(|(id, ..)| id != agent_id);
                self.running.len() != before
            }
            _ if !from_parent => false,
            "PreToolUse" if is_agent_tool(text(&p, "tool_name")) => {
                let id = text(&p, "tool_use_id");
                if self.pending.len() >= MAX_SUBAGENTS
                    || (!id.is_empty() && self.pending.iter().any(|(t, ..)| t == id))
                {
                    return false;
                }
                let input = p.get("tool_input").unwrap_or(&Value::Null);
                let kind = match text(input, "subagent_type") {
                    "" => "general-purpose",
                    k => k,
                };
                let agent = Subagent {
                    kind: clip(kind, MAX_AGENT_TEXT),
                    description: clip(text(input, "description"), MAX_TITLE_TEXT),
                    since_unix_ns: ns,
                };
                let background =
                    input.get("run_in_background").and_then(Value::as_bool) == Some(true);
                self.pending.push((id.to_owned(), agent, background));
                false
            }
            "PostToolUse" if is_agent_tool(text(&p, "tool_name")) => {
                let id = text(&p, "tool_use_id");
                self.pending.retain(|(t, ..)| t != id);
                false
            }
            "Stop" => {
                let before = self.running.len();
                self.pending.clear();
                self.running.retain(|(.., background)| *background);
                self.running.len() != before
            }
            "SessionStart" | "SessionEnd" => {
                let changed = !self.running.is_empty();
                *self = Self::default();
                changed
            }
            _ => false,
        }
    }
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn is_agent_tool(name: &str) -> bool {
    matches!(name, "Agent" | "Task")
}

/// UTF-8 경계에서 `max` 바이트 이하로 자른다.
fn clip(s: &str, max: usize) -> String {
    s.get(..s.floor_char_boundary(max))
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(r: &mut Roster, json: &str, ns: u64, from_parent: bool) -> bool {
        r.observe(json.as_bytes(), ns, from_parent)
    }

    #[test]
    fn pairs_description_with_start_and_clears_on_stop() {
        let mut r = Roster::default();
        let pre = r#"{"hook_event_name":"PreToolUse","tool_name":"Agent","tool_use_id":"t1","tool_input":{"description":"각도1","subagent_type":"Explore","run_in_background":true}}"#;
        assert!(!feed(&mut r, pre, 1, true));
        // 같은 이벤트가 spool로 한 번 더 와도 대기열이 늘지 않는다.
        feed(&mut r, pre, 1, true);
        let fg = r#"{"hook_event_name":"PreToolUse","tool_name":"Agent","tool_use_id":"t2","tool_input":{"description":"각도2"}}"#;
        feed(&mut r, fg, 2, true);
        assert!(feed(
            &mut r,
            r#"{"hook_event_name":"SubagentStart","agent_id":"a2","agent_type":"general-purpose"}"#,
            3,
            false,
        ));
        assert!(feed(
            &mut r,
            r#"{"hook_event_name":"SubagentStart","agent_id":"a1","agent_type":"Explore"}"#,
            4,
            false,
        ));
        let list = r.list();
        assert_eq!(list.len(), 2);
        assert_eq!(
            (list[0].description.as_str(), list[0].since_unix_ns),
            ("각도2", 3)
        );
        assert_eq!(
            (list[1].description.as_str(), list[1].kind.as_str()),
            ("각도1", "Explore")
        );

        // 서브에이전트 안의 Stop은 부모 턴 종료가 아니다.
        assert!(!feed(
            &mut r,
            r#"{"hook_event_name":"Stop","agent_id":"a1"}"#,
            5,
            false
        ));
        // 부모 Stop은 포그라운드(a2)만 내린다.
        assert!(feed(&mut r, r#"{"hook_event_name":"Stop"}"#, 6, true));
        assert_eq!(r.list().len(), 1);
        assert!(feed(
            &mut r,
            r#"{"hook_event_name":"SubagentStop","agent_id":"a1"}"#,
            7,
            false,
        ));
        assert!(r.is_empty());
    }

    #[test]
    fn start_without_pre_tool_use_still_lists_and_session_start_clears() {
        let mut r = Roster::default();
        feed(
            &mut r,
            r#"{"hook_event_name":"SubagentStart","agent_id":"a","agent_type":"Plan"}"#,
            1,
            false,
        );
        assert_eq!(r.list()[0].kind, "Plan");
        assert!(feed(
            &mut r,
            r#"{"hook_event_name":"SessionStart","source":"clear"}"#,
            2,
            true
        ));
        assert!(r.is_empty());
    }
}
