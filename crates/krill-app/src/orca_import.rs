//! Orca 마이그레이션 GUI 카드 & 카테고리 선택 모달 (`OWE-727` / `MK-173`).
//!
//! 아키텍처 원칙:
//! - 마이그레이션 엔진을 `krill-app`에 중복 구현하지 않고, `krill import orca` CLI를
//!   서브프로세스로 호출해 `--json` 표준출력을 파싱한다.
//! - 미리보기: `krill import orca --dry-run --json --from <orca-dir>`
//! - 실행: `krill import orca --json [--only <domains_csv>] --from <orca-dir>`
//! - 완료 즉시 앱 재시작 없이 `settings::reload` + `settings::notify_external_change`를 통해
//!   사이드바 프로젝트/워크트리 트리, `Settings > Accounts`, `Settings > Ssh`, 상태바를 갱신한다.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Deferred, Div, FontWeight, InteractiveElement as _, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::button::ButtonVariants as _;
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use serde::{Deserialize, Serialize};

use crate::i18n::catalog::orca_import as text;
use crate::ui::{
    self, BadgeTone, DIALOG_WIDTH, Palette, RADIUS_LG, RADIUS_SM, SPACE_4, TEXT_LG, TEXT_SM,
    TEXT_XS,
};

/// 이관 대상 도메인(`crates/krill/src/orca_import.rs`의 `ImportDomain`과 1:1 대응).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportDomain {
    Accounts,
    Ssh,
    Projects,
    Settings,
    Sessions,
}

impl ImportDomain {
    pub const ALL: [Self; 5] = [
        Self::Accounts,
        Self::Ssh,
        Self::Projects,
        Self::Settings,
        Self::Sessions,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accounts => "accounts",
            Self::Ssh => "ssh",
            Self::Projects => "projects",
            Self::Settings => "settings",
            Self::Sessions => "sessions",
        }
    }

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Accounts => text::CAT_ACCOUNTS.get(),
            Self::Ssh => text::CAT_SSH.get(),
            Self::Projects => text::CAT_PROJECTS.get(),
            Self::Settings => text::CAT_SETTINGS.get(),
            Self::Sessions => text::CAT_SESSIONS.get(),
        }
    }

    #[must_use]
    pub fn description(self, report: Option<&OrcaImportReport>) -> String {
        let r = report.cloned().unwrap_or_default();
        match self {
            Self::Accounts => text::CAT_ACCOUNTS_DESC.format(&[
                ("claude", &r.claude_count().to_string()),
                ("codex", &r.codex_count().to_string()),
                ("agy", &r.provider_count("agy").to_string()),
                ("opencode", &r.provider_count("opencode").to_string()),
            ]),
            Self::Ssh => text::CAT_SSH_DESC.format(&[("ssh", &r.ssh_count().to_string())]),
            Self::Projects => text::CAT_PROJECTS_DESC.format(&[
                ("projects", &r.projects_count().to_string()),
                ("worktrees", &r.worktrees_count().to_string()),
                ("comments", &r.projects.comments_migrated.to_string()),
            ]),
            Self::Settings => text::CAT_SETTINGS_DESC.format(&[
                ("commands", &r.settings.quick_commands_merged.to_string()),
                ("schedules", &r.settings.schedules_merged.to_string()),
            ]),
            Self::Sessions => {
                text::CAT_SESSIONS_DESC.format(&[("sessions", &r.sessions_count().to_string())])
            }
        }
    }
}

