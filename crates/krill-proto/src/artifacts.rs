#![forbid(unsafe_code)]
//! 아티팩트 공유(`krill artifacts`)의 기기 단위 공개 게이트와 `--json` 출력 스키마.
//!
//! - **공개 게이트(기본 꺼짐)**: 상태 디렉터리에 [`SHARING_ENABLED_FILE`]이 있을 때만
//!   `share`·`update`가 공개 링크를 만든다. 앱 설정에서 켜고 끈다(CLI에는 켜는 명령이 없다).
//!   정책 게이트일 뿐 보안 경계는 아니다: 상태 디렉터리에 쓸 수 있는 프로세스(같은 사용자의 에이전트 포함)는
//!   파일을 직접 만들어 우회할 수 있다. 에이전트에게는 스킬 문서로 우회 금지를 안내한다.
//!   `list`·`unshare`·`delete`·`status`는 막지 않는다(이미 만든 링크를 회수할 수 있어야 한다).
//! - **스키마**: CLI가 출력하고 앱이 파싱하는 JSON 형태를 여기서 고정한다(`worktree_space`와 같은 방식).
//!   서버 응답 필드는 대부분 `Option`이라 서버가 필드를 빼도 파싱이 깨지지 않는다.

use crate::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use serde::{Deserialize, Serialize};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io;
use std::io::Write as _;
use std::path::Path;

/// 공개 허용 표시 파일(상태 디렉터리 바로 아래, 0600). 있으면 허용, 없으면 거부(기본값).
pub const SHARING_ENABLED_FILE: &str = "artifact-sharing.enabled";
/// 게이트가 꺼져 `share`·`update`를 거부할 때의 오류 코드.
pub const SHARING_DISABLED_CODE: &str = "artifact_sharing_disabled";
/// 게이트 거부 시 사람·에이전트에게 주는 다음 단계 안내.
pub const SHARING_DISABLED_NEXT_STEPS: &str = "Artifact sharing is off for this device (not only for agents). \
Ask the user to turn on Settings > Artifacts > Allow public sharing in the Krill app, \
or deliver the file locally instead. Do not retry.";
/// 공유할 수 있는 확장자(소문자).
pub const SHAREABLE_EXTENSIONS: [&str; 4] = ["html", "htm", "md", "markdown"];
/// HTML·Markdown 원본 UTF-8 파일 상한(10 MiB).
pub const MAX_CONTENT_BYTES: u64 = 10 * 1024 * 1024;
/// 기본 보존 기간. 0은 무기한이며, 양수는 갱신 시점부터의 일수다.
pub const DEFAULT_RETENTION_DAYS: u16 = 30;
pub const MAX_RETENTION_DAYS: u16 = 3650;
const RETENTION_FILE: &str = "artifact-retention-days";

/// CLI 기간 표기(`1`~`3650`, `never`)를 파싱한다.
#[must_use]
pub fn parse_retention_days(value: &str) -> Option<u16> {
    if value == "never" {
        return Some(0);
    }
    value
        .parse::<u16>()
        .ok()
        .filter(|days| (1..=MAX_RETENTION_DAYS).contains(days))
}

/// 새로 발행하는 아티팩트의 기기별 기본 보존 기간.
pub fn default_retention_days(state: &Path) -> io::Result<u16> {
    match fs::read_to_string(state.join(RETENTION_FILE)) {
        Ok(value) => value
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|days| *days <= MAX_RETENTION_DAYS)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid artifact retention days",
                )
            }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DEFAULT_RETENTION_DAYS),
        Err(error) => Err(error),
    }
}

/// 기기별 기본 기간을 저장한다. 기존 링크의 기한은 바꾸지 않는다.
pub fn set_default_retention_days(state: &Path, days: u16) -> io::Result<()> {
    if days > MAX_RETENTION_DAYS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid artifact retention days",
        ));
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state)?;
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(state.join(RETENTION_FILE))?
        .write_all(days.to_string().as_bytes())
}

/// 공개 공유가 허용됐는가.
#[must_use]
pub fn sharing_enabled(state: &Path) -> bool {
    state.join(SHARING_ENABLED_FILE).is_file()
}

/// 공개 공유를 허용한다(멱등).
///
/// # Errors
/// 상태 디렉터리를 만들거나 파일을 쓰지 못하면 OS 오류.
pub fn enable_sharing(state: &Path) -> io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state)?;
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(state.join(SHARING_ENABLED_FILE))?;
    Ok(())
}

/// 공개 공유 허용을 끈다(없어도 성공). 이미 만든 링크는 지울 때까지 남는다.
///
/// # Errors
/// 없음 외의 삭제 실패는 OS 오류.
pub fn disable_sharing(state: &Path) -> io::Result<()> {
    match fs::remove_file(state.join(SHARING_ENABLED_FILE)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// 확장자로 공유 가능한 파일인지 본다(내용·크기는 CLI가 검사한다).
#[must_use]
pub fn is_shareable_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SHAREABLE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// `share`·`update --json` 출력.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareOutcome {
    /// `created` 또는 `updated`.
    pub change: String,
    pub slug: String,
    pub share_url: String,
    #[serde(default)]
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u16>,
}

/// `status <file> --json` 출력. 네트워크 없이 로컬 공유 기록만 본다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkStatus {
    pub shared: bool,
    /// 이 기기에서 공개 공유가 허용됐는가([`sharing_enabled`]).
    pub sharing_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u16>,
}

