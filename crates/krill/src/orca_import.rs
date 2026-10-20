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
//!   "domains": ["accounts", "ssh", "projects", "settings"],
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
//!   }
//! }
//! ```
//!
//! 스키마·경로 참고:
//! `.ref/orca/src/main/persistence.ts`, `.ref/orca/src/main/profile-Criterion.ts`,
//! `.ref/orca/src/main/claude-profile-paths.ts`, `.ref/orca/src/main/codex-accounts.ts`
//! (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use krill_proto::agent::{self, Agents, Profile as AgentProfile};
use krill_proto::config_document;
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

/// 이관 대상 도메인(4대 도메인).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportDomain {
    Accounts,
    Ssh,
    Projects,
    Settings,
}

impl ImportDomain {
    pub const ALL: [Self; 4] = [Self::Accounts, Self::Ssh, Self::Projects, Self::Settings];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accounts => "accounts",
            Self::Ssh => "ssh",
            Self::Projects => "projects",
            Self::Settings => "settings",
        }
    }

    pub fn parse(raw: &str) -> io::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "accounts" | "account" => Ok(Self::Accounts),
            "ssh" | "hosts" | "remote" => Ok(Self::Ssh),
            "projects" | "project" | "repos" | "worktrees" => Ok(Self::Projects),
            "settings" | "setting" | "config" => Ok(Self::Settings),
            other => Err(invalid(format!(
                "unknown import domain '{other}' (expected accounts,ssh,projects,settings)"
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
                "--only requires at least one domain (accounts,ssh,projects,settings)",
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
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
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
    home.join("Library")
        .join("Application Support")
        .join("orca")
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
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
        report.accounts.skipped_reason = Some("--only 목록에서 제외됨".to_owned());
    }
    if selected.contains(&ImportDomain::Ssh) {
        report.ssh = retry_on_conflict("ssh", &mut warnings, |w| {
            import_ssh_domain(opts, &domains_map, w)
        })?;
    } else {
        report.ssh.skipped = true;
        report.ssh.skipped_reason = Some("--only 목록에서 제외됨".to_owned());
    }
    if selected.contains(&ImportDomain::Projects) {
        report.projects = retry_on_conflict("projects", &mut warnings, |w| {
            import_projects_domain(opts, &domains_map, w)
        })?;
    } else {
        report.projects.skipped = true;
        report.projects.skipped_reason = Some("--only 목록에서 제외됨".to_owned());
    }
    if selected.contains(&ImportDomain::Settings) {
        report.settings = retry_on_conflict("settings", &mut warnings, |w| {
            import_settings_domain(opts, &domains_map, w)
        })?;
    } else {
        report.settings.skipped = true;
        report.settings.skipped_reason = Some("--only 목록에서 제외됨".to_owned());
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
                a.skipped_reason.as_deref().unwrap_or("데몬 필요")
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

fn query_sqlite_domains(db_path: &Path) -> io::Result<BTreeMap<String, Json>> {
    let sqlite_bin = if Path::new("/usr/bin/sqlite3").is_file() {
        "/usr/bin/sqlite3"
    } else {
        "sqlite3"
    };
    let output = Command::new(sqlite_bin)
        .arg("-readonly")
        .arg("-json")
        .arg(db_path)
        .arg("SELECT domain, payload FROM profile_state_documents;")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
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
        return Ok(BTreeMap::new());
    }
    let rows: Vec<SqliteRow> = serde_json::from_str(trimmed).map_err(invalid_data)?;
    let mut map = BTreeMap::new();
    for row in rows {
        if let Ok(val) = serde_json::from_str::<Json>(&row.payload) {
            map.insert(row.domain, val);
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
            "Orca 암호화 vault 감지 — agy 개별 이관 불가(OWE-896·OWE-721). \
             Krill은 호스트 agy 로그인을 공유해 사용한다"
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

    let mut summary = AccountsImportSummary::default();
    // OWE-901: agy 계정은 Orca 암호화 저장소라 이관하지 않는다. 새로 로그인을 안내한다.
    if let Some(note) = detect_agy_import_note(opts, domains) {
        warnings.push(note);
    }
    if planned.is_empty() {
        return summary;
    }

    let socket = opts.state_dir.join("run").join("krilld.sock");
    let auto_start = should_auto_start_daemon(opts.dry_run);
    match daemon_usage_request(&socket, &UsageRequest::ListAccounts, auto_start) {
        Err(e) if !opts.dry_run => {
            // AS-03: 데몬 자동 기동까지 실패해 연결할 수 없으면 로컬 파일(accounts.toml)을 직접 쓰지 않고
            // '데몬 필요' 사유로 accounts 도메인을 스킵한 뒤 다른 도메인만 진행한다.
            summary.skipped = true;
            summary.skipped_reason = Some(format!(
                "데몬 필요: krilld에 연결할 수 없어 계정 이관을 건너뜀 ({e})"
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
            warnings.push(format!("accounts skipped (데몬 필요): {e}"));
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
        Ok(mut snap) => {
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
                        summary.skipped_reason =
                            Some(format!("데몬 필요: krilld 계정 추가 실패 ({e})"));
                        warnings.push(format!("accounts skipped (데몬 필요): {e}"));
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
            "agy 계정 {count}개 감지: 이관 불가(Orca 암호화 저장소) — Krill에서 새로 로그인하십시오"
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
/// `claude-profiles/<uuid>/home`·`claude-accounts/<uuid>/auth`·`codex-accounts/<uuid>/home`.
/// uuid 형태가 아니면 `None`(사용자 임의 경로와 오인 병합 방지).
fn orca_uuid_from_profile_dir(dir: &str) -> Option<&str> {
    let mut parts = dir.split('/');
    let leaf = parts.next_back()?;
    let uuid = parts.next_back()?;
    let parent = parts.next_back()?;
    if !matches!(
        parent,
        "claude-profiles" | "claude-accounts" | "codex-accounts"
    ) || !matches!(leaf, "home" | "auth")
    {
        return None;
    }
    is_uuid_shape(uuid).then_some(uuid)
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
        | FEAT_USAGE_STATUS
        | FEAT_USAGE_RATELIMIT
        | FEAT_USAGE_MANAGE
        | FEAT_ACCOUNT_CONNECTION
        | FEAT_ACCOUNT_POLICY;
    let mut conn = match daemon::handshake(socket, &hello, Duration::from_secs(10)) {
        Ok(conn) => conn,
        Err(_first_err) if auto_start => {
            let paths = daemon::Paths::dev()
                .map(|mut p| {
                    p.socket = socket.to_path_buf();
                    p
                })
                .map_err(|e| {
                    format!("cannot resolve daemon paths for {}: {e}", socket.display())
                })?;
            daemon::connect(&paths, &hello)
                .map_err(|e| format!("cannot connect to {}: {e}", socket.display()))?
        }
        Err(e) => return Err(format!("cannot connect to {}: {e}", socket.display())),
    };
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
    let rest = endpoint
        .trim()
        .strip_prefix("wss://")
        .or_else(|| endpoint.trim().strip_prefix("ws://"))
        .or_else(|| endpoint.trim().strip_prefix("ssh://"))
        .or_else(|| endpoint.trim().strip_prefix("https://"))
        .or_else(|| endpoint.trim().strip_prefix("http://"))?;
    let authority = rest.split('/').next()?;
    if authority.is_empty() {
        return None;
    }
    let (host, port) = if let Some((h, p)) = authority.rsplit_once(':') {
        (h, p.parse::<u16>().ok())
    } else {
        (authority, None)
    };
    let clean = host.trim_matches(['[', ']']);
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
    worktree_path: Option<String>,
    branch: Option<String>,
    base_ref: Option<String>,
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

            let candidate = StoredProject {
                id: setup
                    .repo_id
                    .or(setup.project_id)
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| setup.id.trim().to_owned()),
                path: clean_path,
                display_name,
                badge_color,
                repo_icon,
                kind,
                host_id: setup
                    .host_id
                    .filter(|h| !h.trim().is_empty() && h.trim() != "local"),
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
                let (repo_id, raw_wt_path) = split_worktree_key(raw_key);
                let wt_path = normalize_import_path_str(&raw_wt_path);
                if wt_path.is_empty() {
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
                let (key_repo_id, key_path) = split_worktree_key(raw_key);
                let wt_path = normalize_import_path_str(
                    &lin.worktree_path
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map_or(key_path, str::to_owned),
                );
                if wt_path.is_empty() {
                    continue;
                }
                if let Some(parent) = lin
                    .parent_worktree_path
                    .as_deref()
                    .map(|p| normalize_import_path_str(&split_worktree_key(p).1))
                    .filter(|s| !s.is_empty())
                {
                    lineage_parents.push((wt_path.clone(), parent));
                }
                let parent_worktree_path = lin
                    .parent_worktree_path
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(normalize_import_path_str);
                insert_lineage_entry(
                    &mut store.lineage,
                    wt_path.clone(),
                    StoredWorktreeLineage {
                        repo_id: lin.repo_id.or(key_repo_id),
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
        (p.host_id == candidate.host_id
            && (p.path == candidate.path || same_import_path(&p.path, &candidate.path)))
            || p.id == candidate.id
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
    if let Some((repo_id, path)) = raw.split_once("::") {
        (
            (!repo_id.trim().is_empty()).then(|| repo_id.trim().to_owned()),
            path.trim().to_owned(),
        )
    } else {
        (None, raw.trim().to_owned())
    }
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
    dtstart: Option<String>,
    timezone: Option<String>,
    enabled: Option<bool>,
    project_id: Option<String>,
    workspace_id: Option<String>,
    run_context: Option<OrcaRunContext>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OrcaRunContext {
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
            "터미널 글꼴 '{family}' 가져오기 건너뜀: macOS 숨김 시스템 글꼴이라 \
             Krill에서 고정폭으로 해석되지 않아 기존 글꼴을 유지합니다"
        )
    } else {
        format!(
            "터미널 글꼴 '{family}' 가져오기 건너뜀: 고정폭 확인이 안 돼 기존 글꼴을 유지합니다"
        )
    }
}

fn import_settings_domain(
    opts: &ImportOptions,
    domains: &BTreeMap<String, Json>,
    warnings: &mut Vec<String>,
) -> io::Result<SettingsImportSummary> {
    let mut summary = SettingsImportSummary::default();
    let Some(settings) = domains.get("settings") else {
        return Ok(summary);
    };

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
                    summary.quick_commands_merged = summary.quick_commands_merged.saturating_add(1);
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

    if settings_table != before_settings {
        summary.settings_updated = true;
        if !opts.dry_run {
            let text = config_document::save_toml(&settings_path, &settings_table)?;
            write_atomic_0600(&settings_path, text.as_bytes(), original.as_deref())?;
        }
    }

    // 2. `<state_dir>/agents.toml` (`agentCmdOverrides`, `agentDefaultArgs`, `agentDefaultEnv`)
    summary.agents_updated = import_agent_profiles(opts, settings, warnings)?;

    // 3. `<state_dir>/schedules.toml` (`automations`)
    summary.schedules_merged = import_automations(opts, domains, settings, warnings)?;

    Ok(summary)
}

fn map_orca_agent_id(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "gemini" => Some("gemini"),
        "opencode" => Some("opencode"),
        "cursor" | "cursor-agent" => Some("cursor"),
        "agy" | "antigravity" => Some("agy"),
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
        .and_then(|v| serde_json::from_value(v.clone()).ok())
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

    let mut current_doc = existing_raw.clone();
    let mut updated_count = 0usize;

    for (orca_key, canon_id) in touched_ids {
        let Some(base_profile) = loaded.profiles.get(canon_id).cloned() else {
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

        if next != base_profile {
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
    let automations: Vec<OrcaAutomation> = match serde_json::from_value(arr.clone()) {
        Ok(list) => list,
        Err(_) => return Ok(0),
    };
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
        if auto.enabled == Some(false) {
            continue;
        }
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
        let cron = rrule_to_utc_cron(auto.rrule.as_deref(), auto.dtstart.as_deref());
        let cwd = resolve_automation_cwd(&opts.home, &auto, &projects);
        let command = resolve_automation_command(auto.agent_id.as_deref());
        let existing_entry = schedules.schedule.iter().find(|s| s.id == id);

        let candidate = Schedule {
            id: id.clone(),
            cron,
            cwd,
            command,
            prompt,
            precheck: existing_entry.and_then(|s| s.precheck.clone()),
            workspace_mode: existing_entry
                .map_or_else(schedule::WorkspaceMode::default, |s| s.workspace_mode),
            enabled: existing_entry.is_none_or(|s| s.enabled),
        };

        if let Err(reason) = validate_schedule_candidate(&candidate, &mut seen_ids) {
            warnings.push(format!("skipping automation '{}': {reason}", auto.name));
            continue;
        }

        if let Some(idx) = schedules.schedule.iter().position(|s| s.id == id) {
            let Some(existing) = schedules.schedule.get_mut(idx) else {
                continue;
            };
            if existing.cron != candidate.cron
                || existing.cwd != candidate.cwd
                || existing.command != candidate.command
                || existing.prompt != candidate.prompt
            {
                let prev = existing.clone();
                existing.cron = candidate.cron;
                existing.cwd = candidate.cwd;
                existing.command = candidate.command;
                existing.prompt = candidate.prompt;
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
    if !seen_ids.insert(entry.id.clone()) {
        return Err("schedule IDs must be unique ASCII letters, digits, _ or -");
    }
    Ok(())
}

fn resolve_automation_cwd(home: &Path, auto: &OrcaAutomation, projects: &ProjectsStore) -> String {
    if let Some(ctx) = &auto.run_context
        && let Some(p) = ctx
            .worktree_path
            .as_deref()
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
    match agent_id.and_then(map_orca_agent_id).unwrap_or("claude") {
        "codex" => vec!["codex".to_owned(), "exec".to_owned()],
        "agy" => vec!["agy".to_owned(), "-p".to_owned()],
        "gemini" => vec!["gemini".to_owned(), "-p".to_owned()],
        "opencode" => vec!["opencode".to_owned(), "run".to_owned()],
        "cursor" => vec!["cursor-agent".to_owned(), "-p".to_owned()],
        _ => vec!["claude".to_owned(), "-p".to_owned()],
    }
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
        "WEEKLY" => {
            let days = if byday.is_empty() {
                "1".to_owned()
            } else {
                byday
                    .iter()
                    .map(u8::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
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

fn expand_tilde(home: &Path, raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("~/") {
        home.join(rest)
    } else if trimmed == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(trimmed)
    }
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
                .is_some_and(|r| r.starts_with("데몬 필요"))
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
                    build: krill_proto::BUILD.to_owned(),
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
        assert_eq!(nightly.command, vec!["codex".to_owned(), "exec".to_owned()]);
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
        assert!(note.contains("3개"), "{note}");
        assert!(note.contains("이관 불가(Orca 암호화 저장소)"), "{note}");
        assert!(note.contains("새로 로그인"), "{note}");
        // 디스크 흔적만 있어도 안내한다.
        fs::create_dir_all(orca_dir.join("antigravity-accounts").join("a1")).unwrap();
        let disk_only: BTreeMap<String, Json> =
            BTreeMap::from([("settings".to_owned(), serde_json::json!({}))]);
        let note = detect_agy_import_note(&opts, &disk_only).unwrap();
        assert!(note.contains("1개"), "{note}");
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
}