/// `krill import orca [--dry-run] --json` 리포트 스키마.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrcaImportReport {
    #[serde(default)]
    pub orca_dir: String,
    #[serde(default)]
    pub profile_id: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub accounts: AccountsImportSummary,
    #[serde(default)]
    pub ssh: SshImportSummary,
    #[serde(default)]
    pub projects: ProjectsImportSummary,
    #[serde(default)]
    pub settings: SettingsImportSummary,
    #[serde(default)]
    pub sessions: SessionsImportSummary,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl OrcaImportReport {
    pub fn provider_count(&self, provider: &str) -> usize {
        self.accounts
            .items
            .iter()
            .filter(|i| i.provider.eq_ignore_ascii_case(provider))
            .count()
    }
    #[must_use]
    pub fn claude_count(&self) -> usize {
        let from_items = self
            .accounts
            .items
            .iter()
            .filter(|i| i.provider.eq_ignore_ascii_case("claude"))
            .count();
        from_items.max(
            self.accounts
                .claude_added
                .saturating_add(self.accounts.claude_updated),
        )
    }

    #[must_use]
    pub fn codex_count(&self) -> usize {
        let from_items = self
            .accounts
            .items
            .iter()
            .filter(|i| i.provider.eq_ignore_ascii_case("codex"))
            .count();
        from_items.max(
            self.accounts
                .codex_added
                .saturating_add(self.accounts.codex_updated),
        )
    }

    #[must_use]
    pub fn ssh_count(&self) -> usize {
        self.ssh
            .items
            .len()
            .max(self.ssh.hosts_added.saturating_add(self.ssh.hosts_updated))
    }

    #[must_use]
    pub fn projects_count(&self) -> usize {
        self.projects.items.len().max(
            self.projects
                .projects_added
                .saturating_add(self.projects.projects_updated),
        )
    }

    #[must_use]
    pub fn worktrees_count(&self) -> usize {
        self.projects.worktrees_migrated
    }

    /// 아직 Krill에서 띄우지 않은 Orca 열린 탭 세션 수.
    #[must_use]
    pub fn sessions_count(&self) -> usize {
        self.sessions.items.iter().filter(|i| !i.resumed).count()
    }

    #[must_use]
    pub fn commands_and_schedules_count(&self) -> usize {
        self.settings
            .quick_commands_merged
            .saturating_add(self.settings.schedules_merged)
    }

    /// 스캔·이관된 실제 항목 합계(OWE-882). 0이면 가져올 것이 없어 배너를 숨긴다.
    #[must_use]
    pub fn total_importable(&self) -> usize {
        self.claude_count()
            .saturating_add(self.codex_count())
            .saturating_add(
                self.provider_count("agy")
                    .max(usize::from(self.accounts.agy_detected)),
            )
            .saturating_add(self.provider_count("opencode"))
            .saturating_add(self.ssh_count())
            .saturating_add(self.ssh.environments_merged)
            .saturating_add(self.projects_count())
            .saturating_add(self.worktrees_count())
            .saturating_add(self.commands_and_schedules_count())
            .saturating_add(self.settings.agents_updated)
            .saturating_add(usize::from(self.settings.settings_updated))
            .saturating_add(self.sessions_count())
    }

    /// 0개인 항목은 빼고 ` · `로 잇는다(예: "원격 호스트 1개 · 프로젝트/폴더 1개").
    #[must_use]
    pub fn summary_line(&self) -> String {
        let counts = [
            ("claude", self.claude_count()),
            ("codex", self.codex_count()),
            ("agy", self.provider_count("agy")),
            ("opencode", self.provider_count("opencode")),
            ("ssh", self.ssh_count()),
            ("projects", self.projects_count()),
            ("worktrees", self.worktrees_count()),
            ("commands", self.commands_and_schedules_count()),
            ("sessions", self.sessions_count()),
        ];
        text::SUMMARY
            .get()
            .split(" · ")
            .filter_map(|part| {
                let (key, n) = counts
                    .iter()
                    .find(|(key, _)| part.contains(&format!("{{{key}}}")))?;
                (*n > 0).then(|| part.replace(&format!("{{{key}}}"), &n.to_string()))
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// 데몬 미실행으로 `accounts` 도메인이 건너뛰어졌을 때 사유 문자열을 돌려준다.
    #[must_use]
    pub fn accounts_skipped_daemon_reason(&self) -> Option<&str> {
        if !self.accounts.skipped {
            return None;
        }
        let reason = self.accounts.skipped_reason.as_deref()?;
        if reason.contains("krilld") {
            Some(reason)
        } else {
            None
        }
    }

    #[must_use]
    pub fn result_line(&self) -> String {
        let accounts_total = self
            .accounts
            .claude_added
            .saturating_add(self.accounts.claude_updated)
            .saturating_add(self.accounts.codex_added)
            .saturating_add(self.accounts.codex_updated);
        let accounts_total = accounts_total
            .saturating_add(self.accounts.agy_added)
            .saturating_add(self.accounts.opencode_added);
        let ssh_total = self.ssh.hosts_added.saturating_add(self.ssh.hosts_updated);
        let proj_total = self
            .projects
            .projects_added
            .saturating_add(self.projects.projects_updated);
        let wt_total = self.projects.worktrees_migrated;
        let settings_label = if self.settings.settings_updated
            || self.settings.quick_commands_merged > 0
            || self.settings.schedules_merged > 0
            || self.settings.agents_updated > 0
        {
            text::SETTINGS_APPLIED.get()
        } else {
            text::SETTINGS_UNCHANGED.get()
        };
        if self.accounts_skipped_daemon_reason().is_some() {
            text::RESULT_DONE_SKIPPED_ACCOUNTS.format(&[
                ("ssh", &ssh_total.to_string()),
                ("projects", &proj_total.to_string()),
                ("worktrees", &wt_total.to_string()),
                ("settings", settings_label),
                ("sessions", &self.sessions.resumed.to_string()),
            ])
        } else {
            text::RESULT_DONE.format(&[
                ("accounts", &accounts_total.to_string()),
                ("ssh", &ssh_total.to_string()),
                ("projects", &proj_total.to_string()),
                ("worktrees", &wt_total.to_string()),
                ("settings", settings_label),
                ("sessions", &self.sessions.resumed.to_string()),
            ])
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountsImportSummary {
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub skipped_reason: Option<String>,
    #[serde(default)]
    pub claude_added: usize,
    #[serde(default)]
    pub claude_updated: usize,
    #[serde(default)]
    pub codex_added: usize,
    #[serde(default)]
    pub codex_updated: usize,
    #[serde(default)]
    pub agy_added: usize,
    #[serde(default)]
    pub opencode_added: usize,
    #[serde(default)]
    pub active_claude: Option<String>,
    #[serde(default)]
    pub active_codex: Option<String>,
    #[serde(default)]
    pub agy_detected: bool,
    #[serde(default)]
    pub agy_note: Option<String>,
    #[serde(default)]
    pub items: Vec<ImportedAccountSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedAccountSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub organization_or_workspace: Option<String>,
    #[serde(default)]
    pub profile_dir: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub runnable: bool,
    #[serde(default)]
    pub connection: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshImportSummary {
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub skipped_reason: Option<String>,
    #[serde(default)]
    pub hosts_added: usize,
    #[serde(default)]
    pub hosts_updated: usize,
    #[serde(default)]
    pub environments_merged: usize,
    #[serde(default)]
    pub items: Vec<ImportedHostSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedHostSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub destination: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub identity_file: Option<String>,
    #[serde(default)]
    pub imported: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectsImportSummary {
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub skipped_reason: Option<String>,
    #[serde(default)]
    pub projects_added: usize,
    #[serde(default)]
    pub projects_updated: usize,
    #[serde(default)]
    pub worktrees_migrated: usize,
    #[serde(default)]
    pub lineage_migrated: usize,
    /// 이관된 워크트리 코멘트 수(OWE-882). 구 JSON과 호환되도록 `default`다.
    #[serde(default)]
    pub comments_migrated: usize,
    #[serde(default)]
    pub items: Vec<ImportedProjectSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedProjectSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub badge_color: Option<String>,
    #[serde(default)]
    pub has_setup_script: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsImportSummary {
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub skipped_reason: Option<String>,
    #[serde(default)]
    pub settings_updated: bool,
    #[serde(default)]
    pub default_agent: Option<String>,
    /// 가져오지 않은 터미널 글꼴과 사유(OWE-899, CLI `SettingsImportSummary`와 1:1).
    #[serde(default)]
    pub terminal_font_skipped: Option<String>,
    #[serde(default)]
    pub quick_commands_merged: usize,
    #[serde(default)]
    pub agents_updated: usize,
    #[serde(default)]
    pub schedules_merged: usize,
}

/// CLI `SessionsImportSummary`와 1:1.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsImportSummary {
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub skipped_reason: Option<String>,
    #[serde(default)]
    pub resumed: usize,
    #[serde(default)]
    pub already_resumed: usize,
    #[serde(default)]
    pub items: Vec<ImportedSessionSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedSessionSummary {
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub resumed: bool,
}

/// `krill import orca` 서브프로세스 기본 실행 제한 시간(`OWE-767`).
pub const IMPORT_SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(60);
/// `krill import orca --json` 표준출력 최대 바이트(4 MiB, `OWE-767`).
pub const MAX_IMPORT_STDOUT_BYTES: u64 = 4 * 1024 * 1024;
/// `krill import orca` 표준에러 최대 바이트(64 KiB, `OWE-767`).
pub const MAX_IMPORT_STDERR_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum DiscoveryPhase {
    #[default]
    Idle,
    Pending,
    Ready,
}

/// 앱 전역 Orca 마이그레이션 상태(온보딩 마법사 및 `Settings > General` / `Accounts` 공유).
#[derive(Debug, Clone)]
pub struct OrcaImportState {
    pub orca_dir_override: Option<PathBuf>,
    discovery_generation: u64,
    discovery: DiscoveryPhase,
    detected_orca_dir: Option<PathBuf>,
    resolved_krill_bin: Option<PathBuf>,
    pub krill_bin_override: Option<PathBuf>,
    pub home_override: Option<PathBuf>,
    pub state_dir_override: Option<PathBuf>,
    pub subprocess_timeout_override: Option<Duration>,
    pub daemon_link_up: Option<bool>,
    pub daemon_available: Option<bool>,
    pub preview: Option<OrcaImportReport>,
    pub last_result: Option<OrcaImportReport>,
    pub selected: BTreeSet<ImportDomain>,
    pub modal_open: bool,
    pub scanning: bool,
    pub importing: bool,
    pub error: Option<String>,
}

impl Default for OrcaImportState {
    fn default() -> Self {
        Self {
            orca_dir_override: None,
            discovery_generation: 0,
            discovery: DiscoveryPhase::Idle,
            detected_orca_dir: None,
            resolved_krill_bin: None,
            krill_bin_override: None,
            home_override: None,
            state_dir_override: None,
            subprocess_timeout_override: None,
            daemon_link_up: None,
            daemon_available: None,
            preview: None,
            last_result: None,
            selected: ImportDomain::ALL.into_iter().collect(),
            modal_open: false,
            scanning: false,
            importing: false,
            error: None,
        }
    }
}

impl gpui::Global for OrcaImportState {}

pub fn ensure_state(cx: &mut App) {
    if cx.try_global::<OrcaImportState>().is_none() {
        cx.set_global(OrcaImportState::default());
    }
}

#[must_use]
pub fn state(cx: &App) -> Option<&OrcaImportState> {
    cx.try_global::<OrcaImportState>()
}

/// 셸의 데몬 링크 상태(`Link::Up`) 변경을 동기화한다(`OWE-767`).
pub fn set_daemon_link_up(up: bool, cx: &mut App) {
    ensure_state(cx);
    let st = cx.global_mut::<OrcaImportState>();
    if st.daemon_link_up != Some(up) {
        st.daemon_link_up = Some(up);
        st.daemon_available = Some(up);
        cx.refresh_windows();
    }
}

/// 온보딩 `Env` 또는 테스트 픽스처의 격리된 `home`/`orca_dir`/`state_dir`/`krill_bin`을 주입한다.
pub fn set_env_paths(
    home: Option<PathBuf>,
    orca_dir: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    krill_bin: Option<PathBuf>,
    cx: &mut App,
) {
    ensure_state(cx);
    let st = cx.global_mut::<OrcaImportState>();
    let mut changed = false;
    if let Some(h) = home
        && st.home_override.as_ref() != Some(&h)
    {
        st.home_override = Some(h);
        changed = true;
    }
    if let Some(o) = orca_dir
        && st.orca_dir_override.as_ref() != Some(&o)
    {
        st.orca_dir_override = Some(o);
        changed = true;
    }
    if let Some(s) = state_dir
        && st.state_dir_override.as_ref() != Some(&s)
    {
        st.state_dir_override = Some(s);
        st.daemon_available = None;
        changed = true;
    }
    if let Some(k) = krill_bin
        && st.krill_bin_override.as_ref() != Some(&k)
    {
        st.krill_bin_override = Some(k);
        changed = true;
    }
    if changed {
        st.discovery_generation = st.discovery_generation.wrapping_add(1);
        st.discovery = DiscoveryPhase::Idle;
        st.detected_orca_dir = None;
        st.resolved_krill_bin = None;
        st.scanning = false;
        st.importing = false;
        st.preview = None;
        st.last_result = None;
        st.error = None;
    }
}

fn effective_state_dir(cx: &App) -> Option<PathBuf> {
    if let Some(dir) = state(cx).and_then(|s| s.state_dir_override.clone()) {
        return Some(dir);
    }
    crate::settings::state_dir(cx)
}

#[must_use]
pub fn default_orca_dir(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("orca")
}

/// 현재 실행 맥락에서 감지된 Orca 데이터 디렉터리(`~/Library/Application Support/orca` 등)를 돌려준다.
///
/// 단위·통합 테스트(`cfg!(test)`)에서는 호스트 개발 머신의 실제 `~/Library/Application Support/orca`가
/// 실수로 감지되지 않도록 명시적 오버라이드 또는 격리된 `home_override`/`state_dir/orca`만 검사한다.
#[must_use]
pub fn detect_orca_dir(cx: &App) -> Option<PathBuf> {
    state(cx).and_then(|state| state.detected_orca_dir.clone())
}

fn find_orca_dir(st: &OrcaImportState, state_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(custom) = &st.orca_dir_override {
        return custom.is_dir().then(|| custom.clone());
    }
    if let Some(home) = &st.home_override {
        let candidate = default_orca_dir(home);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    if cfg!(test) {
        return state_dir
            .map(|dir| dir.join("orca"))
            .filter(|path| path.is_dir());
    }
    let candidate = std::env::var_os("KRILL_ORCA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| default_orca_dir(Path::new(&home))))?;
    candidate.is_dir().then_some(candidate)
}

fn discover_paths(cx: &mut App) {
    ensure_state(cx);
    let current = cx.global::<OrcaImportState>();
    if current.discovery != DiscoveryPhase::Idle {
        return;
    }
    let snapshot = OrcaImportState {
        orca_dir_override: current.orca_dir_override.clone(),
        home_override: current.home_override.clone(),
        krill_bin_override: current.krill_bin_override.clone(),
        ..OrcaImportState::default()
    };
    let generation = current.discovery_generation;
    let dir = effective_state_dir(cx);
    cx.global_mut::<OrcaImportState>().discovery = DiscoveryPhase::Pending;
    let job = cx.background_spawn(async move {
        let orca = find_orca_dir(&snapshot, dir.as_deref());
        let bin = find_krill_bin(snapshot.krill_bin_override);
        (orca, bin)
    });
    cx.spawn(async move |cx| {
        let (orca, bin) = job.await;
        cx.update(|cx| {
            let st = cx.global_mut::<OrcaImportState>();
            if st.discovery_generation != generation {
                return;
            }
            st.discovery = DiscoveryPhase::Ready;
            st.detected_orca_dir = orca;
            st.resolved_krill_bin = bin;
            ensure_scanned(cx);
            cx.refresh_windows();
        });
    })
    .detach();
}

#[must_use]
pub fn is_orca_detected(cx: &App) -> bool {
    detect_orca_dir(cx).is_some()
}

/// 데몬 연결 여부를 돌려준다(`OWE-767`).
///
/// 매 UI 렌더마다 `krilld.sock`에 새 소켓을 열지 않고, 셸의 데몬 링크 상태(`daemon_link_up`),
/// 백그라운드 스캔·이관 시점 캐시 값(`daemon_available`), 또는 온보딩 점검 스냅샷(`ChecksSnapshot`)을 읽는다.
#[must_use]
pub fn daemon_available(cx: &App) -> bool {
    if let Some(st) = state(cx) {
        if let Some(up) = st.daemon_link_up {
            return up;
        }
        if let Some(avail) = st.daemon_available {
            return avail;
        }
    }
    if let Some(snap) = cx.try_global::<crate::onboarding::ChecksSnapshot>()
        && let Some(check) = snap.checks.iter().find(|c| c.id == "daemon.connect")
    {
        return check.status == krill_proto::doctor::Status::Ok;
    }
    false
}

/// 백그라운드 스캔·이관 시점에 데몬 소켓이 정식 `ClientHello` 핸드셰이크에 응답하는지 확인한다.
fn probe_daemon_handshake(state_dir: Option<&Path>) -> bool {
    let Some(dir) = state_dir else {
        return false;
    };
    let sock = dir.join("run").join("krilld.sock");
    if !sock.exists() {
        return false;
    }
    let hello = krill_proto::daemon::client_hello(
        krill_proto::ipc::ClientRole::App,
        "krill-app orca-import probe",
    );
    krill_proto::daemon::handshake(&sock, &hello, Duration::from_secs(2)).is_ok()
}

/// 스테이징된 `krill` CLI 바이너리 경로를 찾는다.
///
/// 1) `OrcaImportState::krill_bin_override`
/// 2) `krill_proto::daemon::sibling_exe("krill")` (번들/설치 경로 또는 `target/debug/krill`)
/// 3) `cargo nextest` 실행 시 `target/debug/deps/<test-bin>`의 상위 디렉터리(`target/debug/krill`)
/// 4) `CARGO_TARGET_DIR/debug/krill`
#[must_use]
pub fn resolve_krill_bin(cx: &App) -> Option<PathBuf> {
    state(cx).and_then(|state| {
        state
            .resolved_krill_bin
            .clone()
            .or_else(|| state.krill_bin_override.clone())
    })
}

fn find_krill_bin(override_bin: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(bin) = override_bin
        && bin.is_file()
    {
        return Some(bin);
    }
    if let Ok(sib) = krill_proto::daemon::sibling_exe("krill") {
        if sib.is_file() {
            return Some(sib);
        }
        if let Some(parent_bin) = sib
            .parent()
            .and_then(Path::parent)
            .map(|p| p.join("krill"))
            .filter(|p| p.is_file())
        {
            return Some(parent_bin);
        }
    }
    if let Some(target_dir) = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) {
        for profile in ["debug", "release"] {
            let candidate = target_dir.join(profile).join("krill");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

struct ReapChild(Child);

impl Drop for ReapChild {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

/// 서브프로세스로 `krill import orca`를 실행하고 JSON 리포트를 파싱한다.
pub fn run_krill_import(
    krill_bin: &Path,
    orca_dir: &Path,
    state_dir: Option<&Path>,
    home_dir: Option<&Path>,
    domains: &[ImportDomain],
    dry_run: bool,
) -> io::Result<OrcaImportReport> {
    run_krill_import_with_timeout(
        krill_bin,
        orca_dir,
        state_dir,
        home_dir,
        domains,
        dry_run,
        IMPORT_SUBPROCESS_TIMEOUT,
    )
}

/// 실행 타임아웃과 stdout/stderr `take` 바이트 상한을 적용해 `krill import orca`를 실행한다(`OWE-767`).
pub fn run_krill_import_with_timeout(
    krill_bin: &Path,
    orca_dir: &Path,
    state_dir: Option<&Path>,
    home_dir: Option<&Path>,
    domains: &[ImportDomain],
    dry_run: bool,
    timeout: Duration,
) -> io::Result<OrcaImportReport> {
    if !krill_bin.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            text::ERR_CLI_MISSING.get(),
        ));
    }
    let mut cmd = crate::platform::background_command(krill_bin);
    cmd.arg("import").arg("orca");
    if dry_run {
        cmd.arg("--dry-run");
    }
    cmd.arg("--json");
    if !domains.is_empty() && domains.len() < ImportDomain::ALL.len() {
        let csv = domains
            .iter()
            .map(|d| d.as_str())
            .collect::<Vec<_>>()
            .join(",");
        cmd.arg("--only").arg(csv);
    }
    cmd.arg("--from").arg(orca_dir);
    if let Some(dir) = state_dir {
        cmd.env("KRILL_STATE_DIR", dir);
    }
    if let Some(home) = home_dir {
        cmd.env("HOME", home);
    }
    cmd.env("KRILL_ORCA_DIR", orca_dir);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = ReapChild(cmd.spawn()?);
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing krill import stdout"))?;
    let stderr = child
        .0
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing krill import stderr"))?;

    let (status, stdout_bytes, stderr_bytes) =
        read_import_output(&mut child, stdout, stderr, Instant::now() + timeout)?;

    if u64::try_from(stdout_bytes.len()).unwrap_or(u64::MAX) > MAX_IMPORT_STDOUT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "krill import orca output exceeds size limit",
        ));
    }
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes).trim().to_owned();
        let stdout = String::from_utf8_lossy(&stdout_bytes).trim().to_owned();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            status.to_string()
        };
        return Err(io::Error::other(detail));
    }
    serde_json::from_slice::<OrcaImportReport>(&stdout_bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

fn import_timeout() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "krill import orca timed out")
}

fn read_import_output(
    child: &mut ReapChild,
    stdout: std::process::ChildStdout,
    stderr: std::process::ChildStderr,
    deadline: Instant,
) -> io::Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    let out_reader = crate::process_io::read(stdout, MAX_IMPORT_STDOUT_BYTES + 1, deadline)?;
    let err_reader = crate::process_io::read(stderr, MAX_IMPORT_STDERR_BYTES + 1, deadline)?;
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            return Err(import_timeout());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // Descendants may still own either pipe after the direct child has exited.
    // The shared helper has its own deadline and bounded worker admission.
    let finish_deadline = deadline + Duration::from_millis(50);
    let stdout = out_reader.finish(finish_deadline)?;
    let stderr = err_reader.finish(finish_deadline)?;
    if stdout.timed_out || stderr.timed_out {
        return Err(import_timeout());
    }
    if u64::try_from(stdout.bytes.len()).unwrap_or(u64::MAX) > MAX_IMPORT_STDOUT_BYTES
        || u64::try_from(stderr.bytes.len()).unwrap_or(u64::MAX) > MAX_IMPORT_STDERR_BYTES
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "krill import orca output exceeds size limit",
        ));
    }
    Ok((status, stdout.bytes, stderr.bytes))
}

/// Orca 디렉터리가 감지되었고 아직 미리보기 스캔이 없으면 백그라운드 `--dry-run --json` 스캔을 시작한다.
pub fn ensure_scanned(cx: &mut App) {
    ensure_state(cx);
    discover_paths(cx);
    let Some(orca_dir) = detect_orca_dir(cx) else {
        return;
    };
    let should_scan = state(cx).is_some_and(|s| {
        !s.scanning
            && s.preview
                .as_ref()
                .is_none_or(|p| Path::new(&p.orca_dir) != orca_dir)
    });
    if should_scan {
        scan_preview(cx);
    }
}

/// `krill import orca --dry-run --json`을 비동기 실행해 감지 요약을 갱신한다.
pub fn scan_preview(cx: &mut App) {
    ensure_state(cx);
    if state(cx).is_some_and(|st| st.scanning || st.importing) {
        return;
    }
    discover_paths(cx);
    let Some(orca_dir) = detect_orca_dir(cx) else {
        return;
    };
    let Some(krill_bin) = resolve_krill_bin(cx) else {
        cx.global_mut::<OrcaImportState>().error = Some(text::ERR_CLI_MISSING.get().to_owned());
        cx.refresh_windows();
        return;
    };
    let generation = cx.global::<OrcaImportState>().discovery_generation;
    let state_dir = effective_state_dir(cx);
    let (home_dir, timeout, link_known) = {
        let st = cx.global_mut::<OrcaImportState>();
        st.scanning = true;
        st.error = None;
        (
            st.home_override.clone(),
            st.subprocess_timeout_override
                .unwrap_or(IMPORT_SUBPROCESS_TIMEOUT),
            st.daemon_link_up.is_some(),
        )
    };
    cx.refresh_windows();

    let rx = cx.background_executor().spawn(async move {
        let res = run_krill_import_with_timeout(
            &krill_bin,
            &orca_dir,
            state_dir.as_deref(),
            home_dir.as_deref(),
            &ImportDomain::ALL,
            true,
            timeout,
        );
        let daemon_ok = (!link_known).then(|| probe_daemon_handshake(state_dir.as_deref()));
        (res, daemon_ok)
    });
    cx.spawn(async move |cx| {
        let (res, daemon_ok) = rx.await;
        cx.update(|cx| {
            let st = cx.global_mut::<OrcaImportState>();
            if st.discovery_generation != generation {
                return;
            }
            st.scanning = false;
            if st.daemon_link_up.is_none()
                && let Some(ok) = daemon_ok
            {
                st.daemon_available = Some(ok);
            }
            match res {
                Ok(report) => {
                    st.preview = Some(report);
                    st.error = None;
                }
                Err(e) => {
                    st.error = Some(text::ERR_IMPORT_FAILED.format(&[("error", &e.to_string())]));
                }
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

pub fn open_modal(cx: &mut App) {
    ensure_scanned(cx);
    let st = cx.global_mut::<OrcaImportState>();
    st.modal_open = true;
    cx.refresh_windows();
}

pub fn close_modal(cx: &mut App) {
    ensure_state(cx);
    let st = cx.global_mut::<OrcaImportState>();
    st.modal_open = false;
    cx.refresh_windows();
}

pub fn toggle_domain(domain: ImportDomain, cx: &mut App) {
    ensure_state(cx);
    let st = cx.global_mut::<OrcaImportState>();
    if !st.selected.remove(&domain) {
        st.selected.insert(domain);
    }
    cx.refresh_windows();
}

pub fn import_all(cx: &mut App) {
    ensure_state(cx);
    {
        let st = cx.global_mut::<OrcaImportState>();
        st.selected = ImportDomain::ALL.into_iter().collect();
    }
    import_selected(cx);
}

/// 선택된 카테고리만 `krill import orca --json [--only ...]`로 이관하고, 완료 즉시 앱 상태를 갱신한다.
pub fn import_selected(cx: &mut App) {
    ensure_state(cx);
    let Some(orca_dir) = detect_orca_dir(cx) else {
        return;
    };
    let (domains, importing) = {
        let st = cx.global_mut::<OrcaImportState>();
        (
            st.selected.iter().copied().collect::<Vec<_>>(),
            st.importing,
        )
    };
    if domains.is_empty() || importing {
        return;
    }
    let Some(krill_bin) = resolve_krill_bin(cx) else {
        cx.global_mut::<OrcaImportState>().error = Some(text::ERR_CLI_MISSING.get().to_owned());
        cx.refresh_windows();
        return;
    };
    let generation = cx.global::<OrcaImportState>().discovery_generation;
    let state_dir = effective_state_dir(cx);
    let (home_dir, timeout, link_known) = {
        let st = cx.global_mut::<OrcaImportState>();
        st.importing = true;
        st.error = None;
        (
            st.home_override.clone(),
            st.subprocess_timeout_override
                .unwrap_or(IMPORT_SUBPROCESS_TIMEOUT),
            st.daemon_link_up.is_some(),
        )
    };
    cx.refresh_windows();

    let rx = cx.background_executor().spawn(async move {
        let res = run_krill_import_with_timeout(
            &krill_bin,
            &orca_dir,
            state_dir.as_deref(),
            home_dir.as_deref(),
            &domains,
            false,
            timeout,
        );
        let daemon_ok = (!link_known).then(|| match &res {
            Ok(report) if domains.contains(&ImportDomain::Accounts) => {
                report.accounts_skipped_daemon_reason().is_none()
            }
            _ => probe_daemon_handshake(state_dir.as_deref()),
        });
        (res, daemon_ok)
    });
    cx.spawn(async move |cx| {
        let (res, daemon_ok) = rx.await;
        cx.update(|cx| {
            {
                let st = cx.global_mut::<OrcaImportState>();
                if st.discovery_generation != generation {
                    return;
                }
                st.importing = false;
                if st.daemon_link_up.is_none()
                    && let Some(ok) = daemon_ok
                {
                    st.daemon_available = Some(ok);
                }
                match res {
                    Ok(report) => {
                        if st.preview.is_none() {
                            st.preview = Some(report.clone());
                        }
                        st.last_result = Some(report);
                        st.modal_open = false;
                        st.error = None;
                    }
                    Err(e) => {
                        st.error =
                            Some(text::ERR_IMPORT_FAILED.format(&[("error", &e.to_string())]));
                    }
                }
            }
            // 무중단 즉시 반영: settings.toml/keys.toml 리로드 + Shell/SettingsView/Sidebar 전역 알림
            if cx.try_global::<crate::settings::Config>().is_some() {
                crate::settings::reload(cx);
                crate::settings::notify_external_change(cx);
            } else if let Some(dir) = effective_state_dir(cx) {
                crate::settings::init_at(Some(dir), cx);
                crate::settings::notify_external_change(cx);
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

/// 온보딩 마법사 및 `Settings > General` / `Settings > Accounts` 상단에 삽입되는 자동 감지 카드.
#[must_use]
pub fn render_banner(id_prefix: &'static str, cx: &App) -> Option<Stateful<Div>> {
    let _orca_dir = detect_orca_dir(cx)?;
    let st = state(cx)?;
    // OWE-882: 스캔이 끝났는데 가져올 항목이 하나도 없으면 빈 카드·[전체 가져오기]를 노출하지 않는다.
    if let Some(report) = st.preview.as_ref().or(st.last_result.as_ref())
        && report.total_importable() == 0
    {
        return None;
    }
    let p = ui::palette(cx);
    let daemon_ok = daemon_available(cx);
    let skipped_reason = st
        .last_result
        .as_ref()
        .and_then(OrcaImportReport::accounts_skipped_daemon_reason)
        .map(str::to_owned);

    let summary_text = if st.scanning && st.preview.is_none() {
        text::SCANNING.get().to_owned()
    } else if let Some(ref prev) = st.preview {
        prev.summary_line()
    } else if let Some(ref last) = st.last_result {
        last.summary_line()
    } else {
        text::SCANNING.get().to_owned()
    };

    let banner_id: SharedString = format!("{id_prefix}-orca-import-card").into();
    let choose_id: SharedString = format!("{id_prefix}-orca-choose-btn").into();
    let choose_sel = choose_id.to_string();
    let import_all_id: SharedString = format!("{id_prefix}-orca-import-all-btn").into();
    let import_all_sel = import_all_id.to_string();

    Some(
        ui::card(p)
            .id(banner_id.clone())
            .debug_selector(move || banner_id.to_string())
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .flex_wrap()
                                    .child(
                                        div()
                                            .text_size(TEXT_SM)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.foreground)
                                            .child(text::TITLE.get()),
                                    )
                                    .when(!daemon_ok || skipped_reason.is_some(), |d| {
                                        d.child(ui::badge(
                                            p,
                                            BadgeTone::Warning,
                                            text::DAEMON_REQUIRED_BADGE.get(),
                                        ))
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::SUBTITLE.get()),
                            )
                            .child(
                                div()
                                    .text_size(TEXT_XS)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.foreground)
                                    .child(summary_text),
                            ),
                    )
                    .child(
                        ui::controls()
                            .child(
                                ui::secondary(choose_id, text::OPEN_MODAL.get())
                                    .debug_selector(move || choose_sel.clone())
                                    .disabled(st.importing)
                                    .on_click(|_, _, cx| open_modal(cx)),
                            )
                            .child(
                                ui::button_with_reserved_label(
                                    import_all_id,
                                    text::IMPORT_ALL.get(),
                                    text::IMPORTING.get(),
                                    st.importing,
                                )
                                .debug_selector(move || import_all_sel.clone())
                                .small()
                                .primary()
                                .disabled(st.importing)
                                .on_click(|_, _, cx| import_all(cx)),
                            ),
                    ),
            )
            .children(st.last_result.as_ref().map(|res| {
                let tone = if res.accounts_skipped_daemon_reason().is_some() {
                    BadgeTone::Warning
                } else {
                    BadgeTone::Success
                };
                let result_id: SharedString = format!("{id_prefix}-orca-import-result").into();
                div()
                    .id(result_id.clone())
                    .debug_selector(move || result_id.to_string())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(ui::badge(p, tone, res.result_line()))
                    .children(res.accounts_skipped_daemon_reason().map(|reason| {
                        div()
                            .text_size(TEXT_XS)
                            .text_color(p.warning)
                            .child(text::DAEMON_REQUIRED_NOTE.format(&[("reason", reason)]))
                    }))
                    // OWE-899: 가져오지 않은 터미널 글꼴이 있으면 사유를 밝힌다.
                    .children(res.settings.terminal_font_skipped.as_deref().map(|reason| {
                        let note_id = format!("{id_prefix}-orca-import-font-skipped");
                        div()
                            .id(SharedString::from(note_id.clone()))
                            .debug_selector(move || note_id.clone())
                            .text_size(TEXT_XS)
                            .text_color(p.warning)
                            .child(reason.to_owned())
                    }))
            }))
            .children(
                st.error
                    .as_ref()
                    .map(|err| ui::error_banner(p, err.clone())),
            ),
    )
}

/// 카테고리별 체크박스 선택 모달(`accounts`, `ssh`, `projects`, `settings`).
#[must_use]
pub fn render_modal(window: &Window, cx: &App) -> Option<Deferred> {
    let st = state(cx)?;
    if !st.modal_open {
        return None;
    }
    let p = ui::palette(cx);
    let report = st.preview.as_ref().or(st.last_result.as_ref());
    let daemon_ok = daemon_available(cx);
    let selected_count = st.selected.len();
    let importing = st.importing;
    let error = st.error.clone();
    let skipped_reason = st
        .last_result
        .as_ref()
        .and_then(OrcaImportReport::accounts_skipped_daemon_reason)
        .map(str::to_owned);

    let submit_label = text::IMPORT_SELECTED.format(&[("n", &selected_count.to_string())]);

    let rows = ImportDomain::ALL.into_iter().map(|domain| {
        let checked = st.selected.contains(&domain);
        let needs_daemon_badge = match domain {
            ImportDomain::Accounts => !daemon_ok || skipped_reason.is_some(),
            ImportDomain::Sessions => !daemon_ok,
            _ => false,
        };
        render_domain_row(domain, checked, needs_daemon_badge, report, p)
    });

    let panel = div()
        .id("orca-import-modal")
        .debug_selector(|| "orca-import-modal".into())
        .w(DIALOG_WIDTH)
        .max_w(window.viewport_size().width - SPACE_4 * 2.)
        .p_5()
        .rounded(RADIUS_LG)
        .bg(p.popover)
        .border_1()
        .border_color(p.border)
        .shadow_lg()
        .text_color(p.popover_foreground)
        .flex()
        .flex_col()
        .gap_3()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        // OWE-882: STYLEGUIDE §6.1·§7.5 — 배경 클릭도 취소와 같은 안전한 닫기로 취급한다.
        // 가져오는 동안에는 실수로 닫히지 않게 막는다(취소 버튼과 동일 조건).
        .on_mouse_down_out(|_, _, cx| {
            if !state(cx).is_some_and(|st| st.importing) {
                close_modal(cx);
            }
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(TEXT_LG)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(text::MODAL_TITLE.get()),
                )
                .child(
                    div()
                        .text_size(TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::MODAL_SUBTITLE.get()),
                ),
        )
        .child(div().flex().flex_col().gap_2().children(rows))
        .when(
            (!daemon_ok && st.selected.contains(&ImportDomain::Accounts))
                || skipped_reason.is_some(),
            |d| {
                let reason = skipped_reason
                    .as_deref()
                    .unwrap_or_else(|| text::DAEMON_REQUIRED_BADGE.get());
                d.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded(RADIUS_SM)
                        .border_1()
                        .border_color(p.warning.opacity(0.35))
                        .bg(p.warning.opacity(0.10))
                        .text_size(TEXT_XS)
                        .text_color(p.warning)
                        .child(text::DAEMON_REQUIRED_NOTE.format(&[("reason", reason)])),
                )
            },
        )
        .children(error.map(|e| ui::error_banner(p, e)))
        .child(
            div()
                .pt_2()
                .border_t_1()
                .border_color(p.border)
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    ui::secondary("orca-modal-rescan", text::RESCAN.get())
                        .debug_selector(|| "orca-modal-rescan".to_owned())
                        .disabled(importing)
                        .on_click(|_, _, cx| scan_preview(cx)),
                )
                .child(
                    ui::controls()
                        .child(
                            // OWE-882: 취소는 destructive가 아니라 STYLEGUIDE §6.1·§7.5의 ghost다.
                            ui::ghost("orca-modal-cancel", text::CANCEL.get())
                                .debug_selector(|| "orca-modal-cancel".into())
                                .disabled(importing)
                                .on_click(|_, _, cx| close_modal(cx)),
                        )
                        .child(
                            ui::button_with_reserved_label(
                                "orca-modal-submit",
                                submit_label,
                                text::IMPORTING.get(),
                                importing,
                            )
                            .debug_selector(|| "orca-modal-submit".to_owned())
                            .small()
                            .primary()
                            .disabled(importing || selected_count == 0)
                            .on_click(|_, _, cx| import_selected(cx)),
                        ),
                ),
        );

    Some(ui::modal_backdrop(window.viewport_size(), panel))
}

fn render_domain_row(
    domain: ImportDomain,
    checked: bool,
    needs_daemon_badge: bool,
    report: Option<&OrcaImportReport>,
    p: &Palette,
) -> Stateful<Div> {
    let row_id: SharedString = format!("orca-domain-{}", domain.as_str()).into();
    let border_col = if checked {
        p.foreground.opacity(0.35)
    } else {
        p.border
    };
    let box_bg = if checked { p.foreground } else { p.background };
    let box_fg = if checked { p.background } else { p.foreground };

    ui::card(p)
        .id(row_id.clone())
        .debug_selector(move || row_id.to_string())
        .border_color(border_col)
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .on_click(move |_, _, cx| toggle_domain(domain, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_start()
                .gap_2p5()
                .child(
                    div()
                        .w(px(16.))
                        .h(px(16.))
                        .mt(px(2.))
                        .rounded(RADIUS_SM)
                        .border_1()
                        .border_color(border_col)
                        .bg(box_bg)
                        .flex()
                        .items_center()
                        .justify_center()
                        .children(checked.then(|| {
                            Icon::new(IconName::Check)
                                .with_size(ui::ICON_XS)
                                .text_color(box_fg)
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .flex_wrap()
                                .child(
                                    div()
                                        .text_size(TEXT_SM)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.foreground)
                                        .child(domain.title()),
                                )
                                .when(needs_daemon_badge, |d| {
                                    d.child(ui::badge(
                                        p,
                                        BadgeTone::Warning,
                                        text::DAEMON_REQUIRED_BADGE.get(),
                                    ))
                                }),
                        )
                        .child(
                            div()
                                .text_size(TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(domain.description(report)),
                        ),
                ),
        )
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    /// 0개 항목은 요약에서 빠진다(맥미니-설 실기 캡처의 "Claude 계정 0개 · …" 잡음).
    #[test]
    fn summary_line_hides_zero_counts() {
        let mut r = OrcaImportReport::default();
        assert_eq!(r.summary_line(), "");
        r.ssh.hosts_added = 1;
        r.projects.projects_added = 2;
        let line = r.summary_line();
        assert!(!line.contains(" 0"), "{line}");
        assert_eq!(line.matches(" · ").count(), 1, "{line}");
        assert!(line.contains('1') && line.contains('2'), "{line}");
    }

    use krill_proto::os::net::UnixStream;
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use gpui::{TestAppContext, VisualTestContext};
    use krill_proto::ipc;
    use krill_proto::screen::Msg as _;
    use krill_proto::usage::{
        FEAT_ACCOUNT_CONNECTION, FEAT_ACCOUNT_POLICY, FEAT_USAGE, FEAT_USAGE_AGY,
        FEAT_USAGE_MANAGE, FEAT_USAGE_RATELIMIT, FEAT_USAGE_RULES, FEAT_USAGE_STATUS, UsageReply,
        UsageRequest,
    };

    use super::*;
    use crate::settings::{AdminTab, SettingsView};
    use crate::sidebar::Sidebar;

    struct OrcaFixture {
        root: PathBuf,
        /// 이관 저장소·워크트리·폴더가 실제로 있는 디렉터리(사이드바는 디스크에 있는 이관 워크트리만 보인다).
        work: PathBuf,
        home: PathBuf,
        state_dir: PathBuf,
        orca_dir: PathBuf,
    }

    impl Drop for OrcaFixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).ok();
        }
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
        let status = crate::platform::background_command("/usr/bin/sqlite3")
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

    fn setup_orca_fixture(tag: &str) -> OrcaFixture {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("owe727-{tag}-{}-{n}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let state_dir = root.join("state");
        let orca_dir = default_orca_dir(&home);
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&orca_dir).unwrap();
        let work = fs::canonicalize(&root).unwrap().join("work");
        for d in ["krill", "krill-wt-1", "notes"] {
            fs::create_dir_all(work.join(d)).unwrap();
        }
        let work_str = work.to_string_lossy().into_owned();
        let at_work = |text: &str| text.replace("/Users/test", &work_str);

        // Active profile "work"
        fs::write(
            orca_dir.join("orca-profile-index.json"),
            r#"{"version":1,"activeProfileId":"work","profiles":[{"id":"default","name":"Default"},{"id":"work","name":"Work"}]}"#,
        )
        .unwrap();

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
            r#"{"oauthAccount":{"emailAddress":"fallback@example.com","organizationName":"Fallback Org"}}"#,
        )
        .unwrap();

        let codex_home_1 = orca_dir
            .join("codex-accounts")
            .join(codex_uuid_1)
            .join("home");
        fs::create_dir_all(&codex_home_1).unwrap();

        let profile_dir = orca_dir.join("profiles").join("work");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("orca-data.json"),
            at_work(r#"{
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
                    {"provider":"linear","kind":"issue","identifier":"OWE-727","url":"https://linear.app/issue/OWE-727"}
                  ]
                }
              ]
            }"#),
        )
        .unwrap();

        fs::write(
            orca_dir.join("orca-environments.json"),
            r#"{
              "version": 1,
              "environments": [
                {
                  "id": "env-remote-box",
                  "name": "Build Box",
                  "endpoints": [{"id":"ep2","type":"direct","endpoint":"wss://build.internal.example.com:8443","priority":1}]
                }
              ]
            }"#,
        )
        .unwrap();

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
                "symlinkPaths": [".env.local"]
            }
        ])
        .to_string();

        let wt_meta_payload = serde_json::json!({
            "repo-krill::/Users/test/krill-wt-1": {
                "displayName": "Feature Import",
                "comment": "Working on Orca migration",
                "linkedPR": 727,
                "linkedLinearIssue": "OWE-727",
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
                ("repos", &at_work(&repos_payload)),
                ("worktreeMeta", &at_work(&wt_meta_payload)),
                ("worktreeLineageById", &at_work(&lineage_payload)),
                ("automations", &automations_payload),
            ],
        );

        OrcaFixture {
            root,
            work,
            home,
            state_dir,
            orca_dir,
        }
    }

    fn wait_until(cx: &mut TestAppContext, mut cond: impl FnMut(&mut TestAppContext) -> bool) {
        for _ in 0..300 {
            cx.executor().run_until_parked();
            if cond(cx) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("condition did not settle in time");
    }

    #[gpui::test]
    fn settings_detect_orca_dir_and_render_preview_counts(cx: &mut TestAppContext) {
        let fx = setup_orca_fixture("detect");
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::settings::init_at(Some(fx.state_dir.clone()), cx);
        });

        // 1. When no Orca dir exists, render_banner returns None
        let empty_home = fx.root.join("empty-home");
        fs::create_dir_all(&empty_home).unwrap();
        cx.update(|cx| {
            set_env_paths(
                Some(empty_home.clone()),
                None,
                Some(fx.state_dir.clone()),
                None,
                cx,
            );
            assert!(!is_orca_detected(cx));
            assert!(render_banner("test", cx).is_none());
        });

        // 2. Point to fixture HOME with `~/Library/Application Support/orca` and open Settings
        //    (온보딩에는 Orca 가져오기 카드가 없다, OWE-904).
        cx.update(|cx| {
            set_env_paths(
                Some(fx.home.clone()),
                None,
                Some(fx.state_dir.clone()),
                None,
                cx,
            );
        });
        let (settings_view, _) = cx.add_window_view(|_, cx| SettingsView::new(cx));

        // Wait for background `krill import orca --dry-run --json` preview scan to complete
        wait_until(cx, |cx| {
            cx.read(|cx| state(cx).is_some_and(|st| !st.scanning && st.preview.is_some()))
        });

        cx.read(|cx| {
            let st = state(cx).expect("OrcaImportState");
            assert!(st.error.is_none(), "unexpected error: {:?}", st.error);
            let prev = st.preview.as_ref().expect("preview report");
            assert!(prev.dry_run);
            assert_eq!(prev.accounts.claude_added, 2);
            assert_eq!(prev.accounts.codex_added, 1);
            assert_eq!(prev.ssh.hosts_added, 2);
            assert_eq!(prev.projects.projects_added, 2);
            assert_eq!(prev.projects.worktrees_migrated, 2);
            assert_eq!(prev.settings.quick_commands_merged, 1);
            assert_eq!(prev.settings.schedules_merged, 1);

            let summary = prev.summary_line();
            assert!(summary.contains("Claude 계정 2"), "{summary}");
            assert!(summary.contains("Codex 계정 1"), "{summary}");
            assert!(summary.contains("원격 호스트 2"), "{summary}");
            assert!(summary.contains("프로젝트/폴더 2"), "{summary}");
            assert!(summary.contains("워크트리 메타 2"), "{summary}");
            assert!(summary.contains("빠른 명령/스케줄 2"), "{summary}");
        });

        // 3. Verify SettingsView General tab renders `settings-general-orca-import-card`
        let settings_win: gpui::AnyWindowHandle = cx
            .windows()
            .into_iter()
            .find(|w| w.downcast::<SettingsView>().is_some())
            .expect("settings window");
        let mut settings_vcx = VisualTestContext::from_window(settings_win, cx);
        settings_vcx.run_until_parked();
        assert!(
            settings_vcx
                .debug_bounds("settings-general-orca-import-card")
                .is_some(),
            "Settings > General should render settings-general-orca-import-card"
        );

        // 4. Switch SettingsView to Accounts tab and verify `settings-accounts-orca-import-card`
        settings_view.update(&mut settings_vcx, |view, cx| {
            view.set_tab(AdminTab::Accounts, cx);
        });
        settings_vcx.run_until_parked();
        assert!(
            settings_vcx
                .debug_bounds("settings-accounts-orca-import-card")
                .is_some(),
            "Settings > Accounts should render settings-accounts-orca-import-card"
        );
    }

    #[gpui::test]
    fn modal_selective_import_and_daemon_offline_skip_then_live_reflection_without_restart(
        cx: &mut TestAppContext,
    ) {
        let fx = setup_orca_fixture("live");
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::settings::init_at(Some(fx.state_dir.clone()), cx);
            set_env_paths(
                Some(fx.home.clone()),
                Some(fx.orca_dir.clone()),
                Some(fx.state_dir.clone()),
                None,
                cx,
            );
        });

        let sidebar_path = fx.state_dir.join("sidebar.bin");
        let (sidebar, _) = cx.add_window_view(|_, _| Sidebar::new(Some(sidebar_path)));
        let (settings_view, _) = cx.add_window_view(|_, cx| SettingsView::new(cx));

        wait_until(cx, |cx| {
            cx.read(|cx| state(cx).is_some_and(|st| !st.scanning && st.preview.is_some()))
        });

        let settings_win: gpui::AnyWindowHandle = cx
            .windows()
            .into_iter()
            .find(|w| w.downcast::<SettingsView>().is_some())
            .expect("settings window");
        let mut settings_vcx = VisualTestContext::from_window(settings_win, cx);
        settings_vcx.run_until_parked();

        // Click "항목 선택…" button on Settings > General banner to open the category selection modal
        let choose_center = settings_vcx
            .debug_bounds("settings-general-orca-choose-btn")
            .expect("choose button")
            .center();
        settings_vcx.simulate_click(choose_center, gpui::Modifiers::default());
        settings_vcx.run_until_parked();

        assert!(
            settings_vcx.debug_bounds("orca-import-modal").is_some(),
            "Orca import modal should be open"
        );
        for selector in [
            "orca-domain-accounts",
            "orca-domain-ssh",
            "orca-domain-projects",
            "orca-domain-settings",
            "orca-domain-sessions",
            "orca-modal-submit",
        ] {
            assert!(
                settings_vcx.debug_bounds(selector).is_some(),
                "expected selector {selector} in modal"
            );
        }

        // Step A: Click submit with all domains while krilld is NOT running.
        // Accounts must be skipped with "데몬 필요", while SSH, Projects, and Settings apply immediately!
        let submit_center = settings_vcx
            .debug_bounds("orca-modal-submit")
            .expect("submit button")
            .center();
        settings_vcx.simulate_click(submit_center, gpui::Modifiers::default());

        wait_until(cx, |cx| {
            cx.read(|cx| state(cx).is_some_and(|st| !st.importing && st.last_result.is_some()))
        });
        // Allow SettingsView background refresh and Sidebar observer to settle
        wait_until(cx, |cx| {
            settings_view.read_with(cx, |v, _| v.ssh_hosts().len() == 2)
        });

        cx.read(|cx| {
            let st = state(cx).expect("OrcaImportState");
            let res = st.last_result.as_ref().expect("last_result");
            assert!(res.accounts.skipped);
            assert!(
                res.accounts
                    .skipped_reason
                    .as_deref()
                    .is_some_and(|r| r.starts_with("데몬 필요")),
                "expected 데몬 필요 reason, got {:?}",
                res.accounts.skipped_reason
            );
            assert_eq!(res.ssh.hosts_added, 2);
            assert_eq!(res.projects.projects_added, 2);
            assert_eq!(res.projects.worktrees_migrated, 2);
            assert!(res.settings.settings_updated);

            // Verify immediate live reflection in Settings (without app restart)
            let s = crate::settings::current(cx);
            assert_eq!(s.terminal_font_family, "JetBrains Mono");
            assert_eq!(s.terminal_font_size(), 15);
            assert_eq!(s.keep_awake, crate::settings::KeepAwake::On);
            assert!(
                s.quick_commands
                    .iter()
                    .any(|q| q.name == "Run Check" && q.text == "scripts/check.sh")
            );

            // Verify immediate live reflection in SettingsView SSH hosts list
            let hosts = settings_view.read(cx).ssh_hosts();
            assert_eq!(hosts.len(), 2);
            assert!(
                hosts
                    .iter()
                    .any(|h| h.label == "Mac Mini M4" && h.destination == "m4-mini")
            );
            assert!(
                hosts.iter().any(
                    |h| h.label == "Build Box" && h.destination == "build.internal.example.com"
                )
            );

            // Verify immediate live reflection in Sidebar project & worktree tree
            let sb = sidebar.read(cx);
            let repo_path = fx.work.join("krill");
            let wt_path = fx.work.join("krill-wt-1");
            let notes_path = fx.work.join("notes");
            assert!(
                sb.trees()
                    .get(&repo_path)
                    .is_some_and(|set| set.contains(&wt_path)),
                "Sidebar trees should contain imported repo and worktree: {:?}",
                sb.trees()
            );
            assert!(
                sb.trees().contains_key(&notes_path),
                "Sidebar trees should contain imported folder workspace: {:?}",
                sb.trees()
            );
            assert_eq!(
                sb.comments().get(&wt_path).map(String::as_str),
                Some("Working on Orca migration")
            );
            assert_eq!(
                sb.issues().get(&wt_path).map(String::as_str),
                Some("OWE-727")
            );
            assert_eq!(
                sb.worktree_statuses().get(&wt_path),
                Some(&krill_proto::orchestration::WorkspaceStatus::InProgress)
            );
            assert_eq!(
                sb.worktree_parent(&wt_path),
                Some(repo_path.clone()),
                "Imported lineage parent_worktree_path should land in WorktreeStore (lineage SSOT) and reach Sidebar::merged_parents"
            );
        });

        // Step B: Start mock krilld on `<state_dir>/run/krilld.sock` and import `accounts` domain only.
        // Verify multi-account list in SettingsView updates immediately without restart!
        let run_dir = fx.state_dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let sock_path = run_dir.join("krilld.sock");
        let listener = krill_proto::os::net::UnixListener::bind(&sock_path).unwrap();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_flag);
        let mock_home = fx.home.clone();
        let mock_state = fx.state_dir.clone();
        let server_thread = std::thread::spawn(move || {
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
                if krill_proto::ipc::ClientHello::decode(kind, &body).is_err() {
                    continue;
                }
                let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) else {
                    continue;
                };
                let Ok(req) = UsageRequest::decode(kind, &body) else {
                    continue;
                };
                let snap =
                    crate::accounts::request_local(&mock_home, Some(&mock_state), &req).unwrap();
                let reply = UsageReply::Accounts(snap);
                stream.write_all(&reply.encode().unwrap()).ok();
            }
        });

        // Open modal, select only `accounts`, and run import_selected
        cx.update(|cx| {
            open_modal(cx);
            toggle_domain(ImportDomain::Ssh, cx);
            toggle_domain(ImportDomain::Projects, cx);
            toggle_domain(ImportDomain::Settings, cx);
            toggle_domain(ImportDomain::Sessions, cx);
            import_selected(cx);
        });

        wait_until(cx, |cx| {
            cx.read(|cx| {
                state(cx).is_some_and(|st| {
                    !st.importing
                        && st
                            .last_result
                            .as_ref()
                            .is_some_and(|r| !r.accounts.skipped && r.accounts.claude_added == 2)
                })
            })
        });

        // Wait for SettingsView background account refresh to reflect the imported accounts
        wait_until(cx, |cx| {
            settings_view.read_with(cx, |v, _| {
                v.accounts()
                    .accounts
                    .iter()
                    .filter(|a| !a.host_default)
                    .count()
                    >= 3
            })
        });

        cx.read(|cx| {
            let accounts = &settings_view.read(cx).accounts().accounts;
            assert!(
                accounts
                    .iter()
                    .any(|a| a.label == "primary@example.com (Acme Corp)" && a.active),
                "expected active primary Claude account in {accounts:?}"
            );
            assert!(
                accounts
                    .iter()
                    .any(|a| a.label == "fallback@example.com (Fallback Org)"),
                "expected fallback Claude account in {accounts:?}"
            );
            assert!(
                accounts
                    .iter()
                    .any(|a| a.label == "codex@example.com (Core Team)" && a.active),
                "expected active Codex account in {accounts:?}"
            );
        });

        stop_flag.store(true, Ordering::Relaxed);
        UnixStream::connect(&sock_path).ok();
        server_thread.join().ok();
    }

    #[gpui::test]
    fn owe767_no_per_render_socket_connect_and_import_enforces_timeout_and_byte_cap(
        cx: &mut TestAppContext,
    ) {
        let fx = setup_orca_fixture("owe767");
        let run_dir = fx.state_dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let sock_path = run_dir.join("krilld.sock");
        let listener = krill_proto::os::net::UnixListener::bind(&sock_path).unwrap();
        listener.set_nonblocking(true).unwrap();

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::settings::init_at(Some(fx.state_dir.clone()), cx);
            set_env_paths(
                Some(fx.home.clone()),
                Some(fx.orca_dir.clone()),
                Some(fx.state_dir.clone()),
                None,
                cx,
            );
            set_daemon_link_up(false, cx);
            // Isolate the render test after background directory discovery has completed.
            cx.global_mut::<OrcaImportState>().detected_orca_dir = Some(fx.orca_dir.clone());
        });
        wait_until(cx, |cx| cx.read(is_orca_detected));

        // 1. Repeatedly call render_banner / daemon_available on the UI thread and verify 0 socket connects
        cx.read(|cx| {
            for _ in 0..10 {
                assert!(!daemon_available(cx));
                assert!(render_banner("test", cx).is_some());
            }
        });
        assert!(
            listener.accept().is_err(),
            "render_banner / daemon_available must never connect to krilld.sock on UI render frames"
        );

        cx.update(|cx| set_daemon_link_up(true, cx));
        cx.read(|cx| {
            assert!(daemon_available(cx));
            assert!(render_banner("test", cx).is_some());
        });
        assert!(
            listener.accept().is_err(),
            "set_daemon_link_up(true) must satisfy daemon_available without opening a socket"
        );

        // 2. Verify subprocess timeout resets scanning/importing and surfaces TimedOut error
        let slow_bin = fx.root.join("krill-slow.sh");
        fs::write(&slow_bin, "#!/bin/sh\nsleep 10\n").unwrap();
        fs::set_permissions(&slow_bin, fs::Permissions::from_mode(0o755)).unwrap();

        let err = run_krill_import_with_timeout(
            &slow_bin,
            &fx.orca_dir,
            Some(&fx.state_dir),
            Some(&fx.home),
            &ImportDomain::ALL,
            true,
            Duration::from_millis(60),
        )
        .expect_err("slow subprocess must time out");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);

        cx.update(|cx| {
            set_env_paths(
                Some(fx.home.clone()),
                Some(fx.orca_dir.clone()),
                Some(fx.state_dir.clone()),
                Some(slow_bin.clone()),
                cx,
            );
            cx.global_mut::<OrcaImportState>()
                .subprocess_timeout_override = Some(Duration::from_millis(60));
            scan_preview(cx);
        });
        wait_until(cx, |cx| {
            cx.read(|cx| state(cx).is_some_and(|st| !st.scanning))
        });
        cx.read(|cx| {
            let st = state(cx).unwrap();
            assert!(!st.scanning);
            assert!(
                st.error.as_deref().is_some_and(|e| e.contains("timed out")),
                "expected timeout error, got {:?}",
                st.error
            );
        });

        // 3. Verify stdout byte cap rejects oversized output with InvalidData
        let huge_bin = fx.root.join("krill-huge.sh");
        fs::write(
            &huge_bin,
            format!(
                "#!/bin/sh\nhead -c {} /dev/zero\n",
                MAX_IMPORT_STDOUT_BYTES + 1024
            ),
        )
        .unwrap();
        fs::set_permissions(&huge_bin, fs::Permissions::from_mode(0o755)).unwrap();

        let err = run_krill_import_with_timeout(
            &huge_bin,
            &fx.orca_dir,
            Some(&fx.state_dir),
            Some(&fx.home),
            &ImportDomain::ALL,
            true,
            Duration::from_secs(5),
        )
        .expect_err("oversized stdout must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("exceeds size limit"), "{err}");
    }

    #[test]
    fn owe794_modal_category_descriptions_match_hosts_toml_and_profile_dir_registration() {
        let mut report = OrcaImportReport::default();
        report.accounts.claude_added = 2;
        report.accounts.codex_added = 1;
        report.ssh.hosts_added = 3;

        let accounts_desc = ImportDomain::Accounts.description(Some(&report));
        assert!(
            accounts_desc.contains("Claude 2") && accounts_desc.contains("Codex 1"),
            "{accounts_desc}"
        );
        assert!(
            accounts_desc.contains("프로필 디렉터리 경로 등록")
                || accounts_desc.contains("profile directory paths registered"),
            "{accounts_desc}"
        );
        assert!(
            !accounts_desc.contains("Keychain")
                && !accounts_desc.contains("인증 토큰")
                && !accounts_desc.contains("auth tokens"),
            "{accounts_desc}"
        );
        let acc_ko = text::CAT_ACCOUNTS_DESC.in_language(crate::i18n::Language::Ko);
        let acc_en = text::CAT_ACCOUNTS_DESC.in_language(crate::i18n::Language::En);
        assert!(!acc_ko.contains("Keychain"));
        assert!(!acc_ko.contains("인증 토큰"));
        assert!(!acc_en.contains("Keychain"));
        assert!(!acc_en.contains("auth tokens"));

        let ssh_desc = ImportDomain::Ssh.description(Some(&report));
        assert!(ssh_desc.contains("hosts.toml"), "{ssh_desc}");
        assert!(!ssh_desc.contains("ssh-hosts.toml"), "{ssh_desc}");
        let ssh_ko = text::CAT_SSH_DESC.in_language(crate::i18n::Language::Ko);
        let ssh_en = text::CAT_SSH_DESC.in_language(crate::i18n::Language::En);
        assert!(ssh_ko.contains("hosts.toml"));
        assert!(!ssh_ko.contains("ssh-hosts.toml"));
        assert!(ssh_en.contains("hosts.toml"));
        assert!(!ssh_en.contains("ssh-hosts.toml"));

        // OWE-882: 프로젝트 설명의 코멘트 수는 워크트리 수가 아니라 comments_migrated다.
        let mut proj_report = OrcaImportReport::default();
        proj_report.projects.projects_added = 2;
        proj_report.projects.worktrees_migrated = 5;
        proj_report.projects.comments_migrated = 3;
        let proj_desc = ImportDomain::Projects.description(Some(&proj_report));
        assert!(proj_desc.contains("코멘트 3"), "{proj_desc}");
        assert!(!proj_desc.contains("코멘트 5"), "{proj_desc}");
    }

    #[gpui::test]
    fn owe882_empty_report_hides_banner(cx: &mut TestAppContext) {
        let fx = setup_orca_fixture("empty-banner");
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::settings::init_at(Some(fx.state_dir.clone()), cx);
            ensure_state(cx);
            set_env_paths(
                Some(fx.home.clone()),
                None,
                Some(fx.state_dir.clone()),
                None,
                cx,
            );
            // This render-only fixture starts with an already discovered directory.
            cx.global_mut::<OrcaImportState>().detected_orca_dir = Some(fx.orca_dir.clone());
            assert!(is_orca_detected(cx));
            // 스캔 전(preview 없음): 스캔 중 카드 노출
            assert!(render_banner("test", cx).is_some());
            // 스캔 완료 0건: 빈 카드·[전체 가져오기] 대신 배너 숨김
            assert_eq!(OrcaImportReport::default().total_importable(), 0);
            cx.global_mut::<OrcaImportState>().preview = Some(OrcaImportReport::default());
            assert!(render_banner("test", cx).is_none());
            // 항목이 하나라도 있으면 노출
            let mut with_items = OrcaImportReport::default();
            with_items.ssh.hosts_added = 1;
            assert_eq!(with_items.total_importable(), 1);
            cx.global_mut::<OrcaImportState>().preview = Some(with_items);
            assert!(render_banner("test", cx).is_some());
        });
    }
    #[gpui::test]
    fn async_discovery_ignores_previous_environment(cx: &mut TestAppContext) {
        let fx = setup_orca_fixture("async-discovery");
        let second = fx.root.join("second-orca");
        fs::create_dir_all(&second).unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            set_env_paths(
                Some(fx.home.clone()),
                Some(fx.orca_dir.clone()),
                Some(fx.state_dir.clone()),
                None,
                cx,
            );
            discover_paths(cx);
            assert_eq!(state(cx).unwrap().discovery, DiscoveryPhase::Pending);
            assert!(detect_orca_dir(cx).is_none());
            set_env_paths(None, Some(second.clone()), None, None, cx);
            let preview = OrcaImportReport {
                orca_dir: second.display().to_string(),
                ..OrcaImportReport::default()
            };
            cx.global_mut::<OrcaImportState>().preview = Some(preview);
            discover_paths(cx);
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(detect_orca_dir(cx), Some(second));
            assert_ne!(state(cx).unwrap().discovery, DiscoveryPhase::Pending);
            assert!(state(cx).unwrap().error.is_none());
        });
    }
    #[test]
    fn async_import_deadline_covers_descendant_owned_output_pipes() {
        let fx = setup_orca_fixture("descendant-pipe");
        let bin = fx.root.join("descendant-output.sh");
        let report = serde_json::to_string(&OrcaImportReport::default()).unwrap();
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\nsleep 1 &\nprintf '%s' '{}'\nexit 0\n",
                report.replace('\'', "'\\''")
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let result = run_krill_import_with_timeout(
            &bin,
            &fx.orca_dir,
            Some(&fx.state_dir),
            Some(&fx.home),
            &ImportDomain::ALL,
            true,
            Duration::from_millis(50),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }
}
