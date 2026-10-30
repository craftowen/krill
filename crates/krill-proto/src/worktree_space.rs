//! 워크스페이스 디스크 용량 분석(`Space`) 및 조건별 워크트리 일괄 클린업(`Cleanup`) 데이터 타입(MK-175, OWE-729).
//!
//! 파일시스템 스캔·git·데몬 호출 엔진은 `krill worktree space|cleanup` CLI(`crates/krill/src/worktree/space.rs`)에 있고
//! (OWE-744), 이 모듈에는 CLI `--json` 출력 스키마와 순수 함수(필터·포맷)만 둔다.
//! `krill-app`은 CLI를 하위 프로세스로 실행해 이 타입으로 결과를 파싱한다.
//!
//! `--json` 스키마(고정):
//! - `krill worktree space --json` → [`SpaceReport`]
//! - `krill worktree cleanup --json` → [`CleanupReport`]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 주요 빌드 아티팩트·의존성 디렉터리 이름 목록.
pub const KNOWN_ARTIFACT_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".next",
    "dist",
    "build",
    ".turbo",
    ".venv",
    "venv",
    ".pytest_cache",
    "__pycache__",
    ".gradle",
    "vendor",
    ".cache",
    "out",
    "coverage",
    ".parcel-cache",
    ".svelte-kit",
    ".nuxt",
    "Pods",
    "DerivedData",
];

/// 하루(초).
pub const SECS_PER_DAY: u64 = 86_400;

/// 주어진 디렉터리 이름이 주요 빌드 아티팩트 디렉터리인지 판정한다.
#[must_use]
pub fn is_artifact_dir_name(name: &str) -> bool {
    KNOWN_ARTIFACT_DIRS.contains(&name)
}

/// 사람이 읽기 쉬운 바이트 단위 문자열(`0 B`, `1.5 KB`, `24.3 MB`, `2.1 GB`)로 포맷한다.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;

    if bytes >= GB {
        let whole = bytes / GB;
        let tenths = (bytes % GB).saturating_mul(10) / GB;
        format!("{whole}.{tenths} GB")
    } else if bytes >= MB {
        let whole = bytes / MB;
        let tenths = (bytes % MB).saturating_mul(10) / MB;
        format!("{whole}.{tenths} MB")
    } else if bytes >= KB {
        let whole = bytes / KB;
        let tenths = (bytes % KB).saturating_mul(10) / KB;
        format!("{whole}.{tenths} KB")
    } else {
        format!("{bytes} B")
    }
}

/// 빌드 아티팩트 종류별 용량 집계.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactUsage {
    /// 아티팩트 디렉터리 분류명(`target`, `node_modules`, `.next` 등).
    pub name: String,
    /// 워크트리 내부 상대 경로(동일 분류가 여러 개면 첫 경로).
    pub rel_path: String,
    /// 합산 바이트 수.
    pub bytes: u64,
    /// 해당 분류 디렉터리 개수.
    pub count: usize,
}

/// 개별 워크트리의 디스크 사용량 분석 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSpaceEntry {
    pub repo: PathBuf,
    pub repo_name: String,
    pub path: PathBuf,
    pub name: String,
    pub branch: Option<String>,
    pub is_main: bool,
    pub total_bytes: u64,
    pub artifact_bytes: u64,
    pub artifacts: Vec<ArtifactUsage>,
}

/// 워크스페이스 전체 디스크 용량 분석 보고서(`Space`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceReport {
    pub scanned_at: u64,
    pub total_bytes: u64,
    pub total_artifact_bytes: u64,
    pub worktrees: Vec<WorktreeSpaceEntry>,
    pub artifacts: Vec<ArtifactUsage>,
}

/// 클린업 후보의 PR/병합 상태.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPrState {
    Merged,
    Closed,
    Open,
    Draft,
    #[default]
    None,
}

impl CleanupPrState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::Closed => "closed",
            Self::Open => "open",
            Self::Draft => "draft",
            Self::None => "none",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "merged" => Some(Self::Merged),
            "closed" => Some(Self::Closed),
            "open" => Some(Self::Open),
            "draft" => Some(Self::Draft),
            "none" | "" => Some(Self::None),
            _ => None,
        }
    }
}

/// 클린업 PR 상태 필터.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPrFilter {
    /// PR이 `Merged`이거나 주 워크트리 `HEAD`에 이미 병합된 브랜치.
    Merged,
    /// PR이 `Closed`인 워크트리.
    Closed,
    /// `Merged` 또는 `Closed`(기본값).
    #[default]
    MergedOrClosed,
    /// PR 상태 무관(경과 일수·클린 상태로만 필터링).
    Any,
}

