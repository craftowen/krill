//! Claude hook 이벤트 버전 게이트(MK-011 실측·버전표). 모르는 이벤트 때문에 구버전이
//! `--settings` 전체를 버리는 일을 막는다. Notification의 최초 버전은 미확인이라 넣지 않는다.

use serde::Deserialize;

/// `StopFailure` hook payload의 한도 도달 `error` 값. Claude 2.1.301 실측·공식 문서.
/// 매처가 이 값과 완전 일치해야 `rate_limit`으로 걸린다.
pub const STOP_FAILURE_RATE_LIMIT: &str = "rate_limit";

/// `StopFailure` payload에서 읽는 필드. 나머지는 무시한다.
#[derive(Deserialize)]
struct StopFailureFields {
    #[serde(alias = "hookEventName", alias = "event")]
    hook_event_name: Option<String>,
    error: Option<String>,
}

/// `claude --version` 첫 줄에서 정식 숫자 버전만 읽는다. 읽지 못하면 보수적 집합을 쓴다.
fn version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split_whitespace().next()?.split('.');
    let v = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(v)
}

/// 지원되는 이벤트만 순서대로 반환한다. 1.0.62 미만은 hook 없이 보조 감지를 쓴다.
/// 버전 미확인은 MK-011 결정의 {`PreToolUse`, `PostToolUse`, `Stop`, `SessionStart`}다.
pub fn events(text: Option<&str>) -> Vec<&'static str> {
    let v = text.and_then(version);
    if v.is_some_and(|v| v < (1, 0, 62)) {
        return Vec::new();
    }
    let gates = [
        ("PreToolUse", (1, 0, 23)),
        ("PostToolUse", (1, 0, 23)),
        ("Stop", (1, 0, 31)),
        ("SessionStart", (1, 0, 62)),
        ("UserPromptSubmit", (1, 0, 54)),
        ("SessionEnd", (1, 0, 85)),
        ("PermissionRequest", (2, 0, 45)),
        ("StopFailure", (2, 1, 78)),
        // 사이드바 서브에이전트 행. 최초 버전은 CHANGELOG 기준 추정(실측 안 함)이라 둘 다 늦은 쪽으로 묶는다.
        ("SubagentStart", (2, 0, 43)),
        ("SubagentStop", (2, 0, 43)),
    ];
    gates
        .into_iter()
        .filter(|(event, min)| {
            v.map_or_else(
                || {
                    matches!(
                        *event,
                        "PreToolUse" | "PostToolUse" | "Stop" | "SessionStart"
                    )
                },
                |v| v >= *min,
            )
        })
        .map(|(event, _)| event)
        .collect()
}

/// `hook_event_name`이 `StopFailure`이면 `error` 값을 돌려준다(OWE-710).
/// Claude 2.1.301 실측 스키마: `{hook_event_name, error, error_details?, last_assistant_message?}`.
/// JSON이 아니거나 해당 필드가 없으면 `None`.
pub fn stop_failure_error(payload: &[u8]) -> Option<String> {
    let f: StopFailureFields = serde_json::from_slice(payload).ok()?;
    if f.hook_event_name.as_deref() == Some("StopFailure") {
        f.error
    } else {
        None
    }
}

/// `StopFailure`의 `error`가 한도 도달(`rate_limit`)이면 참(OWE-710).
/// Claude 매처 기준과 같은 완전 일치(대소문자 구분)다.
#[must_use]
pub fn is_rate_limit(payload: &[u8]) -> bool {
    stop_failure_error(payload).is_some_and(|e| e == STOP_FAILURE_RATE_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest::proptest! {
        #[test]
        fn arbitrary_version_never_panics(text in ".{0,512}") {
            let events = events(Some(&text));
            proptest::prop_assert!(events.len() <= 10);
        }
    }

    #[test]
    fn gates_avoid_unknown_events_on_old_versions() {
        assert!(events(Some("1.0.61 (Claude Code)")).is_empty());
        assert!(!events(Some("2.0.44")).contains(&"PermissionRequest"));
        assert!(events(Some("2.0.45")).contains(&"PermissionRequest"));
        assert!(!events(Some("2.1.77")).contains(&"StopFailure"));
        assert!(events(Some("2.1.78")).contains(&"StopFailure"));
        assert!(!events(Some("2.1.100")).contains(&"Notification"));
        assert_eq!(events(Some("2.1.299 (Claude Code)")).len(), 10);
        assert!(!events(Some("2.0.42")).contains(&"SubagentStart"));
    }

    #[test]
    fn unreadable_version_uses_conservative_set() {
        let base = ["PreToolUse", "PostToolUse", "Stop", "SessionStart"];
        for text in [None, Some(""), Some("unknown"), Some("2.1.299-beta")] {
            assert_eq!(events(text), base);
        }
    }

    /// OWE-710: 문서·바이너리 실측의 전체 `error` 값 분류. `rate_limit`만 한도로 본다.
    #[test]
    fn stop_failure_error_classifies_documented_values() {
        let payload = |error: &str| {
            format!(
                concat!(
                    r#"{{"hook_event_name":"StopFailure","error":"{}","#,
                    r#""error_details":"429 Too Many Requests","#,
                    r#""last_assistant_message":"API Error: Rate limit reached"}}"#,
                ),
                error
            )
        };
        assert_eq!(
            stop_failure_error(payload("rate_limit").as_bytes()),
            Some("rate_limit".to_owned())
        );
        assert!(is_rate_limit(payload("rate_limit").as_bytes()));
        // 문서·바이너리의 나머지 값은 한도가 아니다.
        for error in [
            "overloaded",
            "authentication_failed",
            "oauth_org_not_allowed",
            "account_on_hold",
            "verification_required",
            "billing_error",
            "cloud_credential_error",
            "invalid_request",
            "model_not_found",
            "server_error",
            "max_output_tokens",
            "unknown",
        ] {
            let p = payload(error);
            assert_eq!(
                stop_failure_error(p.as_bytes()),
                Some(error.to_owned()),
                "{error}"
            );
            assert!(!is_rate_limit(p.as_bytes()), "{error}");
        }
    }

    /// OWE-710: 필드 누락·다른 이벤트·비JSON은 한도가 아니다(매처 완전 일치).
    #[test]
    fn stop_failure_rate_limit_rejects_lookalikes() {
        // (payload, error 값 기대, 한도 여부 기대)
        let cases: &[(&str, Option<&str>, bool)] = &[
            (r#"{"hook_event_name":"StopFailure"}"#, None, false),
            (
                r#"{"hook_event_name":"StopFailure","error":""}"#,
                Some(""),
                false,
            ),
            (
                r#"{"hook_event_name":"StopFailure","error":"Rate_Limit"}"#,
                Some("Rate_Limit"),
                false,
            ),
            (
                r#"{"hook_event_name":"StopFailure","error":" rate_limit"}"#,
                Some(" rate_limit"),
                false,
            ),
            (
                r#"{"hook_event_name":"Stop","error":"rate_limit"}"#,
                None,
                false,
            ),
            ("not json", None, false),
            ("", None, false),
            // `hookEventName` 별칭도 스키마대로 읽는다.
            (
                r#"{"hookEventName":"StopFailure","error":"rate_limit"}"#,
                Some("rate_limit"),
                true,
            ),
        ];
        for (p, error, rate) in cases {
            assert_eq!(stop_failure_error(p.as_bytes()).as_deref(), *error, "{p}");
            assert_eq!(is_rate_limit(p.as_bytes()), *rate, "{p}");
        }
    }
}
