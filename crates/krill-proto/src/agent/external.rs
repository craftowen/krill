//! `claude agents --json` 읽기(MK-083, T1#31·D12). CLI JSON만 쓰고 내부 파일은 읽지 않는다.
//!
//! - 입력은 `claude agents --json` stdout 바이트 그대로다. 최상위가 배열이 아니면 빈 목록이다.
//! - `kind: "background"`만 남긴다. interactive는 Krill 세션이 아니라 사이드바 대상이 아니다.
//! - `id`(짧은 id)가 없으면 건너뛴다. `pid`·`startedAt`은 숫자일 때만 `Some`이다(`stop` 뒤 잔류형에 없음이 정상).
//! - `state`·`status`·`waitingFor`는 모르는 문자열이어도 원문을 보관한다(research preview 변경에 대비).
//! - 한 원소가 깨졌으면 그 원소만 건너뛴다. 와이어 변경이 없어 프로토콜 번호가 필요 없다.

use crate::control::AgentState;

/// 표시 문자열 바이트 상한. 넘으면 문자 경계에서 자른다.
pub const MAX_TEXT: usize = 4096;
/// 한 번에 읽는 세션 수 상한. 세션이 수백 개여도 한 번 읽기를 막지 않는다.
pub const MAX_SESSIONS: usize = 4096;
/// 입력 전체 바이트 상한. 넘으면 빈 목록이다.
pub const MAX_INPUT: usize = 8 * 1024 * 1024;

/// agent view 백그라운드 세션 하나. `state_raw`·`status_raw`·`waiting_for`는 표시용 원문이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSession {
    /// `claude attach/logs/stop/rm` 인자인 짧은 id.
    pub id: String,
    /// 전체 세션 id(`sessionId`).
    pub session_id: String,
    /// 표시 이름. 실행 중에는 자동 요약으로 바뀔 수 있다.
    pub name: String,
    /// 시작 cwd.
    pub cwd: String,
    /// `stop` 뒤 잔류형에는 없다.
    pub pid: Option<u32>,
    /// `startedAt`(epoch 밀리초). 숫자일 때만 `Some`이다.
    pub started_at_ms: Option<u64>,
    /// 원문 `state`.
    pub state_raw: String,
    /// 원문 `status`.
    pub status_raw: String,
    /// 원문 `waitingFor`. 대기 때만 있다(실측 `"permission prompt"`).
    pub waiting_for: String,
}

impl ExternalSession {
    /// 사이드바 표시용 상태. `state` 우선, 없거나 모르면 `status`로, 그래도 모르면 Unknown이다.
    /// 백그라운드 `idle`은 턴 종료 후이므로 Done이다(전경 idle과 다름).
    #[must_use]
    pub fn agent_state(&self) -> AgentState {
        match normalized(&self.state_raw).as_str() {
            "working" => AgentState::Working,
            "blocked" => AgentState::Blocked,
            "done" => AgentState::Done,
            "waiting" => AgentState::Waiting,
            _ => match normalized(&self.status_raw).as_str() {
                "busy" => AgentState::Working,
                "waiting" => AgentState::Waiting,
                "idle" => AgentState::Done,
                _ => AgentState::Unknown,
            },
        }
    }
}

fn normalized(text: &str) -> String {
    text.trim().to_ascii_lowercase()
}

fn text(value: Option<&serde_json::Value>) -> String {
    let s = value
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if s.len() <= MAX_TEXT {
        return s.to_owned();
    }
    let mut end = MAX_TEXT.min(s.len());
    while !s.is_char_boundary(end) {
        let Some(prev) = end.checked_sub(1) else {
            return String::new();
        };
        end = prev;
    }
    s.get(..end).unwrap_or_default().to_owned()
}

fn u64_value(value: Option<&serde_json::Value>) -> Option<u64> {
    value?.as_u64()
}

fn parse_one(value: &serde_json::Value) -> Option<ExternalSession> {
    let map = value.as_object()?;
    if map.get("kind").and_then(serde_json::Value::as_str) != Some("background") {
        return None;
    }
    let id = text(map.get("id"));
    if id.is_empty() {
        return None;
    }
    let pid = u64_value(map.get("pid")).and_then(|n| u32::try_from(n).ok());
    Some(ExternalSession {
        id,
        session_id: text(map.get("sessionId")),
        name: text(map.get("name")),
        cwd: text(map.get("cwd")),
        pid,
        started_at_ms: u64_value(map.get("startedAt")),
        state_raw: text(map.get("state")),
        status_raw: text(map.get("status")),
        waiting_for: text(map.get("waitingFor")),
    })
}

/// `claude agents --json` stdout을 읽는다. 배열이 아니면 빈 목록, 깨진 원소는 건너뛴다.
#[must_use]
pub fn parse(text: &str) -> Vec<ExternalSession> {
    if text.len() > MAX_INPUT {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(rows) = value.as_array() else {
        return Vec::new();
    };
    rows.iter()
        .take(MAX_SESSIONS)
        .filter_map(parse_one)
        .collect()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod state_tests {
    use super::*;

    fn session(state_raw: &str, status_raw: &str) -> ExternalSession {
        ExternalSession {
            id: "ext-1".to_owned(),
            session_id: "ext-1-full".to_owned(),
            name: "test".to_owned(),
            cwd: "/tmp".to_owned(),
            pid: Some(1234),
            started_at_ms: Some(1_000),
            state_raw: state_raw.to_owned(),
            status_raw: status_raw.to_owned(),
            waiting_for: String::new(),
        }
    }

    #[test]
    fn agent_state_maps_active_and_idle_sessions() {
        assert_eq!(session("working", "").agent_state(), AgentState::Working);
        assert_eq!(session("", "busy").agent_state(), AgentState::Working);
        assert_eq!(session("waiting", "").agent_state(), AgentState::Waiting);
        assert_eq!(session("blocked", "").agent_state(), AgentState::Blocked);
        assert_eq!(session("done", "").agent_state(), AgentState::Done);
        assert_eq!(session("", "idle").agent_state(), AgentState::Done);
        assert_eq!(
            session(" WORKING ", "idle").agent_state(),
            AgentState::Working
        );
        assert_eq!(
            session("unknown", " WAITING ").agent_state(),
            AgentState::Waiting
        );
        assert_eq!(
            session("unknown", "unknown").agent_state(),
            AgentState::Unknown
        );
    }
}
