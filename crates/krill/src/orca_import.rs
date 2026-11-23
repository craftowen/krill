// Automation design reference: Orca (MIT, Copyright (c) 2026 Lovecast Inc.).
// Shared schedule/preset and automation control behavior; independently implemented for Krill.
//! Orca 데이터 스캐너 & 원클릭 전체 이관 엔진 (MK-172 / OWE-726).
//!
//! `~/Library/Application Support/orca`의 활성 프로필(`orca-profile-index.json` →
//! `profiles/<id>/profile-state.db` 및 `orca-data.json` 폴백), 다계정 디렉터리
//! (`claude-profiles/*/home`, `claude-accounts/*/auth`, `codex-accounts/*/home`),
//! 원격 환경(`orca-environments.json`)을 스캔하고 Krill 설정 파일들로 멱등 이관한다.
//!
//! ## 아키텍처 원칙
//! - **계정 쓰기 단일 경로(AS-03)**: `accounts` 도메인 실제 쓰기(`dry_run == false`)는 로컬에서 `accounts.toml`이나
//!   `active-accounts.env`를 직접 덮어쓰지 않고, 실행 중인 `krilld`에 `UsageRequest::ListAccounts` /
//!   `UsageRequest::AddAccount` / `UsageRequest::SwitchAccount`를 보내 `SAVE_LOCK` 아래에서 처리한다.
//!   `krilld`가 떠 있지 않으면 데몬을 자동 기동하지 않고 `accounts` 도메인을 `'데몬 필요'` 사유(`skipped = true`,
//!   `skipped_reason = Some("데몬 필요: ...")`)로 건너뛰며 다른 도메인(`ssh`, `projects`, `settings`)만 진행한다.
//! - **크레이트 경계**: `sqlite3` 서브프로세스 호출과 파일 이관 엔진은 와이어 포맷 크레이트(`krill-proto`)가 아니라
//!   CLI 크레이트(`crates/krill`)에 위치한다. `OWE-727` GUI 위저드는 `krill import orca [--dry-run] --json`을
//!   호출하여 도메인별 계획·결과·스킵 사유가 담긴 [`OrcaImportReport`] JSON을 파싱해 사용한다.
//!
//! ## `--json` 출력 스키마 (`OWE-727` GUI 위저드 연동 규약)
//! ```json
//! {
//!   "orca_dir": "/Users/alice/Library/Application Support/orca",
//!   "profile_id": "default",
//!   "dry_run": true,
//!   "domains": ["accounts", "ssh", "projects", "settings", "sessions"],
//!   "accounts": {
//!     "skipped": false,
//!     "skipped_reason": null,
//!     "claude_added": 1,
//!     "claude_updated": 0,
//!     "codex_added": 1,
//!     "codex_updated": 0,
//!     "active_claude": "claude-dev-example-com-dev-org",
//!     "active_codex": "codex-codex-dev-example-com-dev-team",
//!     "items": [
//!       {
//!         "id": "claude-dev-example-com-dev-org",
//!         "provider": "claude",
//!         "label": "dev@example.com (Dev Org)",
//!         "email": "dev@example.com",
//!         "organization_or_workspace": "Dev Org",
//!         "profile_dir": "/Users/alice/Library/Application Support/orca/claude-profiles/<uuid>/home",
//!         "active": true,
//!         "runnable": true
//!       }
//!     ]
//!   },
//!   "ssh": {
//!     "skipped": false,
//!     "skipped_reason": null,
//!     "hosts_added": 1,
//!     "hosts_updated": 0,
//!     "environments_merged": 0,
//!     "items": [
//!       {
//!         "id": "prod-srv",
//!         "label": "Prod Server",
//!         "destination": "deploy@prod.example.com",
//!         "port": 2200,
//!         "identity_file": "~/.ssh/id_ed25519",
//!         "imported": false
//!       }
//!     ]
//!   },
//!   "projects": {
//!     "skipped": false,
//!     "skipped_reason": null,
//!     "projects_added": 1,
//!     "projects_updated": 0,
//!     "worktrees_migrated": 1,
//!     "lineage_migrated": 0,
//!     "items": [
//!       {
//!         "id": "r1",
//!         "path": "/Users/dev/project-alpha",
//!         "display_name": "Project Alpha",
//!         "kind": "git",
//!         "badge_color": "#3b82f6",
//!         "has_setup_script": true
//!       }
//!     ]
//!   },
//!   "settings": {
//!     "skipped": false,
//!     "skipped_reason": null,
//!     "settings_updated": true,
//!     "default_agent": "claude",
//!     "quick_commands_merged": 1,
//!     "agents_updated": 1,
//!     "schedules_merged": 1
//!   },
//!   "sessions": {
//!     "skipped": false,
//!     "skipped_reason": null,
//!     "resumed": 1,
//!     "already_resumed": 0,
//!     "items": [
//!       {
//!         "agent": "claude",
//!         "id": "<claude-session-uuid>",
//!         "cwd": "/Users/dev/project-alpha",
//!         "title": "작업 A",
//!         "resumed": true
//!       }
//!     ]
//!   }
//! }
//! ```
//!
//! 스키마·경로 참고:
//! `.ref/orca/src/main/persistence.ts`, `.ref/orca/src/main/profile-Criterion.ts`,
//! `.ref/orca/src/main/claude-profile-paths.ts`, `.ref/orca/src/main/codex-accounts.ts`
//! (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use krill_proto::agent::{self, Agents, Permission, Profile as AgentProfile};
use krill_proto::agent_history::{
    Action, FEAT_AGENT_HISTORY, FEAT_AI_VAULT, Response, VaultAgent, VaultTarget,
};
use krill_proto::bots::{
    BOT_LOOK_AROUND_FOCUS_MAX_BYTES, BotConfig, BotGroupChat, BotId, BotItemActor, BotItemDecision,
    BotItemStatus, BotMember, BotScheduleConfig, BotSlug, BotSuggestion, ExternalBotCandidate,
    ExternalBotSourceApp, ExternalSource, bot_slug_from_name, discover_external_bots_filtered,
    expand_tilde, init_bot_home, soul_description, strip_png_metadata, validate_bot_id,
    validate_bot_slug,
};
use krill_proto::config_document;
use krill_proto::control::{FEAT_CONTROL, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::orchestration::{WorkspaceStatus, WorktreeStore, worktree_key};
use krill_proto::schedule::{self, Cron, MAX_SCHEDULES, Schedule, Schedules};
use krill_proto::screen::Msg as _;
use krill_proto::usage::{
    AccountInfo, AccountsSnapshot, ConnectionStatus, FEAT_ACCOUNT_CONNECTION, FEAT_ACCOUNT_POLICY,
    FEAT_USAGE, FEAT_USAGE_AGY, FEAT_USAGE_MANAGE, FEAT_USAGE_RATELIMIT, FEAT_USAGE_RULES,
    FEAT_USAGE_STATUS, ProviderKind, UsageReply, UsageRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

mod account_sources;

pub const PROJECTS_FILE: &str = "projects.toml";
const HOSTS_FILE: &str = "hosts.toml";
const SETTINGS_FILE: &str = "settings.toml";
const MAX_PROJECTS_BYTES: u64 = 512 * 1024;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
/// 앱이 같은 설정 파일을 동시에 저장해 원본이 바뀌었을 때 도메인을 다시 계산하는 최대 횟수.
const CONFLICT_RETRIES: usize = 3;

/// rename 직전 원본이 처음 읽은 내용과 달라 덮어쓰기를 포기했다(앱 등 다른 프로세스의 저장).
#[derive(Debug)]
struct ConfigConflict(PathBuf);

impl std::fmt::Display for ConfigConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} changed while importing", self.0.display())
    }
}

impl std::error::Error for ConfigConflict {}

fn is_conflict(e: &io::Error) -> bool {
    matches!(e.get_ref(), Some(inner) if inner.is::<ConfigConflict>())
}

/// 설정 파일 원본(없으면 `None`). 파싱보다 먼저 읽어 두면, 그 사이 변경은 거짓 충돌(재계산)로만 이어진다.
fn read_original(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 원본이 바뀌어 쓰기를 포기한 도메인을 처음부터 다시 계산한다(최대 [`CONFLICT_RETRIES`]회).
/// 끝까지 충돌하면 그 도메인은 기본 요약으로 두고 경고만 남긴다. 경고는 마지막 시도의 것만 보탠다.
fn retry_on_conflict<T: Default>(
    domain: &str,
    warnings: &mut Vec<String>,
    mut run: impl FnMut(&mut Vec<String>) -> io::Result<T>,
) -> io::Result<T> {
    let mut attempt = 0;
    loop {
        let mut local = Vec::new();
        match run(&mut local) {
            Err(e) if is_conflict(&e) => {
                if attempt >= CONFLICT_RETRIES {
                    warnings.push(format!(
                        "{domain} skipped: {e}; retried {CONFLICT_RETRIES} times — close Krill or rerun the import"
                    ));
                    return Ok(T::default());
                }
                attempt += 1;
            }
            res => {
                warnings.extend(local);
                return res;
            }
        }
    }
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.into())
}

fn invalid_data(msg: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// 이관 대상 도메인.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportDomain {
    Accounts,
    Ssh,
    Projects,
    Settings,
    Sessions,
    Bots,
}

impl ImportDomain {
    pub const ALL: [Self; 6] = [
        Self::Accounts,
        Self::Ssh,
        Self::Projects,
        Self::Settings,
        Self::Sessions,
        Self::Bots,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accounts => "accounts",
            Self::Ssh => "ssh",
            Self::Projects => "projects",
            Self::Settings => "settings",
            Self::Sessions => "sessions",
            Self::Bots => "bots",
        }
    }

    pub fn parse(raw: &str) -> io::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "accounts" | "account" => Ok(Self::Accounts),
            "ssh" | "hosts" | "remote" => Ok(Self::Ssh),
            "projects" | "project" | "repos" | "worktrees" => Ok(Self::Projects),
            "settings" | "setting" | "config" => Ok(Self::Settings),
            "sessions" | "session" | "tabs" => Ok(Self::Sessions),
            "bots" | "bot" => Ok(Self::Bots),
            other => Err(invalid(format!(
                "unknown import domain '{other}' (expected accounts,ssh,projects,settings,sessions,bots)"
            ))),
        }
    }

    pub fn parse_list(csv: &str) -> io::Result<Vec<Self>> {
        let mut set = BTreeSet::new();
        for part in csv.split(',') {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                continue;
            }
            set.insert(Self::parse(trimmed)?);
        }
        if set.is_empty() {
            return Err(invalid(
                "--only requires at least one domain (accounts,ssh,projects,settings,sessions,bots)",
            ));
        }
        Ok(set.into_iter().collect())
    }
}

/// 이관 실행 옵션.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub home: PathBuf,
    pub state_dir: PathBuf,
    pub orca_dir: PathBuf,
    pub domains: Vec<ImportDomain>,
    pub dry_run: bool,
}

impl ImportOptions {
    /// 환경 변수(`HOME`, `KRILL_STATE_DIR`, `KRILL_ORCA_DIR`)와 기본 경로에서 옵션을 조립한다.
    pub fn from_env(
        orca_dir_override: Option<PathBuf>,
        domains: Vec<ImportDomain>,
        dry_run: bool,
    ) -> io::Result<Self> {
        let home = krill_proto::os::home_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "user home is not set"))?;
        let state_dir = daemon::state_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "state directory not found"))?;
        let orca_dir = orca_dir_override
            .or_else(|| std::env::var_os("KRILL_ORCA_DIR").map(PathBuf::from))
            .unwrap_or_else(|| default_orca_dir(&home));
        Ok(Self {
            home,
            state_dir,
            orca_dir,
            domains: if domains.is_empty() {
                ImportDomain::ALL.to_vec()
            } else {
                domains
            },
            dry_run,
        })
    }
}

#[must_use]
pub fn default_orca_dir(home: &Path) -> PathBuf {
    #[cfg(windows)]
    let roots = [
        std::env::var_os("APPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from),
        Some(home.join("AppData/Roaming")),
    ];
    #[cfg(target_os = "macos")]
    let roots = [Some(home.join("Library/Application Support"))];
    #[cfg(not(any(windows, target_os = "macos")))]
    let roots = [
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from),
        Some(home.join(".config")),
    ];
    discover_orca_dir(roots.into_iter().flatten())
}

fn discover_orca_dir(roots: impl IntoIterator<Item = PathBuf>) -> PathBuf {
    let candidates: Vec<_> = roots
        .into_iter()
        .flat_map(|root| [root.join("orca"), root.join("Orca")])
        .collect();
    candidates
        .iter()
        .find(|p| p.join("orca-profile-index.json").is_file() || p.join("profiles").is_dir())
        .or_else(|| candidates.iter().find(|p| p.is_dir()))
        .or_else(|| candidates.first())
        .cloned()
        .unwrap_or_default()
}

/// 이관 결과 요약 리포트(`--json` 출력 및 `OWE-727` UI 미리보기 겸용).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrcaImportReport {
    pub orca_dir: String,
    pub profile_id: String,
    pub dry_run: bool,
    pub domains: Vec<String>,
    pub accounts: AccountsImportSummary,
    pub ssh: SshImportSummary,
    pub projects: ProjectsImportSummary,
    pub settings: SettingsImportSummary,
    #[serde(default)]
    pub sessions: SessionsImportSummary,
    #[serde(default)]
    pub bots: BotsImportSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// 봇(`bots`) 도메인 계획·결과·외부 앱 후보 목록.
/// `~/.orca/bots/*` 및 `<orca_dir>/bots/*`를 `<state_dir>/bots/`로 멱등 이관한다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotsImportSummary {
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub skipped_reason: Option<String>,
    #[serde(default)]
    pub bots_added: usize,
    #[serde(default)]
    pub bots_skipped: usize,
    #[serde(default)]
    pub items_migrated: usize,
    #[serde(default)]
    pub groups_migrated: usize,
    #[serde(default)]
    pub suggestions_migrated: usize,
    #[serde(default)]
    pub schedules_migrated: usize,
    #[serde(default)]
    pub items: Vec<ImportedBotSummary>,
    /// OpenClaw·Hermes·OpenBot에서 발견된 봇 후보(비밀 설정·키는 열지 않음).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_candidates: Vec<ExternalBotCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedBotSummary {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub agent_id: String,
    pub source_path: String,
    #[serde(default)]
    pub has_soul: bool,
    #[serde(default)]
    pub has_memory: bool,
    #[serde(default)]
    pub memory_topic_count: usize,
    /// 이미 Krill에 존재해 건너뛰었으면 `true`.
    #[serde(default)]
    pub already_exists: bool,
}

/// Orca 열린 탭의 에이전트 세션(`sessions`) 도메인 계획·결과.
/// 실제 이관(`dry_run == false`)은 `krilld`에 `ResumeVault`를 보내 각 세션을 그 워크트리에서 이어 띄운다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsImportSummary {
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub skipped_reason: Option<String>,
    /// 이번 실행에서 `krilld`가 띄운 세션 수.
    #[serde(default)]
    pub resumed: usize,
    /// 이전 이관에서 이미 띄워 건너뛴 세션 수(재실행 멱등).
    #[serde(default)]
    pub already_resumed: usize,
    #[serde(default)]
    pub items: Vec<ImportedSessionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedSessionSummary {
    pub agent: String,
    pub id: String,
    pub cwd: String,
    #[serde(default)]
    pub title: Option<String>,
    /// 이번 실행에서 띄웠거나 이전 이관에서 이미 띄운 세션이면 `true`.
    #[serde(default)]
    pub resumed: bool,
}

/// 다계정(`accounts`) 도메인 계획·결과·스킵 사유.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountsImportSummary {
    /// 도메인이 스킵되었는지 여부 (`krilld` 데몬 미실행 시 `true`, `--only` 제외 시 `true`).
    #[serde(default)]
    pub skipped: bool,
    /// 스킵 사유 (예: `"데몬 필요: ..."`).
    #[serde(default)]
    pub skipped_reason: Option<String>,
    pub claude_added: usize,
    pub claude_updated: usize,
    pub codex_added: usize,
    pub codex_updated: usize,
    pub active_claude: Option<String>,
    pub active_codex: Option<String>,
    /// Antigravity(agy): Orca 암호화 vault 감지 여부. 개별 이관은 불가(OWE-896·OWE-721) —
    /// Krill은 호스트 agy 로그인을 공유해 사용한다.
    #[serde(default)]
    pub agy_detected: bool,
    /// agy 미이관 사유(감지됐을 때만).
    #[serde(default)]
    pub agy_note: Option<String>,
    #[serde(default)]
    pub items: Vec<ImportedAccountSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedAccountSummary {
    pub id: String,
    pub provider: String,
    pub label: String,
    pub email: Option<String>,
    pub organization_or_workspace: Option<String>,
    pub profile_dir: String,
    pub active: bool,
    pub runnable: bool,
    /// 실제 provider auth probe 결과(`connected`·`checking`·`needs_login`·`not_installed`·
    /// `check_failed`; 데몬·프로브 없이 보고하면 `unknown`). 비밀값은 절대 담지 않는다.
    #[serde(default)]
    pub connection: String,
}

/// SSH 호스트(`ssh`) 도메인 계획·결과·스킵 사유.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshImportSummary {
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub skipped_reason: Option<String>,
    pub hosts_added: usize,
    pub hosts_updated: usize,
    pub environments_merged: usize,
    #[serde(default)]
    pub items: Vec<ImportedHostSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedHostSummary {
    pub id: String,
    pub label: String,
    pub destination: String,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub imported: bool,
}

/// 프로젝트·워크트리(`projects`) 도메인 계획·결과·스킵 사유.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectsImportSummary {
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub skipped_reason: Option<String>,
    pub projects_added: usize,
    pub projects_updated: usize,
    pub worktrees_migrated: usize,
    pub lineage_migrated: usize,
    /// 이관된 워크트리 코멘트 수(OWE-882). 구 JSON과 호환되도록 `default`다.
    #[serde(default)]
    pub comments_migrated: usize,
    #[serde(default)]
    pub items: Vec<ImportedProjectSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedProjectSummary {
    pub id: String,
    pub path: String,
    pub display_name: String,
    pub kind: String,
    pub badge_color: Option<String>,
    pub has_setup_script: bool,
}

/// 에이전트·터미널·스케줄 설정(`settings`) 도메인 계획·결과·스킵 사유.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsImportSummary {
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub skipped_reason: Option<String>,
    pub settings_updated: bool,
    pub default_agent: Option<String>,
    /// 가져오지 않은 터미널 글꼴과 사유(OWE-899). `None`이면 글꼴을 반영했거나 지정이 없었다.
    #[serde(default)]
    pub terminal_font_skipped: Option<String>,
    pub quick_commands_merged: usize,
    pub agents_updated: usize,
    pub schedules_merged: usize,
}

/// `<state_dir>/projects.toml`에 저장되는 등록 프로젝트·워크트리 메타·계보 저장소.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectsStore {
    #[serde(default)]
    pub projects: Vec<StoredProject>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub worktrees: BTreeMap<String, StoredWorktreeMeta>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lineage: BTreeMap<String, StoredWorktreeLineage>,
}

/// 프로젝트 아이콘(Orca `RepoIcon`). `kind`는 `lucide`(값=아이콘 이름)·`emoji`·`image`(값=https URL
/// 또는 `<state_dir>/repo-icons/`에 풀어 둔 로컬 파일 경로)다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRepoIcon {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredProject {
    pub id: String,
    pub path: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_icon: Option<StoredRepoIcon>,
    /// `"git"` 또는 `"folder"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_base_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symlink_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_script: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredWorktreeMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_status: Option<String>,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredWorktreeLineage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    pub worktree_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_point_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
}

impl ProjectsStore {
    pub fn load(state_dir: &Path) -> io::Result<Self> {
        let path = state_dir.join(PROJECTS_FILE);
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let mut raw = String::new();
        file.take(MAX_PROJECTS_BYTES.saturating_add(1))
            .read_to_string(&mut raw)?;
        if u64::try_from(raw.len()).unwrap_or(u64::MAX) > MAX_PROJECTS_BYTES {
            return Err(invalid_data("projects.toml exceeds size limit"));
        }
        toml::from_str(&raw).map_err(invalid_data)
    }

    /// `original`(로드 전에 읽은 원본)이 그대로일 때만 원자 교체한다.
    pub fn save(&self, state_dir: &Path, original: Option<&str>) -> io::Result<()> {
        let path = state_dir.join(PROJECTS_FILE);
        let _guard = config_document::lock_toml(&path)?;
        let body = config_document::save_toml(&path, self)?;
        if u64::try_from(body.len()).unwrap_or(u64::MAX) > MAX_PROJECTS_BYTES {
            return Err(invalid_data("projects.toml exceeds size limit"));
        }
        write_atomic_0600(&path, body.as_bytes(), original)
    }
}

/// 저장소 경로(`repo_top`)에 해당하는 이관된 Orca 훅/심볼릭 링크 설정을 조회한다.
#[cfg(test)]
#[must_use]
pub fn repo_hook_config_in(state_dir: &Path, repo_top: &Path) -> Option<StoredProject> {
    let store = ProjectsStore::load(state_dir).ok()?;
    let candidates = path_aliases(repo_top);
    store
        .projects
        .into_iter()
        .find(|p| candidates.iter().any(|c| c == &p.path))
}

#[cfg(test)]
fn path_aliases(path: &Path) -> Vec<String> {
    let mut out = vec![path.to_string_lossy().into_owned()];
    if let Ok(canon) = fs::canonicalize(path) {
        let s = canon.to_string_lossy().into_owned();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    if let Ok(rest) = path.strip_prefix("/private") {
        let s = Path::new("/").join(rest).to_string_lossy().into_owned();
        if !out.contains(&s) {
            out.push(s);
        }
    } else if let Ok(rest) = path.strip_prefix("/") {
        let s = Path::new("/private")
            .join(rest)
            .to_string_lossy()
            .into_owned();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// Orca 데이터를 스캔하고 선택된 도메인들을 이관한다(`dry_run = true`면 파일 변경 없이 결과만 계산).
pub fn run_import(opts: &ImportOptions) -> io::Result<OrcaImportReport> {
    if !opts.orca_dir.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Orca directory not found: {}", opts.orca_dir.display()),
        ));
    }
    let mut warnings = Vec::new();
    let profile_id = resolve_active_profile_id(&opts.orca_dir);
    let domains_map = load_orca_domains(&opts.orca_dir, &profile_id, &mut warnings);

    let mut report = OrcaImportReport {
        orca_dir: opts.orca_dir.display().to_string(),
        profile_id,
        dry_run: opts.dry_run,
        domains: opts.domains.iter().map(|d| d.as_str().to_owned()).collect(),
        ..OrcaImportReport::default()
    };

    let selected: BTreeSet<ImportDomain> = opts.domains.iter().copied().collect();

    if selected.contains(&ImportDomain::Accounts) {
        report.accounts = import_accounts_domain(opts, &domains_map, &mut warnings);
    } else {
        report.accounts.skipped = true;
        report.accounts.skipped_reason = Some("excluded by --only".to_owned());
    }
    if selected.contains(&ImportDomain::Ssh) {
        report.ssh = retry_on_conflict("ssh", &mut warnings, |w| {
            import_ssh_domain(opts, &domains_map, w)
        })?;
    } else {
        report.ssh.skipped = true;
        report.ssh.skipped_reason = Some("excluded by --only".to_owned());
    }
    if selected.contains(&ImportDomain::Projects) {
        report.projects = retry_on_conflict("projects", &mut warnings, |w| {
            import_projects_domain(opts, &domains_map, w)
        })?;
    } else {
        report.projects.skipped = true;
        report.projects.skipped_reason = Some("excluded by --only".to_owned());
    }
    if selected.contains(&ImportDomain::Settings) {
        report.settings = retry_on_conflict("settings", &mut warnings, |w| {
            import_settings_domain(opts, &domains_map, w)
        })?;
    } else {
        report.settings.skipped = true;
        report.settings.skipped_reason = Some("excluded by --only".to_owned());
    }
    if selected.contains(&ImportDomain::Sessions) {
        report.sessions = import_sessions_domain(opts, &domains_map, &mut warnings);
    } else {
        report.sessions.skipped = true;
        report.sessions.skipped_reason = Some("excluded by --only".to_owned());
    }
    if selected.contains(&ImportDomain::Bots) {
        report.bots = import_bots_domain(opts, &report.profile_id, &domains_map, &mut warnings)?;
    } else {
        report.bots.skipped = true;
        report.bots.skipped_reason = Some("excluded by --only".to_owned());
    }

    report.warnings = warnings;
    Ok(report)
}

/// 사람이 읽기 쉬운 요약 문자열로 변환한다.
#[must_use]
pub fn format_report_human(report: &OrcaImportReport) -> String {
    use std::fmt::Write as _;
    let mode = if report.dry_run {
        "DRY-RUN (no files modified)"
    } else {
        "APPLIED"
    };
    let mut out = String::new();
    writeln!(
        out,
        "Orca import [{mode}] — profile '{}' from {}",
        report.profile_id, report.orca_dir
    )
    .ok();
    if report.domains.iter().any(|d| d == "accounts") {
        let a = &report.accounts;
        if a.skipped {
            writeln!(
                out,
                "  accounts: skipped ({})",
                a.skipped_reason.as_deref().unwrap_or("daemon required")
            )
            .ok();
        } else {
            writeln!(
                out,
                "  accounts: claude +{} (~{}), codex +{} (~{}), active claude={}, active codex={}",
                a.claude_added,
                a.claude_updated,
                a.codex_added,
                a.codex_updated,
                a.active_claude.as_deref().unwrap_or("host"),
                a.active_codex.as_deref().unwrap_or("host"),
            )
            .ok();
            for item in &a.items {
                let active_mark = if item.active { " *" } else { "" };
                writeln!(
                    out,
                    "    - [{}] {}{active_mark}: {} ({}) [{}]",
                    item.provider, item.id, item.label, item.profile_dir, item.connection
                )
                .ok();
            }
            if a.agy_detected
                && let Some(note) = a.agy_note.as_deref()
            {
                writeln!(out, "  agy: {note}").ok();
            }
        }
    }
    if report.domains.iter().any(|d| d == "ssh") {
        let s = &report.ssh;
        writeln!(
            out,
            "  ssh: +{} added, ~{} updated ({} from environments)",
            s.hosts_added, s.hosts_updated, s.environments_merged
        )
        .ok();
    }
    if report.domains.iter().any(|d| d == "projects") {
        let p = &report.projects;
        writeln!(
            out,
            "  projects: +{} added, ~{} updated, {} worktrees, {} lineage entries",
            p.projects_added, p.projects_updated, p.worktrees_migrated, p.lineage_migrated
        )
        .ok();
    }
    if report.domains.iter().any(|d| d == "settings") {
        let st = &report.settings;
        writeln!(
            out,
            "  settings: updated={}, default_agent={}, quick_commands={}, agents={}, schedules={}",
            st.settings_updated,
            st.default_agent.as_deref().unwrap_or("unchanged"),
            st.quick_commands_merged,
            st.agents_updated,
            st.schedules_merged
        )
        .ok();
    }
    if report.domains.iter().any(|d| d == "sessions") {
        let se = &report.sessions;
        if se.skipped {
            writeln!(
                out,
                "  sessions: skipped ({})",
                se.skipped_reason.as_deref().unwrap_or("daemon required")
            )
            .ok();
        } else {
            writeln!(
                out,
                "  sessions: {} found, {} resumed, {} already resumed",
                se.items.len(),
                se.resumed,
                se.already_resumed
            )
            .ok();
            for item in &se.items {
                writeln!(
                    out,
                    "    - [{}] {} {}{}",
                    item.agent,
                    item.cwd,
                    item.title.as_deref().unwrap_or(&item.id),
                    if item.resumed { " *" } else { "" }
                )
                .ok();
            }
        }
    }
    if report.domains.iter().any(|d| d == "bots") {
        let b = &report.bots;
        if b.skipped {
            writeln!(
                out,
                "  bots: skipped ({})",
                b.skipped_reason.as_deref().unwrap_or("skipped")
            )
            .ok();
        } else {
            writeln!(
                out,
                "  bots: +{} added, {} existing skipped, {} items, {} groups, {} suggestions, {} schedules, {} external candidate(s)",
                b.bots_added,
                b.bots_skipped,
                b.items_migrated,
                b.groups_migrated,
                b.suggestions_migrated,
                b.schedules_migrated,
                b.external_candidates.len()
            )
            .ok();
            for item in &b.items {
                let state = if item.already_exists {
                    " (existing)"
                } else {
                    ""
                };
                writeln!(
                    out,
                    "    - [{}] {} ({}){state}",
                    item.agent_id, item.name, item.slug
                )
                .ok();
            }
            for cand in &b.external_candidates {
                let app = match cand.app {
                    ExternalBotSourceApp::Orca => "orca",
                    ExternalBotSourceApp::OpenClaw => "openclaw",
                    ExternalBotSourceApp::Hermes => "hermes",
                    ExternalBotSourceApp::OpenBot => "openbot",
                };
                writeln!(
                    out,
                    "    - external [{app}] {} ({}) — {}",
                    cand.name, cand.agent, cand.path
                )
                .ok();
            }
        }
    }
    for w in &report.warnings {
        writeln!(out, "  warning: {w}").ok();
    }
    out.trim_end().to_owned()
}

// ============================================================================
// Orca Profile Index & SQLite / JSON Loader
// ============================================================================

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileIndex {
    #[serde(default)]
    active_profile_id: Option<String>,
    #[serde(default)]
    profiles: Vec<ProfileEntry>,
}

#[derive(Deserialize)]
struct ProfileEntry {
    id: String,
}

#[must_use]
pub fn resolve_active_profile_id(orca_dir: &Path) -> String {
    for name in ["orca-profile-index.json", "orca-profile-index.json.bak"] {
        let path = orca_dir.join(name);
        if let Ok(raw) = fs::read_to_string(&path)
            && let Ok(idx) = serde_json::from_str::<ProfileIndex>(&raw)
        {
            if let Some(active) = idx.active_profile_id.as_deref() {
                let trimmed = active.trim();
                if !trimmed.is_empty()
                    && (idx.profiles.is_empty() || idx.profiles.iter().any(|p| p.id == trimmed))
                {
                    return trimmed.to_owned();
                }
            }
            if let Some(first) = idx.profiles.first()
                && !first.id.trim().is_empty()
            {
                return first.id.trim().to_owned();
            }
        }
    }
    let default_dir = orca_dir.join("profiles").join("default");
    if default_dir.is_dir() {
        return "default".to_owned();
    }
    if let Ok(entries) = fs::read_dir(orca_dir.join("profiles")) {
        let mut dirs: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        dirs.sort();
        if let Some(first) = dirs.into_iter().next() {
            return first;
        }
    }
    "default".to_owned()
}

#[derive(Deserialize)]
struct SqliteRow {
    domain: String,
    payload: String,
}

fn load_orca_domains(
    orca_dir: &Path,
    profile_id: &str,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, Json> {
    let mut out = BTreeMap::new();
    let profile_dir = orca_dir.join("profiles").join(profile_id);

    // 1. Fallback JSON (`orca-data.json` / `orca-data.json.bak` in profile_dir or orca_dir root)
    for candidate in [
        profile_dir.join("orca-data.json.bak"),
        profile_dir.join("orca-data.json"),
        orca_dir.join("orca-data.json.bak"),
        orca_dir.join("orca-data.json"),
    ] {
        if let Ok(raw) = fs::read_to_string(&candidate)
            && let Ok(Json::Object(map)) = serde_json::from_str::<Json>(&raw)
        {
            for (k, v) in map {
                out.insert(k, v);
            }
        }
    }

    // 2. Primary SQLite (`profile-state.db`) overrides fallback JSON per domain
    let db_path = profile_dir.join("profile-state.db");
    if db_path.is_file() {
        match query_sqlite_domains(&db_path) {
            Ok(rows) => {
                for (domain, val) in rows {
                    out.insert(domain, val);
                }
            }
            Err(e) => {
                warnings.push(format!("failed to read {}: {e}", db_path.display()));
            }
        }
    }

    out
}

fn sqlite3_binary() -> &'static str {
    if Path::new("/usr/bin/sqlite3").is_file() {
        "/usr/bin/sqlite3"
    } else {
        "sqlite3"
    }
}

fn is_windows_drive_path(s: &str) -> bool {
    let mut bytes = s.bytes();
    matches!(
        (bytes.next(), bytes.next(), bytes.next()),
        (Some(drive), Some(b':'), Some(b'/')) if drive.is_ascii_alphabetic()
    )
}

fn sqlite_immutable_uri_from_raw(raw: &str, windows_separators: bool) -> String {
    let normalized: std::borrow::Cow<'_, str> = if windows_separators {
        let stripped = if raw
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(r"\\?\UNC\"))
        {
            format!("//{}", raw.get(8..).unwrap_or_default())
        } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
            rest.to_owned()
        } else {
            raw.to_owned()
        };
        std::borrow::Cow::Owned(stripped.replace('\\', "/"))
    } else {
        std::borrow::Cow::Borrowed(raw)
    };
    let mut encoded = String::with_capacity(normalized.len().saturating_add(32));
    if windows_separators && is_windows_drive_path(&normalized) {
        encoded.push_str("file:///");
    } else if normalized.starts_with("//") {
        encoded.push_str("file://");
    } else {
        encoded.push_str("file:");
    }
    for b in normalized.bytes() {
        match b {
            b'%' => encoded.push_str("%25"),
            b'?' => encoded.push_str("%3F"),
            b'#' => encoded.push_str("%23"),
            b' ' => encoded.push_str("%20"),
            0x00..=0x1F | 0x7F..=0xFF => {
                write!(encoded, "%{b:02X}").ok();
            }
            _ => encoded.push(char::from(b)),
        }
    }
    encoded.push_str("?immutable=1");
    encoded
}

fn sqlite_immutable_uri(db_path: &Path) -> String {
    let raw = db_path.to_string_lossy();
    sqlite_immutable_uri_from_raw(&raw, cfg!(windows))
}

fn sqlite_wal_companion_exists(db_path: &Path) -> bool {
    let mut os = db_path.as_os_str().to_os_string();
    os.push("-wal");
    Path::new(&os).exists()
}

fn run_sqlite3_json_rows(db_path: &Path, sql: &str) -> io::Result<Vec<Json>> {
    let bin = sqlite3_binary();
    let run_once = |target: &str| {
        Command::new(bin)
            .arg("-readonly")
            .arg("-json")
            .arg(target)
            .arg(sql)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
    };
    let db_str = db_path.to_string_lossy();
    let output = run_once(db_str.as_ref())?;
    let output = if !output.status.success() && !sqlite_wal_companion_exists(db_path) {
        let uri = sqlite_immutable_uri(db_path);
        let retry = run_once(&uri)?;
        if retry.status.success() {
            retry
        } else {
            output
        }
    } else {
        output
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "sqlite3 exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    let stdout = String::from_utf8(output.stdout).map_err(invalid_data)?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<Vec<Json>>(trimmed).map_err(invalid_data)
}

fn query_sqlite_domains(db_path: &Path) -> io::Result<BTreeMap<String, Json>> {
    let rows = run_sqlite3_json_rows(
        db_path,
        "SELECT domain, payload FROM profile_state_documents;",
    )?;
    let mut map = BTreeMap::new();
    for row in rows {
        let Ok(parsed) = serde_json::from_value::<SqliteRow>(row) else {
            continue;
        };
        if let Ok(val) = serde_json::from_str::<Json>(&parsed.payload) {
            map.insert(parsed.domain, val);
        }
    }
    Ok(map)
}

// ============================================================================
// Domain 1: Multi-Accounts & Profile Paths (`accounts` via `krilld` UsageRequest)
// ============================================================================

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaClaudeAccount {
    id: String,
    email: Option<String>,
    organization_name: Option<String>,
    organization_uuid: Option<String>,
    managed_auth_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaCodexAccount {
    id: String,
    email: Option<String>,
    workspace_label: Option<String>,
    workspace_account_id: Option<String>,
    provider_account_id: Option<String>,
    managed_home_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ClaudeDotJson {
    oauth_account: Option<ClaudeOauthAccount>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ClaudeOauthAccount {
    email_address: Option<String>,
    organization_name: Option<String>,
    organization_uuid: Option<String>,
}

#[derive(Debug, Clone)]
struct PlannedOrcaAccount {
    provider: ProviderKind,
    planned_id: String,
    /// Orca 측 계정 uuid(`claudeManagedAccounts[].id`·디렉터리명). OWE-896 중복 조인 키.
    raw_id: String,
    label: String,
    email: Option<String>,
    org_or_workspace: Option<String>,
    /// Claude 조직 uuid(`organizationUuid`). 이메일이 같아도 조직이 다르면 다른 계정.
    org_uuid: Option<String>,
    /// Codex `providerAccountId`(`auth.json`의 `tokens.account_id`와 대조).
    codex_account_id: Option<String>,
    profile_dir: PathBuf,
    dir_str: String,
    active: bool,
}

fn import_accounts_domain(
    opts: &ImportOptions,
    domains: &BTreeMap<String, Json>,
    warnings: &mut Vec<String>,
) -> AccountsImportSummary {
    let mut summary = import_accounts_domain_inner(opts, domains, warnings);
    // OWE-896 (c, 코디네이터 결정 A): agy 계정은 Orca 암호화 vault에만 있어 개별 이관 불가.
    // vault 존재만 감지해 보고하고, 내용은 읽지 않는다(복호화·키체인 복사 금지).
    // Krill은 Orca가 materialize한 호스트 agy 로그인을 공유해 사용한다.
    if fs::symlink_metadata(opts.orca_dir.join("antigravity-accounts").join("vault"))
        .is_ok_and(|m| m.is_file())
    {
        summary.agy_detected = true;
        summary.agy_note = Some(
            "Orca encrypted vault found: agy accounts can't be imported one by one. \
             Krill shares the host's agy login"
                .to_owned(),
        );
    }
    summary
}

fn import_accounts_domain_inner(
    opts: &ImportOptions,
    domains: &BTreeMap<String, Json>,
    warnings: &mut Vec<String>,
) -> AccountsImportSummary {
    let settings = domains.get("settings");
    let mut claude_candidates: BTreeMap<String, OrcaClaudeAccount> = BTreeMap::new();
    let mut codex_candidates: BTreeMap<String, OrcaCodexAccount> = BTreeMap::new();

    if let Some(arr) = settings.and_then(|s| s.get("claudeManagedAccounts"))
        && let Ok(list) = serde_json::from_value::<Vec<OrcaClaudeAccount>>(arr.clone())
    {
        for item in list {
            let clean_id = item.id.trim().to_owned();
            if !clean_id.is_empty() {
                claude_candidates.insert(clean_id, item);
            }
        }
    }

    if let Some(arr) = settings.and_then(|s| s.get("codexManagedAccounts"))
        && let Ok(list) = serde_json::from_value::<Vec<OrcaCodexAccount>>(arr.clone())
    {
        for item in list {
            let clean_id = item.id.trim().to_owned();
            if !clean_id.is_empty() {
                codex_candidates.insert(clean_id, item);
            }
        }
    }

    // Scan `<orca_dir>/claude-profiles/*/home` and `<orca_dir>/claude-accounts/*/auth` on disk
    for (subdir, leaf) in [("claude-profiles", "home"), ("claude-accounts", "auth")] {
        if let Ok(entries) = fs::read_dir(opts.orca_dir.join(subdir)) {
            for entry in entries.filter_map(Result::ok) {
                let Ok(raw_id) = entry.file_name().into_string() else {
                    continue;
                };
                let candidate_dir = entry.path().join(leaf);
                if candidate_dir.is_dir() {
                    let rec = claude_candidates.entry(raw_id.clone()).or_insert_with(|| {
                        OrcaClaudeAccount {
                            id: raw_id,
                            ..OrcaClaudeAccount::default()
                        }
                    });
                    if rec.managed_auth_path.is_none() {
                        rec.managed_auth_path = Some(candidate_dir.display().to_string());
                    }
                }
            }
        }
    }

    // Scan `<orca_dir>/codex-accounts/*/home` on disk
    if let Ok(entries) = fs::read_dir(opts.orca_dir.join("codex-accounts")) {
        for entry in entries.filter_map(Result::ok) {
            let Ok(raw_id) = entry.file_name().into_string() else {
                continue;
            };
            let candidate_dir = entry.path().join("home");
            if candidate_dir.is_dir() {
                let rec =
                    codex_candidates
                        .entry(raw_id.clone())
                        .or_insert_with(|| OrcaCodexAccount {
                            id: raw_id,
                            ..OrcaCodexAccount::default()
                        });
                if rec.managed_home_path.is_none() {
                    rec.managed_home_path = Some(candidate_dir.display().to_string());
                }
            }
        }
    }

    let active_claude_raw = settings
        .and_then(|s| s.get("activeClaudeManagedAccountId"))
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let active_codex_raw = settings
        .and_then(|s| s.get("activeCodexManagedAccountId"))
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    let mut planned = Vec::new();

    for (raw_id, mut acc) in claude_candidates {
        let profile_dir = resolve_claude_profile_dir(&opts.home, &opts.orca_dir, &raw_id, &acc);
        let auth_fallback_dir = opts
            .orca_dir
            .join("claude-accounts")
            .join(&raw_id)
            .join("auth");

        // Enrich email & organizationName from `.claude.json` inside profile_dir or auth_fallback_dir
        for dir in [&profile_dir, &auth_fallback_dir] {
            if let Ok(raw) = fs::read_to_string(dir.join(".claude.json"))
                && let Ok(parsed) = serde_json::from_str::<ClaudeDotJson>(&raw)
                && let Some(oauth) = parsed.oauth_account
            {
                if acc.email.as_deref().is_none_or(|s| s.trim().is_empty()) {
                    acc.email = oauth.email_address;
                }
                if acc
                    .organization_name
                    .as_deref()
                    .is_none_or(|s| s.trim().is_empty())
                {
                    acc.organization_name = oauth.organization_name;
                }
                if acc
                    .organization_uuid
                    .as_deref()
                    .is_none_or(|s| s.trim().is_empty())
                {
                    acc.organization_uuid = oauth.organization_uuid;
                }
            }
        }

        let email = acc
            .email
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let org = acc
            .organization_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let label = format_account_label(
            email.as_deref(),
            org.as_deref(),
            &format!("Claude ({raw_id})"),
        );
        let planned_id = format!("claude-{}", slugify(&label));
        let dir_str = profile_dir.display().to_string();
        let active = active_claude_raw.as_deref() == Some(raw_id.as_str());

        planned.push(PlannedOrcaAccount {
            provider: ProviderKind::Claude,
            planned_id,
            raw_id,
            label,
            email,
            org_or_workspace: org,
            org_uuid: acc.organization_uuid,
            codex_account_id: None,
            profile_dir,
            dir_str,
            active,
        });
    }

    for (raw_id, acc) in codex_candidates {
        let profile_dir = resolve_codex_profile_dir(&opts.home, &opts.orca_dir, &raw_id, &acc);
        let email = acc
            .email
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let ws = acc
            .workspace_label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let label = format_account_label(
            email.as_deref(),
            ws.as_deref(),
            &format!("Codex ({raw_id})"),
        );
        let planned_id = format!("codex-{}", slugify(&label));
        let dir_str = profile_dir.display().to_string();
        let active = active_codex_raw.as_deref() == Some(raw_id.as_str());

        planned.push(PlannedOrcaAccount {
            provider: ProviderKind::Codex,
            planned_id,
            raw_id,
            label,
            email,
            org_or_workspace: ws,
            org_uuid: None,
            codex_account_id: acc
                .provider_account_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or(acc.workspace_account_id),
            profile_dir,
            dir_str,
            active,
        });
    }

    planned.extend(account_sources::opencode(opts));

    let mut summary = AccountsImportSummary::default();
    // OWE-901: agy 계정은 Orca 암호화 저장소라 이관하지 않는다. 새로 로그인을 안내한다.
    if let Some(note) = detect_agy_import_note(opts, domains) {
        warnings.push(note);
    }
    let hidden = load_hidden_accounts(&opts.home, &opts.state_dir);
    if !hidden.is_empty() {
        planned.retain(|cand| !planned_account_is_hidden(&hidden, cand));
    }
    if planned.is_empty() {
        return summary;
    }

    let socket = daemon::socket_path(&opts.state_dir).map_err(|error| error.to_string());
    let auto_start = should_auto_start_daemon(opts.dry_run);
    match socket.and_then(|socket| {
        daemon_usage_request(&socket, &UsageRequest::ListAccounts, auto_start)
            .map(|snapshot| (socket, snapshot))
    }) {
        Err(e) if !opts.dry_run => {
            // AS-03: 데몬 자동 기동까지 실패해 연결할 수 없으면 로컬 파일(accounts.toml)을 직접 쓰지 않고
            // '데몬 필요' 사유로 accounts 도메인을 스킵한 뒤 다른 도메인만 진행한다.
            summary.skipped = true;
            summary.skipped_reason = Some(format!(
                "Daemon required: couldn't reach krilld, skipped importing accounts ({e})"
            ));
            for cand in planned {
                summary.items.push(ImportedAccountSummary {
                    id: cand.planned_id,
                    provider: cand.provider.as_str().to_owned(),
                    label: cand.label,
                    email: cand.email,
                    organization_or_workspace: cand.org_or_workspace,
                    profile_dir: cand.dir_str,
                    active: cand.active,
                    runnable: false,
                    connection: "unknown".to_owned(),
                });
            }
            warnings.push(format!("accounts skipped (daemon required): {e}"));
            return summary;
        }
        Err(_) => {
            // `--dry-run` 시에는 쓰기를 수행하지 않으므로 데몬이 꺼져 있어도 계획된 항목을 보고한다.
            for cand in planned {
                match cand.provider {
                    ProviderKind::Claude => {
                        summary.claude_added = summary.claude_added.saturating_add(1);
                        if cand.active {
                            summary.active_claude = Some(cand.planned_id.clone());
                        }
                    }
                    ProviderKind::Codex => {
                        summary.codex_added = summary.codex_added.saturating_add(1);
                        if cand.active {
                            summary.active_codex = Some(cand.planned_id.clone());
                        }
                    }
                    ProviderKind::Antigravity | ProviderKind::OpenCode | ProviderKind::Gemini => {}
                }
                summary.items.push(ImportedAccountSummary {
                    id: cand.planned_id,
                    provider: cand.provider.as_str().to_owned(),
                    label: cand.label,
                    email: cand.email,
                    organization_or_workspace: cand.org_or_workspace,
                    profile_dir: cand.dir_str,
                    active: cand.active,
                    runnable: true,
                    connection: "unknown".to_owned(),
                });
            }
            return summary;
        }
        Ok((socket, mut snap)) => {
            for cand in planned {
                if let Some(existing) = find_registered_account(&snap, &cand) {
                    let kept_id = existing.id.clone();
                    let was_active = existing.active;
                    // OWE-896: Orca 원본(`claude-accounts/*`, `claude-profiles/*`)은 읽기만 한다.
                    // 이메일 보강은 위에서 두 디렉터리를 모두 읽어 수행하므로 복사가 필요 없다.
                    if cand.active {
                        record_active_summary(&mut summary, cand.provider, &kept_id);
                        if !opts.dry_run && !was_active {
                            let req = UsageRequest::SwitchAccount {
                                provider: cand.provider,
                                account_id: kept_id.clone(),
                            };
                            match daemon_usage_request(&socket, &req, false) {
                                Ok(updated) => snap = updated,
                                Err(e) => {
                                    warnings.push(format!(
                                        "failed to switch active {} account to {}: {e}",
                                        cand.provider.as_str(),
                                        kept_id
                                    ));
                                }
                            }
                        }
                    }
                    summary.items.push(ImportedAccountSummary {
                        id: kept_id,
                        provider: cand.provider.as_str().to_owned(),
                        label: cand.label,
                        email: cand.email,
                        organization_or_workspace: cand.org_or_workspace,
                        profile_dir: cand.dir_str,
                        active: cand.active,
                        runnable: opts.dry_run || cand.profile_dir.is_dir(),
                        connection: "unknown".to_owned(),
                    });
                    continue;
                }

                if opts.dry_run {
                    match cand.provider {
                        ProviderKind::Claude => {
                            summary.claude_added = summary.claude_added.saturating_add(1);
                        }
                        ProviderKind::Codex => {
                            summary.codex_added = summary.codex_added.saturating_add(1);
                        }
                        ProviderKind::Antigravity
                        | ProviderKind::OpenCode
                        | ProviderKind::Gemini => {}
                    }
                    if cand.active {
                        record_active_summary(&mut summary, cand.provider, &cand.planned_id);
                    }
                    summary.items.push(ImportedAccountSummary {
                        id: cand.planned_id,
                        provider: cand.provider.as_str().to_owned(),
                        label: cand.label,
                        email: cand.email,
                        organization_or_workspace: cand.org_or_workspace,
                        profile_dir: cand.dir_str,
                        active: cand.active,
                        runnable: true,
                        connection: "unknown".to_owned(),
                    });
                    continue;
                }

                let add_req = UsageRequest::AddAccount {
                    provider: cand.provider,
                    label: cand.label.clone(),
                    email: cand.email.clone(),
                    profile_dir: cand.dir_str.clone(),
                };
                match daemon_usage_request(&socket, &add_req, false) {
                    Ok(updated) => snap = updated,
                    Err(e) => {
                        summary.skipped = true;
                        summary.skipped_reason = Some(format!(
                            "Daemon required: adding the account to krilld failed ({e})"
                        ));
                        warnings.push(format!("accounts skipped (daemon required): {e}"));
                        return summary;
                    }
                }

                let assigned_id = find_registered_account(&snap, &cand)
                    .map_or_else(|| cand.planned_id.clone(), |a| a.id.clone());

                match cand.provider {
                    ProviderKind::Claude => {
                        summary.claude_added = summary.claude_added.saturating_add(1);
                    }
                    ProviderKind::Codex => {
                        summary.codex_added = summary.codex_added.saturating_add(1);
                    }
                    ProviderKind::Antigravity | ProviderKind::OpenCode | ProviderKind::Gemini => {}
                }

                if cand.active {
                    record_active_summary(&mut summary, cand.provider, &assigned_id);
                    let switch_req = UsageRequest::SwitchAccount {
                        provider: cand.provider,
                        account_id: assigned_id.clone(),
                    };
                    match daemon_usage_request(&socket, &switch_req, false) {
                        Ok(updated) => snap = updated,
                        Err(e) => {
                            warnings.push(format!(
                                "failed to switch active {} account to {}: {e}",
                                cand.provider.as_str(),
                                assigned_id
                            ));
                        }
                    }
                }

                summary.items.push(ImportedAccountSummary {
                    id: assigned_id,
                    provider: cand.provider.as_str().to_owned(),
                    label: cand.label,
                    email: cand.email,
                    organization_or_workspace: cand.org_or_workspace,
                    profile_dir: cand.dir_str,
                    active: cand.active,
                    runnable: cand.profile_dir.is_dir(),
                    connection: "unknown".to_owned(),
                });
            }

            if summary.active_claude.is_none() {
                summary.active_claude = snap
                    .accounts
                    .iter()
                    .find(|a| a.provider == ProviderKind::Claude && a.active && !a.host_default)
                    .map(|a| a.id.clone());
            }
            if summary.active_codex.is_none() {
                summary.active_codex = snap
                    .accounts
                    .iter()
                    .find(|a| a.provider == ProviderKind::Codex && a.active && !a.host_default)
                    .map(|a| a.id.clone());
            }
            // OWE-896 (e): 방금 등록한 계정의 실제 probe 결과가 확정될 때까지
            // 최대 5초 대기한 뒤 항목별 연결 상태를 채운다(UI 15초 제한 안).
            if !opts.dry_run {
                settle_account_connections(&socket, &mut summary, &mut snap);
            }
        }
    }

    summary
}

/// 등록된 항목의 실제 provider 연결 상태를 스냅샷에서 채운다.
/// 백그라운드 probe가 `Checking`이면 최대 5초까지 `ListAccounts`로 다시 묻는다.
/// 비밀값은 스냅샷에 없으며, 여기서도 상태 문자열만 옮긴다.
fn settle_account_connections(
    socket: &Path,
    summary: &mut AccountsImportSummary,
    snap: &mut AccountsSnapshot,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let pending = summary
            .items
            .iter()
            .any(|item| snap.connection_for(&item.id) == ConnectionStatus::Checking);
        if !pending || std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
        match daemon_usage_request(socket, &UsageRequest::ListAccounts, false) {
            Ok(next) => *snap = next,
            Err(_) => break,
        }
    }
    for item in &mut summary.items {
        connection_name(snap.connection_for(&item.id)).clone_into(&mut item.connection);
    }
}

/// `ConnectionStatus`의 와이어 문자열(`checking`·`connected`·`needs_login`·`not_installed`·`check_failed`).
fn connection_name(status: ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Checking => "checking",
        ConnectionStatus::Connected => "connected",
        ConnectionStatus::NeedsLogin => "needs_login",
        ConnectionStatus::NotInstalled => "not_installed",
        ConnectionStatus::CheckFailed => "check_failed",
    }
}

fn record_active_summary(
    summary: &mut AccountsImportSummary,
    provider: ProviderKind,
    account_id: &str,
) {
    match provider {
        ProviderKind::Claude => summary.active_claude = Some(account_id.to_owned()),
        ProviderKind::Codex => summary.active_codex = Some(account_id.to_owned()),
        ProviderKind::Antigravity | ProviderKind::OpenCode | ProviderKind::Gemini => {}
    }
}

/// OWE-901: Orca 쪽 agy 계정 흔적이 있으면 이관 불가 안내를 돌려준다.
/// Orca는 agy 자격증명을 암호화 저장소에 두어 Krill이 읽을 수 없으므로,
/// 개수만 세고 `warnings`에 "Krill에서 새로 로그인" 안내를 남긴다(스키마 변경 없음).
fn detect_agy_import_note(
    opts: &ImportOptions,
    domains: &BTreeMap<String, Json>,
) -> Option<String> {
    let mut count = 0usize;
    if let Some(settings) = domains.get("settings").and_then(Json::as_object) {
        for (key, val) in settings {
            let lower = key.to_ascii_lowercase();
            if lower == "antigravitymanagedaccounts" || lower == "agymanagedaccounts" {
                count += val.as_array().map_or(1, Vec::len);
            } else if lower == "activeantigravitymanagedaccountid"
                || lower == "activeagymanagedaccountid"
            {
                count += val
                    .as_str()
                    .map_or(0, |s| usize::from(!s.trim().is_empty()));
            }
        }
    }
    // 디스크 흔적(`antigravity-accounts` 등, claude/codex 스캔과 같은 방식).
    for subdir in [
        "antigravity-accounts",
        "antigravity-profiles",
        "agy-accounts",
        "agy-profiles",
    ] {
        if let Ok(entries) = std::fs::read_dir(opts.orca_dir.join(subdir)) {
            count += entries.filter_map(Result::ok).count();
        }
    }
    (count > 0).then(|| {
        format!(
            "agy accounts found: {count}. They can't be imported (Orca encrypted store); sign in again in Krill"
        )
    })
}

/// `snap.accounts`에서 이미 `accounts.toml`에 등록된(`!host_default && !profile_dir.is_empty()`)
/// 대응 계정을 찾는다. OWE-896: Orca uuid 조인(라벨·이메일 변경에도 같은 계정)과
/// 이메일·조직 기준 병합(Orca `isSameClaudeLogin` 의미: 같은 이메일 + 조직 미상·동일)을 함께 본다.
fn find_registered_account<'a>(
    snap: &'a AccountsSnapshot,
    cand: &PlannedOrcaAccount,
) -> Option<&'a AccountInfo> {
    snap.accounts.iter().find(|a| {
        if a.host_default || a.profile_dir.is_empty() || a.provider != cand.provider {
            return false;
        }
        if a.profile_dir == cand.dir_str || a.id == cand.planned_id {
            return true;
        }
        // Orca uuid 조인: 이관 전후 라벨·이메일이 바뀌어도 같은 계정이다.
        if !cand.raw_id.is_empty()
            && orca_uuid_from_profile_dir(&a.profile_dir).is_some_and(|u| u == cand.raw_id)
        {
            return true;
        }
        // Codex 계정 id 조인(`providerAccountId` == 프로필 `auth.json`의 계정 id).
        if cand.provider == ProviderKind::Codex
            && let Some(ref wanted) = cand.codex_account_id
            && read_codex_account_id(Path::new(&a.profile_dir)).is_some_and(|got| &got == wanted)
        {
            return true;
        }
        if a.label.eq_ignore_ascii_case(&cand.label) {
            return true;
        }
        // 이메일·조직 기준: `"email (조직)"` 라벨에서도 이메일을 뽑아 비교한다.
        let peer_mail = a.email.as_deref().unwrap_or(a.label.as_str());
        let cand_mail = cand.email.as_deref().unwrap_or(cand.label.as_str());
        if primary_email(peer_mail).contains('@')
            && emails_equal(peer_mail, cand_mail)
            && org_compatible(&a.profile_dir, cand.org_uuid.as_deref())
        {
            return true;
        }
        false
    })
}

/// Orca 프로필 경로에서 계정 uuid를 뽑는다.
/// `claude-profiles/<uuid>/home`·`claude-accounts/<uuid>/auth`·`codex-accounts/<uuid>/home`·`managed-data-accounts/opencode/<uuid>`.
/// uuid 형태가 아니면 `None`(사용자 임의 경로와 오인 병합 방지).
fn orca_uuid_from_profile_dir(dir: &str) -> Option<&str> {
    let mut parts = dir.split(['/', '\\']);
    let last = parts.next_back()?;
    let mid = parts.next_back()?;
    let root = parts.next_back()?;
    if root == "managed-data-accounts" && mid == "opencode" {
        return is_uuid_shape(last).then_some(last);
    }
    if !matches!(
        root,
        "claude-profiles" | "claude-accounts" | "codex-accounts"
    ) || !matches!(last, "home" | "auth")
    {
        return None;
    }
    is_uuid_shape(mid).then_some(mid)
}

/// uuid v4 텍스트 형태(8-4-4-4-12 hex)만 인정한다.
fn is_uuid_shape(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, c) in bytes.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if *c != b'-' {
                return false;
            }
        } else if !c.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// `"email (조직)"` 라벨에서 비교용 이메일만 뽑는다. `@`가 없으면 라벨 전체.
fn primary_email(label: &str) -> &str {
    match label.split_once(" (") {
        Some((head, _)) if head.contains('@') => head.trim(),
        _ => label.trim(),
    }
}

fn emails_equal(left: &str, right: &str) -> bool {
    primary_email(left).eq_ignore_ascii_case(primary_email(right))
}

fn load_hidden_accounts(home: &Path, state_dir: &Path) -> Vec<String> {
    for path in [
        state_dir.join("accounts.toml"),
        home.join(".config").join("krill").join("accounts.toml"),
    ] {
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(doc) = toml::from_str::<toml::Value>(&raw) else {
            continue;
        };
        if let Some(arr) = doc.get("hidden_accounts").and_then(toml::Value::as_array) {
            return arr
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect();
        }
        return Vec::new();
    }
    Vec::new()
}

fn planned_account_is_hidden(hidden: &[String], cand: &PlannedOrcaAccount) -> bool {
    let orca_id = format!("claude-orca-{}", slugify(&cand.raw_id));
    if hidden.iter().any(|h| {
        h.eq_ignore_ascii_case(&cand.planned_id)
            || h.eq_ignore_ascii_case(&cand.raw_id)
            || (cand.provider == ProviderKind::Claude && h.eq_ignore_ascii_case(&orca_id))
    }) {
        return true;
    }
    if cand.provider != ProviderKind::Claude {
        return false;
    }
    let cand_mail = cand.email.as_deref().unwrap_or(cand.label.as_str());
    let mail = primary_email(cand_mail);
    if mail.contains('@') {
        let marker = format!("email:{}", mail.to_ascii_lowercase());
        if hidden.iter().any(|h| h.eq_ignore_ascii_case(&marker)) {
            return true;
        }
    }
    let clean_label = cand.label.trim();
    if !clean_label.is_empty() {
        let marker = format!("label:{}", clean_label.to_ascii_lowercase());
        if hidden.iter().any(|h| h.eq_ignore_ascii_case(&marker)) {
            return true;
        }
    }
    false
}

/// 기존 프로필의 조직과 후보 조직이 충돌하지 않는지 확인한다.
/// 둘 중 하나라도 미상이면 충돌 아님(Orca `isSameClaudeLogin`과 동일).
fn org_compatible(profile_dir: &str, cand_org: Option<&str>) -> bool {
    let Some(want) = cand_org.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    read_claude_identity(Path::new(profile_dir))
        .and_then(|(_, org)| org)
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_none_or(|got| got == want)
}

/// 등록된 프로필의 `.claude.json`에서 `(email, organizationUuid)`를 읽는다.
/// 비교용 메타데이터만 읽고, 값은 로그·출력하지 않는다.
fn read_claude_identity(profile_dir: &Path) -> Option<(Option<String>, Option<String>)> {
    let raw = fs::read_to_string(profile_dir.join(".claude.json")).ok()?;
    if raw.len() > 1024 * 1024 {
        return None;
    }
    let parsed: ClaudeDotJson = serde_json::from_str(&raw).ok()?;
    let oauth = parsed.oauth_account?;
    Some((oauth.email_address, oauth.organization_uuid))
}

/// Codex 프로필 `auth.json`에서 `tokens.account_id`만 읽는다(토큰 값은 절대 담지 않는다).
fn read_codex_account_id(profile_dir: &Path) -> Option<String> {
    let raw = fs::read(profile_dir.join("auth.json")).ok()?;
    if raw.len() > 64 * 1024 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    value
        .pointer("/tokens/account_id")?
        .as_str()
        .map(str::to_owned)
}

/// 단독 CLI 실행(`!dry_run`, 오프라인·단위테스트 환경 아님)에서는 `daemon::connect`로 데몬 자동 기동을 허용한다.
fn should_auto_start_daemon(dry_run: bool) -> bool {
    if dry_run {
        return false;
    }
    if std::env::var_os("KRILL_ORCA_AUTO_START").is_some_and(|v| v == "1") {
        return true;
    }
    if cfg!(test)
        || std::env::var_os("KRILL_TEST_OFFLINE").is_some()
        || std::env::var_os("NEXTEST").is_some()
        || std::env::var_os("NEXTEST_EXECUTION_MODE").is_some()
        || std::env::var_os("RUST_TEST_THREADS").is_some()
    {
        return false;
    }
    if let Ok(exe) = std::env::current_exe()
        && exe
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "deps")
    {
        return false;
    }
    true
}

/// `krilld` 소켓에 연결한다(`auto_start`면 `daemon::connect`로 데몬 자동 기동 포함).
fn connect_daemon(
    socket: &Path,
    hello: &ipc::ClientHello,
    auto_start: bool,
) -> Result<daemon::Connection, String> {
    match daemon::handshake(socket, hello, Duration::from_secs(10)) {
        Ok(conn) => Ok(conn),
        Err(_first_err) if auto_start => {
            let paths = daemon::Paths::dev()
                .map(|mut p| {
                    p.socket = socket.to_path_buf();
                    p
                })
                .map_err(|e| {
                    format!("cannot resolve daemon paths for {}: {e}", socket.display())
                })?;
            daemon::connect(&paths, hello)
                .map_err(|e| format!("cannot connect to {}: {e}", socket.display()))
        }
        Err(e) => Err(format!("cannot connect to {}: {e}", socket.display())),
    }
}

/// `krilld` 소켓에 연결(단독 CLI 실행 시 `daemon::connect` 자동 기동 포함)해 `UsageRequest`를 보낸다.
fn daemon_usage_request(
    socket: &Path,
    req: &UsageRequest,
    auto_start: bool,
) -> Result<AccountsSnapshot, String> {
    let mut hello = daemon::client_hello(ClientRole::Cli, "krill import orca");
    hello.features = FEAT_USAGE
        | FEAT_USAGE_AGY
        | FEAT_USAGE_RULES
        | krill_proto::usage::FEAT_USAGE_PROVIDER_METRICS
        | FEAT_USAGE_STATUS
        | FEAT_USAGE_RATELIMIT
        | FEAT_USAGE_MANAGE
        | FEAT_ACCOUNT_CONNECTION
        | FEAT_ACCOUNT_POLICY;
    let mut conn = connect_daemon(socket, &hello, auto_start)?;
    if conn.negotiated.features & FEAT_USAGE == 0 {
        return Err("krilld does not support FEAT_USAGE".to_owned());
    }
    let required = req.required_features();
    if conn.negotiated.features & required != required {
        return Err("krilld does not support required account management feature".to_owned());
    }
    let timeout = Some(Duration::from_secs(10));
    conn.stream
        .set_read_timeout(timeout)
        .map_err(|e| e.to_string())?;
    conn.stream
        .set_write_timeout(timeout)
        .map_err(|e| e.to_string())?;
    let frame = req.encode().map_err(|e| e.to_string())?;
    conn.stream.write_all(&frame).map_err(|e| e.to_string())?;
    let (kind, body) = ipc::read_frame(&mut conn.stream)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "krilld disconnected".to_owned())?;
    match UsageReply::decode(kind, &body).map_err(|e| e.to_string())? {
        UsageReply::Accounts(snap) => Ok(snap),
        UsageReply::Failed(e) => Err(e.to_string()),
        _ => Err("unexpected reply from krilld".to_owned()),
    }
}

// ============================================================================
// Domain 5: Open Agent Sessions (`sessions` via `krilld` ResumeVault)
// ============================================================================

/// 이미 이어 띄운 Orca 세션 기록(재실행 시 중복 기동 방지). 한 줄에 `<agent>:<id>` 하나.
const SESSIONS_LEDGER_FILE: &str = "orca-sessions-imported";

/// Orca `workspaceSession`에서 아직 열린 탭에 붙은 에이전트 세션을 모은다. 열린 탭의 `aiVaultTitle`과,
/// 탭이 살아 있는 `sleepingAgentSessionsByPaneKey`(분할 칸 포함)가 대상이다. 닫힌 탭·원격 호스트
/// (`workspaceSessionsByHostId`)·로컬에 없는 경로는 뺀다.
fn orca_open_sessions(domains: &BTreeMap<String, Json>) -> Vec<(VaultTarget, Option<String>)> {
    let Some(ws) = domains.get("workspaceSession") else {
        return Vec::new();
    };
    let mut tabs: BTreeMap<&str, &Json> = BTreeMap::new();
    if let Some(map) = ws.get("tabsByWorktree").and_then(Json::as_object) {
        for tab in map.values().filter_map(Json::as_array).flatten() {
            if let Some(id) = tab.get("id").and_then(Json::as_str) {
                tabs.insert(id, tab);
            }
        }
    }
    let tab_title = |tab: &Json| {
        tab.pointer("/aiVaultTitle/title")
            .or_else(|| tab.get("title"))
            .and_then(Json::as_str)
            .map(str::to_owned)
    };
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut push = |agent: Option<&Json>, id: Option<&Json>, worktree: Option<&Json>, title| {
        let (Some(agent), Some(id), Some(worktree)) = (
            agent.and_then(Json::as_str).and_then(VaultAgent::parse),
            id.and_then(Json::as_str).map(str::trim),
            worktree.and_then(Json::as_str),
        ) else {
            return false;
        };
        let Some((_, cwd)) = resolve_workspace_key(domains, worktree) else {
            return false;
        };
        if id.is_empty() || !Path::new(&cwd).is_dir() {
            return false;
        }
        if !seen.insert((agent, id.to_owned())) {
            return true;
        }
        let target = VaultTarget {
            agent,
            id: id.to_owned(),
            cwd,
            modified: 0,
        };
        out.push((target, title));
        true
    };
    // 분할 칸의 재개 기록이 현재 대화의 원천이다. 탭의 aiVaultTitle은 이전 대화 ID가
    // 남아 있을 수 있으므로 칸 기록이 없는 탭에서만 폴백으로 쓴다.
    let mut captured_tabs = BTreeSet::new();
    if let Some(map) = ws
        .get("sleepingAgentSessionsByPaneKey")
        .and_then(Json::as_object)
    {
        for pane in map.values() {
            let Some(tab) = pane
                .get("tabId")
                .and_then(Json::as_str)
                .and_then(|id| tabs.get(id))
            else {
                continue;
            };
            if push(
                pane.get("agent"),
                pane.pointer("/providerSession/id"),
                pane.get("worktreeId"),
                tab_title(tab),
            ) {
                captured_tabs.insert(tab.get("id").and_then(Json::as_str).unwrap_or_default());
            }
        }
    }
    for (id, tab) in tabs {
        if !captured_tabs.contains(id) {
            push(
                tab.pointer("/aiVaultTitle/agent"),
                tab.pointer("/aiVaultTitle/sessionId"),
                tab.get("worktreeId"),
                tab_title(tab),
            );
        }
    }
    out
}

fn import_sessions_domain(
    opts: &ImportOptions,
    domains: &BTreeMap<String, Json>,
    warnings: &mut Vec<String>,
) -> SessionsImportSummary {
    let mut summary = SessionsImportSummary::default();
    let found = orca_open_sessions(domains);
    if found.is_empty() {
        return summary;
    }
    let ledger = opts.state_dir.join(SESSIONS_LEDGER_FILE);
    let done: BTreeSet<String> = fs::read_to_string(&ledger)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    let mut conn = None;
    if !opts.dry_run {
        let socket = daemon::socket_path(&opts.state_dir).map_err(|error| error.to_string());
        let mut hello = daemon::client_hello(ClientRole::Cli, "krill import orca");
        let required = FEAT_CONTROL | FEAT_AGENT_HISTORY | FEAT_AI_VAULT;
        hello.features |= required;
        match socket
            .and_then(|socket| connect_daemon(&socket, &hello, should_auto_start_daemon(false)))
        {
            Ok(c) if c.negotiated.features & required == required => conn = Some(c),
            Ok(_) => {
                summary.skipped = true;
                summary.skipped_reason =
                    Some("krilld does not support session resume; update the daemon".to_owned());
            }
            Err(e) => {
                summary.skipped = true;
                summary.skipped_reason = Some(format!(
                    "Daemon required: couldn't reach krilld, skipped importing sessions ({e})"
                ));
            }
        }
    }
    for (target, title) in found {
        let key = format!("{}:{}", target.agent.profile(), target.id);
        let mut resumed = done.contains(&key);
        if resumed {
            summary.already_resumed += 1;
        } else if let Some(c) = conn.as_mut() {
            match resume_vault(c, &target) {
                Ok(()) => {
                    resumed = true;
                    summary.resumed += 1;
                    if let Err(e) = append_line(&ledger, &key) {
                        warnings.push(format!(
                            "failed to record {key} in {}: {e}",
                            ledger.display()
                        ));
                    }
                }
                Err(e) => warnings.push(format!("session {key} not resumed: {e}")),
            }
        }
        summary.items.push(ImportedSessionSummary {
            agent: target.agent.profile().to_owned(),
            id: target.id,
            cwd: target.cwd,
            title,
            resumed,
        });
    }
    summary
}

/// 열린 제어 연결로 `ResumeVault`를 보내 세션 하나를 그 cwd에서 이어 띄운다.
fn resume_vault(conn: &mut daemon::Connection, target: &VaultTarget) -> Result<(), String> {
    let timeout = Some(Duration::from_secs(10));
    conn.stream
        .set_read_timeout(timeout)
        .map_err(|e| e.to_string())?;
    conn.stream
        .set_write_timeout(timeout)
        .map_err(|e| e.to_string())?;
    let frame = Request::AgentHistory(Action::ResumeVault(target.clone()))
        .encode()
        .map_err(|e| e.to_string())?;
    conn.stream.write_all(&frame).map_err(|e| e.to_string())?;
    let (kind, body) = ipc::read_frame(&mut conn.stream)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "krilld disconnected".to_owned())?;
    match Reply::decode(kind, &body).map_err(|e| e.to_string())? {
        Reply::AgentHistory(Response::Resumed { .. }) => Ok(()),
        Reply::Failed(e) => Err(e.to_string()),
        _ => Err("unexpected reply from krilld".to_owned()),
    }
}

fn append_line(path: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    writeln!(options.open(path)?, "{line}")
}

fn resolve_claude_profile_dir(
    home: &Path,
    orca_dir: &Path,
    raw_id: &str,
    acc: &OrcaClaudeAccount,
) -> PathBuf {
    let explicit = acc
        .managed_auth_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| expand_tilde(home, s));
    let claude_profiles_home = orca_dir.join("claude-profiles").join(raw_id).join("home");
    let claude_accounts_auth = orca_dir.join("claude-accounts").join(raw_id).join("auth");

    if let Some(ref exp) = explicit
        && exp.is_dir()
    {
        return exp.clone();
    }
    if claude_profiles_home.is_dir() {
        return claude_profiles_home;
    }
    if claude_accounts_auth.is_dir() {
        return claude_accounts_auth;
    }
    explicit.unwrap_or(claude_profiles_home)
}

fn resolve_codex_profile_dir(
    home: &Path,
    orca_dir: &Path,
    raw_id: &str,
    acc: &OrcaCodexAccount,
) -> PathBuf {
    let explicit = acc
        .managed_home_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| expand_tilde(home, s));
    let short_orca_home = home
        .join(".orca")
        .join("codex-homes")
        .join(krill_proto::usage::profiles::codex_short_key(raw_id));
    let codex_accounts_home = orca_dir.join("codex-accounts").join(raw_id).join("home");
    if let Some(ref exp) = explicit
        && exp.is_dir()
    {
        return exp.clone();
    }
    if short_orca_home.is_dir() {
        return short_orca_home;
    }
    if codex_accounts_home.is_dir() {
        return codex_accounts_home;
    }
    explicit.unwrap_or(codex_accounts_home)
}

fn format_account_label(email: Option<&str>, org_or_ws: Option<&str>, fallback: &str) -> String {
    match (email, org_or_ws) {
        (Some(e), Some(o)) if !e.eq_ignore_ascii_case(o) => format!("{e} ({o})"),
        (Some(e), _) => e.to_owned(),
        (None, Some(o)) => o.to_owned(),
        (None, None) => fallback.to_owned(),
    }
}

// ============================================================================
// Domain 2: Remote Hosts (`ssh`)
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HostsTomlFile {
    #[serde(default)]
    hosts: Vec<StoredSshHost>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredSshHost {
    id: String,
    label: String,
    destination: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_socket: Option<String>,
    #[serde(default)]
    imported: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaSshTarget {
    id: String,
    label: String,
    config_host: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    username: Option<String>,
    identity_file: Option<String>,
    source: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaEnvironmentsFile {
    environments: Vec<OrcaEnvironment>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaEnvironment {
    id: String,
    name: String,
    endpoints: Vec<OrcaEndpoint>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaEndpoint {
    endpoint: String,
}

fn import_ssh_domain(
    opts: &ImportOptions,
    domains: &BTreeMap<String, Json>,
    warnings: &mut Vec<String>,
) -> io::Result<SshImportSummary> {
    let hosts_path = opts.state_dir.join(HOSTS_FILE);
    let original = read_original(&hosts_path)?;
    let mut file: HostsTomlFile = match &original {
        Some(raw) => toml::from_str(raw).map_err(invalid_data)?,
        None => HostsTomlFile::default(),
    };

    let mut summary = SshImportSummary::default();

    if let Some(arr) = domains.get("sshTargets")
        && let Ok(targets) = serde_json::from_value::<Vec<OrcaSshTarget>>(arr.clone())
    {
        for target in targets {
            let Some(destination) = resolve_ssh_destination(&target) else {
                continue;
            };
            if !valid_ssh_destination(&destination) {
                warnings.push(format!("skipping invalid SSH destination: {destination}"));
                continue;
            }
            let raw_id = if target.id.trim().is_empty() {
                slugify(&destination)
            } else {
                slugify(target.id.trim())
            };
            let label = if target.label.trim().is_empty() {
                destination.clone()
            } else {
                target.label.trim().to_owned()
            };
            let port = target.port.filter(|&p| p > 0 && p != 22);
            let identity_file = target
                .identity_file
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let imported = target.source.as_deref() == Some("ssh-config");

            upsert_ssh_host(
                &mut file,
                StoredSshHost {
                    id: raw_id,
                    label,
                    destination,
                    port,
                    identity_file,
                    remote_socket: None,
                    imported,
                },
                &mut summary,
                false,
            );
        }
    }

    // Merge `<orca_dir>/orca-environments.json`
    let env_path = opts.orca_dir.join("orca-environments.json");
    if let Ok(raw) = fs::read_to_string(&env_path)
        && let Ok(env_file) = serde_json::from_str::<OrcaEnvironmentsFile>(&raw)
    {
        for env in env_file.environments {
            let Some((host, _port)) = env
                .endpoints
                .iter()
                .find_map(|ep| extract_remote_host(&ep.endpoint))
            else {
                continue;
            };
            if !valid_ssh_destination(&host) {
                continue;
            }
            // Orca 원격 환경 주소에는 SSH 사용자가 없다. 이미 SSH 대상으로 등록된 기기면 사용자 없는 중복을 만들지 않는다.
            let env_host = host.to_ascii_lowercase();
            if file.hosts.iter().any(|h| {
                !h.id.starts_with("orca-env-")
                    && krill_hosts::ssh::machine_key(&h.destination, h.port)
                        .split_once('@')
                        .and_then(|(_, rest)| rest.rsplit_once(':'))
                        .is_some_and(|(name, _)| name == env_host)
            }) {
                continue;
            }
            let id = format!(
                "orca-env-{}",
                slugify(if env.id.trim().is_empty() {
                    &host
                } else {
                    env.id.trim()
                })
            );
            let label = if env.name.trim().is_empty() {
                host.clone()
            } else {
                env.name.trim().to_owned()
            };
            upsert_ssh_host(
                &mut file,
                StoredSshHost {
                    id,
                    label,
                    destination: host,
                    port: None,
                    identity_file: None,
                    remote_socket: None,
                    imported: false,
                },
                &mut summary,
                true,
            );
        }
    }

    if !opts.dry_run && (!file.hosts.is_empty() || hosts_path.exists()) {
        let text = config_document::save_toml(&hosts_path, &file)?;
        write_atomic_0600(&hosts_path, text.as_bytes(), original.as_deref())?;
    }

    Ok(summary)
}

fn upsert_ssh_host(
    file: &mut HostsTomlFile,
    candidate: StoredSshHost,
    summary: &mut SshImportSummary,
    from_env: bool,
) {
    if let Some(existing) = file.hosts.iter_mut().find(|h| {
        h.id == candidate.id || h.destination.eq_ignore_ascii_case(&candidate.destination)
    }) {
        let kept_id = existing.id.clone();
        let mut changed = false;
        if existing.label != candidate.label && !from_env {
            existing.label.clone_from(&candidate.label);
            changed = true;
        }
        if existing.destination != candidate.destination && !from_env {
            existing.destination.clone_from(&candidate.destination);
            changed = true;
        }
        if candidate.port.is_some() && existing.port != candidate.port {
            existing.port = candidate.port;
            changed = true;
        }
        if candidate.identity_file.is_some() && existing.identity_file != candidate.identity_file {
            existing.identity_file.clone_from(&candidate.identity_file);
            changed = true;
        }
        if changed {
            summary.hosts_updated = summary.hosts_updated.saturating_add(1);
            if from_env {
                summary.environments_merged = summary.environments_merged.saturating_add(1);
            }
        }
        if !summary.items.iter().any(|i| i.id == kept_id) {
            summary.items.push(ImportedHostSummary {
                id: kept_id,
                label: existing.label.clone(),
                destination: existing.destination.clone(),
                port: existing.port,
                identity_file: existing.identity_file.clone(),
                imported: existing.imported,
            });
        }
    } else {
        summary.hosts_added = summary.hosts_added.saturating_add(1);
        if from_env {
            summary.environments_merged = summary.environments_merged.saturating_add(1);
        }
        summary.items.push(ImportedHostSummary {
            id: candidate.id.clone(),
            label: candidate.label.clone(),
            destination: candidate.destination.clone(),
            port: candidate.port,
            identity_file: candidate.identity_file.clone(),
            imported: candidate.imported,
        });
        file.hosts.push(candidate);
    }
}

fn resolve_ssh_destination(t: &OrcaSshTarget) -> Option<String> {
    if let Some(ch) = t.config_host.as_deref().map(str::trim)
        && !ch.is_empty()
    {
        return Some(ch.to_owned());
    }
    let host = t.host.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    if let Some(user) = t
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && !host.contains('@')
    {
        return Some(format!("{user}@{host}"));
    }
    Some(host.to_owned())
}

fn valid_ssh_destination(s: &str) -> bool {
    let trimmed = s.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 256
        && !trimmed.starts_with('-')
        && !trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn extract_remote_host(endpoint: &str) -> Option<(String, Option<u16>)> {
    let trimmed = endpoint.trim();
    let rest = trimmed
        .strip_prefix("wss://")
        .or_else(|| trimmed.strip_prefix("ws://"))
        .or_else(|| trimmed.strip_prefix("ssh://"))
        .or_else(|| trimmed.strip_prefix("https://"))
        .or_else(|| trimmed.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    let (clean, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (host, trailing) = inner.split_once(']')?;
        let port = if trailing.is_empty() {
            None
        } else {
            let raw_port = trailing.strip_prefix(':')?;
            Some(raw_port.parse::<u16>().ok()?)
        };
        (host, port)
    } else if let Some((h, p)) = authority.split_once(':') {
        (h, p.parse::<u16>().ok())
    } else {
        (authority, None)
    };
    if clean.is_empty()
        || matches!(
            clean.to_ascii_lowercase().as_str(),
            "localhost" | "127.0.0.1" | "::1" | "0.0.0.0"
        )
    {
        return None;
    }
    Some((clean.to_owned(), port))
}

// ============================================================================
// Domain 3: Projects, Repos, Folder Workspaces & Worktree Metadata (`projects`)
// ============================================================================

/// Orca `RepoIcon`(`.ref/orca/src/shared/repo-icon.ts`):
/// `{type:"lucide",name}` · `{type:"emoji",emoji}` · `{type:"image",src,source}`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct OrcaRepoIcon {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    emoji: String,
    src: String,
}

/// Orca `MAX_REPO_ICON_UPLOAD_BYTES`와 같은 이미지 상한.
const MAX_REPO_ICON_BYTES: usize = 256 * 1024;
pub const REPO_ICON_DIR: &str = "repo-icons";

/// Orca 아이콘을 Krill 저장 형식으로 옮긴다. 인라인 `data:` 이미지는 `projects.toml` 상한을 넘지 않게
/// `<state_dir>/repo-icons/<id>.<ext>`로 풀어 경로만 남긴다(`write = false`면 파일은 쓰지 않는다).
fn import_repo_icon(
    icon: Option<&OrcaRepoIcon>,
    state_dir: &Path,
    id: &str,
    write: bool,
) -> Option<StoredRepoIcon> {
    let icon = icon?;
    let (kind, value) = match icon.kind.trim() {
        "lucide" => {
            let name = icon.name.trim();
            let ok = name.len() <= 40
                && name.starts_with(|c: char| c.is_ascii_alphabetic())
                && name.chars().all(|c| c.is_ascii_alphanumeric());
            ("lucide", ok.then(|| name.to_owned())?)
        }
        "emoji" => {
            let emoji = icon.emoji.trim();
            let ok = !emoji.is_empty() && emoji.chars().count() <= 16;
            ("emoji", ok.then(|| emoji.to_owned())?)
        }
        "image" => {
            let src = icon.src.trim();
            if src.starts_with("https://") {
                ("image", src.to_owned())
            } else {
                let (ext, b64) = src
                    .strip_prefix("data:image/png;base64,")
                    .map(|b| ("png", b))
                    .or_else(|| {
                        src.strip_prefix("data:image/webp;base64,")
                            .map(|b| ("webp", b))
                    })?;
                let bytes = decode_base64(b64)?;
                let is_png = bytes.starts_with(b"\x89PNG\r\n\x1a\n");
                let is_webp = bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP");
                if !(is_png || is_webp) {
                    return None;
                }
                let path = state_dir
                    .join(REPO_ICON_DIR)
                    .join(format!("{}.{ext}", slugify(id)));
                if write && fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
                    let dir = path.parent()?;
                    ensure_dir_0700(dir).ok()?;
                    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
                    if fs::write(&tmp, &bytes)
                        .and_then(|()| fs::rename(&tmp, &path))
                        .is_err()
                    {
                        fs::remove_file(&tmp).ok();
                        return None;
                    }
                }
                ("image", path.to_string_lossy().into_owned())
            }
        }
        _ => return None,
    };
    Some(StoredRepoIcon {
        kind: kind.to_owned(),
        value,
    })
}

/// 표준 base64(패딩 허용) 디코더. 상한([`MAX_REPO_ICON_BYTES`])을 넘거나 잘못된 문자면 `None`.
fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let (mut buf, mut bits) = (0u32, 0u32);
    for b in input.bytes().filter(|b| !b.is_ascii_whitespace()) {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buf >> bits) & 0xFF).ok()?);
            if out.len() > MAX_REPO_ICON_BYTES {
                return None;
            }
        }
    }
    Some(out)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaRepo {
    id: String,
    path: String,
    display_name: String,
    badge_color: Option<String>,
    repo_icon: Option<OrcaRepoIcon>,
    kind: Option<String>,
    worktree_base_ref: Option<String>,
    worktree_base_path: Option<String>,
    symlink_paths: Vec<String>,
    hook_settings: Option<OrcaHookSettings>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaHookSettings {
    mode: Option<String>,
    scripts: Option<OrcaHookScripts>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaHookScripts {
    setup: Option<String>,
    archive: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaProject {
    id: String,
    display_name: String,
    badge_color: Option<String>,
    repo_icon: Option<OrcaRepoIcon>,
    kind: Option<String>,
    source_repo_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaProjectHostSetup {
    id: String,
    project_id: Option<String>,
    host_id: Option<String>,
    repo_id: Option<String>,
    path: String,
    display_name: Option<String>,
    kind: Option<String>,
    worktree_base_path: Option<String>,
    symlink_paths: Vec<String>,
    hook_settings: Option<OrcaHookSettings>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaFolderWorkspace {
    id: String,
    name: String,
    folder_path: String,
    comment: Option<String>,
    is_archived: bool,
    is_unread: bool,
    is_pinned: bool,
    sort_order: Option<i64>,
    workspace_status: Option<String>,
    linked_task: Option<OrcaLinkedItem>,
    linked_items: Vec<OrcaLinkedItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaWorktreeMeta {
    host_id: Option<String>,
    display_name: Option<String>,
    comment: Option<String>,
    linked_issue: Option<u64>,
    #[serde(rename = "linkedPR")]
    linked_pr: Option<u64>,
    linked_linear_issue: Option<String>,
    linked_work_item: Option<OrcaLinkedItem>,
    linked_items: Vec<OrcaLinkedItem>,
    is_archived: bool,
    is_unread: bool,
    is_pinned: bool,
    sort_order: Option<i64>,
    workspace_status: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaLinkedItem {
    provider: Option<String>,
    kind: Option<String>,
    identifier: Option<String>,
    number: Option<u64>,
    url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaWorktreeLineage {
    repo_id: Option<String>,
    #[serde(alias = "worktreeId", alias = "childWorkspaceKey")]
    worktree_path: Option<String>,
    branch: Option<String>,
    base_ref: Option<String>,
    #[serde(alias = "parentWorktreeId", alias = "parentWorkspaceKey")]
    parent_worktree_path: Option<String>,
    parent_branch: Option<String>,
    created_at: Option<u64>,
    fork_point_sha: Option<String>,
}

/// macOS 기본 심볼릭 링크(`/tmp`·`/var`·`/etc` → `/private/...`)를 어휘적으로 푼다.
/// `fs::canonicalize`가 실패하는(존재하지 않는) 경로도 저장 키·비교가 일치하게 한다.
fn lexical_private_alias(path: &Path) -> PathBuf {
    for base in ["tmp", "var", "etc"] {
        let prefix = Path::new("/").join(base);
        if let Ok(rest) = path.strip_prefix(&prefix) {
            return Path::new("/private").join(base).join(rest);
        }
    }
    path.to_path_buf()
}

/// Orca가 준 경로(심볼릭 링크 포함 가능)를 저장용 정규 경로로 바꾼다(OWE-803).
/// 존재하면 `fs::canonicalize`로 풀고, 없으면 macOS 기본 별칭만 어휘적으로 맞춘다.
/// `[[projects]] path`와 `[worktrees]`·`[lineage]` 키가 같은 표기를 쓰게 해
/// 사이드바에서 동일 폴더가 2번 렌더링되지 않게 한다.
fn normalize_import_path(path: &Path) -> PathBuf {
    if let Ok(canon) = fs::canonicalize(path) {
        return canon;
    }
    let aliased = lexical_private_alias(path);
    if aliased != path {
        if let Ok(canon) = fs::canonicalize(&aliased) {
            return canon;
        }
        return aliased;
    }
    path.to_path_buf()
}

fn normalize_import_path_str(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    normalize_import_path(Path::new(trimmed))
        .to_string_lossy()
        .into_owned()
}

/// 저장된 경로와 이관 후보 경로가 심볼릭 링크 별칭(`/tmp` ↔ `/private/tmp` 등)만
/// 다르고 같은 디렉터리면 참이다. 양쪽이 모두 존재하지 않을 때도 어휘 별칭으로 비교한다.
fn same_import_path(a: &str, b: &str) -> bool {
    let (ta, tb) = (a.trim(), b.trim());
    ta == tb || normalize_import_path(Path::new(ta)) == normalize_import_path(Path::new(tb))
}

/// 계보 항목을 별칭 중복 없이 저장한다(OWE-803). 과거 이관에서 원본 경로(`/tmp/...`)로
/// 저장된 키가 있으면 정규 키(`/private/tmp/...`)로 교체해 사이드바 중복을 없앤다.
fn insert_lineage_entry(
    lineage: &mut BTreeMap<String, StoredWorktreeLineage>,
    key: String,
    value: StoredWorktreeLineage,
) {
    let norm = normalize_import_path(Path::new(&key));
    lineage.retain(|k, _| normalize_import_path(Path::new(k)) != norm);
    lineage.insert(key, value);
}

fn import_projects_domain(
    opts: &ImportOptions,
    domains: &BTreeMap<String, Json>,
    warnings: &mut Vec<String>,
) -> io::Result<ProjectsImportSummary> {
    let original = read_original(&opts.state_dir.join(PROJECTS_FILE))?;
    let mut store = ProjectsStore::load(&opts.state_dir)?;
    repair_imported_workspace_paths(&mut store);
    // worktree-comments.toml은 저장 시점에 잠금 안에서 다시 읽어 병합한다(OWE-748). 여기엔 이관분만 모은다.
    let mut wt_store = WorktreeStore::default();
    let mut lineage_parents: Vec<(String, String)> = Vec::new();
    let mut summary = ProjectsImportSummary::default();

    let projects_by_id: BTreeMap<String, OrcaProject> = domains
        .get("projects")
        .and_then(|v| serde_json::from_value::<Vec<OrcaProject>>(v.clone()).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect();

    // 1. `repos`
    if let Some(arr) = domains.get("repos")
        && let Ok(repos) = serde_json::from_value::<Vec<OrcaRepo>>(arr.clone())
    {
        for repo in repos {
            let clean_path = normalize_import_path_str(&repo.path);
            if clean_path.is_empty() {
                continue;
            }
            let proj_meta = projects_by_id
                .values()
                .find(|p| p.source_repo_ids.contains(&repo.id));
            let display_name = if !repo.display_name.trim().is_empty() {
                repo.display_name.trim().to_owned()
            } else if let Some(pm) = proj_meta
                && !pm.display_name.trim().is_empty()
            {
                pm.display_name.trim().to_owned()
            } else {
                basename_str(&clean_path)
            };
            let badge_color = repo
                .badge_color
                .clone()
                .or_else(|| proj_meta.and_then(|p| p.badge_color.clone()))
                .filter(|s| !s.trim().is_empty());
            let repo_icon = import_repo_icon(
                repo.repo_icon
                    .as_ref()
                    .or_else(|| proj_meta.and_then(|p| p.repo_icon.as_ref())),
                &opts.state_dir,
                &repo.id,
                !opts.dry_run,
            );
            let kind = repo
                .kind
                .as_deref()
                .or_else(|| proj_meta.and_then(|p| p.kind.as_deref()))
                .map_or_else(|| "git".to_owned(), |k| k.trim().to_owned());
            let hook_mode = repo
                .hook_settings
                .as_ref()
                .and_then(|h| h.mode.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let setup_script = repo
                .hook_settings
                .as_ref()
                .and_then(|h| h.scripts.as_ref())
                .and_then(|s| s.setup.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let archive_script = repo
                .hook_settings
                .as_ref()
                .and_then(|h| h.scripts.as_ref())
                .and_then(|s| s.archive.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);

            let candidate = StoredProject {
                id: if repo.id.trim().is_empty() {
                    slugify(&display_name)
                } else {
                    repo.id.trim().to_owned()
                },
                path: clean_path,
                display_name,
                badge_color,
                repo_icon,
                kind,
                host_id: None,
                worktree_base_ref: repo.worktree_base_ref.filter(|s| !s.trim().is_empty()),
                worktree_base_path: repo.worktree_base_path.filter(|s| !s.trim().is_empty()),
                symlink_paths: repo
                    .symlink_paths
                    .into_iter()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect(),
                hook_mode,
                setup_script,
                archive_script,
                pinned: false,
                archived: false,
                sort_order: None,
            };
            upsert_project(&mut store, candidate, &mut summary);
        }
    }

    // 2. `projectHostSetups`
    if let Some(arr) = domains.get("projectHostSetups")
        && let Ok(setups) = serde_json::from_value::<Vec<OrcaProjectHostSetup>>(arr.clone())
    {
        for setup in setups {
            let clean_path = normalize_import_path_str(&setup.path);
            if clean_path.is_empty() {
                continue;
            }
            let proj = setup
                .project_id
                .as_deref()
                .and_then(|id| projects_by_id.get(id));
            let display_name = setup
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or_else(|| proj.map(|p| p.display_name.trim().to_owned()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| basename_str(&clean_path));
            let badge_color = proj
                .and_then(|p| p.badge_color.clone())
                .filter(|s| !s.trim().is_empty());
            let repo_icon = proj.and_then(|p| {
                import_repo_icon(p.repo_icon.as_ref(), &opts.state_dir, &p.id, !opts.dry_run)
            });
            let kind = setup
                .kind
                .as_deref()
                .or_else(|| proj.and_then(|p| p.kind.as_deref()))
                .map_or_else(|| "git".to_owned(), |k| k.trim().to_owned());
            let hook_mode = setup
                .hook_settings
                .as_ref()
                .and_then(|h| h.mode.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let setup_script = setup
                .hook_settings
                .as_ref()
                .and_then(|h| h.scripts.as_ref())
                .and_then(|s| s.setup.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let archive_script = setup
                .hook_settings
                .as_ref()
                .and_then(|h| h.scripts.as_ref())
                .and_then(|s| s.archive.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);

            let host_id = setup
                .host_id
                .as_deref()
                .map(str::trim)
                .filter(|h| !h.is_empty() && *h != "local")
                .map(str::to_owned);
            let base_id = setup
                .repo_id
                .as_deref()
                .or(setup.project_id.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    let trimmed = setup.id.trim();
                    (!trimmed.is_empty()).then_some(trimmed)
                })
                .map_or_else(|| slugify(&display_name), str::to_owned);
            let id = match &host_id {
                Some(host) => format!("{host}:{base_id}"),
                None => base_id,
            };

            let candidate = StoredProject {
                id,
                path: clean_path,
                display_name,
                badge_color,
                repo_icon,
                kind,
                host_id,
                worktree_base_ref: None,
                worktree_base_path: setup.worktree_base_path.filter(|s| !s.trim().is_empty()),
                symlink_paths: setup
                    .symlink_paths
                    .into_iter()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect(),
                hook_mode,
                setup_script,
                archive_script,
                pinned: false,
                archived: false,
                sort_order: None,
            };
            upsert_project(&mut store, candidate, &mut summary);
        }
    }

    // 3. `folderWorkspaces`
    if let Some(arr) = domains.get("folderWorkspaces")
        && let Ok(folders) = serde_json::from_value::<Vec<OrcaFolderWorkspace>>(arr.clone())
    {
        for fw in folders {
            let clean_path = normalize_import_path_str(&fw.folder_path);
            if clean_path.is_empty() {
                continue;
            }
            let display_name = if fw.name.trim().is_empty() {
                basename_str(&clean_path)
            } else {
                fw.name.trim().to_owned()
            };
            let candidate = StoredProject {
                id: if fw.id.trim().is_empty() {
                    slugify(&display_name)
                } else {
                    fw.id.trim().to_owned()
                },
                path: clean_path.clone(),
                display_name: display_name.clone(),
                badge_color: None,
                repo_icon: None,
                kind: "folder".to_owned(),
                host_id: None,
                worktree_base_ref: None,
                worktree_base_path: None,
                symlink_paths: Vec::new(),
                hook_mode: None,
                setup_script: None,
                archive_script: None,
                pinned: fw.is_pinned,
                archived: fw.is_archived,
                sort_order: fw.sort_order,
            };
            upsert_project(&mut store, candidate, &mut summary);

            // Also apply folderWorkspace comment/status/unread/links into WorktreeStore
            let meta = OrcaWorktreeMeta {
                host_id: None,
                display_name: Some(display_name),
                comment: fw.comment,
                linked_issue: None,
                linked_pr: None,
                linked_linear_issue: None,
                linked_work_item: fw.linked_task,
                linked_items: fw.linked_items,
                is_archived: fw.is_archived,
                is_unread: fw.is_unread,
                is_pinned: fw.is_pinned,
                sort_order: fw.sort_order,
                workspace_status: fw.workspace_status,
            };
            apply_worktree_meta(&clean_path, None, &meta, &mut store, &mut wt_store);
            summary.worktrees_migrated = summary.worktrees_migrated.saturating_add(1);
            if meta
                .comment
                .as_deref()
                .is_some_and(|c| !c.trim().is_empty())
            {
                summary.comments_migrated = summary.comments_migrated.saturating_add(1);
            }
        }
    }

    // 4. `worktreeMeta` and `worktreeMetaByIdentity`
    for domain_key in ["worktreeMeta", "worktreeMetaByIdentity"] {
        if let Some(Json::Object(map)) = domains.get(domain_key) {
            for (raw_key, val) in map {
                let Ok(meta) = serde_json::from_value::<OrcaWorktreeMeta>(val.clone()) else {
                    continue;
                };
                let Some((repo_id, raw_wt_path)) = resolve_workspace_key(domains, raw_key) else {
                    continue;
                };
                if meta.host_id.as_deref().is_some_and(|id| id != "local") {
                    continue;
                }
                let wt_path = normalize_import_path_str(&raw_wt_path);
                if wt_path.is_empty() {
                    continue;
                }
                if missing_local_worktree(&store, repo_id.as_deref(), &wt_path) {
                    store.worktrees.remove(&wt_path);
                    store.lineage.remove(&wt_path);
                    continue;
                }
                // Krill는 같은 폴더를 한 행에 묶는다. 가상 작업 공간 메타가 실제 폴더의
                // 이름·보관 상태를 덮어써 그 폴더의 모든 세션을 숨기지 않게 한다.
                if workspace_instance_path(raw_key).is_some()
                    && store.worktrees.contains_key(&wt_path)
                {
                    continue;
                }
                apply_worktree_meta(
                    &wt_path,
                    repo_id.as_deref(),
                    &meta,
                    &mut store,
                    &mut wt_store,
                );
                summary.worktrees_migrated = summary.worktrees_migrated.saturating_add(1);
                if meta
                    .comment
                    .as_deref()
                    .is_some_and(|c| !c.trim().is_empty())
                {
                    summary.comments_migrated = summary.comments_migrated.saturating_add(1);
                }
            }
        }
    }

    // 5. `worktreeLineageById` and `workspaceLineageByChildKey`
    for domain_key in ["worktreeLineageById", "workspaceLineageByChildKey"] {
        if let Some(Json::Object(map)) = domains.get(domain_key) {
            for (raw_key, val) in map {
                let Ok(lin) = serde_json::from_value::<OrcaWorktreeLineage>(val.clone()) else {
                    continue;
                };
                let Some((key_repo_id, key_path)) = resolve_workspace_key(
                    domains,
                    lin.worktree_path
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or(raw_key),
                ) else {
                    continue;
                };
                let wt_path = normalize_import_path_str(&key_path);
                if wt_path.is_empty() {
                    continue;
                }
                let repo_id = lin
                    .repo_id
                    .or(key_repo_id)
                    .or_else(|| split_worktree_key(raw_key).0);
                if missing_local_worktree(&store, repo_id.as_deref(), &wt_path) {
                    store.lineage.remove(&wt_path);
                    continue;
                }
                let parent_worktree_path = lin
                    .parent_worktree_path
                    .as_deref()
                    .and_then(|p| resolve_workspace_key(domains, p))
                    .map(|(_, p)| normalize_import_path_str(&p))
                    .filter(|p| !p.is_empty() && *p != wt_path);
                if let Some(parent) = &parent_worktree_path {
                    lineage_parents.push((wt_path.clone(), parent.clone()));
                }
                insert_lineage_entry(
                    &mut store.lineage,
                    wt_path.clone(),
                    StoredWorktreeLineage {
                        repo_id,
                        worktree_path: wt_path,
                        branch: lin.branch.filter(|s| !s.trim().is_empty()),
                        base_ref: lin.base_ref.filter(|s| !s.trim().is_empty()),
                        parent_worktree_path,
                        parent_branch: lin.parent_branch.filter(|s| !s.trim().is_empty()),
                        fork_point_sha: lin.fork_point_sha.filter(|s| !s.trim().is_empty()),
                        created_at: lin.created_at,
                    },
                );
                summary.lineage_migrated = summary.lineage_migrated.saturating_add(1);
            }
        }
    }

    // OWE-803: 동일 디렉터리의 별칭 중복(원본 경로 잔재)을 저장 전에 하나로 합친다.
    dedupe_projects_by_normalized_path(&mut store);

    if !opts.dry_run {
        store.save(&opts.state_dir, original.as_deref())?;
        let saved = WorktreeStore::mutate(&opts.state_dir, |current| {
            merge_imported_worktree_meta(current, wt_store);
            // 부모 계보 단일 원천은 WorktreeStore(OWE-756). 사용자가 이미 정한 부모는 덮어쓰지 않는다.
            for (child, parent) in &lineage_parents {
                if current.parent_of(&worktree_key(Path::new(child))).is_none() {
                    current.set_parent(child, Some(parent)).ok();
                }
            }
            Ok(())
        });
        if let Err(e) = saved {
            warnings.push(format!("failed to save worktree-comments.toml: {e}"));
        }
    }

    Ok(summary)
}

fn upsert_project(
    store: &mut ProjectsStore,
    candidate: StoredProject,
    summary: &mut ProjectsImportSummary,
) {
    if let Some(existing) = store.projects.iter_mut().find(|p| {
        p.host_id == candidate.host_id
            && (p.path == candidate.path
                || same_import_path(&p.path, &candidate.path)
                || p.id == candidate.id)
    }) {
        let mut changed = false;
        if existing.path != candidate.path && same_import_path(&existing.path, &candidate.path) {
            // OWE-803: 과거 이관분이 원본 경로(`/tmp/...`)로 남아 있으면 정규 경로로 수렴시킨다.
            existing.path.clone_from(&candidate.path);
            changed = true;
        }
        if existing.display_name != candidate.display_name && !candidate.display_name.is_empty() {
            existing.display_name.clone_from(&candidate.display_name);
            changed = true;
        }
        if candidate.badge_color.is_some() && existing.badge_color != candidate.badge_color {
            existing.badge_color.clone_from(&candidate.badge_color);
            changed = true;
        }
        if candidate.repo_icon.is_some() && existing.repo_icon != candidate.repo_icon {
            existing.repo_icon.clone_from(&candidate.repo_icon);
            changed = true;
        }
        if candidate.setup_script.is_some() && existing.setup_script != candidate.setup_script {
            existing.setup_script.clone_from(&candidate.setup_script);
            changed = true;
        }
        if candidate.archive_script.is_some() && existing.archive_script != candidate.archive_script
        {
            existing
                .archive_script
                .clone_from(&candidate.archive_script);
            changed = true;
        }
        if candidate.hook_mode.is_some() && existing.hook_mode != candidate.hook_mode {
            existing.hook_mode.clone_from(&candidate.hook_mode);
            changed = true;
        }
        if !candidate.symlink_paths.is_empty() && existing.symlink_paths != candidate.symlink_paths
        {
            existing.symlink_paths.clone_from(&candidate.symlink_paths);
            changed = true;
        }
        if candidate.worktree_base_ref.is_some()
            && existing.worktree_base_ref != candidate.worktree_base_ref
        {
            existing
                .worktree_base_ref
                .clone_from(&candidate.worktree_base_ref);
            changed = true;
        }
        if candidate.worktree_base_path.is_some()
            && existing.worktree_base_path != candidate.worktree_base_path
        {
            existing
                .worktree_base_path
                .clone_from(&candidate.worktree_base_path);
            changed = true;
        }
        if changed {
            summary.projects_updated = summary.projects_updated.saturating_add(1);
        }
        if !summary.items.iter().any(|i| i.path == existing.path) {
            summary.items.push(ImportedProjectSummary {
                id: existing.id.clone(),
                path: existing.path.clone(),
                display_name: existing.display_name.clone(),
                kind: existing.kind.clone(),
                badge_color: existing.badge_color.clone(),
                has_setup_script: existing.setup_script.is_some(),
            });
        }
    } else {
        summary.projects_added = summary.projects_added.saturating_add(1);
        summary.items.push(ImportedProjectSummary {
            id: candidate.id.clone(),
            path: candidate.path.clone(),
            display_name: candidate.display_name.clone(),
            kind: candidate.kind.clone(),
            badge_color: candidate.badge_color.clone(),
            has_setup_script: candidate.setup_script.is_some(),
        });
        store.projects.push(candidate);
    }
}

fn split_worktree_key(raw: &str) -> (Option<String>, String) {
    let raw = raw.trim().strip_prefix("worktree:").unwrap_or(raw.trim());
    let (repo_id, path) = if raw.starts_with('/') || Path::new(raw).is_absolute() {
        (None, raw)
    } else if let Some((repo_id, path)) = raw.split_once("::") {
        (
            (!repo_id.trim().is_empty()).then(|| repo_id.trim().to_owned()),
            path.trim(),
        )
    } else {
        (None, raw)
    };
    (
        repo_id,
        workspace_instance_path(path).unwrap_or(path).to_owned(),
    )
}

/// Orca의 가상 작업 공간 접미사는 경로가 아니다. 일반 파일명의 `::`는 보존한다.
fn workspace_instance_path(raw: &str) -> Option<&str> {
    let (path, id) = raw.trim().rsplit_once("::workspace:")?;
    is_uuid_shape(id).then_some(path)
}

/// 로컬 원본 프로젝트를 확인할 수 있을 때만 삭제된 워크트리 메타를 제외한다.
/// 접근할 수 없는 프로젝트·원격 경로는 부재로 단정하지 않는다.
fn missing_local_worktree(store: &ProjectsStore, repo_id: Option<&str>, path: &str) -> bool {
    store.projects.iter().any(|project| {
        project.host_id.is_none()
            && Some(project.id.as_str()) == repo_id
            && Path::new(&project.path).is_dir()
    }) && fs::metadata(path).is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
}

fn resolve_workspace_key(
    domains: &BTreeMap<String, Json>,
    raw: &str,
) -> Option<(Option<String>, String)> {
    if let Some(id) = raw.trim().strip_prefix("folder:") {
        let folder = domains
            .get("folderWorkspaces")?
            .as_array()?
            .iter()
            .find(|f| f.get("id").and_then(Json::as_str) == Some(id))?;
        let path = folder.get("folderPath")?.as_str()?;
        return Path::new(path)
            .is_absolute()
            .then(|| (None, path.to_owned()));
    }
    let key = raw.trim().strip_prefix("worktree:").unwrap_or(raw.trim());
    for domain in ["worktreeMeta", "worktreeMetaByIdentity"] {
        if domains
            .get(domain)
            .and_then(|m| m.get(key))
            .and_then(|m| m.get("hostId"))
            .and_then(Json::as_str)
            .is_some_and(|id| id != "local")
        {
            return None;
        }
    }
    let (repo_id, path) = split_worktree_key(key);
    Path::new(&path).is_absolute().then_some((repo_id, path))
}

/// 이전 이관에서 접미사를 실제 경로로 저장한 항목도 다음 수동 가져오기에서 복구한다.
fn repair_imported_workspace_paths(store: &mut ProjectsStore) {
    let repair = |raw: &str| {
        workspace_instance_path(raw)
            .filter(|_| !Path::new(raw).exists())
            .map_or_else(|| raw.to_owned(), normalize_import_path_str)
    };
    for project in &mut store.projects {
        if project.host_id.is_some() {
            continue;
        }
        let path = repair(&project.path);
        if path != project.path {
            if project.display_name == basename_str(&project.path) {
                project.display_name = basename_str(&path);
            }
            project.path = path;
        }
    }
    let mut worktrees = BTreeMap::new();
    for (key, meta) in std::mem::take(&mut store.worktrees) {
        worktrees.entry(repair(&key)).or_insert(meta);
    }
    store.worktrees = worktrees;
    let mut lineage = BTreeMap::new();
    for (key, mut meta) in std::mem::take(&mut store.lineage) {
        meta.worktree_path = repair(&meta.worktree_path);
        meta.parent_worktree_path = meta.parent_worktree_path.map(|p| repair(&p));
        lineage.entry(repair(&key)).or_insert(meta);
    }
    store.lineage = lineage;
}

/// 같은 디렉터리를 가리키는 프로젝트 중복(심볼릭 링크 별칭 차이)을 하나로 합친다(OWE-803).
/// 첫 항목을 남기고 뒤 중복을 버린다. 계보·메타는 경로 기준으로도 해소되므로 참조는 유지된다.
fn dedupe_projects_by_normalized_path(store: &mut ProjectsStore) {
    let mut seen: BTreeSet<(Option<String>, PathBuf)> = BTreeSet::new();
    store.projects.retain(|p| {
        seen.insert((
            p.host_id.clone(),
            normalize_import_path(Path::new(p.path.trim())),
        ))
    });
}

/// 이관으로 모은 워크트리 메타(`src`)를 잠금 안에서 다시 읽은 저장본(`dst`)에 덮어쓴다.
fn merge_imported_worktree_meta(dst: &mut WorktreeStore, src: WorktreeStore) {
    dst.comments.extend(src.comments);
    dst.statuses.extend(src.statuses);
    dst.unread.extend(src.unread);
    dst.prs.extend(src.prs);
    dst.issues.extend(src.issues);
    dst.updated_at.extend(src.updated_at);
    for (key, urls) in src.references {
        for url in urls {
            dst.add_reference(&key, &url).ok();
        }
    }
}

fn apply_worktree_meta(
    wt_path: &str,
    repo_id: Option<&str>,
    meta: &OrcaWorktreeMeta,
    store: &mut ProjectsStore,
    wt_store: &mut WorktreeStore,
) {
    let key = worktree_key(Path::new(wt_path));
    let now = now_secs();

    let display_name = meta
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let comment = meta
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let status = meta
        .workspace_status
        .as_deref()
        .and_then(|s| WorkspaceStatus::parse(s).ok())
        .map(|s| s.as_str().to_owned());

    let mut pr = meta.linked_pr;
    let mut issue = meta
        .linked_linear_issue
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| meta.linked_issue.map(|n| format!("#{n}")));

    let mut refs = Vec::new();
    let all_items = meta.linked_work_item.iter().chain(meta.linked_items.iter());
    for item in all_items {
        if pr.is_none()
            && item.kind.as_deref() == Some("pr")
            && let Some(n) = item.number
        {
            pr = Some(n);
        }
        if issue.is_none() && item.kind.as_deref() == Some("issue") {
            if let Some(id_str) = item
                .identifier
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                issue = Some(id_str.to_owned());
            } else if let Some(n) = item.number {
                issue = Some(format!("#{n}"));
            }
        }
        if let Some(url) = item.url.as_deref().map(str::trim).filter(|u| !u.is_empty())
            && !refs.iter().any(|existing| existing == url)
        {
            refs.push(url.to_owned());
        }
    }

    if let Some(ref c) = comment {
        wt_store.comments.insert(key.clone(), c.clone());
        wt_store.updated_at.insert(key.clone(), now);
    }
    if let Some(ref st) = status {
        wt_store.statuses.insert(key.clone(), st.clone());
        wt_store.updated_at.insert(key.clone(), now);
    }
    if meta.is_unread {
        wt_store.unread.insert(key.clone(), true);
    }
    if let Some(pr_num) = pr {
        wt_store.prs.insert(key.clone(), pr_num);
    }
    if let Some(ref iss) = issue {
        wt_store.issues.insert(key.clone(), iss.clone());
    }
    for url in &refs {
        wt_store.add_reference(&key, url).ok();
    }

    // OWE-803: 과거 이관분이 원본 경로(`/tmp/...`) 키로 남아 있으면 정규 키로 교체한다.
    let norm_key = normalize_import_path(Path::new(&key));
    store
        .worktrees
        .retain(|k, _| normalize_import_path(Path::new(k)) != norm_key);
    store.worktrees.insert(
        key,
        StoredWorktreeMeta {
            repo_id: repo_id.map(str::to_owned),
            display_name,
            comment,
            workspace_status: status,
            unread: meta.is_unread,
            pinned: meta.is_pinned,
            archived: meta.is_archived,
            sort_order: meta.sort_order,
            pr,
            issue,
            references: refs,
        },
    );
}

// ============================================================================
// Domain 4: Agent, Workflow & Terminal Settings (`settings`)
// ============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct QuickCmdEntry {
    name: String,
    text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaQuickCommand {
    label: String,
    command: Option<String>,
    prompt: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaAutomation {
    id: String,
    name: String,
    prompt: String,
    agent_id: Option<String>,
    rrule: Option<String>,
    dtstart: Option<Json>,
    timezone: Option<String>,
    enabled: Option<bool>,
    project_id: Option<String>,
    workspace_id: Option<String>,
    run_context: Option<OrcaRunContext>,
    precheck: Option<OrcaAutomationPrecheck>,
    workspace_mode: Option<String>,
    base_branch: Option<String>,
    reuse_session: Option<bool>,
    extra_agent_args: Option<String>,
    missed_run_grace_minutes: Option<u32>,
    bot: Option<OrcaAutomationBot>,
    once: Option<Json>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaAutomationBot {
    bot_id: Option<String>,
    #[serde(alias = "id")]
    id: Option<String>,
    folder: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum OrcaAutomationPrecheck {
    Command(String),
    Detailed {
        command: String,
        #[serde(rename = "timeoutSeconds")]
        timeout_seconds: Option<u64>,
    },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaRunContext {
    path: Option<String>,
    worktree_path: Option<String>,
    cwd: Option<String>,
}

/// Orca `terminalFontFamily` 중 가져올 수 있는 알려진 고정폭 글꼴(OWE-899).
/// CLI에는 글꼴 시스템이 없어 설치 확인이 안 되므로, 이 목록에 없는 이름은 반영하지 않고
/// 건너뛴 사유를 남긴다. 앱 실행 중에는 실제 해석 결과로 Menlo 폴백한다.
const IMPORTABLE_TERMINAL_FONTS: &[&str] = &[
    "Menlo",
    "Monaco",
    "Courier New",
    "Courier",
    "Andale Mono",
    "JetBrains Mono",
    "Fira Code",
    "Fira Mono",
    "Hack",
    "Source Code Pro",
    "IBM Plex Mono",
    "Cascadia Mono",
    "Cascadia Code",
    "Inconsolata",
    "Roboto Mono",
    "Ubuntu Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Consolas",
    "Dank Mono",
    "Operator Mono",
    "Hasklig",
    "Iosevka",
    "Fantasque Sans Mono",
    "Victor Mono",
    "Space Mono",
    "D2Coding",
];

/// 가져온 글꼴 이름을 `settings.toml`에 반영해도 되는지 본다(OWE-899).
#[must_use]
pub fn importable_terminal_font(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && IMPORTABLE_TERMINAL_FONTS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(trimmed))
}

/// 가져오지 않은 터미널 글꼴의 사유(OWE-899). 결과 리포트·경고에 그대로 싣는다.
#[must_use]
pub fn terminal_font_skip_reason(family: &str) -> String {
    let family = family.trim();
    if family.eq_ignore_ascii_case("sf mono") {
        format!(
            "Skipped importing terminal font '{family}': it is a hidden macOS system font \
             that Krill can't resolve as monospaced, keeping the current font"
        )
    } else {
        format!(
            "Skipped importing terminal font '{family}': couldn't confirm it is monospaced, keeping the current font"
        )
    }
}

fn import_settings_domain(
    opts: &ImportOptions,
    domains: &BTreeMap<String, Json>,
    warnings: &mut Vec<String>,
) -> io::Result<SettingsImportSummary> {
    let mut summary = SettingsImportSummary::default();
    if let Some(settings) = domains.get("settings") {
        // 1. `<state_dir>/settings.toml`
        let settings_path = opts.state_dir.join(SETTINGS_FILE);
        let original = read_original(&settings_path)?;
        let mut settings_table: toml::Table = match &original {
            Some(raw) => toml::from_str(raw).map_err(invalid_data)?,
            None => toml::Table::new(),
        };
        let before_settings = settings_table.clone();

        if let Some(raw_agent) = settings.get("defaultTuiAgent").and_then(Json::as_str)
            && let Some(mapped) = map_orca_agent_id(raw_agent)
        {
            settings_table.insert(
                "default_agent".to_owned(),
                toml::Value::String(mapped.to_owned()),
            );
            summary.default_agent = Some(mapped.to_owned());
        }

        if let Some(size) = settings.get("terminalFontSize").and_then(Json::as_u64) {
            let clamped = u8::try_from(size).unwrap_or(13).clamp(10, 24);
            settings_table.insert(
                "terminal_font_size".to_owned(),
                toml::Value::Integer(i64::from(clamped)),
            );
        }

        if let Some(family) = settings
            .get("terminalFontFamily")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            // OWE-899: 고정폭 확인이 안 되는 이름(SF Mono 등)은 셀 계산을 깨뜨리므로
            // 반영하지 않고 사유를 남긴다.
            if importable_terminal_font(family) {
                settings_table.insert(
                    "terminal_font_family".to_owned(),
                    toml::Value::String(family.to_owned()),
                );
            } else {
                let reason = terminal_font_skip_reason(family);
                summary.terminal_font_skipped = Some(reason.clone());
                warnings.push(reason);
            }
        }

        if let Some(cursor) = settings.get("terminalCursorStyle").and_then(Json::as_str) {
            let shape = match cursor.trim().to_ascii_lowercase().as_str() {
                "bar" | "beam" => Some("bar"),
                "underline" => Some("underline"),
                "block" => Some("block"),
                _ => None,
            };
            if let Some(s) = shape {
                settings_table.insert(
                    "terminal_cursor_shape".to_owned(),
                    toml::Value::String(s.to_owned()),
                );
            }
        }

        if let Some(blink) = settings.get("terminalCursorBlink").and_then(Json::as_bool) {
            settings_table.insert(
                "terminal_cursor_blink".to_owned(),
                toml::Value::Boolean(blink),
            );
        }

        if let Some(theme) = settings.get("theme").and_then(Json::as_str) {
            let mapped = match theme.trim().to_ascii_lowercase().as_str() {
                "light" => Some("light"),
                "dark" => Some("dark"),
                "system" => Some("system"),
                _ => None,
            };
            if let Some(t) = mapped {
                settings_table.insert("theme".to_owned(), toml::Value::String(t.to_owned()));
            }
        }

        if let Some(awake) = settings.get("computerAwakeMode").and_then(Json::as_str) {
            let mapped = match awake.trim().to_ascii_lowercase().as_str() {
                "always" | "on" => Some("on"),
                "while-agents-run" | "auto" | "agent" => Some("agent"),
                "never" | "off" => Some("off"),
                _ => None,
            };
            if let Some(a) = mapped {
                settings_table.insert("keep_awake".to_owned(), toml::Value::String(a.to_owned()));
            }
        } else if let Some(keep) = settings
            .get("keepComputerAwakeWhileAgentsRun")
            .and_then(Json::as_bool)
        {
            settings_table.insert(
                "keep_awake".to_owned(),
                toml::Value::String(if keep { "agent" } else { "off" }.to_owned()),
            );
        }

        // Merge `terminalQuickCommands`
        if let Some(arr) = settings.get("terminalQuickCommands")
            && let Ok(orca_cmds) = serde_json::from_value::<Vec<OrcaQuickCommand>>(arr.clone())
        {
            let mut existing_cmds: Vec<QuickCmdEntry> = settings_table
                .get("quick_commands")
                .and_then(|v| v.clone().try_into().ok())
                .unwrap_or_default();

            for qc in orca_cmds {
                let name = qc.label.trim().to_owned();
                let text = qc
                    .command
                    .as_deref()
                    .or(qc.prompt.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                let Some(text) = text else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                if let Some(found) = existing_cmds.iter_mut().find(|c| c.name == name) {
                    if found.text != text {
                        found.text = text;
                        summary.quick_commands_merged =
                            summary.quick_commands_merged.saturating_add(1);
                    }
                } else {
                    existing_cmds.push(QuickCmdEntry { name, text });
                    summary.quick_commands_merged = summary.quick_commands_merged.saturating_add(1);
                }
            }

            if !existing_cmds.is_empty()
                && let Ok(val) = toml::Value::try_from(existing_cmds)
            {
                settings_table.insert("quick_commands".to_owned(), val);
            }
        }

        import_orca_terminal_themes(settings, &mut settings_table);

        if settings_table != before_settings {
            summary.settings_updated = true;
            if !opts.dry_run {
                let text = config_document::save_toml(&settings_path, &settings_table)?;
                write_atomic_0600(&settings_path, text.as_bytes(), original.as_deref())?;
            }
        }

        // 2. `<state_dir>/agents.toml` (`agentCmdOverrides`, `agentDefaultArgs`, `agentDefaultEnv`)
        summary.agents_updated = import_agent_profiles(opts, settings, warnings)?;
    }

    // 3. `<state_dir>/schedules.toml` (`automations`)
    summary.schedules_merged = import_automations(
        opts,
        domains,
        domains.get("settings").unwrap_or(&Json::Null),
        warnings,
    )?;

    Ok(summary)
}

fn import_orca_terminal_themes(settings: &Json, settings_table: &mut toml::Table) {
    const ANSI_KEYS: [&str; 16] = [
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "white",
        "brightBlack",
        "brightRed",
        "brightGreen",
        "brightYellow",
        "brightBlue",
        "brightMagenta",
        "brightCyan",
        "brightWhite",
    ];

    if let Some(dark) = settings
        .get("terminalThemeDark")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        settings_table.insert(
            "terminal_theme_dark".to_owned(),
            toml::Value::String(dark.to_owned()),
        );
    }

    if let Some(light) = settings
        .get("terminalThemeLight")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        settings_table.insert(
            "terminal_theme_light".to_owned(),
            toml::Value::String(light.to_owned()),
        );
    }

    if let Some(use_sep) = settings
        .get("terminalUseSeparateLightTheme")
        .and_then(Json::as_bool)
    {
        settings_table.insert(
            "terminal_use_separate_light_theme".to_owned(),
            toml::Value::Boolean(use_sep),
        );
    }

    let Some(arr) = settings
        .get("terminalCustomThemes")
        .and_then(Json::as_array)
    else {
        return;
    };

    let mut existing: Vec<toml::Table> = settings_table
        .get("terminal_custom_themes")
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(toml::Value::as_table)
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    for entry in arr {
        let Some(id) = entry
            .get("id")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(name) = entry
            .get("name")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let source = entry
            .get("source")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("custom");
        let mode = entry
            .get("mode")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|s| matches!(*s, "dark" | "light"));
        let source_label = entry
            .get("sourceLabel")
            .or_else(|| entry.get("source_label"))
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let colors = entry.get("colors").unwrap_or(entry);
        let Some(bg) = colors
            .get("background")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(fg) = colors
            .get("foreground")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let cursor = colors
            .get("cursor")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(fg);

        let ansi_vals: Vec<toml::Value> = if let Some(ansi_arr) = entry
            .get("ansi")
            .and_then(Json::as_array)
            .filter(|a| a.len() == 16)
        {
            let mapped: Vec<toml::Value> = ansi_arr
                .iter()
                .filter_map(Json::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| toml::Value::String(s.to_owned()))
                .collect();
            if mapped.len() != 16 {
                continue;
            }
            mapped
        } else {
            let mapped: Vec<toml::Value> = ANSI_KEYS
                .iter()
                .filter_map(|k| colors.get(*k).and_then(Json::as_str))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| toml::Value::String(s.to_owned()))
                .collect();
            if mapped.len() != 16 {
                continue;
            }
            mapped
        };

        let mut table = toml::Table::new();
        table.insert("id".to_owned(), toml::Value::String(id.to_owned()));
        table.insert("name".to_owned(), toml::Value::String(name.to_owned()));
        table.insert("source".to_owned(), toml::Value::String(source.to_owned()));
        if let Some(m) = mode {
            table.insert("mode".to_owned(), toml::Value::String(m.to_owned()));
        }
        if let Some(lbl) = source_label {
            table.insert(
                "source_label".to_owned(),
                toml::Value::String(lbl.to_owned()),
            );
        }
        table.insert("foreground".to_owned(), toml::Value::String(fg.to_owned()));
        table.insert("background".to_owned(), toml::Value::String(bg.to_owned()));
        table.insert("cursor".to_owned(), toml::Value::String(cursor.to_owned()));
        table.insert("ansi".to_owned(), toml::Value::Array(ansi_vals));

        if let Some(slot) = existing
            .iter_mut()
            .find(|t| t.get("id").and_then(toml::Value::as_str) == Some(id))
        {
            *slot = table;
        } else {
            existing.push(table);
        }
    }

    if !existing.is_empty() {
        settings_table.insert(
            "terminal_custom_themes".to_owned(),
            toml::Value::Array(existing.into_iter().map(toml::Value::Table).collect()),
        );
    }
}

fn map_orca_agent_id(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "opencode" => Some("opencode"),
        "cursor" | "cursor-agent" => Some("cursor"),
        "agy" | "antigravity" => Some("agy"),
        "grok" | "grok-cli" => Some("grok"),
        "muse" | "muse-cli" => Some("muse"),
        "copilot" | "github-copilot" => Some("copilot"),
        "pi" => Some("pi"),
        "omp" | "oh-my-pi" => Some("omp"),
        "hermes" => Some("hermes"),
        "kimi" | "kimi-cli" => Some("kimi"),
        "qwen" | "qwen-code" => Some("qwen"),
        "droid" => Some("droid"),
        "kiro" | "kiro-cli" => Some("kiro"),
        "aider" => Some("aider"),
        "goose" => Some("goose"),
        "amp" => Some("amp"),
        "mistral-vibe" | "vibe" => Some("mistral-vibe"),
        "crush" => Some("crush"),
        "cline" => Some("cline"),
        "kilo" | "kilocode" => Some("kilo"),
        "continue" | "cn" => Some("continue"),
        "devin" => Some("devin"),
        "trae" | "trae-cli" => Some("trae"),
        "zcode" => Some("zcode"),
        "dsh" | "deepagents" => Some("dsh"),
        "openclaude" | "open-claude" => Some("openclaude"),
        "opencode2" | "open-code-2" => Some("opencode2"),
        "openclaw" | "clawdbot" | "moltbot" => Some("openclaw"),
        _ => None,
    }
}

fn import_agent_profiles(
    opts: &ImportOptions,
    settings: &Json,
    warnings: &mut Vec<String>,
) -> io::Result<usize> {
    let agents_path = opts.state_dir.join(agent::FILE_NAME);
    let existing_raw = read_original(&agents_path)?;
    let loaded = Agents::load(existing_raw.as_deref()).map_err(invalid_data)?;

    let cmd_overrides: BTreeMap<String, String> = settings
        .get("agentCmdOverrides")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let default_args: BTreeMap<String, Vec<String>> = settings
        .get("agentDefaultArgs")
        .and_then(|v| serde_json::from_value::<BTreeMap<String, Json>>(v.clone()).ok())
        .map(|map| {
            map.into_iter()
                .filter_map(|(k, val)| {
                    let parsed = match val {
                        Json::Array(arr) => arr
                            .into_iter()
                            .filter_map(|item| item.as_str().map(str::to_owned))
                            .collect(),
                        Json::String(s) => s.split_whitespace().map(str::to_owned).collect(),
                        _ => return None,
                    };
                    Some((k, parsed))
                })
                .collect()
        })
        .unwrap_or_default();
    let default_env: BTreeMap<String, BTreeMap<String, String>> = settings
        .get("agentDefaultEnv")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let mut touched_ids = BTreeSet::new();
    for k in cmd_overrides
        .keys()
        .chain(default_args.keys())
        .chain(default_env.keys())
    {
        if let Some(canon) = map_orca_agent_id(k) {
            touched_ids.insert((k.clone(), canon));
        }
    }
    if let Some(raw_agent) = settings.get("defaultTuiAgent").and_then(Json::as_str)
        && let Some(canon) = map_orca_agent_id(raw_agent)
        && !loaded.profiles.contains_key(canon)
    {
        touched_ids.insert((raw_agent.to_owned(), canon));
    }

    let mut current_doc = existing_raw.clone();
    let mut updated_count = 0usize;

    for (orca_key, canon_id) in touched_ids {
        let missing_in_user = !loaded.profiles.contains_key(canon_id);
        let Some(base_profile) = loaded.resolve(canon_id).map(|(_, p)| p.clone()) else {
            continue;
        };
        let mut next: AgentProfile = base_profile.clone();

        let prog = cmd_overrides
            .get(&orca_key)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .or_else(|| next.launch.first().cloned())
            .unwrap_or_else(|| canon_id.to_owned());

        if let Some(args) = default_args.get(&orca_key) {
            let clean_args: Vec<String> = args
                .iter()
                .map(|a| a.trim().to_owned())
                .filter(|a| !a.is_empty())
                .collect();
            if !clean_args.is_empty() || cmd_overrides.contains_key(&orca_key) {
                let mut launch = vec![prog];
                launch.extend(clean_args);
                next.launch = launch;
            }
        } else if cmd_overrides.contains_key(&orca_key) {
            if let Some(first) = next.launch.first_mut() {
                *first = prog;
            } else {
                next.launch = vec![prog];
            }
        }

        if let Some(env_map) = default_env.get(&orca_key) {
            for (k, v) in env_map {
                let clean_k = k.trim();
                if !clean_k.is_empty() {
                    next.env.insert(clean_k.to_owned(), v.clone());
                }
            }
        }

        if next != base_profile || missing_in_user {
            match agent::set_user_profile(current_doc.as_deref(), canon_id, &next) {
                Ok(rendered) => {
                    current_doc = Some(rendered);
                    updated_count = updated_count.saturating_add(1);
                }
                Err(e) => {
                    warnings.push(format!("failed to update agent profile {canon_id}: {e}"));
                }
            }
        }
    }

    if !opts.dry_run
        && updated_count > 0
        && let Some(ref text) = current_doc
    {
        write_atomic_0600(&agents_path, text.as_bytes(), existing_raw.as_deref())?;
    }

    Ok(updated_count)
}

fn is_one_shot_automation(auto: &OrcaAutomation) -> bool {
    auto.once
        .as_ref()
        .is_some_and(|v| !matches!(v, Json::Null | Json::Bool(false)))
}

fn import_automations(
    opts: &ImportOptions,
    domains: &BTreeMap<String, Json>,
    settings: &Json,
    warnings: &mut Vec<String>,
) -> io::Result<usize> {
    let raw_automations = domains
        .get("automations")
        .or_else(|| settings.get("automations"));
    let Some(arr) = raw_automations else {
        return Ok(0);
    };
    let Some(rows) = arr.as_array() else {
        return Ok(0);
    };
    let automations = rows
        .iter()
        .filter_map(
            |row| match serde_json::from_value::<OrcaAutomation>(row.clone()) {
                Ok(auto) => Some(auto),
                Err(err) => {
                    warnings.push(format!("skipping malformed automation: {err}"));
                    None
                }
            },
        )
        .filter(|auto| auto.bot.is_none() && !is_one_shot_automation(auto))
        .collect::<Vec<_>>();
    if automations.is_empty() {
        return Ok(0);
    }

    let _guard = if opts.dry_run {
        None
    } else {
        Some(Schedules::exclusive_edit(&opts.state_dir)?)
    };
    let mut schedules = Schedules::load(&opts.state_dir)?;
    let projects = ProjectsStore::load(&opts.state_dir).unwrap_or_default();
    let mut seen_ids = BTreeSet::new();
    let mut merged = 0usize;

    for auto in automations {
        let prompt = auto.prompt.trim().to_owned();
        if prompt.is_empty() {
            continue;
        }
        let id_seed = if auto.id.trim().is_empty() {
            auto.name.trim()
        } else {
            auto.id.trim()
        };
        let mut id = format!("orca-{}", slugify(id_seed));
        if id.len() > 64 {
            id.truncate(64);
        }
        let cron = automation_rrule_to_cron(
            auto.rrule.as_deref(),
            auto.dtstart.as_ref().and_then(Json::as_str),
        );
        let cwd = resolve_automation_cwd(&opts.home, &auto, &projects);
        let command = resolve_automation_command(auto.agent_id.as_deref());
        let existing_entry = schedules.schedule.iter().find(|s| s.id == id);
        let workspace_mode = auto.workspace_mode.as_deref().map_or_else(
            || existing_entry.map_or_else(schedule::WorkspaceMode::default, |s| s.workspace_mode),
            |mode| {
                if mode == "new_per_run" {
                    schedule::WorkspaceMode::Isolate
                } else {
                    schedule::WorkspaceMode::Reuse
                }
            },
        );
        let (precheck, precheck_timeout_secs) = match &auto.precheck {
            Some(OrcaAutomationPrecheck::Command(cmd)) => (Some(cmd.clone()), None),
            Some(OrcaAutomationPrecheck::Detailed {
                command,
                timeout_seconds,
            }) => (Some(command.clone()), *timeout_seconds),
            None => (
                existing_entry.and_then(|s| s.precheck.clone()),
                existing_entry.and_then(|s| s.precheck_timeout_secs),
            ),
        };
        let extra_agent_args = auto
            .extra_agent_args
            .clone()
            .filter(|s| !s.trim().is_empty());
        let candidate = Schedule {
            id: id.clone(),
            name: (!auto.name.trim().is_empty()).then(|| auto.name.clone()),
            cron,
            cwd,
            command,
            prompt,
            agent: auto
                .agent_id
                .as_deref()
                .and_then(map_orca_agent_id)
                .map(str::to_owned),
            precheck: precheck.filter(|s| !s.trim().is_empty()),
            precheck_timeout_secs,
            workspace_mode,
            reuse_session: auto.reuse_session.unwrap_or(false)
                && workspace_mode == schedule::WorkspaceMode::Reuse
                && extra_agent_args.is_none(),
            extra_agent_args,
            base_branch: auto.base_branch.clone().filter(|s| !s.trim().is_empty()),
            missed_run_grace_minutes: auto.missed_run_grace_minutes,
            enabled: auto
                .enabled
                .unwrap_or_else(|| existing_entry.is_none_or(|s| s.enabled)),
        };

        if let Err(reason) = validate_schedule_candidate(&candidate, &mut seen_ids) {
            warnings.push(format!("skipping automation '{}': {reason}", auto.name));
            continue;
        }

        if let Some(idx) = schedules.schedule.iter().position(|s| s.id == id) {
            let Some(existing) = schedules.schedule.get_mut(idx) else {
                continue;
            };
            if *existing != candidate {
                let prev = existing.clone();
                *existing = candidate;
                if toml::to_string(&schedules)
                    .ok()
                    .and_then(|text| Schedules::parse(&text).ok())
                    .is_none()
                {
                    if let Some(slot) = schedules.schedule.get_mut(idx) {
                        *slot = prev;
                    }
                    warnings.push(format!(
                        "skipping automation '{}': schedule file too large",
                        auto.name
                    ));
                    continue;
                }
                merged = merged.saturating_add(1);
            }
        } else if schedules.schedule.len() < MAX_SCHEDULES {
            schedules.schedule.push(candidate);
            if toml::to_string(&schedules)
                .ok()
                .and_then(|text| Schedules::parse(&text).ok())
                .is_none()
            {
                schedules.schedule.pop();
                warnings.push(format!(
                    "skipping automation '{}': schedule file too large",
                    auto.name
                ));
                continue;
            }
            merged = merged.saturating_add(1);
        } else {
            warnings.push(format!(
                "skipping automation '{}': too many schedules",
                auto.name
            ));
        }
    }

    if !opts.dry_run && merged > 0 {
        schedules.save(&opts.state_dir)?;
    }

    Ok(merged)
}

fn validate_schedule_candidate(
    entry: &Schedule,
    seen_ids: &mut BTreeSet<String>,
) -> Result<(), &'static str> {
    if entry.id.is_empty()
        || entry.id.len() > 64
        || !entry
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err("schedule IDs must be unique ASCII letters, digits, _ or -");
    }
    if Cron::parse(&entry.cron).is_err() {
        return Err("invalid cron");
    }
    if !Path::new(&entry.cwd).is_absolute()
        || entry.cwd.contains('\0')
        || entry.command.is_empty()
        || entry.command.first().is_none_or(String::is_empty)
        || entry.command.len() > 128
        || entry
            .command
            .iter()
            .any(|s| s.contains('\0') || s.len() > 8192)
        || entry.prompt.is_empty()
        || entry.prompt.contains('\0')
        || entry.prompt.len() > 32 * 1024
        || entry
            .precheck
            .as_ref()
            .is_some_and(|s| s.trim().is_empty() || s.contains('\0') || s.len() > 8192)
    {
        return Err("invalid schedule cwd, command or prompt");
    }
    if entry.validate().is_err() {
        return Err("invalid schedule settings");
    }
    if !seen_ids.insert(entry.id.clone()) {
        return Err("schedule IDs must be unique ASCII letters, digits, _ or -");
    }
    Ok(())
}

fn resolve_automation_cwd(home: &Path, auto: &OrcaAutomation, projects: &ProjectsStore) -> String {
    if let Some(ctx) = &auto.run_context
        && let Some(p) = ctx
            .path
            .as_deref()
            .or(ctx.worktree_path.as_deref())
            .or(ctx.cwd.as_deref())
            .map(str::trim)
            .filter(|s| Path::new(s).is_absolute())
    {
        return p.to_owned();
    }
    if let Some(ws) = auto
        .workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let (_, path) = split_worktree_key(ws);
        if Path::new(&path).is_absolute() {
            return path;
        }
    }
    if let Some(pid) = auto
        .project_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && let Some(found) = projects.projects.iter().find(|p| p.id == pid)
        && Path::new(&found.path).is_absolute()
    {
        return found.path.clone();
    }
    home.display().to_string()
}

fn resolve_automation_command(agent_id: Option<&str>) -> Vec<String> {
    vec![
        agent_id
            .and_then(map_orca_agent_id)
            .unwrap_or("claude")
            .to_owned(),
    ]
}

/// Routes RRULE strings directly to [`rrule_to_utc_cron`] so `dtstart` wall-clock time is
/// preserved instead of falling back to `schedule::parse_schedule_to_cron`'s hardcoded 09:00.
fn automation_rrule_to_cron(rrule: Option<&str>, dtstart: Option<&str>) -> String {
    if let Some(expr) = rrule {
        let trimmed = expr.trim();
        let is_rrule = trimmed.contains('=')
            || trimmed
                .get(..6)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("RRULE:"));
        if !is_rrule && let Ok(cron) = schedule::parse_schedule_to_cron(trimmed) {
            return cron;
        }
    }
    rrule_to_utc_cron(rrule, dtstart)
}

/// RFC 5545 RRULE(`FREQ=DAILY;BYHOUR=9;BYMINUTE=30` 등)을 Krill 5-필드 UTC cron으로 변환한다.
#[must_use]
pub fn rrule_to_utc_cron(rrule: Option<&str>, dtstart: Option<&str>) -> String {
    let mut freq = "DAILY".to_owned();
    let mut interval = 1u32;
    let mut byminute: Option<u32> = None;
    let mut byhour: Option<u32> = None;
    let mut bymonthday: Option<u32> = None;
    let mut byday: Vec<u8> = Vec::new();

    if let Some(dt) = dtstart
        && let Some((_, time_part)) = dt.split_once('T')
    {
        let mut parts = time_part.trim_end_matches('Z').split(':');
        if let (Some(h), Some(m)) = (parts.next(), parts.next()) {
            byhour = h.parse::<u32>().ok().filter(|&v| v <= 23);
            byminute = m.parse::<u32>().ok().filter(|&v| v <= 59);
        }
    }

    if let Some(raw) = rrule {
        let body = raw.trim().strip_prefix("RRULE:").unwrap_or(raw.trim());
        for token in body.split(';') {
            let Some((k, v)) = token.split_once('=') else {
                continue;
            };
            match k.trim().to_ascii_uppercase().as_str() {
                "FREQ" => freq = v.trim().to_ascii_uppercase(),
                "INTERVAL" => interval = v.trim().parse::<u32>().unwrap_or(1).max(1),
                "BYMINUTE" => {
                    byminute = v
                        .split(',')
                        .next()
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .filter(|&m| m <= 59);
                }
                "BYHOUR" => {
                    byhour = v
                        .split(',')
                        .next()
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .filter(|&h| h <= 23);
                }
                "BYMONTHDAY" => {
                    bymonthday = v
                        .split(',')
                        .next()
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .filter(|&d| (1..=31).contains(&d));
                }
                "BYDAY" => {
                    byday = v
                        .split(',')
                        .filter_map(|d| match d.trim().to_ascii_uppercase().as_str() {
                            "SU" => Some(0),
                            "MO" => Some(1),
                            "TU" => Some(2),
                            "WE" => Some(3),
                            "TH" => Some(4),
                            "FR" => Some(5),
                            "SA" => Some(6),
                            _ => None,
                        })
                        .collect();
                }
                _ => {}
            }
        }
    }

    let m = byminute.unwrap_or(0);
    let h = byhour.unwrap_or(9);

    let format_byday = || {
        byday
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };

    match freq.as_str() {
        "MINUTELY" => {
            let step = interval.clamp(1, 59);
            if step == 1 {
                "* * * * *".to_owned()
            } else {
                format!("*/{step} * * * *")
            }
        }
        "HOURLY" => {
            let step = interval.clamp(1, 23);
            if step == 1 {
                format!("{m} * * * *")
            } else {
                format!("{m} */{step} * * *")
            }
        }
        "DAILY" => {
            if byday.is_empty() {
                format!("{m} {h} * * *")
            } else {
                format!("{m} {h} * * {}", format_byday())
            }
        }
        "WEEKLY" => {
            let days = if byday.is_empty() {
                "1".to_owned()
            } else {
                format_byday()
            };
            format!("{m} {h} * * {days}")
        }
        "MONTHLY" => {
            let dom = bymonthday.unwrap_or(1);
            format!("{m} {h} {dom} * *")
        }
        _ => format!("{m} {h} * * *"),
    }
}

// ============================================================================
// Domain 6: Bots (`bots`) & External Bot Discovery (Orca / OpenClaw / Hermes / OpenBot)
// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
// ============================================================================

const BOTS_DIR: &str = "bots";
const BOTS_INDEX_FILE: &str = "bots.json";
const MAX_BOTS_PER_APP: usize = 24;
const MAX_BOT_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_BOT_COPY_FILES: usize = 128;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaBotMember {
    id: String,
    name: String,
    source: Option<String>,
    workspace_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaBotNotifications {
    needs_permission: Option<bool>,
    task_complete: Option<bool>,
    could_not_finish: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaBotLaunch {
    model: Option<String>,
    effort: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaLookAround {
    on: Option<bool>,
    every: Option<String>,
    weekdays_only: Option<bool>,
    focus: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaTidyUp {
    on: Option<bool>,
    every: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaSlackApp {
    owner_user_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaLinearWorkspace {
    workspace_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaBotRecord {
    id: String,
    name: String,
    #[serde(alias = "agentId", alias = "agent_id")]
    agent: Option<String>,
    folder: Option<String>,
    description: Option<String>,
    emoji: Option<String>,
    avatar: Option<String>,
    avatar_color: Option<String>,
    paused: Option<bool>,
    first_run: Option<bool>,
    first_run_completed: Option<bool>,
    permission_mode: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    launch: Option<OrcaBotLaunch>,
    template_id: Option<String>,
    notifications: Option<OrcaBotNotifications>,
    members: Vec<OrcaBotMember>,
    #[serde(alias = "skills")]
    skill_projects: Vec<String>,
    look_around: Option<OrcaLookAround>,
    tidy_up: Option<OrcaTidyUp>,
    slack: Option<OrcaSlackApp>,
    slack_apps: Vec<OrcaSlackApp>,
    slack_mirror: Option<bool>,
    linear: Option<OrcaLinearWorkspace>,
    linear_workspaces: Vec<OrcaLinearWorkspace>,
    github: Option<Json>,
    google: Option<bool>,
    created_at: Option<u64>,
    updated_at: Option<u64>,
}

const IMPORTED_SOURCES_FILE: &str = "imported_sources.json";
const GROUPS_FILE: &str = "groups.json";
const SUGGESTIONS_FILE: &str = "suggestions.json";
const BOT_ITEMS_DB_FILE: &str = "bot-items.db";

struct PendingNewBot {
    bot_idx: usize,
    bot_dir: PathBuf,
    soul: Option<String>,
    memory: Option<String>,
    memory_topics: Vec<(String, PathBuf)>,
}

struct CreatedBotHome {
    path: PathBuf,
    existed_before: bool,
    bot_id: BotId,
}

#[derive(Default)]
struct BotsIndexCommit {
    committed: bool,
    bots_added: usize,
    bots_skipped: usize,
    schedules_migrated: usize,
    items: Vec<ImportedBotSummary>,
    bot_id_map: BTreeMap<String, BotId>,
    existing_bots: Vec<BotConfig>,
    ledger_updates: BTreeMap<String, String>,
}

fn bot_home_owned_by_or_empty(home: &Path, bot_id: &str) -> bool {
    let Ok(meta) = fs::symlink_metadata(home) else {
        return false;
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return false;
    }
    let manifest = home.join("bot.json");
    match fs::read_to_string(&manifest) {
        Ok(raw) => serde_json::from_str::<Json>(&raw)
            .ok()
            .and_then(|v| v.get("id").and_then(Json::as_str).map(str::to_owned))
            .is_some_and(|existing_id| existing_id == bot_id),
        Err(err) if err.kind() == io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

fn rollback_created_bot_homes(created_homes: Vec<CreatedBotHome>) {
    for home in created_homes.into_iter().rev() {
        if !bot_home_owned_by_or_empty(&home.path, home.bot_id.as_str()) {
            continue;
        }
        if home.existed_before {
            fs::remove_file(home.path.join("bot.json")).ok();
        } else {
            fs::remove_dir_all(&home.path).ok();
        }
    }
}

fn sync_bots_index(
    opts: &ImportOptions,
    bots_root: &Path,
    domains: &BTreeMap<String, Json>,
    warnings: &mut Vec<String>,
) -> io::Result<BotsIndexCommit> {
    let index_path = bots_root.join(BOTS_INDEX_FILE);
    let original = read_original(&index_path)?;
    let mut existing_bots: Vec<BotConfig> = match &original {
        Some(raw) => serde_json::from_str::<Vec<BotConfig>>(raw).map_err(invalid_data)?,
        None => Vec::new(),
    };

    let ledger_path = bots_root.join(IMPORTED_SOURCES_FILE);
    let imported_ledger: BTreeMap<String, String> = match read_original(&ledger_path)? {
        Some(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        None => BTreeMap::new(),
    };

    let db_records = load_orca_db_bot_records(domains);
    let source_dirs = collect_orca_bot_dirs(&opts.home, &opts.orca_dir);

    let mut seen_source_paths: BTreeSet<PathBuf> = BTreeSet::new();
    let mut bots_modified = false;
    let mut bots_added = 0usize;
    let mut bots_skipped = 0usize;
    let mut items = Vec::new();
    let mut bot_id_map: BTreeMap<String, BotId> = BTreeMap::new();
    for b in &existing_bots {
        bot_id_map.insert(b.id.as_str().to_owned(), b.id.clone());
        bot_id_map.insert(b.slug.as_str().to_owned(), b.id.clone());
    }
    let mut ledger_updates: BTreeMap<String, String> = BTreeMap::new();
    let mut pending_new_bots: Vec<PendingNewBot> = Vec::new();
    let mut new_bot_slugs: BTreeSet<String> = BTreeSet::new();
    let now_ms = now_secs().saturating_mul(1000);

    for bot_dir in source_dirs {
        let norm_dir = normalize_import_path(&bot_dir);
        if !seen_source_paths.insert(norm_dir.clone()) {
            continue;
        }
        let canonical_src = krill_proto::os::canonicalize(&bot_dir).map_or_else(
            |_| norm_dir.to_string_lossy().into_owned(),
            |p| p.to_string_lossy().into_owned(),
        );
        let Some(folder_name) = bot_dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let mut disk_rec = read_orca_bot_json(&bot_dir, warnings);
        if disk_rec.is_none()
            && !bot_dir.join("SOUL.md").is_file()
            && !bot_dir.join("MEMORY.md").is_file()
        {
            continue;
        }

        // Enrich on-disk `bot.json` with profile-state DB metadata when available.
        let db_match = db_records.iter().find(|r| {
            disk_rec
                .as_ref()
                .is_some_and(|d| !d.id.trim().is_empty() && d.id == r.id)
                || r.folder.as_deref() == Some(folder_name)
        });
        if disk_rec.is_none() {
            disk_rec = db_match.cloned();
        }

        let soul = read_bounded_utf8(&bot_dir.join("SOUL.md"));
        let memory = read_bounded_utf8(&bot_dir.join("MEMORY.md"));
        let memory_topics = list_memory_topic_files(&bot_dir.join("memory"));

        let mut config = match build_krill_bot_config(
            folder_name,
            disk_rec.as_ref(),
            db_match,
            soul.as_deref(),
            now_ms,
        ) {
            Ok(cfg) => cfg,
            Err(err) => {
                warnings.push(format!("skipping Orca bot '{folder_name}': {err}"));
                continue;
            }
        };

        let orca_orig_id = config.id.as_str().to_owned();
        let matched_existing = imported_ledger
            .get(&canonical_src)
            .and_then(|id_str| existing_bots.iter().find(|b| b.id.as_str() == id_str))
            .or_else(|| existing_bots.iter().find(|b| b.id == config.id))
            .cloned();

        let already_exists = matched_existing.is_some();
        if let Some(ref existing) = matched_existing {
            bot_id_map.insert(orca_orig_id, existing.id.clone());
            bot_id_map.insert(folder_name.to_owned(), existing.id.clone());
            if imported_ledger.get(&canonical_src).map(String::as_str) != Some(existing.id.as_str())
            {
                ledger_updates.insert(canonical_src.clone(), existing.id.as_str().to_owned());
            }
        } else {
            // Allocate non-colliding slug and ID across existing + intra-run bots.
            let unique_slug = allocate_unique_bot_slug(&existing_bots, bots_root, &config.slug);
            config.slug = unique_slug.clone();
            unique_slug.as_str().clone_into(&mut config.folder);
            if existing_bots.iter().any(|b| b.id == config.id) {
                config.id = allocate_unique_bot_id(&existing_bots, config.slug.as_str())
                    .map_err(invalid_data)?;
            }
            bot_id_map.insert(orca_orig_id, config.id.clone());
            bot_id_map.insert(folder_name.to_owned(), config.id.clone());
            ledger_updates.insert(canonical_src, config.id.as_str().to_owned());
        }

        let item = ImportedBotSummary {
            id: matched_existing.as_ref().map_or_else(
                || config.id.as_str().to_owned(),
                |b| b.id.as_str().to_owned(),
            ),
            slug: matched_existing.as_ref().map_or_else(
                || config.slug.as_str().to_owned(),
                |b| b.slug.as_str().to_owned(),
            ),
            name: config.name.clone(),
            agent_id: config.agent_id.clone(),
            source_path: bot_dir.display().to_string(),
            has_soul: soul.is_some(),
            has_memory: memory.is_some(),
            memory_topic_count: memory_topics.len(),
            already_exists,
        };

        if already_exists {
            bots_skipped = bots_skipped.saturating_add(1);
            items.push(item);
            continue;
        }

        let bot_idx = existing_bots.len();
        new_bot_slugs.insert(config.slug.as_str().to_owned());
        existing_bots.push(config);
        pending_new_bots.push(PendingNewBot {
            bot_idx,
            bot_dir,
            soul,
            memory,
            memory_topics,
        });
        bots_modified = true;
        bots_added = bots_added.saturating_add(1);
        items.push(item);
    }

    // Migrate bot-scoped recurring automations into matching bots' `BotConfig.schedules` in memory
    // before touching disk, so a CAS conflict on `bots.json` never leaves partial home edits behind.
    let (schedules_migrated, modified_bot_slugs) =
        migrate_bot_scoped_automations(domains, &mut existing_bots, &bot_id_map, warnings);
    if schedules_migrated > 0 {
        bots_modified = true;
    }

    if !opts.dry_run && bots_modified {
        let mut created_homes: Vec<CreatedBotHome> = Vec::new();
        let commit_res: io::Result<()> = (|| {
            ensure_dir_0700(bots_root)?;
            for pending in &pending_new_bots {
                let Some(cfg) = existing_bots.get(pending.bot_idx) else {
                    continue;
                };
                let target_home = bots_root.join(cfg.slug.as_str());
                let existed_before = fs::symlink_metadata(&target_home).is_ok();
                if !existed_before {
                    created_homes.push(CreatedBotHome {
                        path: target_home,
                        existed_before: false,
                        bot_id: cfg.id.clone(),
                    });
                }
                let created_home = init_bot_home(bots_root, cfg, pending.soul.as_deref())?;
                if existed_before {
                    created_homes.push(CreatedBotHome {
                        path: created_home.clone(),
                        existed_before: true,
                        bot_id: cfg.id.clone(),
                    });
                }
                if let Some(ref mem_text) = pending.memory {
                    let mem_path = created_home.join("MEMORY.md");
                    krill_proto::os::write_atomic_0600(&mem_path, mem_text.as_bytes())?;
                }
                for (topic_name, topic_path) in &pending.memory_topics {
                    if let Some(content) = read_bounded_utf8(topic_path) {
                        let dst = created_home.join("memory").join(topic_name);
                        if !dst.exists() {
                            if let Some(parent) = dst.parent() {
                                ensure_dir_0700(parent)?;
                            }
                            krill_proto::os::write_atomic_0600(&dst, content.as_bytes())?;
                        }
                    }
                }
                let src_skills = pending.bot_dir.join("skills");
                if src_skills.is_dir() {
                    copy_safe_text_tree(&src_skills, &created_home.join("skills"), warnings);
                }
                let src_avatar = pending.bot_dir.join("avatar.png");
                if fs::symlink_metadata(&src_avatar).is_ok_and(|m| m.is_file())
                    && let Ok(raw_png) = fs::read(&src_avatar)
                {
                    match strip_png_metadata(&raw_png) {
                        Ok(clean_png) => {
                            krill_proto::os::write_atomic_0600(
                                &created_home.join("avatar.png"),
                                &clean_png,
                            )?;
                        }
                        Err(err) => {
                            warnings.push(format!(
                                "skipped invalid avatar.png for bot '{}': {err}",
                                cfg.slug
                            ));
                        }
                    }
                }
            }
            let mut rendered =
                serde_json::to_string_pretty(&existing_bots).map_err(invalid_data)?;
            rendered.push('\n');
            write_atomic_0600(&index_path, rendered.as_bytes(), original.as_deref())?;
            Ok(())
        })();
        if let Err(err) = commit_res {
            rollback_created_bot_homes(created_homes);
            return Err(err);
        }

        // Persist updated per-bot `bot.json` for pre-existing bots whose schedules changed,
        // now that `bots.json` CAS has succeeded.
        for b in &existing_bots {
            if modified_bot_slugs.contains(b.slug.as_str())
                && !new_bot_slugs.contains(b.slug.as_str())
            {
                let bot_home = bots_root.join(b.slug.as_str());
                if bot_home.is_dir() {
                    let mut rendered = serde_json::to_string_pretty(b).map_err(invalid_data)?;
                    rendered.push('\n');
                    krill_proto::os::write_atomic_0600(
                        &bot_home.join("bot.json"),
                        rendered.as_bytes(),
                    )?;
                }
            }
        }
    }

    Ok(BotsIndexCommit {
        committed: true,
        bots_added,
        bots_skipped,
        schedules_migrated,
        items,
        bot_id_map,
        existing_bots,
        ledger_updates,
    })
}

fn sync_bots_ledger(bots_root: &Path, updates: &BTreeMap<String, String>) -> io::Result<()> {
    let ledger_path = bots_root.join(IMPORTED_SOURCES_FILE);
    let original_ledger = read_original(&ledger_path)?;
    let mut imported_ledger: BTreeMap<String, String> = match &original_ledger {
        Some(raw) => serde_json::from_str(raw).unwrap_or_default(),
        None => BTreeMap::new(),
    };
    let mut modified = false;
    for (src, id) in updates {
        if imported_ledger.get(src).map(String::as_str) != Some(id.as_str()) {
            imported_ledger.insert(src.clone(), id.clone());
            modified = true;
        }
    }
    if modified {
        ensure_dir_0700(bots_root)?;
        let mut rendered = serde_json::to_string_pretty(&imported_ledger).map_err(invalid_data)?;
        rendered.push('\n');
        write_atomic_0600(
            &ledger_path,
            rendered.as_bytes(),
            original_ledger.as_deref(),
        )?;
    }
    Ok(())
}

fn import_bots_domain(
    opts: &ImportOptions,
    profile_id: &str,
    domains: &BTreeMap<String, Json>,
    warnings: &mut Vec<String>,
) -> io::Result<BotsImportSummary> {
    let bots_root = opts.state_dir.join(BOTS_DIR);
    let bots_step = retry_on_conflict("bots", warnings, |w| {
        sync_bots_index(opts, &bots_root, domains, w)
    })?;
    if !bots_step.committed {
        return Ok(BotsImportSummary::default());
    }

    if !opts.dry_run && !bots_step.ledger_updates.is_empty() {
        retry_on_conflict("bots ledger", warnings, |_w| {
            sync_bots_ledger(&bots_root, &bots_step.ledger_updates)
        })?;
    }

    let mut summary = BotsImportSummary {
        bots_added: bots_step.bots_added,
        bots_skipped: bots_step.bots_skipped,
        schedules_migrated: bots_step.schedules_migrated,
        items: bots_step.items,
        ..BotsImportSummary::default()
    };

    // Migrate Orca bot items, groups, and look-around suggestions idempotently,
    // retrying each independent JSON store separately on concurrent-write conflict.
    summary.items_migrated = import_orca_bot_items(
        opts,
        profile_id,
        &bots_root,
        &bots_step.bot_id_map,
        &bots_step.existing_bots,
        warnings,
    )?;
    summary.groups_migrated = retry_on_conflict("bots groups", warnings, |w| {
        import_orca_bot_groups(
            opts,
            profile_id,
            &bots_root,
            &bots_step.bot_id_map,
            &bots_step.existing_bots,
            w,
        )
    })?;
    summary.suggestions_migrated = retry_on_conflict("bots suggestions", warnings, |w| {
        import_orca_look_around_suggestions(
            opts,
            &bots_root,
            &bots_step.bot_id_map,
            &bots_step.existing_bots,
            w,
        )
    })?;

    // Also discover non-Orca external bot candidates (OpenClaw, Hermes, OpenBot) read-only.
    let mut external = discover_external_bots_filtered(&opts.home, None, None, |key| {
        std::env::var(key).ok().filter(|v| !v.trim().is_empty())
    });
    external.retain(|c| c.app != ExternalBotSourceApp::Orca);
    summary.external_candidates = external;

    Ok(summary)
}

fn allocate_unique_bot_slug(existing: &[BotConfig], bots_root: &Path, base: &BotSlug) -> BotSlug {
    let taken = |cand: &str| {
        existing
            .iter()
            .any(|b| b.slug.as_str() == cand || b.folder == cand)
            || fs::symlink_metadata(bots_root.join(cand)).is_ok()
    };
    if !taken(base.as_str()) {
        return base.clone();
    }
    for n in 2..=9999_u32 {
        let candidate = format!("{}-{n}", base.as_str());
        if validate_bot_slug(&candidate).is_ok()
            && let Ok(slug) = BotSlug::new(&candidate)
            && !taken(slug.as_str())
        {
            return slug;
        }
    }
    base.clone()
}

fn allocate_unique_bot_id(existing: &[BotConfig], base_slug: &str) -> Result<BotId, String> {
    let first = format!("bot-{base_slug}");
    if let Ok(id) = BotId::new(&first)
        && !existing.iter().any(|b| b.id == id)
    {
        return Ok(id);
    }
    for n in 2..=9999_u32 {
        let cand = format!("bot-{base_slug}-{n}");
        if let Ok(id) = BotId::new(&cand)
            && !existing.iter().any(|b| b.id == id)
        {
            return Ok(id);
        }
    }
    Err("unable to allocate unique bot id".to_owned())
}

fn load_orca_db_bot_records(domains: &BTreeMap<String, Json>) -> Vec<OrcaBotRecord> {
    let Some(arr) = domains.get("bots").and_then(Json::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| serde_json::from_value::<OrcaBotRecord>(v.clone()).ok())
        .collect()
}

fn collect_orca_bot_dirs(home: &Path, orca_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![
        home.join(".orca").join(BOTS_DIR),
        orca_dir.join(BOTS_DIR),
        home.join("Library")
            .join("Application Support")
            .join("Orca")
            .join(BOTS_DIR),
        home.join("Library")
            .join("Application Support")
            .join("orca")
            .join(BOTS_DIR),
    ];
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
        let base = expand_tilde(home, &appdata.to_string_lossy());
        roots.push(base.join("Orca").join(BOTS_DIR));
        roots.push(base.join("orca").join(BOTS_DIR));
    } else {
        let fallback = home.join("AppData").join("Roaming");
        roots.push(fallback.join("Orca").join(BOTS_DIR));
        roots.push(fallback.join("orca").join(BOTS_DIR));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        let base = expand_tilde(home, &xdg.to_string_lossy());
        roots.push(base.join("Orca").join(BOTS_DIR));
        roots.push(base.join("orca").join(BOTS_DIR));
    }

    let mut out = Vec::new();
    let mut seen_roots = BTreeSet::new();
    for root in roots {
        let root_key = krill_proto::os::canonicalize(&root).unwrap_or_else(|_| root.clone());
        if !seen_roots.insert(root_key) {
            continue;
        }
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                if name.starts_with('.') {
                    return None;
                }
                let path = e.path();
                let meta = fs::symlink_metadata(&path).ok()?;
                (!meta.file_type().is_symlink() && meta.is_dir()).then_some(path)
            })
            .collect();
        children.sort();
        out.extend(children.into_iter().take(MAX_BOTS_PER_APP));
    }
    out
}

fn read_orca_bot_json(bot_dir: &Path, warnings: &mut Vec<String>) -> Option<OrcaBotRecord> {
    let path = bot_dir.join("bot.json");
    let raw = read_bounded_utf8(&path)?;
    match serde_json::from_str::<OrcaBotRecord>(&raw) {
        Ok(rec) => Some(rec),
        Err(err) => {
            warnings.push(format!("ignoring malformed {}: {err}", path.display()));
            None
        }
    }
}

fn read_bounded_utf8(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_BOT_FILE_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn list_memory_topic_files(memory_dir: &Path) -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<(String, PathBuf)>) {
        if depth > 2 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if meta.is_dir() {
                if depth < 2 {
                    walk(&path, &rel, depth.saturating_add(1), out);
                }
            } else if meta.is_file()
                && Path::new(&name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            {
                out.push((rel, path));
            }
        }
    }

    let mut out = Vec::new();
    walk(memory_dir, "", 0, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn copy_safe_text_tree(src_root: &Path, dst_root: &Path, warnings: &mut Vec<String>) {
    let mut copied = 0usize;
    copy_safe_text_tree_inner(src_root, dst_root, 0, &mut copied, warnings);
}

fn copy_safe_text_tree_inner(
    src_dir: &Path,
    dst_dir: &Path,
    depth: usize,
    copied: &mut usize,
    warnings: &mut Vec<String>,
) {
    if depth > 6 || *copied >= MAX_BOT_COPY_FILES {
        return;
    }
    let Ok(entries) = fs::read_dir(src_dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if *copied >= MAX_BOT_COPY_FILES {
            break;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let src_path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&src_path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        let dst_path = dst_dir.join(&name);
        if meta.is_dir() {
            copy_safe_text_tree_inner(
                &src_path,
                &dst_path,
                depth.saturating_add(1),
                copied,
                warnings,
            );
        } else if meta.is_file() && meta.len() <= MAX_BOT_FILE_BYTES {
            if dst_path.exists() {
                continue;
            }
            if let Ok(bytes) = fs::read(&src_path) {
                if ensure_dir_0700(dst_dir).is_ok()
                    && krill_proto::os::write_atomic_0600(&dst_path, &bytes).is_ok()
                {
                    *copied = copied.saturating_add(1);
                } else {
                    warnings.push(format!("failed to copy skill file {}", src_path.display()));
                }
            }
        }
    }
}

fn build_krill_bot_config(
    folder_name: &str,
    disk: Option<&OrcaBotRecord>,
    db: Option<&OrcaBotRecord>,
    soul: Option<&str>,
    now_ms: u64,
) -> Result<BotConfig, String> {
    let raw_name = disk
        .map(|r| r.name.trim())
        .filter(|s| !s.is_empty())
        .or_else(|| db.map(|r| r.name.trim()).filter(|s| !s.is_empty()))
        .map_or_else(|| bot_name_from_id(folder_name), str::to_owned);

    let slug = validate_bot_slug(folder_name)
        .ok()
        .and_then(|s| BotSlug::new(s).ok())
        .unwrap_or_else(|| bot_slug_from_name(&raw_name));

    let raw_id = disk
        .map(|r| r.id.trim())
        .filter(|s| !s.is_empty())
        .or_else(|| db.map(|r| r.id.trim()).filter(|s| !s.is_empty()));
    let id = raw_id
        .and_then(|id| validate_bot_id(id).ok())
        .and_then(|id| BotId::new(id).ok())
        .or_else(|| BotId::new(format!("orca-{}", slug.as_str())).ok())
        .ok_or_else(|| "unable to derive valid bot id".to_owned())?;

    let raw_agent = disk
        .and_then(|r| r.agent.as_deref())
        .or_else(|| db.and_then(|r| r.agent.as_deref()))
        .unwrap_or("claude");
    let agent_id = map_orca_agent_id(raw_agent).unwrap_or("claude");

    let created_at = db
        .and_then(|r| r.created_at)
        .or_else(|| disk.and_then(|r| r.created_at))
        .unwrap_or(now_ms);
    let updated_at = db
        .and_then(|r| r.updated_at)
        .or_else(|| disk.and_then(|r| r.updated_at))
        .unwrap_or(now_ms);

    let mut cfg = BotConfig::new(id, &raw_name, agent_id, created_at).map_err(|e| e.to_string())?;
    cfg.slug = slug.clone();
    slug.as_str().clone_into(&mut cfg.folder);
    cfg.updated_at_ms = updated_at;

    let description = disk
        .and_then(|r| r.description.as_deref())
        .or_else(|| db.and_then(|r| r.description.as_deref()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| soul.and_then(soul_description))
        .unwrap_or_default();
    cfg.description = description;

    cfg.emoji = disk
        .and_then(|r| r.emoji.clone())
        .or_else(|| db.and_then(|r| r.emoji.clone()));
    cfg.avatar = disk
        .and_then(|r| r.avatar.clone())
        .or_else(|| db.and_then(|r| r.avatar.clone()));
    cfg.avatar_color = disk
        .and_then(|r| r.avatar_color.clone())
        .or_else(|| db.and_then(|r| r.avatar_color.clone()));
    cfg.model = disk
        .and_then(|r| {
            r.model
                .clone()
                .or_else(|| r.launch.as_ref().and_then(|l| l.model.clone()))
        })
        .or_else(|| {
            db.and_then(|r| {
                r.model
                    .clone()
                    .or_else(|| r.launch.as_ref().and_then(|l| l.model.clone()))
            })
        });
    cfg.effort = disk
        .and_then(|r| {
            r.effort
                .clone()
                .or_else(|| r.launch.as_ref().and_then(|l| l.effort.clone()))
        })
        .or_else(|| {
            db.and_then(|r| {
                r.effort
                    .clone()
                    .or_else(|| r.launch.as_ref().and_then(|l| l.effort.clone()))
            })
        });
    cfg.template_id = disk
        .and_then(|r| r.template_id.clone())
        .or_else(|| db.and_then(|r| r.template_id.clone()));
    cfg.paused = db
        .and_then(|r| r.paused)
        .or_else(|| disk.and_then(|r| r.paused))
        .unwrap_or(false);
    cfg.first_run_completed = match db
        .and_then(|r| r.first_run_completed)
        .or_else(|| disk.and_then(|r| r.first_run_completed))
    {
        Some(v) => v,
        None => match db
            .and_then(|r| r.first_run)
            .or_else(|| disk.and_then(|r| r.first_run))
        {
            Some(first_run) => !first_run,
            None => soul.and_then(soul_description).is_some(),
        },
    };

    if let Some(la) = db
        .and_then(|r| r.look_around.as_ref())
        .or_else(|| disk.and_then(|r| r.look_around.as_ref()))
    {
        if let Some(on) = la.on {
            cfg.look_around.enabled = on;
        }
        if let Some(ref every) = la.every {
            cfg.look_around.interval_secs = match every.trim().to_ascii_lowercase().as_str() {
                "every-2h" => 7_200,
                "every-4h" => 14_400,
                "twice-daily" => 43_200,
                "mornings" => 86_400,
                _ => cfg.look_around.interval_secs,
            };
        }
        if let Some(weekdays) = la.weekdays_only {
            cfg.look_around.weekdays_only = weekdays;
        }
        if let Some(ref focus) = la.focus {
            cfg.look_around.focus =
                truncate_utf8_bytes(focus.trim(), BOT_LOOK_AROUND_FOCUS_MAX_BYTES);
        }
    }

    if let Some(tu) = db
        .and_then(|r| r.tidy_up.as_ref())
        .or_else(|| disk.and_then(|r| r.tidy_up.as_ref()))
    {
        if let Some(on) = tu.on {
            cfg.memory_tidy.enabled = on;
        }
        if let Some(ref every) = tu.every {
            cfg.memory_tidy.interval_secs = match every.trim().to_ascii_lowercase().as_str() {
                "week" => 7 * 86_400,
                _ => 86_400,
            };
        }
    }

    if let Some(mirror) = db
        .and_then(|r| r.slack_mirror)
        .or_else(|| disk.and_then(|r| r.slack_mirror))
    {
        cfg.connectors.slack.mirror_desktop = mirror;
    }
    let slack_entries: Vec<&OrcaSlackApp> = [disk, db]
        .into_iter()
        .flatten()
        .flat_map(|r| r.slack.iter().chain(r.slack_apps.iter()))
        .collect();
    if !slack_entries.is_empty() {
        cfg.connectors.slack.enabled = true;
        for entry in slack_entries {
            if let Some(ref uid) = entry.owner_user_id {
                let uid = uid.trim();
                if !uid.is_empty()
                    && !cfg
                        .connectors
                        .slack
                        .allowed_user_ids
                        .iter()
                        .any(|u| u == uid)
                {
                    cfg.connectors.slack.allowed_user_ids.push(uid.to_owned());
                }
            }
        }
    }
    let has_linear = [disk, db]
        .into_iter()
        .flatten()
        .any(|r| r.linear.is_some() || !r.linear_workspaces.is_empty());
    if has_linear {
        cfg.connectors.linear.enabled = true;
    }
    let has_github = [disk, db].into_iter().flatten().any(|r| r.github.is_some());
    if has_github {
        cfg.connectors.github.enabled = true;
    }
    if db
        .and_then(|r| r.google)
        .or_else(|| disk.and_then(|r| r.google))
        == Some(true)
    {
        cfg.connectors.google.enabled = true;
        cfg.connectors.google.services.gmail = true;
        cfg.connectors.google.services.calendar = true;
    }

    if let Some(perm) = db
        .and_then(|r| r.permission_mode.as_deref())
        .or_else(|| disk.and_then(|r| r.permission_mode.as_deref()))
        && matches!(
            perm.trim().to_ascii_lowercase().as_str(),
            "yolo" | "bypass" | "bypasspermissions" | "bypass-permissions"
        )
    {
        cfg.permission_mode = Permission::Yolo;
    }

    if let Some(n) = db
        .and_then(|r| r.notifications.as_ref())
        .or_else(|| disk.and_then(|r| r.notifications.as_ref()))
    {
        if let Some(v) = n.needs_permission {
            cfg.notifications.needs_permission = v;
        }
        if let Some(v) = n.task_complete {
            cfg.notifications.task_complete = v;
        }
        if let Some(v) = n.could_not_finish {
            cfg.notifications.could_not_finish = v;
        }
    }

    let raw_members = disk
        .filter(|r| !r.members.is_empty())
        .or(db)
        .map_or(&[][..], |r| r.members.as_slice());
    for m in raw_members {
        let mid = m.id.trim();
        if mid.is_empty() {
            continue;
        }
        let source = match m
            .source
            .as_deref()
            .unwrap_or("slack")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "slack" => ExternalSource::Slack,
            "telegram" => ExternalSource::Telegram,
            "discord" => ExternalSource::Discord,
            "google" => ExternalSource::Google,
            "github" => ExternalSource::GitHub,
            "linear" => ExternalSource::Linear,
            "composio" => ExternalSource::Composio,
            _ => continue,
        };
        cfg.members.push(BotMember {
            id: mid.to_owned(),
            name: m.name.trim().to_owned(),
            source,
            workspace_id: m
                .workspace_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        });
    }

    let mut skills = Vec::new();
    for rec in [disk, db].into_iter().flatten() {
        for s in &rec.skill_projects {
            let trimmed = s.trim();
            if !trimmed.is_empty() && !skills.iter().any(|e| e == trimmed) {
                skills.push(trimmed.to_owned());
            }
        }
    }
    cfg.skills = skills;

    cfg.validate().map_err(|e| e.to_string())?;
    Ok(cfg)
}

fn truncate_utf8_bytes(s: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if out.len().saturating_add(ch.len_utf8()) > max_bytes {
            break;
        }
        out.push(ch);
    }
    out
}

fn migrate_bot_scoped_automations(
    domains: &BTreeMap<String, Json>,
    existing_bots: &mut [BotConfig],
    bot_id_map: &BTreeMap<String, BotId>,
    warnings: &mut Vec<String>,
) -> (usize, BTreeSet<String>) {
    let settings = domains.get("settings").cloned().unwrap_or(Json::Null);
    let raw_automations = domains
        .get("automations")
        .or_else(|| settings.get("automations"));
    let Some(rows) = raw_automations.and_then(Json::as_array) else {
        return (0, BTreeSet::new());
    };
    let mut added = 0usize;
    let mut modified_bot_slugs = BTreeSet::new();

    for row in rows {
        let Ok(auto) = serde_json::from_value::<OrcaAutomation>(row.clone()) else {
            continue;
        };
        if is_one_shot_automation(&auto) {
            continue;
        }
        let Some(ref bot_ref) = auto.bot else {
            continue;
        };
        let prompt = auto.prompt.trim().to_owned();
        if prompt.is_empty() {
            continue;
        }
        let target_id = bot_ref
            .bot_id
            .as_deref()
            .or(bot_ref.id.as_deref())
            .or(bot_ref.folder.as_deref())
            .and_then(|raw| bot_id_map.get(raw.trim()).cloned());
        let Some(target_bot_id) = target_id else {
            continue;
        };
        let id_seed = if auto.id.trim().is_empty() {
            auto.name.trim()
        } else {
            auto.id.trim()
        };
        let mut sched_id = format!("orca-{}", slugify(id_seed));
        if sched_id.len() > 64 {
            sched_id.truncate(64);
        }
        let cron = automation_rrule_to_cron(
            auto.rrule.as_deref(),
            auto.dtstart.as_ref().and_then(Json::as_str),
        );
        if let Err(err) = Cron::parse(&cron) {
            warnings.push(format!(
                "skipping bot automation '{}': invalid cron '{cron}': {err}",
                auto.name
            ));
            continue;
        }
        let Some(bot) = existing_bots.iter_mut().find(|b| b.id == target_bot_id) else {
            continue;
        };
        if bot.schedules.iter().any(|s| s.id == sched_id) {
            continue;
        }
        bot.schedules.push(BotScheduleConfig {
            id: sched_id,
            name: if auto.name.trim().is_empty() {
                "Imported schedule".to_owned()
            } else {
                auto.name.trim().to_owned()
            },
            cron,
            prompt,
            enabled: auto.enabled.unwrap_or(true),
            timezone: auto
                .timezone
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        });
        modified_bot_slugs.insert(bot.slug.as_str().to_owned());
        added = added.saturating_add(1);
    }

    (added, modified_bot_slugs)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[derive(Default)]
struct OrcaLookAroundFile {
    bots: BTreeMap<String, OrcaLookAroundBotState>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[derive(Default)]
struct OrcaLookAroundBotState {
    suggestions: Vec<OrcaLookAroundSuggestion>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[derive(Default)]
struct OrcaLookAroundSuggestion {
    id: String,
    title: String,
    why: String,
    ask: String,
    link: Option<String>,
    source: Option<String>,
    created_at: u64,
    status: String,
}

fn import_orca_look_around_suggestions(
    opts: &ImportOptions,
    bots_root: &Path,
    bot_id_map: &BTreeMap<String, BotId>,
    existing_bots: &[BotConfig],
    warnings: &mut Vec<String>,
) -> io::Result<usize> {
    let src_path = opts.orca_dir.join("bot-look-around.json");
    let Some(raw) = read_bounded_utf8(&src_path) else {
        return Ok(0);
    };
    let parsed: OrcaLookAroundFile = match serde_json::from_str(&raw) {
        Ok(p) => p,
        Err(err) => {
            warnings.push(format!("ignoring malformed {}: {err}", src_path.display()));
            return Ok(0);
        }
    };
    let dst_path = bots_root.join(SUGGESTIONS_FILE);
    let original = read_original(&dst_path)?;
    let mut existing: Vec<BotSuggestion> = match &original {
        Some(text) => serde_json::from_str(text).unwrap_or_default(),
        None => Vec::new(),
    };
    let mut seen: BTreeSet<(String, String)> = existing
        .iter()
        .map(|s| (s.bot_id.as_str().to_owned(), s.id.clone()))
        .collect();
    let mut added = 0usize;

    for (raw_bot_id, state) in parsed.bots {
        let Some(bot_id) = bot_id_map.get(raw_bot_id.trim()).cloned().or_else(|| {
            BotId::new(raw_bot_id.trim())
                .ok()
                .filter(|id| existing_bots.iter().any(|b| &b.id == id))
        }) else {
            continue;
        };
        for s in state.suggestions {
            let sid = s.id.trim();
            if sid.is_empty() || s.status == "replaced" {
                continue;
            }
            if !seen.insert((bot_id.as_str().to_owned(), sid.to_owned())) {
                continue;
            }
            let source = match s.source.as_deref() {
                Some("slack") => Some(ExternalSource::Slack),
                Some("linear") => Some(ExternalSource::Linear),
                Some("github") => Some(ExternalSource::GitHub),
                Some("google") => Some(ExternalSource::Google),
                Some("telegram") => Some(ExternalSource::Telegram),
                Some("discord") => Some(ExternalSource::Discord),
                Some("composio") => Some(ExternalSource::Composio),
                _ => None,
            };
            existing.push(BotSuggestion {
                id: sid.to_owned(),
                bot_id: bot_id.clone(),
                title: s.title,
                why: s.why,
                ask: s.ask,
                link: s.link,
                source,
                dismissed: s.status == "dismissed",
                created_at_ms: s.created_at,
            });
            added = added.saturating_add(1);
        }
    }

    if !opts.dry_run && added > 0 {
        ensure_dir_0700(bots_root)?;
        let mut rendered = serde_json::to_string_pretty(&existing).map_err(invalid_data)?;
        rendered.push('\n');
        write_atomic_0600(&dst_path, rendered.as_bytes(), original.as_deref())?;
    }
    Ok(added)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum OrcaGroupMemberRef {
    Id(String),
    Obj {
        #[serde(rename = "botId", alias = "id")]
        bot_id: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaBotGroupRecord {
    id: String,
    name: String,
    description: Option<String>,
    members: Vec<OrcaGroupMemberRef>,
    #[serde(alias = "botIds")]
    bot_ids: Vec<String>,
    created_at: Option<u64>,
    updated_at: Option<u64>,
}

fn import_orca_bot_groups(
    opts: &ImportOptions,
    profile_id: &str,
    bots_root: &Path,
    bot_id_map: &BTreeMap<String, BotId>,
    existing_bots: &[BotConfig],
    warnings: &mut Vec<String>,
) -> io::Result<usize> {
    let mut db_candidates = vec![opts.orca_dir.join("orchestration.db")];
    if !profile_id.is_empty() {
        db_candidates.push(
            opts.orca_dir
                .join("profiles")
                .join(profile_id)
                .join("orchestration.db"),
        );
    }

    let dst_path = bots_root.join(GROUPS_FILE);
    let original = read_original(&dst_path)?;
    let mut groups: Vec<BotGroupChat> = match &original {
        Some(raw) => serde_json::from_str(raw).unwrap_or_default(),
        None => Vec::new(),
    };
    let mut seen_ids: BTreeSet<String> = groups.iter().map(|g| g.id.clone()).collect();
    let mut added = 0usize;
    let mut seen_dbs = BTreeSet::new();

    for db_path in db_candidates {
        if !db_path.is_file() {
            continue;
        }
        let key = krill_proto::os::canonicalize(&db_path).unwrap_or_else(|_| db_path.clone());
        if !seen_dbs.insert(key) {
            continue;
        }
        let rows = match run_sqlite3_json_rows(
            &db_path,
            "SELECT id, record FROM bot_groups ORDER BY rowid;",
        ) {
            Ok(r) => r,
            Err(err) => {
                if !err.to_string().contains("no such table") {
                    warnings.push(format!("failed to read {}: {err}", db_path.display()));
                }
                continue;
            }
        };
        for row in rows {
            let row_id = row.get("id").and_then(Json::as_str).unwrap_or("").trim();
            let Some(record_str) = row.get("record").and_then(Json::as_str) else {
                continue;
            };
            let Ok(rec) = serde_json::from_str::<OrcaBotGroupRecord>(record_str) else {
                continue;
            };
            let gid = if rec.id.trim().is_empty() {
                row_id
            } else {
                rec.id.trim()
            };
            if gid.is_empty() || !seen_ids.insert(gid.to_owned()) {
                continue;
            }
            let mut bot_ids = Vec::new();
            let raw_member_ids = rec
                .members
                .iter()
                .filter_map(|m| match m {
                    OrcaGroupMemberRef::Id(s) => Some(s.as_str()),
                    OrcaGroupMemberRef::Obj { bot_id } => bot_id.as_deref(),
                })
                .chain(rec.bot_ids.iter().map(String::as_str));
            for raw_id in raw_member_ids {
                let resolved = bot_id_map.get(raw_id.trim()).cloned().or_else(|| {
                    BotId::new(raw_id.trim())
                        .ok()
                        .filter(|id| existing_bots.iter().any(|b| &b.id == id))
                });
                if let Some(bid) = resolved
                    && !bot_ids.contains(&bid)
                {
                    bot_ids.push(bid);
                }
            }
            if bot_ids.is_empty() {
                continue;
            }
            let created_at_ms = rec
                .created_at
                .unwrap_or_else(|| now_secs().saturating_mul(1000));
            groups.push(BotGroupChat {
                id: gid.to_owned(),
                name: if rec.name.trim().is_empty() {
                    gid.to_owned()
                } else {
                    rec.name.trim().to_owned()
                },
                bot_ids,
                description: rec.description.unwrap_or_default(),
                created_at_ms,
                updated_at_ms: rec.updated_at.unwrap_or(created_at_ms),
            });
            added = added.saturating_add(1);
        }
    }

    if !opts.dry_run && added > 0 {
        ensure_dir_0700(bots_root)?;
        let mut rendered = serde_json::to_string_pretty(&groups).map_err(invalid_data)?;
        rendered.push('\n');
        write_atomic_0600(&dst_path, rendered.as_bytes(), original.as_deref())?;
    }
    Ok(added)
}

struct TempSqlFileGuard(PathBuf);

impl Drop for TempSqlFileGuard {
    fn drop(&mut self) {
        fs::remove_file(&self.0).ok();
    }
}

fn import_orca_bot_items(
    opts: &ImportOptions,
    profile_id: &str,
    bots_root: &Path,
    bot_id_map: &BTreeMap<String, BotId>,
    existing_bots: &[BotConfig],
    warnings: &mut Vec<String>,
) -> io::Result<usize> {
    let mut candidates = vec![opts.orca_dir.join("bot-items.sqlite")];
    if !profile_id.is_empty() {
        candidates.push(
            opts.orca_dir
                .join("profiles")
                .join(profile_id)
                .join("bot-items.sqlite"),
        );
    }
    let Some(src_db) = candidates.into_iter().find(|p| p.is_file()) else {
        return Ok(0);
    };

    let item_rows = match run_sqlite3_json_rows(
        &src_db,
        "SELECT uid, number, owner_bot_id, title, description, done_means, status, close_note, snoozed_until, version, last_activity_at, created_at, updated_at, closed_at FROM items ORDER BY created_at ASC;",
    ) {
        Ok(rows) => rows,
        Err(err) => {
            warnings.push(format!("failed to read {}: {err}", src_db.display()));
            return Ok(0);
        }
    };
    if item_rows.is_empty() {
        return Ok(0);
    }

    let link_rows = run_sqlite3_json_rows(
        &src_db,
        "SELECT item_uid, kind, ref FROM item_links ORDER BY rowid ASC;",
    )
    .unwrap_or_default();
    let event_rows = run_sqlite3_json_rows(
        &src_db,
        "SELECT item_uid, kind, actor, data, created_at FROM item_events ORDER BY id ASC;",
    )
    .unwrap_or_default();

    let dst_db = bots_root.join(BOT_ITEMS_DB_FILE);
    let mut existing_uids = BTreeSet::new();
    if dst_db.is_file() {
        match run_sqlite3_json_rows(&dst_db, "SELECT uid, owner_bot_id, number FROM items;") {
            Ok(existing_rows) => {
                for row in existing_rows {
                    if let Some(uid) = row.get("uid").and_then(Json::as_str) {
                        existing_uids.insert(uid.to_owned());
                    }
                }
            }
            Err(err)
                if err
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("no such table") => {}
            Err(err) => return Err(err),
        }
    }

    let mut pending_items = Vec::new();
    let mut pending_uids = BTreeSet::new();
    let mut resolved_owner_by_uid = BTreeMap::new();

    for row in item_rows {
        let Some(uid) = row
            .get("uid")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if !existing_uids.insert(uid.to_owned()) {
            continue;
        }
        let Some(raw_owner) = row.get("owner_bot_id").and_then(Json::as_str) else {
            continue;
        };
        let Some(owner_bot_id) = bot_id_map.get(raw_owner.trim()).cloned().or_else(|| {
            BotId::new(raw_owner.trim())
                .ok()
                .filter(|id| existing_bots.iter().any(|b| &b.id == id))
        }) else {
            continue;
        };
        let owner_str = owner_bot_id.as_str().to_owned();
        let raw_num = row.get("number").and_then(Json::as_u64).unwrap_or(1);
        let title = row
            .get("title")
            .and_then(Json::as_str)
            .unwrap_or("Untitled")
            .to_owned();
        let status = match row
            .get("status")
            .and_then(Json::as_str)
            .unwrap_or("planned")
        {
            "working" => BotItemStatus::Working.as_str(),
            "waiting" => BotItemStatus::Waiting.as_str(),
            "needs_you" => BotItemStatus::NeedsYou.as_str(),
            "done" => BotItemStatus::Done.as_str(),
            "canceled" => BotItemStatus::Canceled.as_str(),
            _ => BotItemStatus::Planned.as_str(),
        };
        pending_uids.insert(uid.to_owned());
        resolved_owner_by_uid.insert(uid.to_owned(), owner_bot_id);
        pending_items.push((uid.to_owned(), raw_num, owner_str, title, status, row));
    }

    if pending_items.is_empty() {
        return Ok(0);
    }

    let mut imported_count = pending_items.len();

    if !opts.dry_run {
        ensure_dir_0700(bots_root)?;
        let mut sql = String::from(
            "PRAGMA busy_timeout = 5000;\n\
             PRAGMA journal_mode = WAL;\n\
             PRAGMA foreign_keys = ON;\n\
             BEGIN IMMEDIATE;\n\
             CREATE TABLE IF NOT EXISTS items (\n\
                 uid TEXT PRIMARY KEY,\n\
                 number INTEGER NOT NULL,\n\
                 owner_bot_id TEXT NOT NULL,\n\
                 title TEXT NOT NULL,\n\
                 description TEXT,\n\
                 done_means TEXT,\n\
                 status TEXT NOT NULL,\n\
                 close_note TEXT,\n\
                 snoozed_until_ms INTEGER,\n\
                 version INTEGER NOT NULL,\n\
                 last_activity_at_ms INTEGER NOT NULL,\n\
                 created_at_ms INTEGER NOT NULL,\n\
                 updated_at_ms INTEGER NOT NULL,\n\
                 closed_at_ms INTEGER,\n\
                 UNIQUE(owner_bot_id, number)\n\
             );\n\
             CREATE INDEX IF NOT EXISTS idx_items_bot_status ON items(owner_bot_id, status);\n\
             CREATE TABLE IF NOT EXISTS item_links (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 item_uid TEXT NOT NULL REFERENCES items(uid) ON DELETE CASCADE,\n\
                 kind TEXT NOT NULL,\n\
                 target TEXT NOT NULL,\n\
                 UNIQUE(item_uid, kind, target)\n\
             );\n\
             CREATE INDEX IF NOT EXISTS idx_item_links_uid ON item_links(item_uid);\n\
             CREATE TABLE IF NOT EXISTS item_events (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 item_uid TEXT NOT NULL REFERENCES items(uid) ON DELETE CASCADE,\n\
                 kind TEXT NOT NULL,\n\
                 actor_json TEXT NOT NULL,\n\
                 text TEXT NOT NULL,\n\
                 decisions_json TEXT NOT NULL,\n\
                 created_at_ms INTEGER NOT NULL\n\
             );\n\
             CREATE INDEX IF NOT EXISTS idx_item_events_uid ON item_events(item_uid);\n\
             CREATE TEMP TABLE _imported_uids (uid TEXT PRIMARY KEY);\n",
        );

        for (uid, raw_num, owner, title, status, row) in &pending_items {
            let desc = row.get("description").and_then(Json::as_str);
            let done_means = row.get("done_means").and_then(Json::as_str);
            let close_note = row.get("close_note").and_then(Json::as_str);
            let snoozed = row.get("snoozed_until").and_then(Json::as_u64);
            let version = row.get("version").and_then(Json::as_u64).unwrap_or(1);
            let created = row.get("created_at").and_then(Json::as_u64).unwrap_or(0);
            let updated = row
                .get("updated_at")
                .and_then(Json::as_u64)
                .unwrap_or(created);
            let last_act = row
                .get("last_activity_at")
                .and_then(Json::as_u64)
                .unwrap_or(updated);
            let closed = row.get("closed_at").and_then(Json::as_u64);
            let uid_sql = sql_hex_text(uid);
            let owner_sql = sql_hex_text(owner);
            writeln!(
                sql,
                "INSERT INTO _imported_uids (uid) SELECT {uid_sql} WHERE NOT EXISTS (SELECT 1 FROM items WHERE uid = {uid_sql});\n\
                 INSERT INTO items (uid, number, owner_bot_id, title, description, done_means, status, close_note, snoozed_until_ms, version, last_activity_at_ms, created_at_ms, updated_at_ms, closed_at_ms) \
                 SELECT {uid_sql}, \
                 CASE WHEN NOT EXISTS (SELECT 1 FROM items WHERE owner_bot_id = {owner_sql} AND number = {raw_num}) \
                 THEN {raw_num} \
                 ELSE COALESCE((SELECT MAX(number) FROM items WHERE owner_bot_id = {owner_sql}), 0) + 1 END, \
                 {owner_sql}, {}, {}, {}, {}, {}, {}, {version}, {last_act}, {created}, {updated}, {} \
                 WHERE EXISTS (SELECT 1 FROM _imported_uids WHERE uid = {uid_sql});",
                sql_hex_text(title),
                sql_opt_hex_text(desc),
                sql_opt_hex_text(done_means),
                sql_hex_text(status),
                sql_opt_hex_text(close_note),
                sql_opt_u64(snoozed),
                sql_opt_u64(closed),
            )
            .ok();
        }

        for link in &link_rows {
            let Some(uid) = link.get("item_uid").and_then(Json::as_str) else {
                continue;
            };
            if !pending_uids.contains(uid) {
                continue;
            }
            let Some(kind_raw) = link.get("kind").and_then(Json::as_str) else {
                continue;
            };
            let Some(target) = link.get("ref").and_then(Json::as_str) else {
                continue;
            };
            let kind = match kind_raw {
                "dispatch" | "pr" | "url" | "file" => kind_raw,
                _ => continue,
            };
            let uid_sql = sql_hex_text(uid);
            writeln!(
                sql,
                "INSERT OR IGNORE INTO item_links (item_uid, kind, target) SELECT {uid_sql}, {}, {} WHERE EXISTS (SELECT 1 FROM _imported_uids WHERE uid = {uid_sql});",
                sql_hex_text(kind),
                sql_hex_text(target),
            )
            .ok();
        }

        for ev in &event_rows {
            let Some(uid) = ev.get("item_uid").and_then(Json::as_str) else {
                continue;
            };
            if !pending_uids.contains(uid) {
                continue;
            }
            let Some(owner_bot) = resolved_owner_by_uid.get(uid) else {
                continue;
            };
            let kind = match ev.get("kind").and_then(Json::as_str).unwrap_or("commented") {
                "made" => "made",
                "edited" => "edited",
                "status" => "status",
                "linked" => "linked",
                "snoozed" => "snoozed",
                "unsnoozed" => "unsnoozed",
                _ => "commented",
            };
            let actor = translate_orca_item_actor(
                ev.get("actor").and_then(Json::as_str).unwrap_or("{}"),
                bot_id_map,
                owner_bot,
            );
            let actor_json = serde_json::to_string(&actor).map_err(invalid_data)?;
            let (text, decisions) = translate_orca_item_event_data(
                kind,
                ev.get("data").and_then(Json::as_str).unwrap_or("{}"),
            );
            let decisions_json = serde_json::to_string(&decisions).map_err(invalid_data)?;
            let created = ev.get("created_at").and_then(Json::as_u64).unwrap_or(0);
            let uid_sql = sql_hex_text(uid);
            writeln!(
                sql,
                "INSERT INTO item_events (item_uid, kind, actor_json, text, decisions_json, created_at_ms) SELECT {uid_sql}, {}, {}, {}, {}, {created} WHERE EXISTS (SELECT 1 FROM _imported_uids WHERE uid = {uid_sql});",
                sql_hex_text(kind),
                sql_hex_text(&actor_json),
                sql_hex_text(&text),
                sql_hex_text(&decisions_json),
            )
            .ok();
        }

        sql.push_str("SELECT 'IMPORTED_COUNT=' || COUNT(*) FROM _imported_uids;\nCOMMIT;\n");

        let sql_path = bots_root.join(format!(
            ".orca-import-items-{}-{}.sql",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        {
            let mut open = OpenOptions::new();
            open.create_new(true).write(true);
            #[cfg(unix)]
            open.mode(0o600);
            let mut file = open.open(&sql_path)?;
            file.write_all(sql.as_bytes())?;
        }
        let _sql_guard = TempSqlFileGuard(sql_path.clone());
        let sql_stdin = File::open(&sql_path)?;
        let out = Command::new(sqlite3_binary())
            .arg("-bail")
            .arg(&dst_db)
            .stdin(Stdio::from(sql_stdin))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(io::Error::other(format!(
                "sqlite3 write bot-items.db failed ({}): {}",
                out.status,
                stderr.trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        if let Some(count) = stdout
            .lines()
            .rev()
            .find_map(|line| line.trim().strip_prefix("IMPORTED_COUNT="))
            .and_then(|n| n.parse::<usize>().ok())
        {
            imported_count = count;
        }
        #[cfg(unix)]
        {
            fs::set_permissions(&dst_db, fs::Permissions::from_mode(0o600)).ok();
        }
    }

    Ok(imported_count)
}

fn translate_orca_item_actor(
    raw_actor_json: &str,
    bot_id_map: &BTreeMap<String, BotId>,
    fallback_bot: &BotId,
) -> BotItemActor {
    let Ok(val) = serde_json::from_str::<Json>(raw_actor_json) else {
        return BotItemActor::System {
            reason: "orca".to_owned(),
        };
    };
    match val.get("kind").and_then(Json::as_str) {
        Some("owner") => BotItemActor::Owner {
            device: val.get("device").and_then(Json::as_str).map(str::to_owned),
        },
        Some("bot") => {
            let mapped = val
                .get("botId")
                .or_else(|| val.get("bot_id"))
                .and_then(Json::as_str)
                .and_then(|s| {
                    bot_id_map
                        .get(s.trim())
                        .cloned()
                        .or_else(|| BotId::new(s.trim()).ok())
                })
                .unwrap_or_else(|| fallback_bot.clone());
            BotItemActor::Bot {
                bot_id: mapped,
                session_id: None,
            }
        }
        other => BotItemActor::System {
            reason: other.unwrap_or("orca").to_owned(),
        },
    }
}

fn translate_orca_item_event_data(
    kind: &str,
    raw_data_json: &str,
) -> (String, Vec<BotItemDecision>) {
    let Ok(val) = serde_json::from_str::<Json>(raw_data_json) else {
        return (String::new(), Vec::new());
    };
    let text = if let Some(t) = val.get("text").and_then(Json::as_str) {
        t.to_owned()
    } else if kind == "status"
        && let (Some(from), Some(to)) = (
            val.get("from").and_then(Json::as_str),
            val.get("to").and_then(Json::as_str),
        )
    {
        format!("{from} -> {to}")
    } else if kind == "made"
        && let Some(title) = val.get("title").and_then(Json::as_str)
    {
        title.to_owned()
    } else {
        String::new()
    };

    let raw_decisions = val
        .get("ask")
        .and_then(|a| a.get("questions"))
        .or_else(|| val.get("decisions"))
        .and_then(Json::as_array);
    let mut decisions = Vec::new();
    if let Some(arr) = raw_decisions {
        for q in arr {
            let Some(question) = q
                .get("question")
                .and_then(Json::as_str)
                .filter(|s| !s.trim().is_empty())
            else {
                continue;
            };
            let options = q
                .get("options")
                .and_then(Json::as_array)
                .map(|opts| {
                    opts.iter()
                        .filter_map(|o| {
                            o.as_str()
                                .or_else(|| o.get("label").and_then(Json::as_str))
                                .or_else(|| o.get("text").and_then(Json::as_str))
                                .map(str::to_owned)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let recommended = q
                .get("recommended")
                .and_then(Json::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .filter(|&idx| idx < options.len());
            let multi = q.get("multi").and_then(Json::as_bool).unwrap_or(false);
            decisions.push(BotItemDecision {
                question: question.to_owned(),
                options,
                recommended,
                multi,
            });
        }
    }
    (text, decisions)
}

fn sql_hex_text(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len().saturating_mul(2).saturating_add(18));
    out.push_str("CAST(X'");
    for b in s.as_bytes() {
        write!(out, "{b:02X}").ok();
    }
    out.push_str("' AS TEXT)");
    out
}

fn sql_opt_hex_text(s: Option<&str>) -> String {
    s.map_or_else(|| "NULL".to_owned(), sql_hex_text)
}

fn sql_opt_u64(v: Option<u64>) -> String {
    v.map_or_else(|| "NULL".to_owned(), |n| n.to_string())
}

/// Deterministic, environment-independent external bot discovery helper (also used in tests),
/// delegating directly to `krill_proto::bots::external::discover_external_bots_filtered`.
#[cfg(test)]
#[must_use]
pub fn discover_external_bots_in(
    home: &Path,
    openbot_app_data: Option<&Path>,
    openclaw_state_dir: Option<&Path>,
    hermes_home: Option<&Path>,
    filter: Option<ExternalBotSourceApp>,
) -> Vec<ExternalBotCandidate> {
    let appdata_dir = openbot_app_data
        .and_then(|p| p.parent())
        .map(|p| p.to_string_lossy().into_owned());
    discover_external_bots_filtered(home, filter, None, |key| match key {
        "OPENCLAW_STATE_DIR" => openclaw_state_dir.map(|p| p.to_string_lossy().into_owned()),
        "HERMES_HOME" => hermes_home.map(|p| p.to_string_lossy().into_owned()),
        "APPDATA" => appdata_dir.clone(),
        _ => None,
    })
}

// ============================================================================
// Common Helpers
// ============================================================================

#[must_use]
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | ' ' | '.' | '@')
            && !out.ends_with('-')
            && !out.is_empty()
        {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "item".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn bot_name_from_id(id: &str) -> String {
    let words: String = id
        .chars()
        .map(|c| if c == '-' || c == '_' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = words.chars();
    let Some(first) = chars.next() else {
        return id.to_owned();
    };
    let mut out = String::new();
    for upper in first.to_uppercase() {
        out.push(upper);
    }
    out.push_str(chars.as_str());
    out
}

fn basename_str(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_owned()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn ensure_dir_0700(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).ok();
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

/// 임시 파일+rename 원자 쓰기. rename 직전 원본을 다시 읽어 `original`(처음 읽은 내용, 없던 파일은 `None`)과
/// 다르면 덮어쓰지 않고 [`ConfigConflict`]로 실패한다(앱이 그 사이 저장한 내용을 지우지 않는다).
fn write_atomic_0600(path: &Path, bytes: &[u8], original: Option<&str>) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(invalid("path has no parent directory"));
    };
    ensure_dir_0700(parent)?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(
        ".{}.tmp.{}.{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id(),
        seq
    ));
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let res = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        #[cfg(test)]
        tests::before_rename(path);
        if read_original(path)?.as_deref() != original {
            return Err(io::Error::other(ConfigConflict(path.to_path_buf())));
        }
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        fs::remove_file(&tmp).ok();
    }
    res
}

#[cfg(test)]
mod workspace_tests;

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    type RenameHook = Box<dyn FnMut(&Path)>;

    thread_local! {
        /// rename 직전에 불리는 테스트 훅: 앱의 동시 저장을 흉내 낸다.
        static BEFORE_RENAME: RefCell<Option<RenameHook>> = const { RefCell::new(None) };
    }

    pub(super) fn before_rename(path: &Path) {
        BEFORE_RENAME.with(|hook| {
            if let Some(f) = hook.borrow_mut().as_mut() {
                f(path);
            }
        });
    }

    fn settings_only_fixture(label: &str) -> (PathBuf, ImportOptions) {
        let root = std::env::temp_dir().join(format!("owe744-{label}-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let orca_dir = root.join("orca");
        fs::create_dir_all(&orca_dir).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        fs::write(
            orca_dir.join("orca-data.json"),
            r#"{"settings":{"defaultTuiAgent":"codex"}}"#,
        )
        .unwrap();
        let opts = ImportOptions {
            home: root.join("home"),
            state_dir: root.join("state"),
            orca_dir,
            domains: vec![ImportDomain::Settings],
            dry_run: false,
        };
        fs::create_dir_all(&opts.state_dir).unwrap();
        fs::write(opts.state_dir.join(SETTINGS_FILE), "theme = \"dark\"\n").unwrap();
        (root, opts)
    }

    /// OWE-899: 알려진 고정폭만 대소문자·공백 무시하고 반영한다.
    #[test]
    fn importable_terminal_font_matches_known_monospace() {
        assert!(importable_terminal_font("Menlo"));
        assert!(importable_terminal_font("  jetbrains mono  "));
        assert!(importable_terminal_font("D2Coding"));
        assert!(!importable_terminal_font("SF Mono"));
        assert!(!importable_terminal_font("Helvetica"));
        assert!(!importable_terminal_font(""));
        assert!(!importable_terminal_font("   "));
    }

    /// OWE-899: 고정폭 확인이 안 되는 글꼴(SF Mono 등)은 반영하지 않고 사유를 남긴다.
    #[test]
    fn unimportable_terminal_font_is_skipped_with_reason() {
        for (label, family, applied) in [
            ("font-sfmono", "SF Mono", false),
            ("font-helvetica", "Helvetica", false),
            ("font-jetbrains", "JetBrains Mono", true),
        ] {
            let root = std::env::temp_dir().join(format!("owe899-{label}-{}", std::process::id()));
            fs::remove_dir_all(&root).ok();
            let orca_dir = root.join("orca");
            fs::create_dir_all(&orca_dir).unwrap();
            fs::write(
                orca_dir.join("orca-data.json"),
                format!(r#"{{"settings":{{"terminalFontFamily":"{family}"}}}}"#),
            )
            .unwrap();
            let opts = ImportOptions {
                home: root.join("home"),
                state_dir: root.join("state"),
                orca_dir,
                domains: vec![ImportDomain::Settings],
                dry_run: false,
            };
            fs::create_dir_all(&opts.state_dir).unwrap();
            fs::create_dir_all(&opts.home).unwrap();
            fs::write(opts.state_dir.join(SETTINGS_FILE), "theme = \"dark\"\n").unwrap();

            let report = run_import(&opts).unwrap();
            let saved: toml::Table =
                toml::from_str(&fs::read_to_string(opts.state_dir.join(SETTINGS_FILE)).unwrap())
                    .unwrap();
            if applied {
                assert_eq!(
                    saved
                        .get("terminal_font_family")
                        .and_then(toml::Value::as_str),
                    Some(family),
                    "{label}"
                );
                assert!(report.settings.terminal_font_skipped.is_none(), "{label}");
            } else {
                assert!(
                    saved.get("terminal_font_family").is_none(),
                    "{label}: {saved:?}"
                );
                let reason = report
                    .settings
                    .terminal_font_skipped
                    .as_deref()
                    .unwrap_or_else(|| panic!("{label}: skip reason missing"));
                assert!(reason.contains(family), "{label}: {reason}");
                assert!(
                    report.warnings.iter().any(|w| w == reason),
                    "{label}: {:?}",
                    report.warnings
                );
            }
            fs::remove_dir_all(root).ok();
        }
    }

    /// OWE-744: rename 직전 앱이 settings.toml을 저장하면 덮어쓰지 않고 새 원본으로 다시 병합한다.
    #[test]
    fn concurrent_app_save_is_merged_not_overwritten() {
        let (root, opts) = settings_only_fixture("cas-merge");
        let settings_path = opts.state_dir.join(SETTINGS_FILE);
        let target = settings_path.clone();
        let mut fired = false;
        BEFORE_RENAME.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |path: &Path| {
                if path == target && !fired {
                    fired = true;
                    fs::write(path, "theme = \"dark\"\nzoom = 1.5\n").unwrap();
                }
            }));
        });
        let report = run_import(&opts).unwrap();
        BEFORE_RENAME.with(|h| h.borrow_mut().take());

        assert!(report.settings.settings_updated);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let saved: toml::Table =
            toml::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved.get("zoom").and_then(toml::Value::as_float), Some(1.5));
        assert_eq!(
            saved.get("theme").and_then(toml::Value::as_str),
            Some("dark")
        );
        assert_eq!(report.settings.default_agent.as_deref(), Some("codex"));
        fs::remove_dir_all(root).ok();
    }

    /// OWE-744: 매번 원본이 바뀌면 3회 재시도 후 앱 내용을 그대로 두고 경고로 보고한다.
    #[test]
    fn persistent_conflict_gives_up_with_warning_and_keeps_app_file() {
        let (root, opts) = settings_only_fixture("cas-giveup");
        let settings_path = opts.state_dir.join(SETTINGS_FILE);
        let target = settings_path.clone();
        let mut n = 0u32;
        BEFORE_RENAME.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |path: &Path| {
                if path == target {
                    n += 1;
                    fs::write(path, format!("app_save = {n}\n")).unwrap();
                }
            }));
        });
        let report = run_import(&opts).unwrap();
        BEFORE_RENAME.with(|h| h.borrow_mut().take());

        let attempts = u32::try_from(CONFLICT_RETRIES).unwrap() + 1;
        assert_eq!(
            fs::read_to_string(&settings_path).unwrap(),
            format!("app_save = {attempts}\n")
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.starts_with("settings skipped") && w.contains("retried 3 times")),
            "{:?}",
            report.warnings
        );
        assert!(!report.settings.settings_updated);
        let leftovers: Vec<_> = fs::read_dir(&opts.state_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "temp files must be cleaned up");
        fs::remove_dir_all(root).ok();
    }

    /// OWE-896: uuid 조인·이메일·조직 기준·Codex 계정 id 매칭과 헬퍼 단위 동작.
    #[test]
    fn registered_lookup_matches_by_uuid_org_and_codex_account_id() {
        let root = std::env::temp_dir().join(format!("owe896-match-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();

        // uuid 조인용 프로필(라벨·이메일이 후보와 전부 다름).
        let uuid = "96f8c826-8b89-4470-a327-f4918efb8177";
        let profile = root.join("claude-profiles").join(uuid).join("home");
        fs::create_dir_all(&profile).unwrap();
        let dir_str = profile.display().to_string();
        // 조직 충돌 판정용 프로필.
        let org_profile = root.join("org-profile");
        fs::create_dir_all(&org_profile).unwrap();
        fs::write(
            org_profile.join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"a@example.com","organizationUuid":"org-1"}}"#,
        )
        .unwrap();
        // Codex 계정 id 판정용 프로필.
        let codex_profile = root.join("codex-profile");
        fs::create_dir_all(&codex_profile).unwrap();
        fs::write(
            codex_profile.join("auth.json"),
            r#"{"tokens":{"account_id":"acct-9","access_token":"x"},"last_refresh":"t"}"#,
        )
        .unwrap();

        let stored = |id: &str,
                      provider: ProviderKind,
                      label: &str,
                      email: Option<&str>,
                      profile_dir: &str| {
            AccountInfo {
                id: id.to_owned(),
                provider,
                label: label.to_owned(),
                email: email.map(str::to_owned),
                profile_dir: profile_dir.to_owned(),
                active: false,
                host_default: false,
                stale: false,
                usage: krill_proto::usage::Snapshot::default(),
            }
        };
        let snap = AccountsSnapshot {
            accounts: vec![
                stored("old-id", ProviderKind::Claude, "옛라벨", None, &dir_str),
                stored(
                    "org-id",
                    ProviderKind::Claude,
                    "a@example.com (Org 1)",
                    Some("a@example.com"),
                    &org_profile.display().to_string(),
                ),
                stored(
                    "codex-1",
                    ProviderKind::Codex,
                    "옛 Codex",
                    None,
                    &codex_profile.display().to_string(),
                ),
                stored(
                    "other",
                    ProviderKind::Claude,
                    "z@example.com",
                    None,
                    "/tmp/other",
                ),
            ],
            ..AccountsSnapshot::default()
        };
        let cand = |raw_id: &str,
                    provider: ProviderKind,
                    label: &str,
                    email: Option<&str>,
                    org_uuid: Option<&str>,
                    codex_account_id: Option<&str>| {
            PlannedOrcaAccount {
                provider,
                planned_id: format!("{raw_id}-planned"),
                raw_id: raw_id.to_owned(),
                label: label.to_owned(),
                email: email.map(str::to_owned),
                org_or_workspace: None,
                org_uuid: org_uuid.map(str::to_owned),
                codex_account_id: codex_account_id.map(str::to_owned),
                profile_dir: PathBuf::from("/tmp/cand"),
                dir_str: "/tmp/cand".to_owned(),
                active: false,
            }
        };

        // uuid 조인: 라벨·이메일·조직이 달라도 같은 계정.
        let hit = find_registered_account(
            &snap,
            &cand(
                uuid,
                ProviderKind::Claude,
                "new@example.com (New)",
                Some("new@example.com"),
                Some("org-9"),
                None,
            ),
        )
        .unwrap();
        assert_eq!(hit.id, "old-id");
        // 같은 이메일 + 조직 충돌 → 다른 계정.
        assert!(
            find_registered_account(
                &snap,
                &cand(
                    "other-uuid",
                    ProviderKind::Claude,
                    "a@example.com (Org 2)",
                    Some("a@example.com"),
                    Some("org-2"),
                    None,
                ),
            )
            .is_none()
        );
        // 같은 이메일 + 후보 조직 미상 → 같은 계정.
        let hit = find_registered_account(
            &snap,
            &cand(
                "other-uuid",
                ProviderKind::Claude,
                "a@example.com",
                Some("a@example.com"),
                None,
                None,
            ),
        )
        .unwrap();
        assert_eq!(hit.id, "org-id");
        // `"email (조직)"` 라벨 속 이메일로도 병합.
        let hit = find_registered_account(
            &snap,
            &cand(
                "other-uuid",
                ProviderKind::Claude,
                "a@example.com",
                Some("A@EXAMPLE.COM"),
                None,
                None,
            ),
        )
        .unwrap();
        assert_eq!(hit.id, "org-id");
        // Codex 계정 id 조인.
        let hit = find_registered_account(
            &snap,
            &cand(
                "codex-uuid",
                ProviderKind::Codex,
                "새 Codex",
                Some("other@example.com"),
                None,
                Some("acct-9"),
            ),
        )
        .unwrap();
        assert_eq!(hit.id, "codex-1");
        // 무관한 후보는 매칭 없음.
        assert!(
            find_registered_account(
                &snap,
                &cand(
                    "nope",
                    ProviderKind::Claude,
                    "nobody@example.com",
                    Some("nobody@example.com"),
                    None,
                    None,
                ),
            )
            .is_none()
        );
        // 헬퍼 단위 동작.
        assert_eq!(
            orca_uuid_from_profile_dir(
                "/o/claude-profiles/96f8c826-8b89-4470-a327-f4918efb8177/home"
            ),
            Some("96f8c826-8b89-4470-a327-f4918efb8177")
        );
        assert_eq!(
            orca_uuid_from_profile_dir(
                "/o/claude-accounts/96f8c826-8b89-4470-a327-f4918efb8177/auth"
            ),
            Some("96f8c826-8b89-4470-a327-f4918efb8177")
        );
        assert_eq!(
            orca_uuid_from_profile_dir(
                "/o/managed-data-accounts/opencode/96f8c826-8b89-4470-a327-f4918efb8177"
            ),
            Some("96f8c826-8b89-4470-a327-f4918efb8177")
        );
        assert_eq!(orca_uuid_from_profile_dir("/tmp/other"), None);
        assert_eq!(
            orca_uuid_from_profile_dir("/o/claude-profiles/not-a-uuid/home"),
            None
        );
        assert_eq!(primary_email("a@example.com (Org A)"), "a@example.com");
        assert_eq!(primary_email("표시명"), "표시명");
        assert!(emails_equal("A@EXAMPLE.COM", "a@example.com (Org A)"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn import_accounts_domain_includes_opencode_data_accounts_in_dry_run() {
        let root =
            std::env::temp_dir().join(format!("owe744-opencode-domain-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let base = root.join("orca/managed-data-accounts/opencode");
        let id = "96f8c826-8b89-4470-a327-f4918efb8177";
        fs::create_dir_all(base.join(id).join("data/opencode")).unwrap();
        fs::write(base.join(id).join("data/opencode/auth.json"), "{}").unwrap();
        fs::write(
            base.join("accounts.json"),
            format!(
                r#"{{"accounts":[{{"id":"{id}","label":"OpenCode Work"}}],"activeAccountId":"{id}"}}"#
            ),
        )
        .unwrap();
        let opts = ImportOptions {
            home: root.join("home"),
            state_dir: root.join("state"),
            orca_dir: root.join("orca"),
            dry_run: true,
            domains: vec![ImportDomain::Accounts],
        };
        let mut warnings = Vec::new();
        let summary = import_accounts_domain(&opts, &BTreeMap::new(), &mut warnings);
        assert_eq!(summary.items.len(), 1);
        assert_eq!(summary.items[0].provider, "opencode");
        assert_eq!(summary.items[0].label, "OpenCode Work");
        assert!(summary.items[0].active);
        fs::remove_dir_all(root).ok();
    }

    fn create_sqlite_db(db_path: &Path, rows: &[(&str, &str)]) {
        use std::fmt::Write as _;
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut sql = String::from(
            "CREATE TABLE profile_state_documents (domain TEXT PRIMARY KEY, payload TEXT NOT NULL, updated_at INTEGER NOT NULL);\n",
        );
        for (domain, payload) in rows {
            let escaped = payload.replace('\'', "''");
            writeln!(
                sql,
                "INSERT INTO profile_state_documents (domain, payload, updated_at) VALUES ('{domain}', '{escaped}', 1700000000);"
            )
            .unwrap();
        }
        let status = Command::new("/usr/bin/sqlite3")
            .arg(db_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(sql.as_bytes())?;
                }
                child.wait()
            })
            .unwrap();
        assert!(status.success());
    }

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "macOS 전용: Orca macOS 데이터 경로"
    )]
    fn full_import_sqlite_and_json_fallback_is_idempotent_and_respects_dry_run_and_only() {
        let root = std::env::temp_dir().join(format!("owe726-proto-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state_dir = root.join("state");
        let orca_dir = root.join("orca");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&orca_dir).unwrap();

        // 1. Profile index pointing to "work"
        fs::write(
            orca_dir.join("orca-profile-index.json"),
            r#"{"version":1,"activeProfileId":"work","profiles":[{"id":"default","name":"Default"},{"id":"work","name":"Work"}]}"#,
        )
        .unwrap();
        assert_eq!(resolve_active_profile_id(&orca_dir), "work");

        // 2. Claude & Codex profile directories on disk
        let claude_uuid_1 = "11111111-2222-3333-4444-555555555555";
        let claude_uuid_2 = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let codex_uuid_1 = "99999999-8888-7777-6666-555555555555";

        let claude_home_1 = orca_dir
            .join("claude-profiles")
            .join(claude_uuid_1)
            .join("home");
        fs::create_dir_all(&claude_home_1).unwrap();
        let claude_auth_2 = orca_dir
            .join("claude-accounts")
            .join(claude_uuid_2)
            .join("auth");
        fs::create_dir_all(&claude_auth_2).unwrap();
        fs::write(
            claude_auth_2.join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":" fallback@example.com ","organizationName":"Fallback Org"}}"#,
        )
        .unwrap();

        let codex_home_1 = orca_dir
            .join("codex-accounts")
            .join(codex_uuid_1)
            .join("home");
        fs::create_dir_all(&codex_home_1).unwrap();

        // 3. Fallback `orca-data.json` has `folderWorkspaces` (to test fallback merge alongside SQLite)
        let profile_dir = orca_dir.join("profiles").join("work");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("orca-data.json"),
            r#"{
              "folderWorkspaces": [
                {
                  "id": "fw-1",
                  "name": "Notes Workspace",
                  "folderPath": "/Users/test/notes",
                  "comment": "review architecture notes",
                  "isArchived": false,
                  "isUnread": true,
                  "isPinned": true,
                  "sortOrder": 3,
                  "workspaceStatus": "in-review",
                  "linkedItems": [
                    {"provider":"linear","kind":"issue","identifier":"OWE-726","url":"https://linear.app/issue/OWE-726"}
                  ]
                }
              ]
            }"#,
        )
        .unwrap();

        // 4. `orca-environments.json`
        fs::write(
            orca_dir.join("orca-environments.json"),
            r#"{
              "version": 1,
              "environments": [
                {
                  "id": "env-local",
                  "name": "Local",
                  "endpoints": [{"id":"ep1","type":"direct","endpoint":"ws://127.0.0.1:4100","priority":1}]
                },
                {
                  "id": "env- remote-box",
                  "name": "Build Box",
                  "endpoints": [{"id":"ep2","type":"direct","endpoint":"wss://build.internal.example.com:8443","priority":1}]
                }
              ]
            }"#,
        )
        .unwrap();

        // 5. SQLite `profile-state.db`
        let settings_payload = serde_json::json!({
            "defaultTuiAgent": "codex",
            "terminalFontSize": 15,
            "terminalFontFamily": "JetBrains Mono",
            "terminalCursorStyle": "bar",
            "terminalCursorBlink": true,
            "theme": "dark",
            "computerAwakeMode": "always",
            "claudeManagedAccounts": [
                {
                    "id": claude_uuid_1,
                    "email": "primary@example.com",
                    "organizationName": "Acme Corp",
                    "managedAuthPath": claude_home_1.display().to_string()
                },
                {
                    "id": claude_uuid_2
                }
            ],
            "activeClaudeManagedAccountId": claude_uuid_1,
            "codexManagedAccounts": [
                {
                    "id": codex_uuid_1,
                    "email": "codex@example.com",
                    "workspaceLabel": "Core Team",
                    "managedHomePath": codex_home_1.display().to_string()
                }
            ],
            "activeCodexManagedAccountId": codex_uuid_1,
            "terminalQuickCommands": [
                {"id": "qc1", "label": "Run Check", "command": "scripts/check.sh"}
            ],
            "agentCmdOverrides": {"codex": "codex-custom"},
            "agentDefaultArgs": {"codex": ["--full-auto"]},
            "agentDefaultEnv": {"codex": {"RUST_LOG": "info"}}
        })
        .to_string();

        let ssh_payload = serde_json::json!([
            {
                "id": "mac-mini",
                "label": "Mac Mini M4",
                "configHost": "m4-mini",
                "host": "100.64.0.10",
                "port": 2222,
                "username": "owen",
                "identityFile": "~/.ssh/id_ed25519",
                "source": "ssh-config"
            }
        ])
        .to_string();

        let repos_payload = serde_json::json!([
            {
                "id": "repo-krill",
                "path": "/Users/test/krill",
                "displayName": "Krill App",
                "badgeColor": "#22c55e",
                "kind": "git",
                "worktreeBaseRef": "origin/main",
                "symlinkPaths": [".env.local"],
                "hookSettings": {
                    "mode": "override",
                    "scripts": {
                        "setup": "cargo fetch",
                        "archive": "git clean -fd"
                    }
                }
            }
        ])
        .to_string();

        let wt_meta_payload = serde_json::json!({
            "repo-krill::/Users/test/krill-wt-1": {
                "displayName": "Feature Import",
                "comment": "Working on Orca migration",
                "linkedPR": 726,
                "linkedLinearIssue": "OWE-726",
                "linkedItems": [
                    {"provider": "github", "kind": "pr", "number": 726, "url": "https://github.com/org/krill/pull/726"}
                ],
                "isArchived": false,
                "isUnread": true,
                "isPinned": true,
                "sortOrder": 1,
                "workspaceStatus": "in-progress"
            }
        })
        .to_string();

        let lineage_payload = serde_json::json!({
            "repo-krill::/Users/test/krill-wt-1": {
                "repoId": "repo-krill",
                "worktreePath": "/Users/test/krill-wt-1",
                "branch": "feat/orca-import",
                "baseRef": "origin/main",
                "parentWorktreePath": "/Users/test/krill",
                "parentBranch": "main",
                "createdAt": 1_700_000_123_u64,
                "forkPointSha": "deadbeef"
            }
        })
        .to_string();

        let automations_payload = serde_json::json!([
            {
                "id": "nightly-check",
                "name": "Nightly Check",
                "prompt": "Run nightly diagnostics",
                "agentId": "claude",
                "rrule": "FREQ=WEEKLY;BYDAY=MO,WE,FR;BYHOUR=7;BYMINUTE=30",
                "enabled": true,
                "projectId": "repo-krill"
            }
        ])
        .to_string();

        create_sqlite_db(
            &profile_dir.join("profile-state.db"),
            &[
                ("settings", &settings_payload),
                ("sshTargets", &ssh_payload),
                ("repos", &repos_payload),
                ("worktreeMeta", &wt_meta_payload),
                ("worktreeLineageById", &lineage_payload),
                ("automations", &automations_payload),
            ],
        );

        // 6. Test `--dry-run`: must report all items without writing any files in `state_dir`
        let dry_opts = ImportOptions {
            home: home.clone(),
            state_dir: state_dir.clone(),
            orca_dir: orca_dir.clone(),
            domains: ImportDomain::ALL.to_vec(),
            dry_run: true,
        };
        let dry_report = run_import(&dry_opts).unwrap();
        assert!(dry_report.dry_run);
        assert_eq!(dry_report.profile_id, "work");
        assert!(!dry_report.accounts.skipped);
        assert_eq!(dry_report.accounts.claude_added, 2);
        assert_eq!(dry_report.accounts.codex_added, 1);
        assert_eq!(dry_report.ssh.hosts_added, 2);
        assert_eq!(dry_report.ssh.environments_merged, 1);
        assert_eq!(dry_report.projects.projects_added, 2);
        assert_eq!(dry_report.projects.worktrees_migrated, 2);
        assert_eq!(dry_report.projects.comments_migrated, 2);
        assert_eq!(dry_report.projects.lineage_migrated, 1);
        assert!(dry_report.settings.settings_updated);
        assert_eq!(dry_report.settings.quick_commands_merged, 1);
        assert_eq!(dry_report.settings.agents_updated, 1);
        assert_eq!(dry_report.settings.schedules_merged, 1);
        assert!(!state_dir.join("accounts.toml").exists());
        assert!(!state_dir.join(HOSTS_FILE).exists());
        assert!(!state_dir.join(PROJECTS_FILE).exists());
        assert!(!state_dir.join(SETTINGS_FILE).exists());

        // 7. Test `--only accounts` without running krilld: must skip with '데몬 필요' and write no files
        let only_acc_opts = ImportOptions {
            home: home.clone(),
            state_dir: state_dir.clone(),
            orca_dir: orca_dir.clone(),
            domains: vec![ImportDomain::Accounts],
            dry_run: false,
        };
        let no_daemon_acc = run_import(&only_acc_opts).unwrap();
        assert!(no_daemon_acc.accounts.skipped);
        assert!(
            no_daemon_acc
                .accounts
                .skipped_reason
                .as_deref()
                .is_some_and(|r| r.starts_with("Daemon required"))
        );
        assert_eq!(no_daemon_acc.accounts.claude_added, 0);
        assert_eq!(no_daemon_acc.accounts.codex_added, 0);
        assert_eq!(no_daemon_acc.accounts.items.len(), 3);
        assert!(!state_dir.join("accounts.toml").exists());
        assert!(!state_dir.join("active-accounts.env").exists());

        // 8. Spawn a mock krilld listener on `<state_dir>/run/krilld.sock` to test daemon IPC account migration
        let run_dir = state_dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let sock_path = run_dir.join("krilld.sock");
        let listener = krill_proto::os::net::UnixListener::bind(&sock_path).unwrap();
        let stop_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = std::sync::Arc::clone(&stop_flag);
        let server_thread = std::thread::spawn(move || {
            let mut accounts: Vec<AccountInfo> = Vec::new();
            while !stop_clone.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                if stop_clone.load(Ordering::Relaxed) {
                    break;
                }
                let server_hello = krill_proto::ipc::ServerHello {
                    proto_min: ipc::PROTO_MIN,
                    proto_max: ipc::PROTO_MAX,
                    features: ipc::FEATURES
                        | FEAT_USAGE
                        | FEAT_USAGE_RATELIMIT
                        | FEAT_USAGE_AGY
                        | FEAT_USAGE_RULES
                        | FEAT_USAGE_STATUS
                        | FEAT_USAGE_MANAGE
                        | FEAT_ACCOUNT_POLICY
                        | FEAT_ACCOUNT_CONNECTION,
                    instance_id: 1,
                    build: krill_version::BUILD.to_owned(),
                };
                if stream.write_all(&server_hello.encode().unwrap()).is_err() {
                    continue;
                }
                let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) else {
                    continue;
                };
                let Ok(_client_hello) = krill_proto::ipc::ClientHello::decode(kind, &body) else {
                    continue;
                };
                let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) else {
                    continue;
                };
                let Ok(req) = UsageRequest::decode(kind, &body) else {
                    continue;
                };
                match req {
                    UsageRequest::AddAccount {
                        provider,
                        label,
                        email,
                        profile_dir,
                    } => {
                        let id = format!(
                            "{}-{}",
                            provider.as_str(),
                            slugify(email.as_deref().unwrap_or(&label))
                        );
                        if !accounts.iter().any(|a| {
                            a.provider == provider && (a.profile_dir == profile_dir || a.id == id)
                        }) {
                            let first_for_provider =
                                !accounts.iter().any(|a| a.provider == provider);
                            accounts.push(AccountInfo {
                                id,
                                provider,
                                label,
                                email,
                                profile_dir,
                                active: first_for_provider,
                                host_default: false,
                                stale: false,
                                usage: krill_proto::usage::Snapshot::default(),
                            });
                        }
                    }
                    UsageRequest::SwitchAccount {
                        provider,
                        account_id,
                    } => {
                        for acc in accounts.iter_mut().filter(|a| a.provider == provider) {
                            acc.active = acc.id == account_id;
                        }
                    }
                    _ => {}
                }
                let reply = UsageReply::Accounts(AccountsSnapshot {
                    accounts: accounts.clone(),
                    ..AccountsSnapshot::default()
                });
                stream.write_all(&reply.encode().unwrap()).ok();
            }
        });

        let acc_report = run_import(&only_acc_opts).unwrap();
        assert!(!acc_report.accounts.skipped);
        assert!(acc_report.accounts.skipped_reason.is_none());
        assert_eq!(acc_report.accounts.claude_added, 2);
        assert_eq!(acc_report.accounts.codex_added, 1);
        assert!(!state_dir.join(HOSTS_FILE).exists());

        // 9. Run full import (all 4 domains) with daemon running
        let full_opts = ImportOptions {
            home: home.clone(),
            state_dir: state_dir.clone(),
            orca_dir: orca_dir.clone(),
            domains: ImportDomain::ALL.to_vec(),
            dry_run: false,
        };
        let applied = run_import(&full_opts).unwrap();
        // Accounts were already added to the daemon in step 8, so 0 added on re-run!
        assert!(!applied.accounts.skipped);
        assert_eq!(applied.accounts.claude_added, 0);
        assert_eq!(applied.accounts.claude_updated, 0);
        assert_eq!(applied.accounts.codex_added, 0);
        assert_eq!(applied.accounts.codex_updated, 0);
        assert!(applied.accounts.items.iter().all(|i| i.runnable));
        assert!(
            applied
                .accounts
                .items
                .iter()
                .any(|i| i.label == "primary@example.com (Acme Corp)" && i.active)
        );
        assert!(
            applied
                .accounts
                .items
                .iter()
                .any(|i| i.label == "fallback@example.com (Fallback Org)")
        );
        assert!(
            applied
                .accounts
                .items
                .iter()
                .any(|i| i.label == "codex@example.com (Core Team)" && i.active)
        );

        // Verify `repo_hook_config_in`
        let hook_cfg = repo_hook_config_in(&state_dir, Path::new("/Users/test/krill")).unwrap();
        assert_eq!(hook_cfg.hook_mode.as_deref(), Some("override"));
        assert_eq!(hook_cfg.setup_script.as_deref(), Some("cargo fetch"));
        assert_eq!(hook_cfg.symlink_paths, vec![".env.local".to_owned()]);

        // Verify `WorktreeStore` checkpoints
        let wt_store = WorktreeStore::load(&state_dir).unwrap();
        let cp = wt_store.checkpoint("/Users/test/krill-wt-1");
        assert_eq!(cp.comment.as_deref(), Some("Working on Orca migration"));
        assert_eq!(cp.workspace_status.as_deref(), Some("in-progress"));
        assert!(cp.unread);
        assert_eq!(cp.pr, Some(726));
        assert_eq!(cp.issue.as_deref(), Some("OWE-726"));
        assert_eq!(
            cp.references,
            vec!["https://github.com/org/krill/pull/726".to_owned()]
        );
        // 계보 부모는 WorktreeStore(단일 원천)에 기록된다(OWE-756).
        let wt_key = worktree_key(Path::new("/Users/test/krill-wt-1"));
        assert_eq!(
            wt_store.parents.get(&wt_key).map(String::as_str),
            Some("/Users/test/krill")
        );

        // Verify `Schedules`
        let schedules = Schedules::load(&state_dir).unwrap();
        assert_eq!(schedules.schedule.len(), 1);
        assert_eq!(schedules.schedule[0].cron, "30 7 * * 1,3,5");
        assert_eq!(schedules.schedule[0].cwd, "/Users/test/krill");

        // 사용자가 CLI로 바꾼 부모는 재이관이 덮어쓰지 않는다.
        WorktreeStore::mutate(&state_dir, |s| {
            s.set_parent("/Users/test/krill-wt-1", Some("/Users/test/notes"))
        })
        .unwrap();

        // 10. Run full import a second time to verify complete idempotency
        let second = run_import(&full_opts).unwrap();
        assert_eq!(
            WorktreeStore::load(&state_dir)
                .unwrap()
                .parents
                .get(&wt_key)
                .map(String::as_str),
            Some("/Users/test/notes")
        );
        assert_eq!(second.accounts.claude_added, 0);
        assert_eq!(second.accounts.claude_updated, 0);
        assert_eq!(second.accounts.codex_added, 0);
        assert_eq!(second.accounts.codex_updated, 0);
        assert_eq!(second.ssh.hosts_added, 0);
        assert_eq!(second.ssh.hosts_updated, 0);
        assert_eq!(second.projects.projects_added, 0);
        assert_eq!(second.projects.projects_updated, 0);
        assert!(!second.settings.settings_updated);
        assert_eq!(second.settings.quick_commands_merged, 0);
        assert_eq!(second.settings.agents_updated, 0);
        assert_eq!(second.settings.schedules_merged, 0);

        stop_flag.store(true, Ordering::Relaxed);
        krill_proto::os::net::UnixStream::connect(&sock_path).ok();
        server_thread.join().ok();
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn current_orca_automations_import_numeric_dates_paths_disabled_and_options() {
        let root = std::env::temp_dir().join(format!("r504-import-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let opts = ImportOptions {
            home: root.join("home"),
            state_dir: root.join("state"),
            orca_dir: root.join("orca"),
            domains: vec![ImportDomain::Settings],
            dry_run: false,
        };
        fs::create_dir_all(&opts.home).unwrap();
        fs::create_dir_all(&opts.state_dir).unwrap();
        let domains = BTreeMap::from([(
            "automations".to_owned(),
            serde_json::json!([
                { "id": "current", "name": "Current Job", "prompt": "audit", "agentId": "codex", "rrule": "15 3 * * 1-5", "dtstart": 1_792_850_000_123_u64, "enabled": false,
                  "runContext": { "path": opts.home.join("current") }, "precheck": { "command": "git diff --quiet", "timeoutSeconds": 120 }, "workspaceMode": "new_per_run", "baseBranch": "dev", "reuseSession": true, "extraAgentArgs": "--model gpt", "missedRunGraceMinutes": 720 },
                { "id": "float-date", "name": "Float", "prompt": "review", "dtstart": 1.5, "rrule": "FREQ=HOURLY;BYMINUTE=5", "precheck": "exit 0", "reuseSession": true },
                { "id": "malformed", "prompt": 99 },
                { "id": "null-date", "prompt": "null date", "dtstart": null, "rrule": "FREQ=DAILY;BYHOUR=10" },
                { "id": "string-date", "prompt": "legacy", "dtstart": "2026-10-01T08:15:00", "rrule": "FREQ=MONTHLY;BYMONTHDAY=2" }
            ]),
        )]);
        let mut warnings = Vec::new();
        assert_eq!(
            import_automations(&opts, &domains, &serde_json::json!({}), &mut warnings).unwrap(),
            4
        );
        assert_eq!(warnings.len(), 1);
        let schedules = Schedules::load(&opts.state_dir).unwrap();
        let entry = schedules
            .schedule
            .iter()
            .find(|s| s.id == "orca-current")
            .unwrap();
        assert_eq!(entry.name.as_deref(), Some("Current Job"));
        assert!(!entry.enabled);
        assert_eq!(entry.cron, "15 3 * * 1-5");
        assert_eq!(Path::new(&entry.cwd), opts.home.join("current"));
        assert_eq!(entry.agent.as_deref(), Some("codex"));
        assert_eq!(entry.command, ["codex"]);
        assert_eq!(entry.precheck.as_deref(), Some("git diff --quiet"));
        assert_eq!(entry.precheck_timeout_secs, Some(120));
        assert_eq!(entry.workspace_mode, schedule::WorkspaceMode::Isolate);
        assert_eq!(entry.base_branch.as_deref(), Some("dev"));
        assert!(!entry.reuse_session);
        assert_eq!(entry.extra_agent_args.as_deref(), Some("--model gpt"));
        assert_eq!(entry.missed_run_grace_minutes, Some(720));
        assert!(
            schedules
                .schedule
                .iter()
                .find(|s| s.id == "orca-float-date")
                .unwrap()
                .reuse_session
        );
        assert_eq!(
            schedules
                .schedule
                .iter()
                .find(|s| s.id == "orca-string-date")
                .unwrap()
                .cron,
            "15 8 2 * *"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn automations_import_respects_edit_lock_preserves_precheck_and_workspace_mode_and_skips_invalid_items()
     {
        let root = std::env::temp_dir().join(format!("owe749-auto-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state_dir = root.join("state");
        let orca_dir = root.join("orca");
        let profile_dir = orca_dir.join("profiles").join("default");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&profile_dir).unwrap();

        let write_orca_automations = |automations: serde_json::Value| {
            let doc = serde_json::json!({
                "settings": {},
                "automations": automations,
            });
            fs::write(profile_dir.join("orca-data.json"), doc.to_string()).unwrap();
        };

        // 1. Initial import of `nightly-check`
        write_orca_automations(serde_json::json!([
            {
                "id": "nightly-check",
                "name": "Nightly Check",
                "prompt": "Initial prompt",
                "agentId": "claude",
                "rrule": "FREQ=DAILY;BYHOUR=6;BYMINUTE=0",
                "enabled": true
            }
        ]));

        let opts = ImportOptions {
            home: home.clone(),
            state_dir: state_dir.clone(),
            orca_dir: orca_dir.clone(),
            domains: vec![ImportDomain::Settings],
            dry_run: false,
        };
        let first = run_import(&opts).unwrap();
        assert_eq!(first.settings.schedules_merged, 1);

        // User configures `precheck` and `workspace_mode = Isolate` in Krill
        {
            let _guard = Schedules::exclusive_edit(&state_dir).unwrap();
            let mut schedules = Schedules::load(&state_dir).unwrap();
            assert_eq!(schedules.schedule.len(), 1);
            schedules.schedule[0].precheck = Some("git diff --quiet".to_owned());
            schedules.schedule[0].workspace_mode = schedule::WorkspaceMode::Isolate;
            schedules.save(&state_dir).unwrap();
        }

        // 2. Update prompt & schedule in Orca, plus add >32KiB prompt and colliding Korean-only names
        let oversized_prompt = "x".repeat(32 * 1024 + 10);
        write_orca_automations(serde_json::json!([
            {
                "id": "nightly-check",
                "name": "Nightly Check",
                "prompt": "Updated prompt from Orca",
                "agentId": "codex",
                "rrule": "FREQ=DAILY;BYHOUR=8;BYMINUTE=15",
                "enabled": true
            },
            {
                "id": "oversized-auto",
                "name": "Oversized Prompt",
                "prompt": oversized_prompt,
                "agentId": "claude",
                "rrule": "FREQ=DAILY;BYHOUR=9;BYMINUTE=0",
                "enabled": true
            },
            {
                "id": "",
                "name": "아침 점검",
                "prompt": "Korean schedule 1",
                "agentId": "claude",
                "rrule": "FREQ=DAILY;BYHOUR=10;BYMINUTE=0",
                "enabled": true
            },
            {
                "id": "",
                "name": "저녁 점검",
                "prompt": "Korean schedule 2",
                "agentId": "claude",
                "rrule": "FREQ=DAILY;BYHOUR=18;BYMINUTE=0",
                "enabled": true
            }
        ]));

        // 3. While `schedules-edit.lock` is held, `run_import` must fail instead of bypassing the lock
        {
            let _held_lock = Schedules::exclusive_edit(&state_dir).unwrap();
            assert!(run_import(&opts).is_err());
            let unchanged = Schedules::load(&state_dir).unwrap();
            assert_eq!(unchanged.schedule.len(), 1);
            assert_eq!(unchanged.schedule[0].prompt, "Initial prompt");
        }

        // 4. After releasing the lock, `run_import` updates `nightly-check` (preserving `precheck` &
        //    `workspace_mode`), imports the first Korean-named schedule, and skips the >32KiB and
        //    colliding duplicate-ID schedules with warnings.
        let second = run_import(&opts).unwrap();
        assert_eq!(second.settings.schedules_merged, 2);
        assert_eq!(second.warnings.len(), 2);
        assert!(
            second
                .warnings
                .iter()
                .any(|w| w.contains("Oversized Prompt"))
        );
        assert!(second.warnings.iter().any(|w| w.contains("저녁 점검")));

        let updated = Schedules::load(&state_dir).unwrap();
        assert_eq!(updated.schedule.len(), 2);
        let nightly = updated
            .schedule
            .iter()
            .find(|s| s.id == "orca-nightly-check")
            .unwrap();
        assert_eq!(nightly.prompt, "Updated prompt from Orca");
        assert_eq!(nightly.cron, "15 8 * * *");
        assert_eq!(nightly.command, vec!["codex".to_owned()]);
        assert_eq!(nightly.precheck.as_deref(), Some("git diff --quiet"));
        assert_eq!(nightly.workspace_mode, schedule::WorkspaceMode::Isolate);

        let korean = updated
            .schedule
            .iter()
            .find(|s| s.id == "orca-item")
            .unwrap();
        assert_eq!(korean.prompt, "Korean schedule 1");

        fs::remove_dir_all(&root).ok();
    }

    /// OWE-803: 심볼릭 링크(`/tmp` 등) 아래 경로를 이관하면 `[[projects]]`와
    /// `[worktrees]`·`[lineage]` 키가 같은 정규 경로로 저장되고 재이관이 멱등하다.
    #[cfg(unix)]
    #[test]
    fn symlink_alias_paths_are_normalized_and_idempotent() {
        let root = std::env::temp_dir().join(format!("owe803-alias-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        // 실제 디렉터리 + 심볼릭 링크 별칭을 만든다(OS 기본 `/tmp` 링크 가정 없이 재현).
        let real = root.join("real");
        let notes = real.join("notes");
        fs::create_dir_all(&notes).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let link_str = link.display().to_string();
        let notes_link_str = link.join("notes").display().to_string();
        let canon_real = fs::canonicalize(&real).unwrap();
        let canon_real_str = canon_real.display().to_string();
        let canon_notes_str = fs::canonicalize(&notes).unwrap().display().to_string();

        // 과거 이관분: `[[projects]]`에 원본(심볼릭 링크) 경로가 남아 있다.
        let state_dir = root.join("state");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            state_dir.join(PROJECTS_FILE),
            format!(
                "[[projects]]\nid = \"r1\"\npath = \"{link_str}\"\n\
                 display_name = \"Link Repo\"\nkind = \"git\"\n"
            ),
        )
        .unwrap();

        let orca_dir = root.join("orca");
        fs::create_dir_all(orca_dir.join("profiles").join("default")).unwrap();
        let mut wt_meta = serde_json::Map::new();
        wt_meta.insert(
            format!("r1::{link_str}"),
            serde_json::json!({"displayName": "Link Repo"}),
        );
        let mut wt_lin = serde_json::Map::new();
        wt_lin.insert(
            format!("r1::{notes_link_str}"),
            serde_json::json!({
                "repoId": "r1",
                "worktreePath": notes_link_str,
                "parentWorktreePath": link_str,
            }),
        );
        let doc = serde_json::json!({
            "repos": [{"id": "r1", "path": link_str, "displayName": "Link Repo", "kind": "git"}],
            "folderWorkspaces": [{"id": "fw-1", "name": "Notes", "folderPath": notes_link_str}],
            "worktreeMeta": wt_meta,
            "worktreeLineageById": wt_lin,
        });
        fs::write(
            orca_dir
                .join("profiles")
                .join("default")
                .join("orca-data.json"),
            doc.to_string(),
        )
        .unwrap();

        let opts = ImportOptions {
            home: root.join("home"),
            state_dir: state_dir.clone(),
            orca_dir,
            domains: vec![ImportDomain::Projects],
            dry_run: false,
        };
        fs::create_dir_all(&opts.home).unwrap();
        let report = run_import(&opts).unwrap();

        let store = ProjectsStore::load(&state_dir).unwrap();
        // `[[projects]]`는 정규 경로로 수렴하고 중복 없이 2개(r1 + fw-1)만 있다.
        assert_eq!(store.projects.len(), 2, "{:?}", store.projects);
        assert!(
            store.projects.iter().all(|p| !p.path.contains("/link")),
            "{:?}",
            store.projects
        );
        let r1 = store.projects.iter().find(|p| p.id == "r1").unwrap();
        assert_eq!(r1.path, canon_real_str);
        // `[worktrees]`·`[lineage]` 키도 같은 정규 경로를 쓴다.
        assert!(store.worktrees.contains_key(&canon_real_str));
        assert!(
            store.worktrees.keys().all(|k| !k.contains("/link")),
            "{:?}",
            store.worktrees.keys().collect::<Vec<_>>()
        );
        assert!(
            store.lineage.keys().all(|k| !k.contains("/link")),
            "{:?}",
            store.lineage.keys().collect::<Vec<_>>()
        );
        assert!(store.lineage.contains_key(&canon_notes_str));
        // fw-1만 신규, r1은 경로 수렴 업데이트.
        assert_eq!(report.projects.projects_added, 1);
        assert_eq!(report.projects.projects_updated, 1);

        // `WorktreeStore` 부모 계보도 정규 경로로 기록된다.
        let wt_store = WorktreeStore::load(&state_dir).unwrap();
        assert_eq!(
            wt_store.parents.get(&canon_notes_str).map(String::as_str),
            Some(canon_real_str.as_str())
        );

        // 재이관은 완전히 멱등이다.
        let second = run_import(&opts).unwrap();
        assert_eq!(second.projects.projects_added, 0);
        assert_eq!(second.projects.projects_updated, 0);

        fs::remove_dir_all(root).ok();
    }

    /// OWE-901: agy 계정은 암호화 저장소라 이관하지 않고 새로 로그인을 안내한다(스키마 변경 없음).
    #[test]
    fn agy_import_note_reports_non_transferable_accounts() {
        let root = std::env::temp_dir().join(format!("owe901-agy-import-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let orca_dir = root.join("orca");
        fs::create_dir_all(&orca_dir).unwrap();
        let opts = ImportOptions {
            home: root.join("home"),
            state_dir: root.join("state"),
            orca_dir: orca_dir.clone(),
            domains: vec![ImportDomain::Accounts],
            dry_run: true,
        };
        // 설정 흔적이 없으면 안내 없음.
        let empty: BTreeMap<String, Json> = BTreeMap::from([(
            "settings".to_owned(),
            serde_json::json!({"claudeManagedAccounts": []}),
        )]);
        assert!(detect_agy_import_note(&opts, &empty).is_none());
        // 배열 + 활성 id 흔적이면 개수와 함께 안내한다.
        let domains: BTreeMap<String, Json> = BTreeMap::from([(
            "settings".to_owned(),
            serde_json::json!({
                "antigravityManagedAccounts": [{"id": "a1"}, {"id": "a2"}],
                "activeAgyManagedAccountId": "x",
            }),
        )]);
        let note = detect_agy_import_note(&opts, &domains).unwrap();
        assert!(note.contains("found: 3"), "{note}");
        assert!(
            note.contains("can't be imported (Orca encrypted store)"),
            "{note}"
        );
        assert!(note.contains("sign in again"), "{note}");
        // 디스크 흔적만 있어도 안내한다.
        fs::create_dir_all(orca_dir.join("antigravity-accounts").join("a1")).unwrap();
        let disk_only: BTreeMap<String, Json> =
            BTreeMap::from([("settings".to_owned(), serde_json::json!({}))]);
        let note = detect_agy_import_note(&opts, &disk_only).unwrap();
        assert!(note.contains("found: 1"), "{note}");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn imports_orca_repo_icons_and_extracts_data_urls_to_files() {
        let root = std::env::temp_dir().join(format!("mk-repo-icons-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let state_dir = root.join("state");
        let orca_dir = root.join("orca");
        fs::create_dir_all(orca_dir.join("profiles").join("default")).unwrap();
        fs::create_dir_all(&state_dir).unwrap();
        // 1x1 투명 PNG.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let doc = serde_json::json!({
            "repos": [
                {"id": "r-gh", "path": "/tmp/mk-icons/gh", "kind": "git",
                 "repoIcon": {"type": "image", "src": "https://github.com/craftowen.png?size=64", "source": "github"}},
                {"id": "r-file", "path": "/tmp/mk-icons/file", "kind": "git",
                 "repoIcon": {"type": "image", "src": format!("data:image/png;base64,{png}"), "source": "file"}},
                {"id": "r-lucide", "path": "/tmp/mk-icons/lucide", "kind": "git",
                 "repoIcon": {"type": "lucide", "name": "Rocket"}},
                {"id": "r-emoji", "path": "/tmp/mk-icons/emoji", "kind": "git",
                 "repoIcon": {"type": "emoji", "emoji": "🦀"}},
                {"id": "r-bad", "path": "/tmp/mk-icons/bad", "kind": "git",
                 "repoIcon": {"type": "image", "src": "data:image/png;base64,bm90IGEgcG5n", "source": "upload"}},
            ],
        });
        fs::write(
            orca_dir
                .join("profiles")
                .join("default")
                .join("orca-data.json"),
            doc.to_string(),
        )
        .unwrap();
        let mut opts = ImportOptions {
            home: root.join("home"),
            state_dir: state_dir.clone(),
            orca_dir,
            domains: vec![ImportDomain::Projects],
            dry_run: true,
        };
        fs::create_dir_all(&opts.home).unwrap();
        run_import(&opts).unwrap();
        assert!(
            !state_dir.join(REPO_ICON_DIR).exists(),
            "dry-run은 아이콘 파일을 쓰지 않는다"
        );
        opts.dry_run = false;
        run_import(&opts).unwrap();

        let store = ProjectsStore::load(&state_dir).unwrap();
        let icon = |id: &str| {
            store
                .projects
                .iter()
                .find(|p| p.id == id)
                .and_then(|p| p.repo_icon.clone())
                .map(|i| (i.kind, i.value))
        };
        assert_eq!(
            icon("r-gh"),
            Some((
                "image".to_owned(),
                "https://github.com/craftowen.png?size=64".to_owned()
            ))
        );
        let file = state_dir.join(REPO_ICON_DIR).join("r-file.png");
        assert_eq!(
            icon("r-file"),
            Some(("image".to_owned(), file.to_string_lossy().into_owned()))
        );
        assert!(fs::read(&file).unwrap().starts_with(b"\x89PNG"));
        assert_eq!(
            icon("r-lucide"),
            Some(("lucide".to_owned(), "Rocket".to_owned()))
        );
        assert_eq!(icon("r-emoji"), Some(("emoji".to_owned(), "🦀".to_owned())));
        assert_eq!(icon("r-bad"), None, "PNG가 아닌 data URL은 버린다");
        let raw = fs::read_to_string(state_dir.join(PROJECTS_FILE)).unwrap();
        assert!(
            !raw.contains("base64"),
            "data URL을 projects.toml에 넣지 않는다"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn imports_orca_terminal_themes_and_custom_themes() {
        let root = std::env::temp_dir().join(format!("owe956-orca-themes-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let orca_dir = root.join("orca");
        let state_dir = root.join("state");
        fs::create_dir_all(orca_dir.join("profiles").join("default")).unwrap();
        fs::create_dir_all(&state_dir).unwrap();

        let doc = serde_json::json!({
            "settings": {
                "terminalThemeDark": "custom:ghostty:catppuccin-mocha",
                "terminalThemeLight": "Catppuccin Latte",
                "terminalUseSeparateLightTheme": true,
                "terminalCustomThemes": [
                    {
                        "id": "ghostty:catppuccin-mocha",
                        "name": "Catppuccin Mocha",
                        "source": "ghostty",
                        "mode": "dark",
                        "sourceLabel": "ghostty",
                        "colors": {
                            "background": "#1e1e2e",
                            "foreground": "#cdd6f4",
                            "cursor": "#f5e0dc",
                            "black": "#45475a",
                            "red": "#f38ba8",
                            "green": "#a6e3a1",
                            "yellow": "#f9e2af",
                            "blue": "#89b4fa",
                            "magenta": "#f5c2e7",
                            "cyan": "#94e2d5",
                            "white": "#bac2de",
                            "brightBlack": "#585b70",
                            "brightRed": "#f38ba8",
                            "brightGreen": "#a6e3a1",
                            "brightYellow": "#f9e2af",
                            "brightBlue": "#89b4fa",
                            "brightMagenta": "#f5c2e7",
                            "brightCyan": "#94e2d5",
                            "brightWhite": "#a6adc8"
                        }
                    }
                ]
            }
        });
        fs::write(
            orca_dir
                .join("profiles")
                .join("default")
                .join("orca-data.json"),
            doc.to_string(),
        )
        .unwrap();

        let opts = ImportOptions {
            home: root.join("home"),
            state_dir: state_dir.clone(),
            orca_dir,
            domains: vec![ImportDomain::Settings],
            dry_run: false,
        };
        let report = run_import(&opts).unwrap();
        assert!(report.settings.settings_updated);

        let body = fs::read_to_string(state_dir.join("settings.toml")).unwrap();
        let table: toml::Table = toml::from_str(&body).unwrap();
        assert_eq!(
            table
                .get("terminal_theme_dark")
                .and_then(toml::Value::as_str),
            Some("custom:ghostty:catppuccin-mocha")
        );
        assert_eq!(
            table
                .get("terminal_theme_light")
                .and_then(toml::Value::as_str),
            Some("Catppuccin Latte")
        );
        assert_eq!(
            table
                .get("terminal_use_separate_light_theme")
                .and_then(toml::Value::as_bool),
            Some(true)
        );
        let customs = table
            .get("terminal_custom_themes")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert_eq!(customs.len(), 1);
        let first = customs[0].as_table().unwrap();
        assert_eq!(
            first.get("id").and_then(toml::Value::as_str),
            Some("ghostty:catppuccin-mocha")
        );
        assert_eq!(
            first.get("background").and_then(toml::Value::as_str),
            Some("#1e1e2e")
        );
        assert_eq!(
            first
                .get("ansi")
                .and_then(toml::Value::as_array)
                .map(Vec::len),
            Some(16)
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sessions_import_collects_open_tab_agents_and_skips_resumed() {
        let root = std::env::temp_dir().join(format!("orca-sessions-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let wt = root.join("wt");
        fs::create_dir_all(&wt).unwrap();
        let wt_key = format!("repo1::{}", wt.display());
        let gone_key = format!("repo1::{}", root.join("missing").display());
        let domains: BTreeMap<String, Json> = BTreeMap::from([(
            "workspaceSession".to_owned(),
            serde_json::json!({
                "tabsByWorktree": {
                    (wt_key.as_str()): [
                        {"id": "t1", "worktreeId": wt_key, "title": "shell",
                         "aiVaultTitle": {"agent": "claude", "sessionId": "c-1", "title": "작업 A"}},
                        {"id": "t2", "worktreeId": wt_key, "title": "split"},
                        {"id": "t3", "worktreeId": gone_key, "title": "gone",
                         "aiVaultTitle": {"agent": "claude", "sessionId": "c-gone"}}
                    ]
                },
                "sleepingAgentSessionsByPaneKey": {
                    // 같은 탭의 같은 세션은 한 번만, 분할 칸의 다른 에이전트는 따로.
                    "t1:l1": {"tabId": "t1", "worktreeId": wt_key, "agent": "claude",
                              "providerSession": {"key": "session_id", "id": "c-1"}},
                    "t2:l1": {"tabId": "t2", "worktreeId": wt_key, "agent": "antigravity",
                              "providerSession": {"key": "conversation_id", "id": "g-1"}},
                    // 닫힌 탭의 세션은 뺀다.
                    "t9:l1": {"tabId": "t9", "worktreeId": wt_key, "agent": "claude",
                              "providerSession": {"key": "session_id", "id": "c-closed"}}
                }
            }),
        )]);
        let found = orca_open_sessions(&domains);
        let ids: Vec<_> = found
            .iter()
            .map(|(t, title)| (t.agent.profile(), t.id.as_str(), title.as_deref()))
            .collect();
        assert_eq!(
            ids,
            [
                ("claude", "c-1", Some("작업 A")),
                ("agy", "g-1", Some("split")),
            ]
        );
        assert!(found.iter().all(|(t, _)| t.cwd == wt.display().to_string()));

        let opts = ImportOptions {
            home: root.join("home"),
            state_dir: root.join("state"),
            orca_dir: root.join("orca"),
            domains: vec![ImportDomain::Sessions],
            dry_run: true,
        };
        append_line(&opts.state_dir.join(SESSIONS_LEDGER_FILE), "claude:c-1").unwrap();
        let mut warnings = Vec::new();
        let summary = import_sessions_domain(&opts, &domains, &mut warnings);
        assert!(!summary.skipped);
        assert_eq!((summary.resumed, summary.already_resumed), (0, 1));
        assert_eq!(
            summary.items.iter().map(|i| i.resumed).collect::<Vec<_>>(),
            [true, false]
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        fs::remove_dir_all(&root).ok();
    }

    struct BeforeRenameGuard;

    impl Drop for BeforeRenameGuard {
        fn drop(&mut self) {
            BEFORE_RENAME.with(|h| h.borrow_mut().take());
        }
    }

    fn set_before_rename(hook: impl FnMut(&Path) + 'static) -> BeforeRenameGuard {
        BEFORE_RENAME.with(|h| {
            *h.borrow_mut() = Some(Box::new(hook));
        });
        BeforeRenameGuard
    }

    /// PC-13: `import_automations` must skip bot-scoped and one-shot entries so they never leak
    /// into global `schedules.toml`.
    #[test]
    fn import_automations_skips_bot_scoped_and_one_shot_entries() {
        let root = std::env::temp_dir().join(format!("krill-pc13-auto-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state_dir = root.join("state");
        let orca_dir = root.join("orca");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&orca_dir).unwrap();

        let bot_dir = home.join(".orca").join("bots").join("triage");
        fs::create_dir_all(&bot_dir).unwrap();
        fs::write(
            bot_dir.join("bot.json"),
            serde_json::json!({
                "id": "orca-triage-1",
                "name": "Triage Bot",
                "agent": "claude"
            })
            .to_string(),
        )
        .unwrap();
        fs::write(bot_dir.join("SOUL.md"), "# Triage\n\nRoutes bugs.\n").unwrap();

        fs::write(
            orca_dir.join("orca-data.json"),
            serde_json::json!({
                "settings": {},
                "automations": [
                    {
                        "id": "global-daily",
                        "name": "Global Daily",
                        "prompt": "Check repo health",
                        "agentId": "claude",
                        "rrule": "FREQ=DAILY;BYHOUR=7;BYMINUTE=0",
                        "enabled": true
                    },
                    {
                        "id": "global-null-once",
                        "name": "Global Null Once",
                        "prompt": "Check null once",
                        "agentId": "claude",
                        "rrule": "FREQ=DAILY;BYHOUR=10;BYMINUTE=15",
                        "once": null,
                        "enabled": true
                    },
                    {
                        "id": "bot-recurring",
                        "name": "Bot Recurring",
                        "prompt": "Check bot inbox",
                        "rrule": "0 9 * * 1-5",
                        "enabled": true,
                        "bot": { "botId": "orca-triage-1" }
                    },
                    {
                        "id": "global-once",
                        "name": "Global One Shot",
                        "prompt": "Run once only",
                        "agentId": "claude",
                        "rrule": "FREQ=DAILY;BYHOUR=8;BYMINUTE=0",
                        "once": { "at": 1_700_000_000_000_u64 },
                        "enabled": true
                    },
                    {
                        "id": "bot-once",
                        "name": "Bot One Shot",
                        "prompt": "Bot run once only",
                        "rrule": "0 9 * * *",
                        "once": { "at": 1_700_000_000_000_u64 },
                        "bot": { "botId": "orca-triage-1" }
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let opts = ImportOptions {
            home,
            state_dir: state_dir.clone(),
            orca_dir,
            domains: vec![ImportDomain::Settings, ImportDomain::Bots],
            dry_run: false,
        };
        let report = run_import(&opts).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.settings.schedules_merged, 2);
        assert_eq!(report.bots.schedules_migrated, 1);

        let schedules = Schedules::load(&state_dir).unwrap();
        let global_ids: BTreeSet<&str> = schedules.schedule.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            global_ids,
            BTreeSet::from(["orca-global-daily", "orca-global-null-once"])
        );

        let bots: Vec<BotConfig> = serde_json::from_str(
            &fs::read_to_string(state_dir.join(BOTS_DIR).join(BOTS_INDEX_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].schedules.len(), 1);
        assert_eq!(bots[0].schedules[0].id, "orca-bot-recurring");

        fs::remove_dir_all(&root).ok();
    }

    /// PC-14: `sqlite_immutable_uri` must percent-encode UTF-8 bytes without Latin-1 mojibake and
    /// normalize Windows separators and verbatim prefixes.
    #[test]
    fn sqlite_immutable_uri_percent_encodes_utf8_and_normalizes_windows_paths() {
        let unix_uri =
            sqlite_immutable_uri_from_raw("/Users/홍길동/Orca #1?v=2%3/profile-state.db", false);
        assert!(unix_uri.is_ascii(), "{unix_uri}");
        assert_eq!(
            unix_uri,
            "file:/Users/%ED%99%8D%EA%B8%B8%EB%8F%99/Orca%20%231%3Fv=2%253/profile-state.db?immutable=1"
        );

        let win_drive = sqlite_immutable_uri_from_raw(
            r"C:\Users\홍길동\AppData\Roaming\orca\profile-state.db",
            true,
        );
        assert_eq!(
            win_drive,
            "file:///C:/Users/%ED%99%8D%EA%B8%B8%EB%8F%99/AppData/Roaming/orca/profile-state.db?immutable=1"
        );

        let win_verbatim = sqlite_immutable_uri_from_raw(r"\\?\C:\Users\홍길동\db.sqlite", true);
        assert_eq!(
            win_verbatim,
            "file:///C:/Users/%ED%99%8D%EA%B8%B8%EB%8F%99/db.sqlite?immutable=1"
        );

        let win_unc = sqlite_immutable_uri_from_raw(r"\\server\share\홍길동\db.sqlite", true);
        assert_eq!(
            win_unc,
            "file:////server/share/%ED%99%8D%EA%B8%B8%EB%8F%99/db.sqlite?immutable=1"
        );

        let win_verbatim_unc =
            sqlite_immutable_uri_from_raw(r"\\?\UNC\server\share\홍길동\db.sqlite", true);
        assert_eq!(
            win_verbatim_unc,
            "file:////server/share/%ED%99%8D%EA%B8%B8%EB%8F%99/db.sqlite?immutable=1"
        );

        let win_verbatim_unc_lower =
            sqlite_immutable_uri_from_raw(r"\\?\unc\localhost\share\db.sqlite", true);
        assert_eq!(
            win_verbatim_unc_lower,
            "file:////localhost/share/db.sqlite?immutable=1"
        );

        let unix_double_slash = sqlite_immutable_uri_from_raw("//tmp/profile-state.db", false);
        assert_eq!(
            unix_double_slash,
            "file:////tmp/profile-state.db?immutable=1"
        );
        let unix_localhost = sqlite_immutable_uri_from_raw("//localhost/db.sqlite", false);
        assert_eq!(unix_localhost, "file:////localhost/db.sqlite?immutable=1");
    }

    /// PC-14: WAL-mode SQLite DBs without `-shm` under non-ASCII paths succeed via immutable URI.
    #[test]
    #[cfg_attr(windows, ignore = "requires sqlite3 CLI on PATH")]
    fn sqlite_immutable_uri_fallback_reads_wal_db_under_non_ascii_path() {
        let root = std::env::temp_dir().join(format!("krill-pc14-wal-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let db_dir = root.join("사용자 홍길동 #1?2%3");
        fs::create_dir_all(&db_dir).unwrap();
        let db_path = db_dir.join("profile-state.db");

        let sql = "\
PRAGMA journal_mode = WAL;
CREATE TABLE profile_state_documents (domain TEXT PRIMARY KEY, payload TEXT NOT NULL);
INSERT INTO profile_state_documents VALUES ('settings', '{\"theme\":\"dark\"}');
PRAGMA wal_checkpoint(TRUNCATE);
";
        let status = Command::new(sqlite3_binary())
            .arg(&db_path)
            .arg(sql)
            .status()
            .unwrap();
        assert!(status.success());
        fs::remove_file(db_dir.join("profile-state.db-wal")).ok();
        fs::remove_file(db_dir.join("profile-state.db-shm")).ok();

        let domains = query_sqlite_domains(&db_path).unwrap();
        assert_eq!(
            domains
                .get("settings")
                .and_then(|v| v.get("theme"))
                .and_then(Json::as_str),
            Some("dark")
        );

        fs::remove_dir_all(&root).ok();
    }

    /// PC-38: `import_orca_bot_groups` and `import_orca_bot_items` use the resolved `profile_id`
    /// fallback (`.bak`, empty `activeProfileId`, or missing index) instead of silently skipping.
    #[test]
    #[cfg_attr(windows, ignore = "requires sqlite3 CLI on PATH")]
    fn orca_bots_migrates_profile_scoped_groups_and_items_using_resolved_profile_fallback() {
        let cases = [
            (
                "bak-index",
                Some((
                    "orca-profile-index.json.bak",
                    r#"{"version":1,"activeProfileId":"work","profiles":[{"id":"work"}]}"#,
                )),
                "work",
            ),
            (
                "empty-active",
                Some((
                    "orca-profile-index.json",
                    r#"{"version":1,"activeProfileId":"","profiles":[{"id":"team"}]}"#,
                )),
                "team",
            ),
            ("missing-index", None, "default"),
        ];

        for (label, index_file, expected_profile) in cases {
            let root =
                std::env::temp_dir().join(format!("krill-pc38-{label}-{}", std::process::id()));
            fs::remove_dir_all(&root).ok();
            let home = root.join("home");
            let state_dir = root.join("state");
            let orca_dir = root.join("orca");
            let profile_dir = orca_dir.join("profiles").join(expected_profile);
            fs::create_dir_all(&state_dir).unwrap();
            fs::create_dir_all(&profile_dir).unwrap();

            if let Some((file_name, body)) = index_file {
                fs::write(orca_dir.join(file_name), body).unwrap();
            }

            let bot_dir = home.join(".orca").join("bots").join("triage");
            fs::create_dir_all(&bot_dir).unwrap();
            fs::write(
                bot_dir.join("bot.json"),
                serde_json::json!({
                    "id": "orca-triage-1",
                    "name": "Triage Bot",
                    "agent": "claude"
                })
                .to_string(),
            )
            .unwrap();
            fs::write(bot_dir.join("SOUL.md"), "# Triage\n\nRoutes bugs.\n").unwrap();

            let group_record = serde_json::json!({
                "id": "grp-profile-1",
                "name": "Profile Squad",
                "createdAt": 1_700_000_000_000_u64,
                "members": [{ "botId": "orca-triage-1" }]
            })
            .to_string();
            let orch_sql = format!(
                "CREATE TABLE bot_groups (id TEXT PRIMARY KEY, record TEXT NOT NULL); \
                 INSERT INTO bot_groups VALUES ('grp-profile-1', {});",
                sql_hex_text(&group_record)
            );
            let status_orch = Command::new(sqlite3_binary())
                .arg(profile_dir.join("orchestration.db"))
                .arg(orch_sql)
                .status()
                .unwrap();
            assert!(status_orch.success(), "{label}");

            let items_sql = "\
CREATE TABLE items (
    uid TEXT PRIMARY KEY,
    number INTEGER NOT NULL UNIQUE,
    owner_bot_id TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    done_means TEXT,
    status TEXT NOT NULL,
    close_note TEXT,
    snoozed_until INTEGER,
    version INTEGER NOT NULL,
    last_activity_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    closed_at INTEGER
);
CREATE TABLE item_links (item_uid TEXT NOT NULL, kind TEXT NOT NULL, ref TEXT NOT NULL);
CREATE TABLE item_events (id INTEGER PRIMARY KEY AUTOINCREMENT, item_uid TEXT NOT NULL, kind TEXT NOT NULL, actor TEXT NOT NULL, data TEXT NOT NULL, created_at INTEGER NOT NULL);
INSERT INTO items VALUES ('item-prof-1', 1, 'orca-triage-1', 'Check profile', NULL, NULL, 'planned', NULL, NULL, 1, 1700000000000, 1700000000000, 1700000000000, NULL);
";
            let status_items = Command::new(sqlite3_binary())
                .arg(profile_dir.join("bot-items.sqlite"))
                .arg(items_sql)
                .status()
                .unwrap();
            assert!(status_items.success(), "{label}");

            let opts = ImportOptions {
                home,
                state_dir,
                orca_dir,
                domains: vec![ImportDomain::Bots],
                dry_run: false,
            };
            let report = run_import(&opts).unwrap();
            assert!(report.warnings.is_empty(), "{label}: {:?}", report.warnings);
            assert_eq!(report.profile_id, expected_profile, "{label}");
            assert_eq!(report.bots.groups_migrated, 1, "{label}");
            assert_eq!(report.bots.items_migrated, 1, "{label}");

            fs::remove_dir_all(&root).ok();
        }
    }

    /// PC-39: A `bots.json` CAS conflict rolls back newly created bot homes so retry reuses the
    /// original slug (no orphan dir), and persistent conflict leaves no orphaned home on disk.
    #[test]
    fn bots_json_conflict_cleans_up_home_and_retries_without_orphan_slug() {
        // 1. Transient conflict on attempt 1 -> retry merges cleanly with original slug `pr-reviewer`.
        let root =
            std::env::temp_dir().join(format!("krill-pc39-bots-retry-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state_dir = root.join("state");
        let orca_dir = root.join("orca");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&orca_dir).unwrap();

        let bot_dir = home.join(".orca").join("bots").join("pr-reviewer");
        fs::create_dir_all(&bot_dir).unwrap();
        fs::write(
            bot_dir.join("bot.json"),
            serde_json::json!({
                "id": "orca-bot-1",
                "name": "PR Reviewer",
                "agent": "claude"
            })
            .to_string(),
        )
        .unwrap();
        fs::write(bot_dir.join("SOUL.md"), "# PR Reviewer\n\nReviews PRs.\n").unwrap();

        let index_path = state_dir.join(BOTS_DIR).join(BOTS_INDEX_FILE);
        let concurrent_bot = BotConfig::new(
            BotId::new("concurrent-bot").unwrap(),
            "Concurrent Bot",
            "claude",
            1_700_000_000_000,
        )
        .unwrap();
        let concurrent_json = format!(
            "{}\n",
            serde_json::to_string_pretty(&[concurrent_bot]).unwrap()
        );
        let target = index_path.clone();
        let mut fired = false;
        let guard = set_before_rename(move |path: &Path| {
            if path == target && !fired {
                fired = true;
                fs::write(path, &concurrent_json).unwrap();
            }
        });
        let opts = ImportOptions {
            home,
            state_dir: state_dir.clone(),
            orca_dir,
            domains: vec![ImportDomain::Bots],
            dry_run: false,
        };
        let report = run_import(&opts).unwrap();
        drop(guard);

        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.bots.bots_added, 1);
        let saved: Vec<BotConfig> =
            serde_json::from_str(&fs::read_to_string(&index_path).unwrap()).unwrap();
        assert_eq!(saved.len(), 2);
        let imported = saved
            .iter()
            .find(|b| b.id.as_str() == "orca-bot-1")
            .unwrap();
        assert_eq!(imported.slug.as_str(), "pr-reviewer");
        assert!(state_dir.join(BOTS_DIR).join("pr-reviewer").is_dir());
        assert!(!state_dir.join(BOTS_DIR).join("pr-reviewer-2").exists());
        fs::remove_dir_all(&root).ok();

        // 2. Persistent conflict on all attempts -> no orphaned home dir left behind.
        let root2 =
            std::env::temp_dir().join(format!("krill-pc39-bots-giveup-{}", std::process::id()));
        fs::remove_dir_all(&root2).ok();
        let home2 = root2.join("home");
        let state_dir2 = root2.join("state");
        let orca_dir2 = root2.join("orca");
        fs::create_dir_all(&state_dir2).unwrap();
        fs::create_dir_all(&orca_dir2).unwrap();

        let bot_dir2 = home2.join(".orca").join("bots").join("pr-reviewer");
        fs::create_dir_all(&bot_dir2).unwrap();
        fs::write(bot_dir2.join("SOUL.md"), "# PR Reviewer\n\nReviews PRs.\n").unwrap();

        let index_path2 = state_dir2.join(BOTS_DIR).join(BOTS_INDEX_FILE);
        let target2 = index_path2.clone();
        let mut n = 0_u64;
        let guard2 = set_before_rename(move |path: &Path| {
            if path == target2 {
                n += 1;
                let other = BotConfig::new(
                    BotId::new(format!("concurrent-{n}")).unwrap(),
                    "Concurrent",
                    "claude",
                    1_700_000_000_000 + n,
                )
                .unwrap();
                let body = format!("{}\n", serde_json::to_string_pretty(&[other]).unwrap());
                fs::write(path, body).unwrap();
            }
        });
        let report2 = run_import(&ImportOptions {
            home: home2,
            state_dir: state_dir2.clone(),
            orca_dir: orca_dir2,
            domains: vec![ImportDomain::Bots],
            dry_run: false,
        })
        .unwrap();
        drop(guard2);

        assert_eq!(report2.bots.bots_added, 0);
        assert!(
            report2
                .warnings
                .iter()
                .any(|w| w.starts_with("bots skipped") && w.contains("retried 3 times")),
            "{:?}",
            report2.warnings
        );
        assert!(!state_dir2.join(BOTS_DIR).join("pr-reviewer").exists());
        assert!(!state_dir2.join(BOTS_DIR).join("pr-reviewer-2").exists());
        fs::remove_dir_all(&root2).ok();
    }

    /// PC-39: Conflicts on `imported_sources.json` and `suggestions.json` retry independently
    /// without re-running `bots.json` or duplicating bots.
    #[test]
    fn bots_secondary_store_conflicts_retry_independently_without_duplicating_bots() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        let root =
            std::env::temp_dir().join(format!("krill-pc39-secondary-cas-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state_dir = root.join("state");
        let orca_dir = root.join("orca");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&orca_dir).unwrap();

        let bot_dir = home.join(".orca").join("bots").join("triage");
        fs::create_dir_all(&bot_dir).unwrap();
        fs::write(
            bot_dir.join("bot.json"),
            serde_json::json!({
                "id": "orca-triage-1",
                "name": "Triage Bot",
                "agent": "claude"
            })
            .to_string(),
        )
        .unwrap();
        fs::write(bot_dir.join("SOUL.md"), "# Triage\n\nRoutes bugs.\n").unwrap();
        fs::write(
            orca_dir.join("bot-look-around.json"),
            serde_json::json!({
                "bots": {
                    "orca-triage-1": {
                        "suggestions": [
                            { "id": "s-orca", "title": "Check flake", "why": "2 retries", "ask": "Re-run?", "status": "open", "createdAt": 100 }
                        ]
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let bots_root = state_dir.join(BOTS_DIR);
        let index_path = bots_root.join(BOTS_INDEX_FILE);
        let ledger_path = bots_root.join(IMPORTED_SOURCES_FILE);
        let suggestions_path = bots_root.join(SUGGESTIONS_FILE);

        let concurrent_suggestion = BotSuggestion {
            id: "s-concurrent".to_owned(),
            bot_id: BotId::new("orca-triage-1").unwrap(),
            title: "Concurrent".to_owned(),
            why: "Saved concurrently".to_owned(),
            ask: "Keep?".to_owned(),
            link: None,
            source: None,
            dismissed: false,
            created_at_ms: 50,
        };
        let concurrent_sugg_json = format!(
            "{}\n",
            serde_json::to_string_pretty(&[concurrent_suggestion]).unwrap()
        );
        let bots_writes = Arc::new(AtomicUsize::new(0));
        let bots_writes_hook = Arc::clone(&bots_writes);
        let mut ledger_fired = false;
        let mut sugg_fired = false;
        let idx_target = index_path.clone();
        let ledger_target = ledger_path.clone();
        let sugg_target = suggestions_path.clone();

        let guard = set_before_rename(move |path: &Path| {
            if path == idx_target {
                bots_writes_hook.fetch_add(1, Ordering::Relaxed);
            } else if path == ledger_target && !ledger_fired {
                ledger_fired = true;
                fs::write(path, "{\"/other/src\":\"other-id\"}\n").unwrap();
            } else if path == sugg_target && !sugg_fired {
                sugg_fired = true;
                fs::write(path, &concurrent_sugg_json).unwrap();
            }
        });

        let report = run_import(&ImportOptions {
            home,
            state_dir,
            orca_dir,
            domains: vec![ImportDomain::Bots],
            dry_run: false,
        })
        .unwrap();
        drop(guard);

        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(bots_writes.load(Ordering::Relaxed), 1);
        assert_eq!(report.bots.bots_added, 1);
        assert_eq!(report.bots.suggestions_migrated, 1);

        let bots: Vec<BotConfig> =
            serde_json::from_str(&fs::read_to_string(&index_path).unwrap()).unwrap();
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].id.as_str(), "orca-triage-1");

        let ledger: BTreeMap<String, String> =
            serde_json::from_str(&fs::read_to_string(&ledger_path).unwrap()).unwrap();
        assert_eq!(
            ledger.get("/other/src").map(String::as_str),
            Some("other-id")
        );
        assert_eq!(ledger.len(), 2);

        let suggestions: Vec<BotSuggestion> =
            serde_json::from_str(&fs::read_to_string(&suggestions_path).unwrap()).unwrap();
        let sugg_ids: BTreeSet<&str> = suggestions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(sugg_ids, BTreeSet::from(["s-concurrent", "s-orca"]));

        fs::remove_dir_all(&root).ok();
    }

    /// PC-39: A `groups.json` CAS conflict retries only group migration without re-writing `bots.json`.
    #[test]
    #[cfg_attr(windows, ignore = "requires sqlite3 CLI on PATH")]
    fn bots_groups_conflict_retries_independently_without_rewriting_bots_json() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        let root =
            std::env::temp_dir().join(format!("krill-pc39-groups-cas-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state_dir = root.join("state");
        let orca_dir = root.join("orca");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&orca_dir).unwrap();

        let bot_dir = home.join(".orca").join("bots").join("triage");
        fs::create_dir_all(&bot_dir).unwrap();
        fs::write(
            bot_dir.join("bot.json"),
            serde_json::json!({
                "id": "orca-triage-1",
                "name": "Triage Bot",
                "agent": "claude"
            })
            .to_string(),
        )
        .unwrap();
        fs::write(bot_dir.join("SOUL.md"), "# Triage\n\nRoutes bugs.\n").unwrap();

        let group_record = serde_json::json!({
            "id": "grp-orca",
            "name": "Orca Squad",
            "createdAt": 1_700_000_000_000_u64,
            "members": [{ "botId": "orca-triage-1" }]
        })
        .to_string();
        let orch_sql = format!(
            "CREATE TABLE bot_groups (id TEXT PRIMARY KEY, record TEXT NOT NULL); \
             INSERT INTO bot_groups VALUES ('grp-orca', {});",
            sql_hex_text(&group_record)
        );
        let status = Command::new(sqlite3_binary())
            .arg(orca_dir.join("orchestration.db"))
            .arg(orch_sql)
            .status()
            .unwrap();
        assert!(status.success());

        let bots_root = state_dir.join(BOTS_DIR);
        let index_path = bots_root.join(BOTS_INDEX_FILE);
        let groups_path = bots_root.join(GROUPS_FILE);
        let concurrent_group = BotGroupChat {
            id: "grp-concurrent".to_owned(),
            name: "Concurrent Squad".to_owned(),
            bot_ids: vec![BotId::new("orca-triage-1").unwrap()],
            description: String::new(),
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_000_000,
        };
        let concurrent_groups_json = format!(
            "{}\n",
            serde_json::to_string_pretty(&[concurrent_group]).unwrap()
        );

        let bots_writes = Arc::new(AtomicUsize::new(0));
        let bots_writes_hook = Arc::clone(&bots_writes);
        let idx_target = index_path.clone();
        let groups_target = groups_path.clone();
        let mut groups_fired = false;

        let guard = set_before_rename(move |path: &Path| {
            if path == idx_target {
                bots_writes_hook.fetch_add(1, Ordering::Relaxed);
            } else if path == groups_target && !groups_fired {
                groups_fired = true;
                fs::write(path, &concurrent_groups_json).unwrap();
            }
        });

        let report = run_import(&ImportOptions {
            home,
            state_dir,
            orca_dir,
            domains: vec![ImportDomain::Bots],
            dry_run: false,
        })
        .unwrap();
        drop(guard);

        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(bots_writes.load(Ordering::Relaxed), 1);
        assert_eq!(report.bots.bots_added, 1);
        assert_eq!(report.bots.groups_migrated, 1);

        let groups: Vec<BotGroupChat> =
            serde_json::from_str(&fs::read_to_string(&groups_path).unwrap()).unwrap();
        let group_ids: BTreeSet<&str> = groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(group_ids, BTreeSet::from(["grp-concurrent", "grp-orca"]));

        fs::remove_dir_all(&root).ok();
    }

    /// PC-40: `rrule_to_utc_cron` preserves `BYDAY` on `FREQ=DAILY` instead of expanding to `*`.
    #[test]
    fn rrule_to_utc_cron_preserves_byday_for_daily_frequency() {
        assert_eq!(
            rrule_to_utc_cron(
                Some("FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR;BYHOUR=9;BYMINUTE=30"),
                None
            ),
            "30 9 * * 1,2,3,4,5"
        );
        assert_eq!(
            rrule_to_utc_cron(
                Some("RRULE:FREQ=DAILY;BYDAY=MO,WE,FR"),
                Some("2026-10-01T08:15:00Z")
            ),
            "15 8 * * 1,3,5"
        );
        assert_eq!(
            rrule_to_utc_cron(Some("FREQ=DAILY;BYHOUR=9;BYMINUTE=30"), None),
            "30 9 * * *"
        );

        let root = std::env::temp_dir().join(format!("krill-pc40-byday-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let opts = ImportOptions {
            home: root.join("home"),
            state_dir: root.join("state"),
            orca_dir: root.join("orca"),
            domains: vec![ImportDomain::Settings],
            dry_run: false,
        };
        fs::create_dir_all(&opts.home).unwrap();
        fs::create_dir_all(&opts.state_dir).unwrap();
        let domains = BTreeMap::from([(
            "automations".to_owned(),
            serde_json::json!([
                {
                    "id": "weekdays-daily",
                    "name": "Weekdays Daily",
                    "prompt": "Weekday standup prep",
                    "agentId": "claude",
                    "rrule": "FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR;BYHOUR=9;BYMINUTE=30",
                    "enabled": true
                }
            ]),
        )]);
        let mut warnings = Vec::new();
        assert_eq!(
            import_automations(&opts, &domains, &serde_json::json!({}), &mut warnings).unwrap(),
            1
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let schedules = Schedules::load(&opts.state_dir).unwrap();
        assert_eq!(schedules.schedule.len(), 1);
        assert_eq!(schedules.schedule[0].cron, "30 9 * * 1,2,3,4,5");
        assert_eq!(
            schedule::classify_schedule(&schedules.schedule[0].cron).preset,
            schedule::SchedulePreset::Weekdays
        );
        fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
mod source_discovery_tests {
    use super::*;
    #[test]
    fn windows_roaming_candidates_prefer_profile_index_and_keep_override() {
        let root =
            std::env::temp_dir().join(format!("krill-orca-discovery-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let custom = root.join("custom-roaming");
        let fallback = root.join("home/AppData/Roaming");
        fs::create_dir_all(custom.join("orca")).unwrap();
        fs::create_dir_all(fallback.join("Orca/profiles/default")).unwrap();
        let discovered = discover_orca_dir([custom.clone(), fallback.clone()]);
        assert!(same_file::is_same_file(&discovered, fallback.join("Orca")).unwrap());
        assert!(discovered.join("profiles/default").is_dir());
        fs::write(custom.join("orca/orca-profile-index.json"), "{}").unwrap();
        let discovered = discover_orca_dir([custom.clone(), fallback]);
        assert!(same_file::is_same_file(&discovered, custom.join("orca")).unwrap());
        assert!(discovered.join("orca-profile-index.json").is_file());
        // from_env preserves explicit --orca-dir and KRILL_ORCA_DIR before discovery.
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod multi_provider_tests {
    use super::*;

    #[test]
    fn orca_import_preserves_grok_and_muse_presets() {
        let root =
            std::env::temp_dir().join(format!("krill-orca-multi-provider-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let orca_dir = root.join("orca");
        let state_dir = root.join("state");
        fs::create_dir_all(&orca_dir).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let payload = serde_json::json!({
            "settings": {
                "defaultTuiAgent": "grok",
                "agentCmdOverrides": {"muse": "/custom/muse"},
                "agentDefaultArgs": {
                    "grok": "-m grok-4",
                    "muse": ["--fast"]
                },
                "agentDefaultEnv": {
                    "grok": {"XAI_API_KEY": "secret"}
                }
            }
        });
        fs::write(orca_dir.join("orca-data.json"), payload.to_string()).unwrap();

        let opts = ImportOptions {
            home: root.join("home"),
            state_dir: state_dir.clone(),
            orca_dir,
            domains: vec![ImportDomain::Settings],
            dry_run: false,
        };
        let report = run_import(&opts).unwrap();
        assert_eq!(report.settings.default_agent.as_deref(), Some("grok"));
        assert!(report.settings.agents_updated >= 2);

        let settings_toml = fs::read_to_string(state_dir.join(SETTINGS_FILE)).unwrap();
        assert!(
            settings_toml.contains("default_agent = \"grok\""),
            "{settings_toml}"
        );

        let agents_toml = fs::read_to_string(state_dir.join(agent::FILE_NAME)).unwrap();
        let loaded = Agents::load(Some(&agents_toml)).unwrap();
        assert_eq!(loaded.profiles["grok"].launch, ["grok", "-m", "grok-4"]);
        assert_eq!(
            loaded.profiles["grok"]
                .env
                .get("XAI_API_KEY")
                .map(String::as_str),
            Some("secret")
        );
        assert_eq!(loaded.profiles["muse"].launch, ["/custom/muse", "--fast"]);

        fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
mod bots_import_tests {
    use super::*;

    fn test_png_with_metadata() -> Vec<u8> {
        fn chunk(kind: [u8; 4], data: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
            let mut body = Vec::with_capacity(data.len().saturating_add(4));
            body.extend_from_slice(&kind);
            body.extend_from_slice(data);
            let crc = krill_proto::bots::crc32(&body);
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc.to_be_bytes());
            out
        }
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&chunk(*b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]));
        png.extend_from_slice(&chunk(*b"tEXt", b"Author\0OrcaSecretMeta"));
        png.extend_from_slice(&chunk(
            *b"IDAT",
            &[0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01],
        ));
        png.extend_from_slice(&chunk(*b"IEND", &[]));
        png
    }

    #[test]
    fn import_domain_parses_bots_and_includes_in_all() {
        assert_eq!(ImportDomain::parse("bots").unwrap(), ImportDomain::Bots);
        assert_eq!(ImportDomain::parse("bot").unwrap(), ImportDomain::Bots);
        assert!(ImportDomain::ALL.contains(&ImportDomain::Bots));
        assert_eq!(
            ImportDomain::parse_list("settings,bots").unwrap(),
            vec![ImportDomain::Settings, ImportDomain::Bots]
        );
    }

    #[test]
    fn orca_bots_domain_dry_run_apply_and_idempotency() {
        let root =
            std::env::temp_dir().join(format!("krill-orca-bots-import-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let orca_dir = root.join("orca");
        let state_dir = root.join("state");
        fs::create_dir_all(&orca_dir).unwrap();
        fs::create_dir_all(&state_dir).unwrap();

        // Seed ~/.orca/bots/pr-reviewer/
        let bot_dir = home.join(".orca/bots/pr-reviewer");
        fs::create_dir_all(bot_dir.join("memory")).unwrap();
        fs::create_dir_all(bot_dir.join("skills/pr-review-checklist")).unwrap();
        fs::write(
            bot_dir.join("bot.json"),
            serde_json::json!({
                "id": "orca-bot-1",
                "name": "PR Reviewer",
                "agent": "codex",
                "avatar": "B1",
                "members": [
                    { "id": "U100", "name": "Alice", "source": "slack", "workspaceId": "T1" }
                ]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            bot_dir.join("SOUL.md"),
            "# PR Reviewer\n\nReviews pull requests carefully.\n",
        )
        .unwrap();
        fs::write(
            bot_dir.join("MEMORY.md"),
            "# Memory\n\n- 2026-11-19 Watch retry timeouts.\n",
        )
        .unwrap();
        fs::write(
            bot_dir.join("memory/repos.md"),
            "# Repos\n\n- craftowen/krill\n",
        )
        .unwrap();
        fs::write(
            bot_dir.join("skills/pr-review-checklist/SKILL.md"),
            "---\nname: pr-review-checklist\n---\n",
        )
        .unwrap();
        fs::write(bot_dir.join("avatar.png"), test_png_with_metadata()).unwrap();

        // Also seed profile metadata in orca-data.json
        fs::write(
            orca_dir.join("orca-data.json"),
            serde_json::json!({
                "bots": [{
                    "id": "orca-bot-1",
                    "name": "PR Reviewer",
                    "folder": "pr-reviewer",
                    "agent": "codex",
                    "templateId": "pr-review",
                    "paused": true,
                    "launch": { "model": "gpt-5", "effort": "high" }
                }]
            })
            .to_string(),
        )
        .unwrap();

        // 1. Dry-run: detects bot, writes zero files under state_dir/bots
        let dry_opts = ImportOptions {
            home: home.clone(),
            state_dir: state_dir.clone(),
            orca_dir: orca_dir.clone(),
            domains: vec![ImportDomain::Bots],
            dry_run: true,
        };
        let dry_report = run_import(&dry_opts).unwrap();
        assert!(!dry_report.bots.skipped);
        assert_eq!(dry_report.bots.bots_added, 1);
        assert_eq!(dry_report.bots.bots_skipped, 0);
        assert_eq!(dry_report.bots.items.len(), 1);
        assert_eq!(dry_report.bots.items[0].slug, "pr-reviewer");
        assert_eq!(dry_report.bots.items[0].agent_id, "codex");
        assert!(dry_report.bots.items[0].has_soul);
        assert!(dry_report.bots.items[0].has_memory);
        assert_eq!(dry_report.bots.items[0].memory_topic_count, 1);
        assert!(!dry_report.bots.items[0].already_exists);
        assert!(!state_dir.join("bots").exists());

        // 2. Apply: writes bots.json + home files, strips PNG metadata
        let apply_opts = ImportOptions {
            dry_run: false,
            ..dry_opts.clone()
        };
        let applied = run_import(&apply_opts).unwrap();
        assert_eq!(applied.bots.bots_added, 1);
        assert_eq!(applied.bots.bots_skipped, 0);

        let krill_bots_root = state_dir.join("bots");
        let index: Vec<BotConfig> =
            serde_json::from_str(&fs::read_to_string(krill_bots_root.join("bots.json")).unwrap())
                .unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].id.as_str(), "orca-bot-1");
        assert_eq!(index[0].slug.as_str(), "pr-reviewer");
        assert_eq!(index[0].agent_id, "codex");
        assert_eq!(index[0].model.as_deref(), Some("gpt-5"));
        assert_eq!(index[0].effort.as_deref(), Some("high"));
        assert_eq!(index[0].template_id.as_deref(), Some("pr-review"));
        assert!(index[0].paused);
        assert_eq!(index[0].members.len(), 1);
        assert_eq!(index[0].description, "Reviews pull requests carefully.");

        let krill_home = krill_bots_root.join("pr-reviewer");
        assert_eq!(
            fs::read_to_string(krill_home.join("SOUL.md")).unwrap(),
            "# PR Reviewer\n\nReviews pull requests carefully.\n"
        );
        assert_eq!(
            fs::read_to_string(krill_home.join("MEMORY.md")).unwrap(),
            "# Memory\n\n- 2026-11-19 Watch retry timeouts.\n"
        );
        assert_eq!(
            fs::read_to_string(krill_home.join("memory/repos.md")).unwrap(),
            "# Repos\n\n- craftowen/krill\n"
        );
        assert!(
            krill_home
                .join("skills/pr-review-checklist/SKILL.md")
                .is_file()
        );
        assert!(krill_home.join("AGENTS.md").is_file());
        assert_eq!(
            fs::read_to_string(krill_home.join("CLAUDE.md")).unwrap(),
            "@AGENTS.md\n"
        );
        let copied_avatar = fs::read(krill_home.join("avatar.png")).unwrap();
        assert!(!copied_avatar.windows(4).any(|w| w == b"tEXt"));

        // 3. User modifies SOUL.md and MEMORY.md, then re-runs import -> must skip safely!
        fs::write(krill_home.join("SOUL.md"), "# User Edited Soul\n").unwrap();
        fs::write(
            krill_home.join("MEMORY.md"),
            "# Memory\n\n- Keep my edits\n",
        )
        .unwrap();

        // Also add a second Orca bot to verify incremental migration
        let bot2 = home.join(".orca/bots/release-notes");
        fs::create_dir_all(&bot2).unwrap();
        fs::write(
            bot2.join("SOUL.md"),
            "# Release notes\n\nDrafts changelogs.\n",
        )
        .unwrap();

        let rerun = run_import(&apply_opts).unwrap();
        assert_eq!(rerun.bots.bots_added, 1);
        assert_eq!(rerun.bots.bots_skipped, 1);
        assert_eq!(
            fs::read_to_string(krill_home.join("SOUL.md")).unwrap(),
            "# User Edited Soul\n"
        );
        assert_eq!(
            fs::read_to_string(krill_home.join("MEMORY.md")).unwrap(),
            "# Memory\n\n- Keep my edits\n"
        );
        assert!(krill_bots_root.join("release-notes/SOUL.md").is_file());

        let human = format_report_human(&rerun);
        assert!(human.contains("bots: +1 added, 1 existing skipped"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn discovers_openclaw_hermes_and_openbot_fixtures_without_reading_secrets() {
        let root = std::env::temp_dir().join(format!(
            "krill-external-bots-discovery-{}",
            std::process::id()
        ));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");

        // 1. OpenClaw (.openclaw with IDENTITY.md + planted secret in openclaw.json)
        fs::create_dir_all(home.join(".openclaw/agents/main/agent")).unwrap();
        fs::create_dir_all(home.join(".openclaw/agents/scout")).unwrap();
        fs::create_dir_all(home.join(".openclaw/workspace")).unwrap();
        fs::create_dir_all(home.join(".openclaw/workspace-scout")).unwrap();
        fs::write(
            home.join(".openclaw/openclaw.json"),
            r#"{"channels":{"slack":{"botToken":"xoxb-PLANTED-NEVER-READ"}}}"#,
        )
        .unwrap();
        fs::write(
            home.join(".openclaw/workspace/IDENTITY.md"),
            "# Identity\n\n- **Name:** Molty\n- **Vibe:** calm\n",
        )
        .unwrap();
        fs::write(
            home.join(".openclaw/workspace/SOUL.md"),
            "# Molty\n\nCalm engineering helper.\n",
        )
        .unwrap();

        // 2. Hermes (~/.hermes root + profiles/research-desk, ignoring empty & .deleted)
        fs::create_dir_all(home.join(".hermes/profiles/research-desk")).unwrap();
        fs::create_dir_all(home.join(".hermes/profiles/empty")).unwrap();
        fs::create_dir_all(home.join(".hermes/profiles/.deleted/old")).unwrap();
        fs::write(
            home.join(".hermes/SOUL.md"),
            "# Hermes\n\nYou are Hermes.\n",
        )
        .unwrap();
        fs::write(
            home.join(".hermes/profiles/research-desk/config.yaml"),
            "model: x\n",
        )
        .unwrap();
        fs::write(home.join(".hermes/profiles/.deleted/old/SOUL.md"), "old").unwrap();

        // 3. OpenBot (data dir + Agents/agent-research + Agents/bot-<uuid>)
        let openbot_data = root.join("app-data/OpenBot");
        fs::create_dir_all(&openbot_data).unwrap();
        fs::create_dir_all(home.join("OpenBot/Agents/agent-research")).unwrap();
        fs::create_dir_all(home.join("OpenBot/Agents/bot-8c41d0f2-6b5a-4e93-8d17-2a0b3c4d5e6f"))
            .unwrap();

        let found = discover_external_bots_in(&home, Some(&openbot_data), None, None, None);
        let serialized = serde_json::to_string(&found).unwrap();
        assert!(!serialized.contains("xoxb-PLANTED-NEVER-READ"));

        let openclaw: Vec<_> = found
            .iter()
            .filter(|c| c.app == ExternalBotSourceApp::OpenClaw)
            .collect();
        assert_eq!(openclaw.len(), 2);
        assert_eq!(openclaw[0].agent, "main");
        assert_eq!(openclaw[0].name, "Molty");
        assert_eq!(
            openclaw[0].path,
            home.join(".openclaw")
                .join("workspace")
                .display()
                .to_string()
        );
        assert_eq!(
            openclaw[0].soul_preview.as_deref(),
            Some("Calm engineering helper.")
        );
        assert_eq!(openclaw[1].agent, "scout");
        assert_eq!(openclaw[1].name, "Scout");
        assert_eq!(
            openclaw[1].path,
            home.join(".openclaw")
                .join("workspace-scout")
                .display()
                .to_string()
        );

        let hermes: Vec<_> = found
            .iter()
            .filter(|c| c.app == ExternalBotSourceApp::Hermes)
            .collect();
        assert_eq!(hermes.len(), 2);
        assert_eq!(hermes[0].agent, "default");
        assert_eq!(hermes[0].name, "Hermes");
        assert_eq!(hermes[1].agent, "research-desk");
        assert_eq!(hermes[1].name, "Research desk");

        let openbot: Vec<_> = found
            .iter()
            .filter(|c| c.app == ExternalBotSourceApp::OpenBot)
            .collect();
        assert_eq!(openbot.len(), 2);
        assert_eq!(openbot[0].name, "Research");
        assert_eq!(openbot[1].name, "OpenBot agent");

        // Legacy OpenClaw folder fallback (`.clawdbot/workspace` without `agents/`)
        let legacy_home = root.join("legacy-home");
        fs::create_dir_all(legacy_home.join(".clawdbot/workspace")).unwrap();
        let legacy = discover_external_bots_in(
            &legacy_home,
            None,
            None,
            None,
            Some(ExternalBotSourceApp::OpenClaw),
        );
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].agent, "main");
        assert_eq!(
            legacy[0].path,
            legacy_home
                .join(".clawdbot")
                .join("workspace")
                .display()
                .to_string()
        );

        // Legacy `.moltbot` fallback
        let molt_home = root.join("molt-home");
        fs::create_dir_all(molt_home.join(".moltbot/workspace")).unwrap();
        let molt = discover_external_bots_in(
            &molt_home,
            None,
            None,
            None,
            Some(ExternalBotSourceApp::OpenClaw),
        );
        assert_eq!(molt.len(), 1);
        assert_eq!(
            molt[0].path,
            molt_home
                .join(".moltbot")
                .join("workspace")
                .display()
                .to_string()
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn orca_bots_handles_v2_fields_slug_collision_nested_topics_and_ledger() {
        let root = std::env::temp_dir().join(format!("krill-orca-bots-v2-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let orca_dir = root.join("orca");
        let state_dir = root.join("state");
        fs::create_dir_all(&orca_dir).unwrap();
        fs::create_dir_all(&state_dir).unwrap();

        // Two non-ASCII named bots in distinct folders whose derived slug both fall back to `bot`!
        let b1 = home.join("Library/Application Support/Orca/bots/크릴봇");
        let b2 = home.join(".orca/bots/도움봇");
        fs::create_dir_all(b1.join("memory/nested/deep/too-deep")).unwrap();
        fs::create_dir_all(&b2).unwrap();

        fs::write(
            b1.join("bot.json"),
            serde_json::json!({
                "id": "orca-korean-1",
                "name": "크릴봇",
                "agent": "openclaw",
                "skillProjects": ["code-review", "release-notes"],
                "lookAround": { "on": true, "every": "twice-daily", "weekdaysOnly": false, "focus": "PR backlog" },
                "tidyUp": { "on": false, "every": "week" },
                "slack": { "appId": "A1", "teamId": "T1", "ownerUserId": "U999" },
                "slackMirror": true,
                "linearWorkspaces": [{ "workspaceId": "W1", "workspaceName": "Ops" }],
                "github": { "ownerUserId": "G1" },
                "google": true,
                "firstRun": true
            })
            .to_string(),
        )
        .unwrap();
        fs::write(b1.join("SOUL.md"), "# 크릴봇\n\nHelpful.\n").unwrap();
        fs::write(b1.join("memory/top.md"), "top\n").unwrap();
        fs::write(b1.join("memory/nested/mid.md"), "mid\n").unwrap();
        fs::write(b1.join("memory/nested/deep/d2.md"), "d2\n").unwrap();
        fs::write(b1.join("memory/nested/deep/too-deep/d3.md"), "d3\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(b1.join("SOUL.md"), b1.join("memory/evil-link.md")).ok();

        fs::write(
            b2.join("bot.json"),
            serde_json::json!({
                "id": "orca-korean-2",
                "name": "도움봇",
                "agent": "claude",
                "firstRun": false
            })
            .to_string(),
        )
        .unwrap();
        fs::write(b2.join("SOUL.md"), "# 도움봇\n\nHelper.\n").unwrap();

        let opts = ImportOptions {
            home: home.clone(),
            state_dir: state_dir.clone(),
            orca_dir: orca_dir.clone(),
            domains: vec![ImportDomain::Bots],
            dry_run: false,
        };
        let report = run_import(&opts).unwrap();
        assert_eq!(report.bots.bots_added, 2);
        assert_eq!(report.bots.bots_skipped, 0);

        let index: Vec<BotConfig> =
            serde_json::from_str(&fs::read_to_string(state_dir.join("bots/bots.json")).unwrap())
                .unwrap();
        assert_eq!(index.len(), 2);
        let first = index
            .iter()
            .find(|b| b.id.as_str() == "orca-korean-1")
            .unwrap();
        let second = index
            .iter()
            .find(|b| b.id.as_str() == "orca-korean-2")
            .unwrap();
        assert_ne!(first.slug, second.slug);
        assert_eq!(first.agent_id, "openclaw");
        assert!(!first.first_run_completed);
        assert!(second.first_run_completed);
        assert_eq!(first.skills, vec!["code-review", "release-notes"]);
        assert_eq!(first.look_around.interval_secs, 43_200);
        assert!(!first.look_around.weekdays_only);
        assert_eq!(first.look_around.focus, "PR backlog");
        assert!(!first.memory_tidy.enabled);
        assert_eq!(first.memory_tidy.interval_secs, 7 * 86_400);
        assert!(first.connectors.slack.enabled);
        assert!(first.connectors.slack.mirror_desktop);
        assert_eq!(first.connectors.slack.allowed_user_ids, vec!["U999"]);
        assert!(first.connectors.linear.enabled);
        assert!(first.connectors.github.enabled);
        assert!(first.connectors.google.enabled);
        assert!(first.connectors.google.services.gmail);
        assert!(first.connectors.google.services.calendar);

        let first_mem = state_dir
            .join("bots")
            .join(first.slug.as_str())
            .join("memory");
        assert!(first_mem.join("top.md").is_file());
        assert!(first_mem.join("nested/mid.md").is_file());
        assert!(first_mem.join("nested/deep/d2.md").is_file());
        assert!(!first_mem.join("nested/deep/too-deep/d3.md").exists());
        assert!(!first_mem.join("evil-link.md").exists());

        // Ledger imported_sources.json contains canonical paths for both imported Orca bots
        let ledger: BTreeMap<String, String> = serde_json::from_str(
            &fs::read_to_string(state_dir.join("bots/imported_sources.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(ledger.len(), 2);

        // Windows ~\ tilde expansion via shared expand_tilde
        assert_eq!(
            expand_tilde(&home, r"~\bots\demo"),
            home.join("bots").join("demo")
        );

        // External candidate details appear in human report
        let mut fake_report = report;
        fake_report
            .bots
            .external_candidates
            .push(ExternalBotCandidate {
                app: ExternalBotSourceApp::OpenClaw,
                agent: "main".to_owned(),
                name: "Molty".to_owned(),
                path: "/tmp/molty".to_owned(),
                soul_preview: None,
            });
        let human = format_report_human(&fake_report);
        assert!(human.contains("[openclaw] Molty (main)"));
        assert!(human.contains("/tmp/molty"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg_attr(windows, ignore = "requires sqlite3 CLI on PATH")]
    fn orca_bots_migrates_wal_items_without_shm_groups_suggestions_and_schedules_idempotently() {
        let root =
            std::env::temp_dir().join(format!("krill-orca-bots-full-mig-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let orca_dir = root.join("orca");
        let state_dir = root.join("state");
        fs::create_dir_all(&orca_dir).unwrap();
        fs::create_dir_all(&state_dir).unwrap();

        let b1 = home.join(".orca/bots/triage");
        fs::create_dir_all(&b1).unwrap();
        fs::write(
            b1.join("bot.json"),
            serde_json::json!({
                "id": "orca-triage-1",
                "name": "Triage Bot",
                "agent": "claude"
            })
            .to_string(),
        )
        .unwrap();
        fs::write(b1.join("SOUL.md"), "# Triage\n\nRoutes bugs.\n").unwrap();

        // Seed WAL-mode bot-items.sqlite, then force checkpoint and remove -wal/-shm files!
        let items_sqlite = orca_dir.join("bot-items.sqlite");
        let items_sql = r#"
PRAGMA journal_mode = WAL;
CREATE TABLE items (
    uid TEXT PRIMARY KEY,
    number INTEGER NOT NULL UNIQUE,
    owner_bot_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    source TEXT NOT NULL,
    made_by TEXT,
    title TEXT NOT NULL,
    description TEXT,
    done_means TEXT,
    status TEXT NOT NULL,
    question_event INTEGER,
    close_note TEXT,
    version INTEGER NOT NULL,
    last_event_id INTEGER NOT NULL,
    last_activity_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    closed_at INTEGER,
    snoozed_until INTEGER
);
CREATE TABLE item_links (
    item_uid TEXT NOT NULL,
    kind TEXT NOT NULL,
    ref TEXT NOT NULL,
    added_event INTEGER NOT NULL,
    PRIMARY KEY (item_uid, kind, ref)
);
CREATE TABLE item_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    item_uid TEXT NOT NULL,
    kind TEXT NOT NULL,
    actor TEXT NOT NULL,
    data TEXT NOT NULL,
    mail TEXT,
    action INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
INSERT INTO items VALUES ('item-uid-1', 7, 'orca-triage-1', 'chat-1', 'bot', 'bot', '빌드 확인', 'Check CI', 'All green', 'needs_you', 1, NULL, 2, 1, 1700000001000, 1700000000000, 1700000001000, NULL, NULL);
INSERT INTO item_links VALUES ('item-uid-1', 'pr', 'https://github.com/org/repo/pull/7', 1);
INSERT INTO item_events (item_uid, kind, actor, data, mail, action, created_at) VALUES (
    'item-uid-1',
    'commented',
    '{"kind":"bot","botId":"orca-triage-1","chatId":"chat-1"}',
    '{"text":"Deploy now?","ask":{"questions":[{"question":"Deploy now?","options":["Yes","Wait"],"recommended":0}]}}',
    NULL,
    1,
    1700000001000
);
PRAGMA wal_checkpoint(TRUNCATE);
"#;
        let status = Command::new(sqlite3_binary())
            .arg(&items_sqlite)
            .arg(items_sql)
            .status()
            .unwrap();
        assert!(status.success());
        fs::remove_file(orca_dir.join("bot-items.sqlite-wal")).ok();
        fs::remove_file(orca_dir.join("bot-items.sqlite-shm")).ok();

        // Seed orchestration.db bot_groups
        let orch_db = orca_dir.join("orchestration.db");
        let group_record = serde_json::json!({
            "id": "bot-group-1",
            "name": "Release Squad",
            "description": "Ships builds",
            "createdAt": 1_700_000_000_000_u64,
            "members": [{ "botId": "orca-triage-1", "sessionId": "s1" }]
        })
        .to_string();
        let orch_sql = format!(
            "CREATE TABLE bot_groups (id TEXT PRIMARY KEY, record TEXT NOT NULL); INSERT INTO bot_groups VALUES ('bot-group-1', {});",
            sql_hex_text(&group_record)
        );
        let status_orch = Command::new(sqlite3_binary())
            .arg(&orch_db)
            .arg(orch_sql)
            .status()
            .unwrap();
        assert!(status_orch.success());

        // Seed bot-look-around.json
        fs::write(
            orca_dir.join("bot-look-around.json"),
            serde_json::json!({
                "bots": {
                    "orca-triage-1": {
                        "suggestions": [
                            { "id": "s-open", "title": "Check flake", "why": "2 retries", "ask": "Re-run?", "status": "open", "createdAt": 100 },
                            { "id": "s-dismiss", "title": "Old", "why": "Resolved", "ask": "Skip", "status": "dismissed", "createdAt": 90 },
                            { "id": "s-replaced", "title": "Superseded", "why": "N/A", "ask": "No", "status": "replaced", "createdAt": 80 }
                        ]
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        // Seed bot-scoped recurring automation + one-shot automation in orca-data.json
        fs::write(
            orca_dir.join("orca-data.json"),
            serde_json::json!({
                "automations": [
                    {
                        "id": "bot-sched-1",
                        "name": "Morning Triage",
                        "prompt": "Check open PRs",
                        "rrule": "0 9 * * 1-5",
                        "timezone": "Asia/Seoul",
                        "enabled": true,
                        "bot": { "botId": "orca-triage-1" }
                    },
                    {
                        "id": "bot-once-skip",
                        "name": "One Shot",
                        "prompt": "Ignore me",
                        "rrule": "0 9 * * *",
                        "once": { "at": 1_700_000_000_000_u64 },
                        "bot": { "botId": "orca-triage-1" }
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let dry_opts = ImportOptions {
            home,
            state_dir: state_dir.clone(),
            orca_dir,
            domains: vec![ImportDomain::Bots],
            dry_run: true,
        };
        let dry = run_import(&dry_opts).unwrap();
        assert_eq!(dry.bots.bots_added, 1);
        assert_eq!(dry.bots.items_migrated, 1);
        assert_eq!(dry.bots.groups_migrated, 1);
        assert_eq!(dry.bots.suggestions_migrated, 2);
        assert_eq!(dry.bots.schedules_migrated, 1);
        assert!(!state_dir.join("bots/bot-items.db").exists());

        let applied = run_import(&ImportOptions {
            dry_run: false,
            ..dry_opts.clone()
        })
        .unwrap();
        assert_eq!(applied.bots.items_migrated, 1);
        assert_eq!(applied.bots.groups_migrated, 1);
        assert_eq!(applied.bots.suggestions_migrated, 2);
        assert_eq!(applied.bots.schedules_migrated, 1);

        // Verify Krill schema inside bot-items.db
        let krill_items_db = state_dir.join("bots/bot-items.db");
        let item_rows = run_sqlite3_json_rows(
            &krill_items_db,
            "SELECT uid, number, owner_bot_id, title, last_activity_at_ms FROM items;",
        )
        .unwrap();
        assert_eq!(item_rows.len(), 1);
        assert_eq!(item_rows[0]["uid"], "item-uid-1");
        assert_eq!(item_rows[0]["title"], "빌드 확인");

        let link_rows = run_sqlite3_json_rows(
            &krill_items_db,
            "SELECT item_uid, kind, target FROM item_links;",
        )
        .unwrap();
        assert_eq!(link_rows.len(), 1);
        assert_eq!(link_rows[0]["target"], "https://github.com/org/repo/pull/7");

        let ev_rows = run_sqlite3_json_rows(
            &krill_items_db,
            "SELECT actor_json, text, decisions_json FROM item_events;",
        )
        .unwrap();
        assert_eq!(ev_rows.len(), 1);
        let actor: BotItemActor =
            serde_json::from_str(ev_rows[0]["actor_json"].as_str().unwrap()).unwrap();
        assert!(
            matches!(actor, BotItemActor::Bot { ref bot_id, .. } if bot_id.as_str() == "orca-triage-1")
        );
        let decisions: Vec<BotItemDecision> =
            serde_json::from_str(ev_rows[0]["decisions_json"].as_str().unwrap()).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].question, "Deploy now?");
        assert_eq!(decisions[0].recommended, Some(0));

        // Verify groups.json, suggestions.json, and bot.schedules
        let groups: Vec<BotGroupChat> =
            serde_json::from_str(&fs::read_to_string(state_dir.join("bots/groups.json")).unwrap())
                .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Release Squad");

        let suggestions: Vec<BotSuggestion> = serde_json::from_str(
            &fs::read_to_string(state_dir.join("bots/suggestions.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(suggestions.len(), 2);
        assert!(!suggestions[0].dismissed);
        assert!(suggestions[1].dismissed);

        let bots: Vec<BotConfig> =
            serde_json::from_str(&fs::read_to_string(state_dir.join("bots/bots.json")).unwrap())
                .unwrap();
        assert_eq!(bots[0].schedules.len(), 1);
        assert_eq!(bots[0].schedules[0].cron, "0 9 * * 1-5");

        // Second apply must be 100% idempotent
        let rerun = run_import(&ImportOptions {
            dry_run: false,
            ..dry_opts
        })
        .unwrap();
        assert_eq!(rerun.bots.bots_added, 0);
        assert_eq!(rerun.bots.items_migrated, 0);
        assert_eq!(rerun.bots.groups_migrated, 0);
        assert_eq!(rerun.bots.suggestions_migrated, 0);
        assert_eq!(rerun.bots.schedules_migrated, 0);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn orca_bots_migrates_suggestions_and_schedules_without_sqlite_idempotently() {
        let root =
            std::env::temp_dir().join(format!("krill-orca-bots-json-mig-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let orca_dir = root.join("orca");
        let state_dir = root.join("state");
        fs::create_dir_all(&orca_dir).unwrap();
        fs::create_dir_all(&state_dir).unwrap();

        let b1 = home.join(".orca").join("bots").join("triage");
        fs::create_dir_all(&b1).unwrap();
        fs::write(
            b1.join("bot.json"),
            serde_json::json!({
                "id": "orca-triage-1",
                "name": "Triage Bot",
                "agent": "claude"
            })
            .to_string(),
        )
        .unwrap();
        fs::write(b1.join("SOUL.md"), "# Triage\n\nRoutes bugs.\n").unwrap();

        fs::write(
            orca_dir.join("bot-look-around.json"),
            serde_json::json!({
                "bots": {
                    "orca-triage-1": {
                        "suggestions": [
                            { "id": "s-open", "title": "Check flake", "why": "2 retries", "ask": "Re-run?", "status": "open", "createdAt": 100 },
                            { "id": "s-dismiss", "title": "Old", "why": "Resolved", "ask": "Skip", "status": "dismissed", "createdAt": 90 },
                            { "id": "s-replaced", "title": "Superseded", "why": "N/A", "ask": "No", "status": "replaced", "createdAt": 80 }
                        ]
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        fs::write(
            orca_dir.join("orca-data.json"),
            serde_json::json!({
                "automations": [
                    {
                        "id": "bot-sched-1",
                        "name": "Morning Triage",
                        "prompt": "Check open PRs",
                        "rrule": "0 9 * * 1-5",
                        "timezone": "Asia/Seoul",
                        "enabled": true,
                        "bot": { "botId": "orca-triage-1" }
                    },
                    {
                        "id": "bot-once-skip",
                        "name": "One Shot",
                        "prompt": "Ignore me",
                        "rrule": "0 9 * * *",
                        "once": { "at": 1_700_000_000_000_u64 },
                        "bot": { "botId": "orca-triage-1" }
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let dry_opts = ImportOptions {
            home,
            state_dir: state_dir.clone(),
            orca_dir,
            domains: vec![ImportDomain::Bots],
            dry_run: true,
        };
        let dry = run_import(&dry_opts).unwrap();
        assert_eq!(dry.bots.bots_added, 1);
        assert_eq!(dry.bots.suggestions_migrated, 2);
        assert_eq!(dry.bots.schedules_migrated, 1);
        assert!(!state_dir.join("bots").exists());

        let applied = run_import(&ImportOptions {
            dry_run: false,
            ..dry_opts.clone()
        })
        .unwrap();
        assert_eq!(applied.bots.bots_added, 1);
        assert_eq!(applied.bots.suggestions_migrated, 2);
        assert_eq!(applied.bots.schedules_migrated, 1);

        let suggestions: Vec<BotSuggestion> = serde_json::from_str(
            &fs::read_to_string(state_dir.join("bots").join("suggestions.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(suggestions.len(), 2);
        assert!(!suggestions[0].dismissed);
        assert!(suggestions[1].dismissed);

        let bots: Vec<BotConfig> = serde_json::from_str(
            &fs::read_to_string(state_dir.join("bots").join("bots.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(bots[0].schedules.len(), 1);
        assert_eq!(bots[0].schedules[0].cron, "0 9 * * 1-5");

        let rerun = run_import(&ImportOptions {
            dry_run: false,
            ..dry_opts
        })
        .unwrap();
        assert_eq!(rerun.bots.bots_added, 0);
        assert_eq!(rerun.bots.suggestions_migrated, 0);
        assert_eq!(rerun.bots.schedules_migrated, 0);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn extract_remote_host_handles_ipv6_and_rejects_loopback() {
        assert_eq!(
            extract_remote_host("https://[2001:db8::1]/api"),
            Some(("2001:db8::1".to_owned(), None))
        );
        assert_eq!(
            extract_remote_host("wss://[2001:db8::1]:8443/ws"),
            Some(("2001:db8::1".to_owned(), Some(8443)))
        );
        assert_eq!(extract_remote_host("https://[::1]/api"), None);
        assert_eq!(extract_remote_host("wss://[::1]:8443/ws"), None);
        assert_eq!(extract_remote_host("https://[2001:db8::1/broken"), None);
        assert_eq!(
            extract_remote_host("https://dev.example.com:9000?token=1"),
            Some(("dev.example.com".to_owned(), Some(9000)))
        );
    }

    #[test]
    fn import_settings_domain_imports_automations_without_settings_domain_and_honors_dtstart() {
        let root = std::env::temp_dir().join(format!("krill-r2p-09-10-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state_dir = root.join("state");
        let orca_dir = root.join("orca");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&orca_dir).unwrap();

        let mut domains = BTreeMap::new();
        domains.insert(
            "automations".to_owned(),
            serde_json::json!([
                {
                    "id": "daily-digest",
                    "name": "Daily digest",
                    "prompt": "Summarize today",
                    "rrule": "FREQ=DAILY",
                    "dtstart": "2026-10-01T14:30:00Z",
                    "enabled": true
                },
                {
                    "id": "weekly-sync",
                    "name": "Weekly sync",
                    "prompt": "Check PRs",
                    "rrule": "FREQ=WEEKLY;BYDAY=MO,WE",
                    "dtstart": "2026-10-01T14:30:00Z",
                    "enabled": true
                },
                {
                    "id": "hourly-ping",
                    "name": "Hourly ping",
                    "prompt": "Ping status",
                    "rrule": "FREQ=HOURLY",
                    "dtstart": "2026-10-01T14:30:00Z",
                    "enabled": true
                },
                {
                    "id": "plain-cron",
                    "name": "Plain cron",
                    "prompt": "Plain cron job",
                    "rrule": "15 8 * * 1-5",
                    "dtstart": "2026-10-01T14:30:00Z",
                    "enabled": true
                }
            ]),
        );

        let opts = ImportOptions {
            home,
            state_dir: state_dir.clone(),
            orca_dir,
            domains: vec![ImportDomain::Settings],
            dry_run: false,
        };
        let mut warnings = Vec::new();
        let summary = import_settings_domain(&opts, &domains, &mut warnings).unwrap();
        assert_eq!(summary.schedules_merged, 4, "warnings: {warnings:?}");

        let loaded = Schedules::load(&state_dir).unwrap();
        let cron_of = |id: &str| {
            loaded
                .schedule
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.cron.clone())
        };
        assert_eq!(cron_of("orca-daily-digest").as_deref(), Some("30 14 * * *"));
        assert_eq!(
            cron_of("orca-weekly-sync").as_deref(),
            Some("30 14 * * 1,3")
        );
        assert_eq!(cron_of("orca-hourly-ping").as_deref(), Some("30 * * * *"));
        assert_eq!(cron_of("orca-plain-cron").as_deref(), Some("15 8 * * 1-5"));

        // Bot-scoped RRULE automation also preserves dtstart wall-clock time.
        let mut bot_domains = BTreeMap::new();
        bot_domains.insert(
            "automations".to_owned(),
            serde_json::json!([{
                "id": "bot-daily",
                "name": "Bot daily",
                "prompt": "Check queue",
                "rrule": "FREQ=DAILY",
                "dtstart": "2026-10-01T14:30:00Z",
                "bot": { "botId": "bot-1" }
            }]),
        );
        let mut bot_cfg = build_krill_bot_config("helper", None, None, None, 0).unwrap();
        bot_cfg.id = BotId::new("bot-1").unwrap();
        let mut bots = vec![bot_cfg];
        let mut map = BTreeMap::new();
        map.insert("bot-1".to_owned(), BotId::new("bot-1").unwrap());
        let mut bot_warnings = Vec::new();
        let (migrated, _) =
            migrate_bot_scoped_automations(&bot_domains, &mut bots, &map, &mut bot_warnings);
        assert_eq!(migrated, 1, "warnings: {bot_warnings:?}");
        assert_eq!(bots[0].schedules[0].cron, "30 14 * * *");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn allocate_unique_bot_slug_avoids_existing_directory_without_bot_json() {
        let root = std::env::temp_dir().join(format!("krill-r2p-12-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let bots_root = root.join("bots");
        let existing_dir = bots_root.join("helper");
        fs::create_dir_all(&existing_dir).unwrap();
        fs::write(existing_dir.join("notes.txt"), "keep me").unwrap();

        let base = BotSlug::new("helper").unwrap();
        let allocated = allocate_unique_bot_slug(&[], &bots_root, &base);
        assert_eq!(allocated.as_str(), "helper-2");
        assert_eq!(
            fs::read_to_string(existing_dir.join("notes.txt")).unwrap(),
            "keep me"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn remote_project_host_setups_do_not_overwrite_local_or_other_host_projects() {
        let root = std::env::temp_dir().join(format!("krill-r2p-13-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state_dir = root.join("state");
        let orca_dir = root.join("orca");
        let local_repo = root.join("local-repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&orca_dir).unwrap();
        fs::create_dir_all(&local_repo).unwrap();

        let mut domains = BTreeMap::new();
        domains.insert(
            "repos".to_owned(),
            serde_json::json!([{
                "id": "repo-shared",
                "path": local_repo.to_string_lossy(),
                "displayName": "Local App",
                "hookSettings": { "scripts": { "setup": "local-setup.sh" } }
            }]),
        );
        domains.insert(
            "projectHostSetups".to_owned(),
            serde_json::json!([
                {
                    "id": "setup-a",
                    "repoId": "repo-shared",
                    "hostId": "host-a",
                    "path": "/srv/a/app",
                    "displayName": "Host A App",
                    "hookSettings": { "scripts": { "setup": "host-a-setup.sh" } }
                },
                {
                    "id": "setup-b",
                    "repoId": "repo-shared",
                    "hostId": "host-b",
                    "path": "/srv/b/app",
                    "displayName": "Host B App",
                    "hookSettings": { "scripts": { "setup": "host-b-setup.sh" } }
                }
            ]),
        );

        let opts = ImportOptions {
            home,
            state_dir: state_dir.clone(),
            orca_dir,
            domains: vec![ImportDomain::Projects],
            dry_run: false,
        };
        let mut warnings = Vec::new();
        let summary = import_projects_domain(&opts, &domains, &mut warnings).unwrap();
        assert_eq!(summary.projects_added, 3, "warnings: {warnings:?}");

        let store = ProjectsStore::load(&state_dir).unwrap();
        assert_eq!(store.projects.len(), 3);
        let local = store.projects.iter().find(|p| p.host_id.is_none()).unwrap();
        assert_eq!(local.display_name, "Local App");
        assert_eq!(local.setup_script.as_deref(), Some("local-setup.sh"));

        let host_a = store
            .projects
            .iter()
            .find(|p| p.host_id.as_deref() == Some("host-a"))
            .unwrap();
        assert_eq!(host_a.id, "host-a:repo-shared");
        assert_eq!(host_a.setup_script.as_deref(), Some("host-a-setup.sh"));

        let host_b = store
            .projects
            .iter()
            .find(|p| p.host_id.as_deref() == Some("host-b"))
            .unwrap();
        assert_eq!(host_b.id, "host-b:repo-shared");
        assert_eq!(host_b.setup_script.as_deref(), Some("host-b-setup.sh"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg_attr(windows, ignore = "requires sqlite3 CLI on PATH")]
    fn import_orca_bot_items_propagates_dst_errors_and_allocates_numbers_in_tx() {
        let root = std::env::temp_dir().join(format!("krill-r2p-08-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state_dir = root.join("state");
        let orca_dir = root.join("orca");
        let profile_dir = orca_dir.join("profiles").join("default");
        let bots_root = state_dir.join(BOTS_DIR);
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&profile_dir).unwrap();
        fs::create_dir_all(&bots_root).unwrap();

        // Create Orca source DB with two items both numbered 1 for bot-1.
        let src_db = profile_dir.join("bot-items.sqlite");
        let src_sql = "\
CREATE TABLE items (
    uid TEXT PRIMARY KEY,
    number INTEGER NOT NULL,
    owner_bot_id TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    done_means TEXT,
    status TEXT NOT NULL,
    close_note TEXT,
    snoozed_until INTEGER,
    version INTEGER NOT NULL,
    last_activity_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    closed_at INTEGER
);
CREATE TABLE item_links (item_uid TEXT NOT NULL, kind TEXT NOT NULL, ref TEXT NOT NULL);
CREATE TABLE item_events (id INTEGER PRIMARY KEY AUTOINCREMENT, item_uid TEXT NOT NULL, kind TEXT NOT NULL, actor TEXT NOT NULL, data TEXT NOT NULL, created_at INTEGER NOT NULL);
INSERT INTO items VALUES ('uid-1', 1, 'bot-1', 'First', NULL, NULL, 'planned', NULL, NULL, 1, 100, 100, 100, NULL);
INSERT INTO items VALUES ('uid-2', 1, 'bot-1', 'Second', NULL, NULL, 'working', NULL, NULL, 1, 200, 200, 200, NULL);
INSERT INTO item_links VALUES ('uid-1', 'url', 'https://example.com/1');
INSERT INTO item_events (item_uid, kind, actor, data, created_at) VALUES ('uid-1', 'made', '{\"kind\":\"owner\"}', '{\"text\":\"created\"}', 100);
";
        assert!(
            Command::new(sqlite3_binary())
                .arg(&src_db)
                .arg(src_sql)
                .status()
                .unwrap()
                .success()
        );

        let bot_id = BotId::new("bot-1").unwrap();
        let mut bot_id_map = BTreeMap::new();
        bot_id_map.insert("bot-1".to_owned(), bot_id.clone());
        let mut bot_cfg = build_krill_bot_config("b1", None, None, None, 0).unwrap();
        bot_cfg.id = bot_id;
        let existing_bots = vec![bot_cfg];
        let opts = ImportOptions {
            home,
            state_dir,
            orca_dir,
            domains: vec![ImportDomain::Bots],
            dry_run: false,
        };

        // 1. Corrupt destination DB must propagate error rather than silently swallowing it.
        let dst_db = bots_root.join(BOT_ITEMS_DB_FILE);
        fs::write(&dst_db, b"not a valid sqlite database header").unwrap();
        let mut warnings = Vec::new();
        let err = import_orca_bot_items(
            &opts,
            "default",
            &bots_root,
            &bot_id_map,
            &existing_bots,
            &mut warnings,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("sqlite3"),
            "corrupt dst_db error should propagate: {err}"
        );
        fs::remove_file(&dst_db).unwrap();

        // 2. Pre-existing item with (bot-1, 1) in dst_db: both imported items allocate non-colliding
        // numbers (2 and 3) inside BEGIN IMMEDIATE with intact links/events and no leftover temp SQL.
        let pre_sql = "\
CREATE TABLE items (
    uid TEXT PRIMARY KEY,
    number INTEGER NOT NULL,
    owner_bot_id TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    done_means TEXT,
    status TEXT NOT NULL,
    close_note TEXT,
    snoozed_until_ms INTEGER,
    version INTEGER NOT NULL,
    last_activity_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    closed_at_ms INTEGER,
    UNIQUE(owner_bot_id, number)
);
INSERT INTO items VALUES ('uid-existing', 1, 'bot-1', 'Existing', NULL, NULL, 'planned', NULL, NULL, 1, 50, 50, 50, NULL);
";
        assert!(
            Command::new(sqlite3_binary())
                .arg(&dst_db)
                .arg(pre_sql)
                .status()
                .unwrap()
                .success()
        );

        let count = import_orca_bot_items(
            &opts,
            "default",
            &bots_root,
            &bot_id_map,
            &existing_bots,
            &mut warnings,
        )
        .unwrap();
        assert_eq!(count, 2);

        let rows = run_sqlite3_json_rows(
            &dst_db,
            "SELECT uid, number FROM items ORDER BY number ASC;",
        )
        .unwrap();
        let got: Vec<(String, u64)> = rows
            .iter()
            .map(|r| {
                (
                    r["uid"].as_str().unwrap().to_owned(),
                    r["number"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("uid-existing".to_owned(), 1),
                ("uid-1".to_owned(), 2),
                ("uid-2".to_owned(), 3),
            ]
        );
        let events = run_sqlite3_json_rows(&dst_db, "SELECT item_uid FROM item_events;").unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            fs::read_dir(&bots_root)
                .unwrap()
                .filter_map(Result::ok)
                .all(|e| !e
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".orca-import-items-")),
            "temp SQL script must be cleaned up"
        );

        // 3. Re-running import is idempotent and does not duplicate events.
        let rerun = import_orca_bot_items(
            &opts,
            "default",
            &bots_root,
            &bot_id_map,
            &existing_bots,
            &mut warnings,
        )
        .unwrap();
        assert_eq!(rerun, 0);
        let events_after =
            run_sqlite3_json_rows(&dst_db, "SELECT item_uid FROM item_events;").unwrap();
        assert_eq!(events_after.len(), 1);

        fs::remove_dir_all(&root).ok();
    }
}