/// `list --json` 출력(서버 응답을 그대로 넘긴다).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPage {
    #[serde(default)]
    pub artifacts: Vec<ListEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// 목록 한 줄.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListEntry {
    pub artifact: ArtifactMeta,
    #[serde(default)]
    pub share_url: String,
}

/// 서버의 아티팩트 메타데이터. `slug` 외에는 서버가 빼도 된다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactMeta {
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_size: Option<u64>,
}

impl ArtifactMeta {
    /// 표시 이름: 제목 → 원본 파일명 → slug.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.title
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| self.original_file_name.as_deref().filter(|s| !s.is_empty()))
            .unwrap_or(&self.slug)
    }

    /// Markdown 원본이면 참(아니면 HTML로 본다).
    #[must_use]
    pub fn is_markdown(&self) -> bool {
        self.source_content_type
            .as_deref()
            .is_some_and(|t| t.contains("markdown"))
            || self.original_file_name.as_deref().is_some_and(|n| {
                Path::new(n)
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown"))
            })
    }
}

/// `--json` 실패 출력: `{"error":{"code","message","nextSteps"?}}`(stdout, 비0 종료).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorOutput {
    pub error: ErrorDetail,
}

/// 기계가 읽는 오류.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_steps: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_defaults_persist_and_reject_invalid_settings() {
        let dir = std::env::temp_dir().join(format!("krill-art-retention-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        assert_eq!(default_retention_days(&dir).unwrap(), 30);
        for days in [0, 7, 30, 90, 365, MAX_RETENTION_DAYS] {
            set_default_retention_days(&dir, days).unwrap();
            assert_eq!(default_retention_days(&dir).unwrap(), days);
        }
        assert!(set_default_retention_days(&dir, MAX_RETENTION_DAYS + 1).is_err());
        assert_eq!(default_retention_days(&dir).unwrap(), MAX_RETENTION_DAYS);
        fs::write(dir.join(RETENTION_FILE), "invalid").unwrap();
        assert_eq!(
            default_retention_days(&dir).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_dir_all(dir).unwrap();
    }

    proptest::proptest! {
        #[test]
        fn retention_parser_accepts_only_supported_periods(value in ".{0,64}") {
            let parsed = parse_retention_days(&value);
            if let Some(days) = parsed {
                proptest::prop_assert!(days <= MAX_RETENTION_DAYS);
                proptest::prop_assert!(days != 0 || value == "never");
            }
        }
    }

    #[test]
    fn retention_wire_fields_keep_legacy_responses_readable() {
        for (raw, expected) in [
            ("never", Some(0)),
            ("1", Some(1)),
            ("3650", Some(3650)),
            ("0", None),
            ("-1", None),
            ("1.5", None),
            ("3651", None),
            ("", None),
        ] {
            assert_eq!(parse_retention_days(raw), expected);
        }
        let legacy: ShareOutcome = serde_json::from_str(
            r#"{"change":"created","slug":"s","shareUrl":"u","expiresAt":"date"}"#,
        )
        .unwrap();
        assert_eq!(legacy.retention_days, None);
        let permanent: ListPage = serde_json::from_str(r#"{"artifacts":[{"artifact":{"slug":"s","retentionDays":0,"expiresAt":null},"shareUrl":"u"}]}"#).unwrap();
        assert_eq!(permanent.artifacts[0].artifact.retention_days, Some(0));
        assert_eq!(permanent.artifacts[0].artifact.expires_at, None);
    }

    #[test]
    fn gate_defaults_off_and_toggles() {
        let dir = std::env::temp_dir().join(format!("krill-art-gate-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        assert!(!sharing_enabled(&dir));
        disable_sharing(&dir).unwrap();
        enable_sharing(&dir).unwrap();
        enable_sharing(&dir).unwrap();
        assert!(sharing_enabled(&dir));
        disable_sharing(&dir).unwrap();
        assert!(!sharing_enabled(&dir));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shareable_extensions_ignore_case() {
        for ok in ["a.html", "a.HTM", "b.md", "c.Markdown"] {
            assert!(is_shareable_path(Path::new(ok)), "{ok}");
        }
        for bad in ["a.txt", "html", "a.html.bak", "a"] {
            assert!(!is_shareable_path(Path::new(bad)), "{bad}");
        }
    }

    #[test]
    fn list_page_tolerates_missing_fields() {
        let page: ListPage = serde_json::from_str(
            r#"{"artifacts":[{"artifact":{"slug":"s1","title":"","originalFileName":"r.md","extra":1},"shareUrl":"u"}],"nextCursor":"c"}"#,
        )
        .unwrap();
        let a = &page.artifacts[0].artifact;
        assert_eq!(a.display_name(), "r.md");
        assert!(a.is_markdown());
        assert_eq!(page.next_cursor.as_deref(), Some("c"));
        let empty: ListPage = serde_json::from_str("{}").unwrap();
        assert!(empty.artifacts.is_empty());
        let html = ArtifactMeta {
            slug: "x".into(),
            source_content_type: Some("text/html".into()),
            ..ArtifactMeta::default()
        };
        assert!(!html.is_markdown());
        assert_eq!(html.display_name(), "x");
    }
}