impl CleanupPrFilter {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::Closed => "closed",
            Self::MergedOrClosed => "merged-or-closed",
            Self::Any => "any",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "merged" => Ok(Self::Merged),
            "closed" => Ok(Self::Closed),
            "merged-or-closed" | "merged_or_closed" | "both" => Ok(Self::MergedOrClosed),
            "any" | "all" => Ok(Self::Any),
            other => Err(format!(
                "invalid --pr filter {other:?}; expected merged|closed|merged-or-closed|any"
            )),
        }
    }

    #[must_use]
    pub fn matches(self, pr_state: CleanupPrState, merged_into_main: bool) -> bool {
        match self {
            Self::Merged => {
                pr_state == CleanupPrState::Merged
                    || (pr_state == CleanupPrState::None && merged_into_main)
            }
            Self::Closed => pr_state == CleanupPrState::Closed,
            Self::MergedOrClosed => {
                matches!(pr_state, CleanupPrState::Merged | CleanupPrState::Closed)
                    || (pr_state == CleanupPrState::None && merged_into_main)
            }
            Self::Any => true,
        }
    }
}

/// 마지막 활동 경과 일수 프리셋(전체 / 7일 / 14일 / 30일 이상).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdleDaysPreset {
    #[default]
    Any,
    Days7,
    Days14,
    Days30,
}

impl IdleDaysPreset {
    pub const ALL: [Self; 4] = [Self::Any, Self::Days7, Self::Days14, Self::Days30];

    #[must_use]
    pub fn days(self) -> u64 {
        match self {
            Self::Any => 0,
            Self::Days7 => 7,
            Self::Days14 => 14,
            Self::Days30 => 30,
        }
    }

    #[must_use]
    pub fn from_days(days: u64) -> Self {
        match days {
            7 => Self::Days7,
            14 => Self::Days14,
            30 => Self::Days30,
            _ => Self::Any,
        }
    }
}

/// 워크트리 일괄 클린업 필터 설정.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupFilter {
    pub pr_filter: CleanupPrFilter,
    pub min_idle_days: u64,
    /// `dirty`(미커밋·미추적 변경) 워크트리를 제외하는 안전 필터(기본 `true`).
    pub exclude_dirty: bool,
}

impl Default for CleanupFilter {
    fn default() -> Self {
        Self {
            pr_filter: CleanupPrFilter::MergedOrClosed,
            min_idle_days: 0,
            exclude_dirty: true,
        }
    }
}

/// 클린업 검사 대상 워크트리 항목.
#[expect(
    clippy::struct_excessive_bools,
    reason = "서로 독립인 워크트리 git 상태 플래그(주 워크트리·잠금·dirty·HEAD 병합 여부)"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCleanupCandidate {
    pub repo: PathBuf,
    pub repo_name: String,
    pub path: PathBuf,
    pub name: String,
    pub branch: Option<String>,
    pub is_main: bool,
    pub locked: bool,
    pub dirty: bool,
    pub dirty_count: usize,
    pub merged_into_main: bool,
    pub pr_state: CleanupPrState,
    pub pr_number: Option<u64>,
    pub last_active_secs: u64,
    pub idle_days: u64,
    pub disk_bytes: u64,
    pub artifact_bytes: u64,
}

impl WorktreeCleanupCandidate {
    /// 안전 규칙에 따라 삭제 가능한 후보인지 돌려준다(주 워크트리·잠긴 워크트리는 항상 제외).
    #[must_use]
    pub fn is_removable(&self, exclude_dirty: bool) -> bool {
        if self.is_main || self.locked {
            return false;
        }
        if exclude_dirty && self.dirty {
            return false;
        }
        true
    }

    /// 주어진 클린업 필터 조건에 부합하는지 검사한다.
    #[must_use]
    pub fn matches_filter(&self, filter: &CleanupFilter) -> bool {
        if !self.is_removable(filter.exclude_dirty) {
            return false;
        }
        if self.idle_days < filter.min_idle_days {
            return false;
        }
        filter
            .pr_filter
            .matches(self.pr_state, self.merged_into_main)
    }
}

/// 개별 워크트리 클린업 실행 결과.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupItemOutcome {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub removed: bool,
    pub branch_deleted: bool,
    pub freed_bytes: u64,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

/// 일괄 클린업 실행 요약.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupSummary {
    pub removed_count: usize,
    pub failed_count: usize,
    pub branches_deleted: usize,
    pub branches_preserved: usize,
    pub freed_bytes: u64,
    pub items: Vec<CleanupItemOutcome>,
}

/// 후보 목록에 [`CleanupFilter`]를 적용해 조건을 만족하는 워크트리만 돌려준다.
#[must_use]
pub fn filter_cleanup_candidates(
    candidates: &[WorktreeCleanupCandidate],
    filter: &CleanupFilter,
) -> Vec<WorktreeCleanupCandidate> {
    candidates
        .iter()
        .filter(|c| c.matches_filter(filter))
        .cloned()
        .collect()
}

/// `krill worktree cleanup [--dry-run] --json` 표준출력 스키마.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupReport {
    /// CLI에 적용된 필터.
    pub filter: CleanupFilter,
    /// `--dry-run`이면 `true`(삭제하지 않고 `candidates`만 채운다).
    pub dry_run: bool,
    /// 필터(와 `--worktree` 지정 시 그 경로 집합)에 일치한 후보.
    pub candidates: Vec<WorktreeCleanupCandidate>,
    /// 실행 결과(`dry_run`이면 실행 중인 세션 때문에 건너뛸 항목만 사유와 함께 담고 개수는 0이다).
    pub summary: CleanupSummary,
}
