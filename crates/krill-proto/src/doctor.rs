//! `krill doctor --json` 공용 점검 스키마(MK-128, OWE-689).
//!
//! 앱 온보딩·설정 점검 화면·CLI가 같은 점검 ID와 판정을 쓴다. 여기에는
//! serde 타입만 둔다. 수집(파일·프로세스·소켓)과 판정(`krill doctor`)은
//! `krill` 크레이트, 사람용 렌더링도 같은 타입에서 나온다.

use serde::{Deserialize, Serialize};

/// `--json` 문서의 스키마 버전. 올리면 앱 파서도 함께 올린다.
pub const SCHEMA: u32 = 1;

/// stdout 한 개 JSON 문서(`krill doctor --json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub schema: u32,
    pub checks: Vec<Check>,
}

/// 점검 하나. `id`는 OWE-689 고정(`daemon.connect` 등), 앱과 공유한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub level: Level,
    pub status: Status,
    pub summary: String,
    pub detail: String,
    pub fix: Fix,
}

/// 점검 등급. 필수(`required`)가 하나라도 `fail`이면 doctor 종료 코드 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Required,
    Recommended,
    Optional,
}

/// 판정. `warn`은 종료 코드에 영향이 없다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Skip,
}

/// 고치는 방법. `value`는 `kind`에 따라 명령·복사용 텍스트·URL이다.
/// 해당 없으면(`none`·`reinstall`) 빈 문자열이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    pub kind: FixKind,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixKind {
    Command,
    Copy,
    OpenUrl,
    Reinstall,
    None,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Report {
        Report {
            schema: SCHEMA,
            checks: vec![Check {
                id: "daemon.connect".to_owned(),
                level: Level::Required,
                status: Status::Ok,
                summary: "krilld에 붙었다".to_owned(),
                detail: "instance 1".to_owned(),
                fix: Fix {
                    kind: FixKind::None,
                    value: String::new(),
                },
            }],
        }
    }

    #[test]
    fn report_json_roundtrip() {
        let text = serde_json::to_string(&sample()).unwrap();
        let back: Report = serde_json::from_str(&text).unwrap();
        assert_eq!(back, sample());
        assert_eq!(back.schema, 1);
    }

    #[test]
    fn wire_words_match_spec() {
        let text = serde_json::to_string(&sample()).unwrap();
        for word in [
            "\"required\"",
            "\"ok\"",
            "\"none\"",
            "\"daemon.connect\"",
            "\"schema\":1",
        ] {
            assert!(text.contains(word), "{word} missing: {text}");
        }
        let fix = Fix {
            kind: FixKind::OpenUrl,
            value: "https://example.com".to_owned(),
        };
        let text = serde_json::to_string(&fix).unwrap();
        assert!(text.contains("\"open_url\""), "{text}");
    }
}
