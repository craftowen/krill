//! MK-046 sidebar. The gpui dependency and polling live behind this module (MK-030 provisional).
//! Unchanged snapshots never notify the view; only visible rows are constructed.
//! OWE-576: 레포 > 워크트리 행(브랜치·미커밋 수·상태 점·에이전트), 세션 행(의미 있는 이름·스피너),
//! 행 컨텍스트 메뉴(Open in·핀·이름 변경), 키보드 탐색. 색·치수는 `crate::ui` 토큰만 쓴다.
//! MK-131: 다중 호스트(내 Mac + 원격 호스트) 섹션 그룹핑·상태 점·재연결.
//! 호스트 섹션 그룹핑·헤더 배치 참고:
//! `.ref/orca/src/renderer/src/components/sidebar/worktree-list/host-section-rows.ts`,
//! `.ref/orca/src/renderer/src/components/sidebar/worktree-list/rows/HostSectionHeader.tsx`
//! (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use krill_proto::os::fs::OpenOptionsExt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, KeyBinding, ScrollStrategy,
    Subscription, Task, UniformListScrollHandle, Window, actions,
};
use gpui_component::input::{InputEvent, InputState};
use krill_proto::SessionId;
use krill_proto::agent::Agents;
use krill_proto::control::{AgentProfile, AgentState, AgentStatus, NewWorktree, Program, Reply};
use serde::{Deserialize, Serialize};

use crate::hosts::{HostId, HostManager, HostStatus, HostTarget};
use crate::jump::Item;
use crate::open_in::Target;
use crate::worktree;

pub mod external;
mod ipc;
pub mod lineage;
pub mod options;
mod pr;
mod view;

pub use lineage::{MAX_LINEAGE_DEPTH, can_assign_parent};
pub use options::{
    CardLayout, CardProperties, CardProperty, GroupBy, GroupKey, PrGroup, SortBy, WorkspaceFilters,
    WorkspaceOptions,
};

/// 창과 독립된 메뉴바 모니터도 같은 IPC 스냅샷 경계를 쓴다.
pub fn status_source(target: impl Into<HostTarget>) -> Arc<dyn Source> {
    Arc::new(ipc::Socket::new(target))
}

use crate::i18n::catalog::sidebar as text;

actions!(
    sidebar,
    [
        SelectPrev,
        SelectNext,
        Activate,
        Collapse,
        Expand,
        Dismiss,
        TogglePin,
        Rename,
        OpenInFinder,
        OpenInVsCode,
        OpenInCursor,
        OpenInZed,
        NewAgent1,
        NewAgent2,
        NewAgent3,
        NewAgent4,
    ]
);

const KEY_CONTEXT: &str = "Sidebar";
/// 행 높이(`uniform_list`라 모든 행이 같다).
const ROW_HEIGHT: f32 = 32.;
/// 가상 리스트로 전환하는 행 수. 이 이상이면 `uniform_list`(고정 높이)라
/// 워크트리 카드는 고정 높이 컴팩트 변형으로 렌더한다(OWE-900).
const VIRTUALIZED_ROWS: usize = 100;
/// 프로젝트(저장소·폴더·그룹) 아래 워크트리·세션 행의 들여쓰기(px). 호스트 > 프로젝트 > 워크트리 단계가 보이게 한다.
const CHILD_INDENT: f32 = 20.;
/// 호스트 머리줄이 있을 때(원격 호스트 등록) 그 아래 프로젝트 행의 들여쓰기(px).
const HOST_CHILD_INDENT: f32 = 12.;
/// 세션 없는 워크트리가 이 수를 넘으면 프로젝트를 접힌 상태로 시작한다(OWE-900).
/// 코디네이터 결정: 기본값 `hide_sleeping = false` 유지, 대신 다량 유휴 프로젝트 자동 접힘.
const AUTO_COLLAPSE_IDLE_WORKTREES: usize = 10;
/// hover 액션을 드러내는 그룹 이름(가장 가까운 행에 걸린다).
const ROW_GROUP: &str = "sidebar-row";
const POLL_INTERVAL_ACTIVE: Duration = Duration::from_millis(500);
const POLL_INTERVAL_INACTIVE: Duration = Duration::from_millis(2000);

/// 사이드바가 포커스를 가졌을 때의 키(↑↓ 이동, Enter 열기·새 터미널, ←→ 접기, p 핀, r/F2 이름 변경,
/// o/v/c/z Open in, ⌥⌘1~4 새 에이전트, Esc 터미널로).
pub fn bind_keys(cx: &mut App) {
    let ctx = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, ctx),
        KeyBinding::new("down", SelectNext, ctx),
        KeyBinding::new("enter", Activate, ctx),
        KeyBinding::new("left", Collapse, ctx),
        KeyBinding::new("right", Expand, ctx),
        KeyBinding::new("escape", Dismiss, ctx),
        KeyBinding::new("p", TogglePin, ctx),
        KeyBinding::new("r", Rename, ctx),
        KeyBinding::new("f2", Rename, ctx),
        KeyBinding::new("o", OpenInFinder, ctx),
        KeyBinding::new("v", OpenInVsCode, ctx),
        KeyBinding::new("c", OpenInCursor, ctx),
        KeyBinding::new("z", OpenInZed, ctx),
        crate::keys::bind("alt-cmd-1", NewAgent1, ctx),
        crate::keys::bind("alt-cmd-2", NewAgent2, ctx),
        crate::keys::bind("alt-cmd-3", NewAgent3, ctx),
        crate::keys::bind("alt-cmd-4", NewAgent4, ctx),
    ]);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    pub status: AgentStatus,
    pub worktree: Option<PathBuf>,
    /// 세션의 현재 폴더. git 저장소가 아닌 폴더에도 Open in을 제공한다(MK-055).
    pub cwd: Option<PathBuf>,
    /// 실행 당시 계정 표시용(OWE-707). 셸·구 기록이면 `None`이라 배지를 숨긴다.
    pub account: Option<krill_proto::control::SessionAccount>,
}

/// 워크트리 행에 붙는 git 정보. 모르면 `None`(UI가 숨긴다).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tree {
    pub branch: Option<String>,
    pub dirty: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub sessions: Vec<Session>,
    /// Repository -> worktrees, including worktrees without a session.
    pub trees: BTreeMap<PathBuf, Vec<PathBuf>>,
}

/// Worktree -> 브랜치·미커밋 수.
pub type Meta = BTreeMap<PathBuf, Tree>;

/// IPC boundary: gpui tests supply snapshots without waiting for external IO.
pub trait Source: Send + Sync {
    fn fetch(&self) -> io::Result<Snapshot>;
    /// 워크트리 행의 git 정보. 모르면 비워 둔다(UI가 숨긴다).
    fn meta(&self, _trees: &BTreeMap<PathBuf, Vec<PathBuf>>) -> Meta {
        Meta::new()
    }
    /// 사이드바 가시성·창 활성 여부가 바뀔 때 git 감시를 켜거나 끈다(N19).
    fn set_visible(&self, _visible: bool) {}
    /// OWE-496 `Request::NewWorktree` 한 번. 생성이 끝난 뒤에 답한다.
    fn new_worktree(&self, _req: NewWorktree) -> io::Result<Reply> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// OWE-730 `Request::RemoveWorktree` 한 번. 삭제가 끝난 뒤에 답한다.
    fn remove_worktree(&self, _req: krill_proto::control::RemoveWorktree) -> io::Result<Reply> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// MK-095: only the open creation dialog owns this speculative preparation token.
    fn prepare_worktree(&self, _req: krill_proto::control::PrepareWorktree) -> io::Result<Reply> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// MK-093: 워크트리별 CLI 코멘트(`krill set --comment`).
    fn comments(&self) -> BTreeMap<PathBuf, String> {
        BTreeMap::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct Preference {
    pinned: bool,
    name: String,
}

type Preferences = BTreeMap<PathBuf, Preference>;

/// 워크트리 삭제 확인 모달 상태(OWE-847). 메뉴 클릭 시 즉시 삭제하지 않고
/// 이 상태로 모달을 띄운 뒤, 확인 버튼에서 `force` 플래그와 함께 삭제한다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DeleteConfirm {
    pub path: PathBuf,
    pub with_descendants: bool,
    pub descendant_count: usize,
    pub dirty: usize,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    HostHeader {
        host: HostId,
        label: String,
        status: HostStatus,
        sessions: usize,
    },
    NeedsYou,
    Repository(PathBuf),
    GroupHeader {
        key: GroupKey,
        count: usize,
    },
    Worktree(PathBuf),
    HiddenWorktrees(PathBuf, usize, bool),
    Session(Session, bool),
    /// git 밖 폴더 프로젝트(세션 작업 디렉터리). Orca처럼 폴더 > 세션으로 묶는다(OWE-654).
    Folder(PathBuf),
    /// 작업 디렉터리를 모르는 세션.
    Loose,
}

/// 사이드바에 표시되는 원격 호스트 섹션 상태.
#[derive(Clone)]
pub struct RemoteHost {
    pub id: String,
    pub label: String,
    pub status: HostStatus,
    pub snapshot: Snapshot,
    pub meta: Meta,
    pub source: Option<Arc<dyn Source>>,
}

impl std::fmt::Debug for RemoteHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteHost")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("status", &self.status)
            .field("sessions", &self.snapshot.sessions.len())
            .finish_non_exhaustive()
    }
}

fn needs_you(status: &AgentStatus) -> bool {
    matches!(status.state, AgentState::Waiting | AgentState::Blocked)
}

/// 상태 문구.
fn state_text(state: AgentState) -> &'static str {
    let i = match state {
        AgentState::Working => 0,
        AgentState::Waiting => 1,
        AgentState::Blocked => 2,
        AgentState::Done => 3,
        AgentState::Exited => 4,
        AgentState::Unknown => 5,
    };
    [
        text::STATE_0,
        text::STATE_1,
        text::STATE_2,
        text::STATE_3,
        text::STATE_4,
        text::STATE_5,
    ][i]
        .get()
}

/// OWE-662: 미확인(unread) 전이. 보고 있지 않은 세션이 완료·대기·승인 대기로 바뀌면 켜고,
/// 다시 작업을 시작하거나 사라지면 끈다. 보고 있는 세션은 [`Sidebar::set_focused`]가 끈다. 앱 전용 상태.
fn track_unread(
    unread: &mut BTreeSet<SessionId>,
    old: &[Session],
    new: &[Session],
    focused: Option<SessionId>,
) {
    unread.retain(|id| new.iter().any(|s| s.id == *id));
    for s in new {
        let before = old.iter().find(|o| o.id == s.id).map(|o| o.status.state);
        let state = s.status.state;
        if state == AgentState::Working {
            unread.remove(&s.id);
        } else if before.is_some_and(|b| b != state)
            && matches!(
                state,
                AgentState::Done | AgentState::Waiting | AgentState::Blocked
            )
            && focused != Some(s.id)
        {
            unread.insert(s.id);
        }
    }
}

fn basename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// 세션 행의 이름과 보조 문구. 이름은 터미널 제목 → 에이전트 → 폴더 이름 → "세션 n" 순서로 고른다.
/// 탭·패인 헤더도 이 함수로 이름을 정한다(OWE-653).
pub fn session_label(session: &Session, title: Option<&str>) -> (String, String) {
    let title = title.map(str::trim).filter(|t| !t.is_empty());
    let agent = Some(session.status.agent.as_str()).filter(|a| !a.is_empty());
    let place = session
        .cwd
        .as_deref()
        .or(session.worktree.as_deref())
        .map(basename);
    let name = title
        .or(agent)
        .map(str::to_owned)
        .or_else(|| place.clone())
        .unwrap_or_else(|| format!("{} {}", text::SESSION.get(), session.id.0));
    let detail: Vec<_> = [agent.map(str::to_owned), place]
        .into_iter()
        .flatten()
        .filter(|d| *d != name)
        .collect();
    (name, detail.join(" · "))
}

#[cfg(test)]
#[cfg(unix)]
fn rows(snapshot: &Snapshot, prefs: &Preferences, collapsed: &BTreeSet<PathBuf>) -> Vec<Row> {
    options::rows_with_options(
        snapshot,
        prefs,
        collapsed,
        &BTreeSet::new(),
        &BTreeSet::new(),
        &WorkspaceOptions::default(),
        &options::WorktreeContext::default(),
    )
}

const FORMAT_VERSION: u8 = 1;
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// 사이드바 접힘/펼침·제거 UI 상태 및 마지막 선택 워크트리(`sidebar.bin` 4번째 구간, OWE-842·OWE-845).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UiExpansionState {
    #[serde(default)]
    pub collapsed: BTreeSet<PathBuf>,
    #[serde(default)]
    pub collapsed_groups: BTreeSet<GroupKey>,
    #[serde(default)]
    pub collapsed_hosts: BTreeSet<HostId>,
    #[serde(default)]
    pub expanded_cards: BTreeSet<PathBuf>,
    #[serde(default)]
    pub selected_worktree: Option<PathBuf>,
    #[serde(default)]
    pub shown_external: BTreeSet<PathBuf>,
    #[serde(default)]
    pub removed_projects: BTreeSet<PathBuf>,
}

impl UiExpansionState {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.collapsed.is_empty()
            && self.collapsed_groups.is_empty()
            && self.collapsed_hosts.is_empty()
            && self.expanded_cards.is_empty()
            && self.selected_worktree.is_none()
            && self.shown_external.is_empty()
            && self.removed_projects.is_empty()
    }
}

type PersistedSidebarState = (
    Preferences,
    WorkspaceOptions,
    lineage::LineageState,
    UiExpansionState,
);

/// 저장 파일을 읽는다. 손상·미지원 버전은 덮어쓰지 않도록 `.corrupt`로 보존한 뒤 오류를 돌려준다.
#[cfg(test)]
#[cfg(unix)]
fn load(path: &Path) -> io::Result<Preferences> {
    load_state(path).map(|(prefs, _, _, _)| prefs)
}

fn load_state(path: &Path) -> io::Result<PersistedSidebarState> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let result = decode_state(&bytes);
    if result
        .as_ref()
        .is_err_and(|e| e.kind() == io::ErrorKind::InvalidData)
    {
        crate::layout::preserve_corrupt(path)?;
    }
    result
}

fn decode_state(bytes: &[u8]) -> io::Result<PersistedSidebarState> {
    let invalid = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_owned());
    if u64::try_from(bytes.len()).map_or(true, |n| n > MAX_FILE_BYTES) {
        return Err(invalid("sidebar file too large"));
    }
    let Some((&version, body)) = bytes.split_first() else {
        return Err(invalid("empty sidebar file"));
    };
    if version != FORMAT_VERSION {
        return Err(invalid("unknown sidebar format version"));
    }
    let (prefs, rest) =
        postcard::take_from_bytes::<Preferences>(body).map_err(|e| invalid(&e.to_string()))?;
    if rest.is_empty() {
        return Ok((
            prefs,
            WorkspaceOptions::default(),
            lineage::LineageState::default(),
            UiExpansionState::default(),
        ));
    }
    let (opts, rest) =
        postcard::take_from_bytes::<WorkspaceOptions>(rest).map_err(|e| invalid(&e.to_string()))?;
    if rest.is_empty() {
        return Ok((
            prefs,
            opts,
            lineage::LineageState::default(),
            UiExpansionState::default(),
        ));
    }
    let (lin, rest) = postcard::take_from_bytes::<lineage::LineageState>(rest)
        .map_err(|e| invalid(&e.to_string()))?;
    let ui = if rest.is_empty() {
        UiExpansionState::default()
    } else {
        postcard::from_bytes::<UiExpansionState>(rest).map_err(|e| invalid(&e.to_string()))?
    };
    Ok((prefs, opts, lin, ui))
}

#[cfg(test)]
#[cfg(unix)]
fn save(path: &Path, prefs: &Preferences) -> io::Result<()> {
    save_state(
        path,
        prefs,
        &WorkspaceOptions::default(),
        &lineage::LineageState::default(),
        &UiExpansionState::default(),
    )
}

fn save_state(
    path: &Path,
    prefs: &Preferences,
    opts: &WorkspaceOptions,
    lin: &lineage::LineageState,
    ui: &UiExpansionState,
) -> io::Result<()> {
    let mut bytes = postcard::to_stdvec(prefs).map_err(io::Error::other)?;
    if opts.is_customized() || !lin.is_empty() || !ui.is_empty() {
        bytes.extend(postcard::to_stdvec(opts).map_err(io::Error::other)?);
        if !lin.is_empty() || !ui.is_empty() {
            bytes.extend(postcard::to_stdvec(lin).map_err(io::Error::other)?);
            if !ui.is_empty() {
                bytes.extend(postcard::to_stdvec(ui).map_err(io::Error::other)?);
            }
        }
    }
    if u64::try_from(bytes.len()).map_or(true, |n| n >= MAX_FILE_BYTES) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sidebar file too large",
        ));
    }
    if let Some(parent) = path.parent() {
        krill_proto::os::fs::DirBuilderExt::mode(fs::DirBuilder::new().recursive(true), 0o700)
            .create(parent)?;
    }
    let temp = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(&[FORMAT_VERSION])?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temp, path)?;
    crate::layout::sync_parent(path)
}

#[derive(Debug)]
pub struct Selected(pub SessionId);

/// 빈 상태의 '폴더 열기' 버튼. 셸이 기존 폴더 열기 경로(`choose_folder`)로 받는다(OWE-654).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFolder;

/// 워크트리·폴더 행에서 새 세션(로그인 셸·에이전트)을 띄울 때 내보내는 이벤트(N4, PRD 시나리오 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub cwd: PathBuf,
    pub program: Program,
}

/// OWE-854: 사이드바 세션 행 메뉴의 '세션 종료…'. 셸이 확인 후 데몬에 `Request::Close`를 보낸다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseSession(pub SessionId);

/// OWE-854: 사이드바 세션 행 메뉴의 '새 세션에 이어하기…'. 셸이 이어하기 모달을 연다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinueSession(pub SessionId);

/// OWE-854: 사이드바 세션 행 메뉴의 '이름 변경'. 셸이 그 세션의 탭을 열고 탭 이름 변경(OWE-870)을 띄운다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenameSession(pub SessionId);

/// 끊긴 원격 호스트 섹션에서 '다시 연결'을 눌렀을 때 내보내는 이벤트(MK-131).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectHost(pub HostId);

pub struct Sidebar {
    snapshot: Snapshot,
    meta: Meta,
    local_status: HostStatus,
    remote_hosts: Vec<RemoteHost>,
    remote_task: Option<Task<()>>,
    remote_manager: Option<HostManager>,
    remote_info: BTreeMap<String, krill_hosts::HostConnectionInfo>,
    collapsed_hosts: BTreeSet<HostId>,
    row_hosts: Vec<HostId>,
    action_host: Option<HostId>,
    prs: BTreeMap<PathBuf, crate::pull_request::Status>,
    pr_guidance_dismissed: bool,
    pr_task: Option<Task<()>>,
    pr_service: Option<Arc<crate::pull_request::Service>>,
    rows: Vec<Row>,
    prefs: Preferences,
    options: WorkspaceOptions,
    collapsed_groups: BTreeSet<GroupKey>,
    worktree_statuses: BTreeMap<PathBuf, krill_proto::orchestration::WorkspaceStatus>,
    worktree_updated_at: BTreeMap<PathBuf, u64>,
    issues: BTreeMap<PathBuf, String>,
    ports: BTreeMap<PathBuf, Vec<u16>>,
    worktree_hosts: BTreeMap<PathBuf, String>,
    automation_worktrees: BTreeSet<PathBuf>,
    cli_worktrees: BTreeSet<PathBuf>,
    comments: BTreeMap<PathBuf, String>,
    parents: BTreeMap<PathBuf, PathBuf>,
    collapsed_lineage: BTreeSet<PathBuf>,
    sleeping_worktrees: BTreeSet<PathBuf>,
    shown_external: BTreeSet<PathBuf>,
    collapsed: BTreeSet<PathBuf>,
    /// 카드 하단 요약 알약을 눌러 개별 세션 행을 펼친 워크트리 집합(Orca 기본값: 접힘).
    expanded_cards: BTreeSet<PathBuf>,
    /// 마지막으로 선택·활성화된 워크트리/폴더 경로(`sidebar.bin`에 영속화, OWE-842).
    selected_worktree: Option<PathBuf>,
    /// 목록에서 제거한 저장소·폴더 프로젝트(OWE-845). `projects.toml` 정리와 별개로 행을 숨긴다.
    removed_projects: BTreeSet<PathBuf>,
    /// 워크트리 삭제 확인 모달 상태(OWE-847). `Some`일 때 모달을 띄운다.
    delete_confirm: Option<DeleteConfirm>,
    path: Option<PathBuf>,
    error: Option<String>,
    /// 연결 오류는 폴링 복구 시 지운다. 저장·손상 오류(`error`)는 사용자가 확인할 때까지 유지한다.
    poll_error: Option<String>,
    /// Open in 실패처럼 사용자가 닫을 때까지 남기는 알림.
    notice: Option<String>,
    task: Option<Task<()>>,
    source: Option<Arc<dyn Source>>,
    scroll: UniformListScrollHandle,
    editing: Option<(PathBuf, Entity<InputState>)>,
    edit_subscription: Option<Subscription>,
    orca_import_subscription: Option<Subscription>,
    pub(crate) creating: Option<(Entity<worktree::Dialog>, Subscription)>,
    /// OWE-924: 워크트리 생성 시 예약된 초기 프롬프트(워크트리 경로 → 프롬프트).
    pending_prompts: BTreeMap<PathBuf, String>,
    /// OWE-854: 셸 탭에서 사용자가 직접 지정한 세션별 제목. 세션 행 라벨의 최우선 후보.
    custom_titles: BTreeMap<SessionId, String>,
    /// 셸이 아는 세션별 터미널 제목(보이는 칸만). 세션 이름의 첫 후보.
    titles: BTreeMap<SessionId, String>,
    /// 첫 프롬프트 훅에서 자동 생성된 세션 제목(`session-titles.json`, OWE-737).
    auto_titles: BTreeMap<SessionId, String>,
    /// 활성 Claude 프롬프트 캐시 카운트다운의 마지막 1초 틱(`now_ns / 1e9`, OWE-737).
    last_cache_tick_sec: Option<u64>,
    /// 테스트용 시각 고정(ns, OWE-737).
    now_override_ns: Option<u64>,
    /// 셸의 포커스 칸 세션(OWE-662 미확인 판정).
    focused: Option<SessionId>,
    /// 보지 않은 사이 완료·대기로 바뀐 세션(OWE-662).
    unread: BTreeSet<SessionId>,
    /// 워크트리 행 메뉴·단축키에서 띄울 에이전트 프로필 목록.
    agents: Vec<AgentProfile>,
    /// 사이드바 열이 펼쳐져 있는지.
    visible: bool,
    /// 창이 활성 상태인지.
    active: bool,
    /// 키보드 탐색 위치(행 인덱스).
    cursor: Option<usize>,
    focus: Option<FocusHandle>,
    /// Enter·Esc 뒤 돌려줄 포커스(셸).
    back: Option<FocusHandle>,
    #[cfg(test)]
    last_open_in: Option<(Target, PathBuf)>,
    #[cfg(test)]
    rendered_rows: usize,
    #[cfg(test)]
    renders: usize,
}

impl EventEmitter<Selected> for Sidebar {}
impl EventEmitter<NewSession> for Sidebar {}
impl EventEmitter<CloseSession> for Sidebar {}
impl EventEmitter<ContinueSession> for Sidebar {}
impl EventEmitter<RenameSession> for Sidebar {}
impl EventEmitter<OpenFolder> for Sidebar {}
impl EventEmitter<ReconnectHost> for Sidebar {}

impl std::fmt::Debug for Sidebar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sidebar")
            .field("rows", &self.rows.len())
            .field("remote_hosts", &self.remote_hosts.len())
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

fn sort_agents(mut list: Vec<AgentProfile>) -> Vec<AgentProfile> {
    list.sort_by(|a, b| {
        krill_proto::agent::profile_rank(&a.id)
            .cmp(&krill_proto::agent::profile_rank(&b.id))
            .then_with(|| a.id.cmp(&b.id))
    });
    list
}

fn load_auto_titles_from_sidebar_path(sidebar_path: Option<&Path>) -> BTreeMap<SessionId, String> {
    let Some(dir) = sidebar_path.and_then(Path::parent) else {
        return BTreeMap::new();
    };
    if !krill_proto::git::is_auto_rename_enabled(Some(dir)) {
        return BTreeMap::new();
    }
    krill_proto::git::load_session_auto_titles(dir)
}

impl Sidebar {
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut error = None;
        let mut writable = true;
        let (prefs, options, lin, ui) = match path.as_deref().map(load_state) {
            Some(Ok(state)) => state,
            Some(Err(err)) if err.kind() != io::ErrorKind::NotFound => {
                writable = path
                    .as_deref()
                    .is_some_and(|p| matches!(p.try_exists(), Ok(false)));
                error = Some(err.to_string());
                (
                    Preferences::default(),
                    WorkspaceOptions::default(),
                    lineage::LineageState::default(),
                    UiExpansionState::default(),
                )
            }
            _ => (
                Preferences::default(),
                WorkspaceOptions::default(),
                lineage::LineageState::default(),
                UiExpansionState::default(),
            ),
        };
        let comments = external::load_comments_from_sidebar_path(path.as_deref());
        let auto_titles = load_auto_titles_from_sidebar_path(path.as_deref());
        let mut this = Self {
            snapshot: Snapshot::default(),
            meta: Meta::new(),
            local_status: HostStatus::Connected,
            remote_hosts: Vec::new(),
            remote_task: None,
            remote_manager: None,
            remote_info: BTreeMap::new(),
            collapsed_hosts: ui.collapsed_hosts,
            row_hosts: Vec::new(),
            action_host: None,
            prs: BTreeMap::new(),
            pr_guidance_dismissed: false,
            pr_task: None,
            pr_service: None,
            rows: Vec::new(),
            prefs,
            options,
            collapsed_groups: ui.collapsed_groups,
            worktree_statuses: BTreeMap::new(),
            worktree_updated_at: BTreeMap::new(),
            issues: BTreeMap::new(),
            ports: BTreeMap::new(),
            worktree_hosts: BTreeMap::new(),
            automation_worktrees: BTreeSet::new(),
            cli_worktrees: BTreeSet::new(),
            comments,
            parents: lin.parents,
            collapsed_lineage: lin.collapsed_lineage,
            sleeping_worktrees: lin.sleeping_worktrees,
            shown_external: ui.shown_external,
            collapsed: ui.collapsed,
            expanded_cards: ui.expanded_cards,
            selected_worktree: ui.selected_worktree,
            removed_projects: ui.removed_projects,
            delete_confirm: None,
            path: path.filter(|_| writable),
            error,
            poll_error: None,
            notice: None,
            task: None,
            source: None,
            scroll: UniformListScrollHandle::new(),
            editing: None,
            edit_subscription: None,
            orca_import_subscription: None,
            creating: None,
            pending_prompts: BTreeMap::new(),
            custom_titles: BTreeMap::new(),
            titles: BTreeMap::new(),
            auto_titles,
            last_cache_tick_sec: None,
            now_override_ns: None,
            focused: None,
            unread: BTreeSet::new(),
            agents: sort_agents(
                Agents::load(None)
                    .unwrap_or_default()
                    .profiles
                    .into_iter()
                    .map(|(id, p)| AgentProfile { id, name: p.name })
                    .collect(),
            ),
            visible: true,
            active: true,
            cursor: None,
            focus: None,
            back: None,
            #[cfg(test)]
            last_open_in: None,
            #[cfg(test)]
            rendered_rows: 0,
            #[cfg(test)]
            renders: 0,
        };
        if let Some(dir) = this.state_dir().map(Path::to_path_buf) {
            if !this.parents.is_empty() && !dir.join(external::COMMENTS_FILE).exists() {
                let legacy = std::mem::take(&mut this.parents);
                krill_proto::orchestration::WorktreeStore::mutate(&dir, |store| {
                    for (child, parent) in legacy {
                        let c = child.to_string_lossy().into_owned();
                        let p = parent.to_string_lossy().into_owned();
                        store.set_parent(&c, Some(&p)).ok();
                        if store.parents.get(&c) != Some(&p) {
                            store.parents.insert(c, p);
                        }
                    }
                    Ok(())
                })
                .ok();
                lineage::invalidate_parents_cache();
            }
            this.parents = this.merged_parents();
        }
        // `sync` 안에서 자동 접힘이 먼저 반영되므로 행은 접힘 상태로 만든다.
        let (trees_changed, auto_changed) = this.sync_imported_projects_from_disk();
        if trees_changed {
            this.rows = if this.options.filters.contains_host(&HostId::Local) {
                this.local_rows()
            } else {
                Vec::new()
            };
            this.row_hosts = vec![HostId::Local; this.rows.len()];
            if let Some(sel) = &this.selected_worktree {
                this.cursor = this.rows.iter().position(|r| match r {
                    Row::Worktree(p) | Row::Folder(p) | Row::Repository(p) => p == sel,
                    _ => false,
                });
            }
        }
        if auto_changed {
            this.write_state();
        }
        this
    }

    /// OWE-900: 세션 없는 워크트리가 많은 프로젝트는 접힌 상태로 시작한다.
    ///
    /// 반드시 `include_imported_projects` 직후·`apply_imported_project_prefs` 직전에
    /// 불러야 한다. 이 시점에 `prefs`에 항목이 없는 저장소는 이번에 처음 본 것으로 보고,
    /// 세션이 하나도 없고 유휴 워크트리가 [`AUTO_COLLAPSE_IDLE_WORKTREES`]개를 넘으면 접는다.
    /// - 이관 프로젝트는 뒤따르는 prefs 동기화가 표시 이름·핀을 심어 다음부터 건너뛴다.
    /// - 데몬 발견 프로젝트는 빈 prefs 항목을 표시로 남겨 재시작 후 재접힘을 막는다
    ///   (사용자가 펼치면 그 상태가 유지된다). 핀·세션이 있으면 접지 않는다.
    /// - 저장은 호출자가 맡는다(`new`는 `write_state`, 나머지는 `persist`).
    fn auto_collapse_idle_heavy_repos(&mut self, state_dir: &Path) -> bool {
        let imported = external::imported_repo_paths(state_dir);
        let mut changed = false;
        let repos: Vec<PathBuf> = self.snapshot.trees.keys().cloned().collect();
        for repo in repos {
            if self.collapsed.contains(&repo) || self.prefs.contains_key(&repo) {
                continue;
            }
            let Some(trees) = self.snapshot.trees.get(&repo) else {
                continue;
            };
            let has_session = trees.iter().any(|t| {
                self.snapshot
                    .sessions
                    .iter()
                    .any(|s| s.worktree.as_deref() == Some(t.as_path()))
            });
            if has_session {
                continue;
            }
            if trees
                .iter()
                .any(|t| self.prefs.get(t).is_some_and(|p| p.pinned))
            {
                continue;
            }
            let idle = trees
                .iter()
                .filter(|t| t.as_path() != repo.as_path())
                .count();
            if idle <= AUTO_COLLAPSE_IDLE_WORKTREES {
                continue;
            }
            self.collapsed.insert(repo.clone());
            if !external::path_aliases(&repo)
                .iter()
                .any(|a| imported.contains(a))
            {
                self.prefs.entry(repo).or_default();
            }
            changed = true;
        }
        changed
    }

    fn sync_imported_projects_from_disk(&mut self) -> (bool, bool) {
        let Some(dir) = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
        else {
            return (false, false);
        };
        if let Ok(wt_store) = krill_proto::orchestration::WorktreeStore::load(&dir) {
            for (k, v) in &wt_store.statuses {
                if let Ok(st) = krill_proto::orchestration::WorkspaceStatus::parse(v) {
                    self.worktree_statuses.insert(PathBuf::from(k), st);
                }
            }
            for (k, &ts) in &wt_store.updated_at {
                self.worktree_updated_at.insert(PathBuf::from(k), ts);
            }
            for (k, iss) in &wt_store.issues {
                self.issues.insert(PathBuf::from(k), iss.clone());
            }
        }
        let prev_trees = self.snapshot.trees.clone();
        external::include_imported_projects(&dir, &mut self.snapshot.trees, &mut self.meta);
        let auto_changed = self.auto_collapse_idle_heavy_repos(&dir);
        external::apply_imported_project_prefs(
            &dir,
            &mut self.prefs,
            &mut self.comments,
            &mut self.issues,
            &mut self.worktree_statuses,
        );
        (
            self.snapshot.trees != prev_trees || !self.snapshot.trees.is_empty(),
            auto_changed,
        )
    }

    /// OWE-898: 라이브 워크트리 집합에 없는 파생 상태(상태·갱신시각·이슈·포트·호스트 라벨·자동/CLI 표시)를
    /// 정리한다. 디스크 저장소(`WorktreeStore`)가 삭제된 워크트리 항목을 계속 내놓아도 메모리·디스크
    /// 재삽입으로 누적되지 않게 한다. 사용자 접힘·핀·부모·이름 설정은 건드리지 않는다.
    fn prune_stale_worktree_state(&mut self) {
        use std::collections::BTreeSet;
        let mut live: BTreeSet<&Path> = BTreeSet::new();
        for wts in self.snapshot.trees.values() {
            for wt in wts {
                live.insert(wt.as_path());
            }
        }
        for s in &self.snapshot.sessions {
            if let Some(wt) = s.worktree.as_deref() {
                live.insert(wt);
            }
        }
        self.worktree_statuses
            .retain(|path, _| live.contains(path.as_path()));
        self.worktree_updated_at
            .retain(|path, _| live.contains(path.as_path()));
        self.issues.retain(|path, _| live.contains(path.as_path()));
        self.ports.retain(|path, _| live.contains(path.as_path()));
        self.worktree_hosts
            .retain(|path, _| live.contains(path.as_path()));
        self.automation_worktrees
            .retain(|path| live.contains(path.as_path()));
        self.cli_worktrees
            .retain(|path| live.contains(path.as_path()));
    }

    /// 외부 파일(`projects.toml`, `worktree-comments.toml`) 변경을 즉시 반영해 사이드바 트리를 다시 그린다.
    pub fn reload_external(&mut self, cx: &mut Context<Self>) {
        lineage::invalidate_parents_cache();
        self.comments = external::load_comments_from_sidebar_path(self.path.as_deref());
        let (_, auto_changed) = self.sync_imported_projects_from_disk();
        self.prune_stale_worktree_state();
        if auto_changed {
            self.persist(cx);
        } else {
            self.rebuild(cx);
        }
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn trees(&self) -> &BTreeMap<PathBuf, Vec<PathBuf>> {
        &self.snapshot.trees
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn comments(&self) -> &BTreeMap<PathBuf, String> {
        &self.comments
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn issues(&self) -> &BTreeMap<PathBuf, String> {
        &self.issues
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn worktree_statuses(
        &self,
    ) -> &BTreeMap<PathBuf, krill_proto::orchestration::WorkspaceStatus> {
        &self.worktree_statuses
    }

    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    fn poll_interval(&self) -> Duration {
        if self.active {
            POLL_INTERVAL_ACTIVE
        } else {
            POLL_INTERVAL_INACTIVE
        }
    }

    /// 사이드바 열 가시성을 바꾼다(N19). 접히면 폴링과 git 감시를 즉시 멈춘다.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            self.sync_polling(cx);
        }
    }

    /// 창 활성 여부를 반영한다(N19·OWE-844). 창이 비포커스 상태여도 사이드바가 표시 중(`visible`)이면
    /// 완화된 주기로 백그라운드 폴링·알림을 유지하고, 포커스를 되찾으면 즉시 최신 스냅샷을 갱신한다.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            if active && self.visible {
                self.refresh(cx);
            } else {
                cx.notify();
            }
        }
    }

    /// 셸이 데몬에서 읽은 에이전트 프로필 목록을 반영한다.
    pub fn set_agents(&mut self, agents: Vec<AgentProfile>, cx: &mut Context<Self>) {
        let agents = sort_agents(agents);
        if self.agents != agents {
            self.agents = agents;
            cx.notify();
        }
    }

    /// 마지막 사이드바 이벤트(`Selected` / `NewSession`)가 발생한 호스트를 꺼낸다.
    pub fn take_action_host(&mut self) -> Option<HostId> {
        self.action_host.take()
    }

    /// 등록된 원격 호스트 섹션 목록.
    pub fn remote_hosts(&self) -> &[RemoteHost] {
        &self.remote_hosts
    }

    /// 행 인덱스가 속한 호스트.
    pub fn row_host(&self, index: usize) -> &HostId {
        self.row_hosts.get(index).unwrap_or(&HostId::Local)
    }

    /// 행이 끊긴 원격 호스트의 마지막 세션/워크트리 행이어서 흐리게 표시해야 하는지.
    pub fn is_row_dimmed(&self, index: usize) -> bool {
        if matches!(self.rows.get(index), Some(Row::HostHeader { .. }) | None) {
            return false;
        }
        match self.row_host(index) {
            HostId::Local => false,
            HostId::Remote(id) => self
                .remote_hosts
                .iter()
                .find(|r| r.id == *id)
                .is_some_and(|r| r.status == HostStatus::Disconnected),
        }
    }

    /// 호스트 섹션이 접혀 있는지.
    pub fn is_host_collapsed(&self, host: &HostId) -> bool {
        self.collapsed_hosts.contains(host)
    }

    /// 워크트리·폴더가 접혀 있는지.
    #[must_use]
    pub fn is_collapsed(&self, path: &Path) -> bool {
        self.collapsed.contains(path)
    }

    /// 상태·PR 그룹 헤더가 접혀 있는지.
    #[must_use]
    pub fn is_group_collapsed(&self, key: GroupKey) -> bool {
        self.collapsed_groups.contains(&key)
    }

    /// 워크트리 카드의 하위 세션 행이 펼쳐져 있는지.
    #[must_use]
    pub fn is_card_expanded(&self, path: &Path) -> bool {
        self.expanded_cards.contains(path)
    }

    /// 마지막으로 선택·활성화된 워크트리/폴더 경로.
    #[must_use]
    pub fn selected_worktree(&self) -> Option<&Path> {
        self.selected_worktree.as_deref()
    }

    /// 선택된 워크트리/폴더 경로를 갱신하고 영속화한다(OWE-842).
    pub fn select_worktree(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.selected_worktree != path {
            self.selected_worktree = path;
            self.persist(cx);
        }
    }

    /// 호스트 섹션 접기/펼치기(OWE-845: `UiExpansionState`에 저장해 재시작 후에도 유지).
    pub fn fold_host(&mut self, host: &HostId, open: Option<bool>, cx: &mut Context<Self>) {
        let closed = self.collapsed_hosts.contains(host);
        if open.unwrap_or(closed) {
            self.collapsed_hosts.remove(host);
        } else {
            self.collapsed_hosts.insert(host.clone());
        }
        self.persist(cx);
    }

    /// 끊긴 원격 호스트의 '다시 연결' 버튼을 눌렀을 때 상태를 `Connecting`으로 바꾸고 재연결을 요청한다.
    pub fn reconnect_host(&mut self, host: HostId, cx: &mut Context<Self>) {
        if let HostId::Remote(id) = &host
            && let Some(r) = self.remote_hosts.iter_mut().find(|r| r.id == *id)
        {
            r.status = HostStatus::Connecting;
        }
        self.rebuild(cx);
        cx.emit(ReconnectHost(host));
        self.remote_task = None;
        self.start_remote_polling(cx);
    }

    /// [`HostManager`]의 원격 호스트 목록과 상태를 동기화하고 백그라운드 폴링을 시작한다.
    pub fn sync_remote_hosts(&mut self, manager: &HostManager, cx: &mut Context<Self>) {
        self.remote_manager = Some(manager.clone());
        let statuses = manager.statuses();
        let mut next = Vec::with_capacity(statuses.len());
        let info = statuses
            .iter()
            .filter_map(|(spec, _)| {
                manager
                    .connection_info(&spec.id)
                    .map(|info| (spec.id.clone(), info))
            })
            .collect();
        let mut changed = statuses.len() != self.remote_hosts.len() || self.remote_info != info;
        self.remote_info = info;
        for (spec, status) in statuses {
            let label = spec.display_name().to_owned();
            if let Some(mut existing) = self.remote_hosts.iter().find(|r| r.id == spec.id).cloned()
            {
                if existing.label != label || existing.status != status {
                    changed = true;
                }
                existing.label = label;
                existing.status = status;
                if existing.source.is_none() && self.is_visible() {
                    existing.source = manager
                        .target_for(&HostId::Remote(spec.id.clone()), None)
                        .map(|t| Arc::new(ipc::Socket::new(t)) as Arc<dyn Source>);
                }
                next.push(existing);
            } else {
                changed = true;
                let source = self
                    .is_visible()
                    .then(|| manager.target_for(&HostId::Remote(spec.id.clone()), None))
                    .flatten()
                    .map(|t| Arc::new(ipc::Socket::new(t)) as Arc<dyn Source>);
                next.push(RemoteHost {
                    id: spec.id,
                    label,
                    status,
                    snapshot: Snapshot::default(),
                    meta: Meta::new(),
                    source,
                });
            }
        }
        self.remote_hosts = next;
        if changed {
            self.rebuild(cx);
        }
        self.start_remote_polling(cx);
    }

    /// 단위·통합 테스트나 커스텀 소스로 원격 호스트를 등록한다.
    pub fn set_remote_host_source(
        &mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        status: HostStatus,
        source: Arc<dyn Source>,
        cx: &mut Context<Self>,
    ) {
        let id = id.into();
        let label = label.into();
        if let Some(existing) = self.remote_hosts.iter_mut().find(|r| r.id == id) {
            existing.label = label;
            existing.status = status;
            existing.source = Some(source);
        } else {
            self.remote_hosts.push(RemoteHost {
                id,
                label,
                status,
                snapshot: Snapshot::default(),
                meta: Meta::new(),
                source: Some(source),
            });
        }
        self.rebuild(cx);
        self.remote_task = None;
        self.start_remote_polling(cx);
    }

    /// 원격 호스트 상태를 직접 갱신한다(마지막 스냅샷은 그대로 유지).
    pub fn set_remote_host_status(&mut self, id: &str, status: HostStatus, cx: &mut Context<Self>) {
        if let Some(r) = self.remote_hosts.iter_mut().find(|r| r.id == id)
            && r.status != status
        {
            r.status = status;
            self.rebuild(cx);
        }
    }

    fn ensure_remote_sources(&mut self) {
        if let Some(manager) = self.remote_manager.clone() {
            for remote in &mut self.remote_hosts {
                if remote.source.is_none() {
                    remote.source = manager
                        .target_for(&HostId::Remote(remote.id.clone()), None)
                        .map(status_source);
                }
            }
        }
    }

    fn start_remote_polling(&mut self, cx: &mut Context<Self>) {
        self.spawn_remote_polling(false, cx);
    }

    fn spawn_remote_polling(&mut self, force_once: bool, cx: &mut Context<Self>) {
        if (!self.is_visible() && !force_once)
            || self.remote_hosts.is_empty()
            || self.remote_task.is_some()
        {
            return;
        }
        if force_once {
            self.ensure_remote_sources();
        }
        self.remote_task = Some(cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            let mut forced = force_once;
            loop {
                let Ok(Some(sources)) = this.read_with(cx, |s, _| {
                    if (!s.is_visible() && !forced) || s.remote_hosts.is_empty() {
                        return None;
                    }
                    let list: Vec<(String, Arc<dyn Source>)> = s
                        .remote_hosts
                        .iter()
                        .filter_map(|r| Some((r.id.clone(), r.source.clone()?)))
                        .collect();
                    (!list.is_empty()).then_some(list)
                }) else {
                    return;
                };
                let tasks: Vec<_> = sources
                    .into_iter()
                    .map(|(id, source)| {
                        bg.spawn(async move {
                            let res = source.fetch().map(|snap| {
                                let meta = source.meta(&snap.trees);
                                (snap, meta)
                            });
                            (id, res)
                        })
                    })
                    .collect();
                let mut results = Vec::with_capacity(tasks.len());
                for t in tasks {
                    results.push(t.await);
                }
                let next_delay = this.update(cx, |s, cx| {
                    if (!s.is_visible() && !forced) || s.remote_hosts.is_empty() {
                        return None;
                    }
                    let mut changed = false;
                    for (id, res) in results {
                        let transport = s
                            .remote_manager
                            .as_ref()
                            .and_then(|m| m.connection_info(&id));
                        let Some(entry) = s.remote_hosts.iter_mut().find(|r| r.id == id) else {
                            continue;
                        };
                        if let Ok((snap, meta)) = res {
                            if entry.status != HostStatus::Connected
                                || entry.snapshot != snap
                                || entry.meta != meta
                            {
                                entry.status = HostStatus::Connected;
                                entry.snapshot = snap;
                                entry.meta = meta;
                                changed = true;
                            }
                        } else {
                            let status = transport
                                .as_ref()
                                .map_or(HostStatus::Disconnected, |t| t.status);
                            if entry.status != status {
                                entry.status = status;
                                changed = true;
                            }
                        }
                    }
                    if changed {
                        s.rebuild(cx);
                    }
                    s.is_visible().then(|| s.poll_interval())
                });
                forced = false;
                let Ok(Some(delay)) = next_delay else {
                    return;
                };
                bg.timer(delay).await;
            }
        }));
    }

    fn sync_polling(&mut self, cx: &mut Context<Self>) {
        if self.is_visible() {
            if self.task.is_none()
                && let Some(source) = self.source.clone()
            {
                self.connect_source(source, cx);
            }
            if self.remote_task.is_none() && !self.remote_hosts.is_empty() {
                self.ensure_remote_sources();
                self.start_remote_polling(cx);
            }
        } else {
            self.task = None;
            self.remote_task = None;
            if let Some(source) = self.source.clone() {
                cx.background_executor()
                    .spawn(async move {
                        source.set_visible(false);
                    })
                    .detach();
            }
            let has_manager = self.remote_manager.is_some();
            for r in &mut self.remote_hosts {
                let source = if has_manager {
                    r.source.take()
                } else {
                    r.source.clone()
                };
                if let Some(source) = source {
                    cx.background_executor()
                        .spawn(async move {
                            source.set_visible(false);
                        })
                        .detach();
                }
            }
        }
        cx.notify();
    }

    /// 탭 바도 같은 스냅샷으로 상태를 표시한다(추가 폴링 없음).
    pub fn session(&self, id: SessionId) -> Option<&Session> {
        self.snapshot
            .sessions
            .iter()
            .find(|s| s.id == id)
            .or_else(|| {
                self.remote_hosts
                    .iter()
                    .flat_map(|r| &r.snapshot.sessions)
                    .find(|s| s.id == id)
            })
    }

    /// 지정한 호스트의 스냅샷에서 세션을 찾는다.
    pub fn session_on(&self, host: &HostId, id: SessionId) -> Option<&Session> {
        self.snapshot_for(host).sessions.iter().find(|s| s.id == id)
    }

    /// 세션이 속한 호스트(로컬 또는 원격)를 찾는다.
    #[must_use]
    pub fn host_for_session(&self, id: SessionId) -> Option<HostId> {
        if self.snapshot.sessions.iter().any(|s| s.id == id) {
            return Some(HostId::Local);
        }
        self.remote_hosts.iter().find_map(|r| {
            r.snapshot
                .sessions
                .iter()
                .any(|s| s.id == id)
                .then(|| HostId::Remote(r.id.clone()))
        })
    }

    pub(super) fn snapshot_for(&self, host: &HostId) -> &Snapshot {
        static EMPTY_SNAPSHOT: Snapshot = Snapshot {
            sessions: Vec::new(),
            trees: BTreeMap::new(),
        };
        match host {
            HostId::Local => &self.snapshot,
            HostId::Remote(id) => self
                .remote_hosts
                .iter()
                .find(|r| r.id == *id)
                .map_or(&EMPTY_SNAPSHOT, |r| &r.snapshot),
        }
    }

    pub(super) fn meta_for(&self, host: &HostId) -> &Meta {
        static EMPTY_META: Meta = BTreeMap::new();
        match host {
            HostId::Local => &self.meta,
            HostId::Remote(id) => self
                .remote_hosts
                .iter()
                .find(|r| r.id == *id)
                .map_or(&EMPTY_META, |r| &r.meta),
        }
    }

    /// 사이드바 상단 '에이전트 대시보드' 행에 표시할 `(작업 중·대기 수, 완료 수)`.
    #[must_use]
    pub fn agent_counts(&self) -> (usize, usize) {
        let mut working = 0usize;
        let mut done = 0usize;
        for s in &self.snapshot.sessions {
            match s.status.state {
                AgentState::Working | AgentState::Waiting | AgentState::Blocked => working += 1,
                AgentState::Done => done += 1,
                AgentState::Exited | AgentState::Unknown => {
                    if !s.status.agent.is_empty() {
                        done += 1;
                    }
                }
            }
        }
        (working, done)
    }

    /// OWE-662: 보지 않은 사이 완료·대기로 바뀐 세션인지.
    #[must_use]
    pub fn is_unread(&self, id: SessionId) -> bool {
        self.unread.contains(&id)
    }

    /// OWE-662: 셸이 포커스 칸의 세션을 알린다. 그 세션의 미확인 표시를 끈다.
    pub fn set_focused(&mut self, session: Option<SessionId>, cx: &mut Context<Self>) {
        let changed = self.focused != session;
        self.focused = session;
        let cleared = session.is_some_and(|id| self.unread.remove(&id));
        if let Some(wt) = session
            .and_then(|id| self.folder(id))
            .map(Path::to_path_buf)
            && self.selected_worktree.as_ref() != Some(&wt)
        {
            self.selected_worktree = Some(wt);
            self.write_state();
        }
        if changed || cleared {
            cx.notify();
        }
    }

    /// 현재 포커스된 세션(또는 그 워크트리) 행으로 스크롤·선택한다.
    pub fn reveal_focused(&mut self, cx: &mut Context<Self>) {
        let Some(focused_id) = self.focused else {
            return;
        };
        let target_wt = self.folder(focused_id).map(Path::to_path_buf);
        let mut uncollapsed = false;
        if let Some(wt) = &target_wt {
            if self.collapsed.remove(wt) {
                uncollapsed = true;
            }
            if self.expand_lineage_ancestors(wt) {
                uncollapsed = true;
            }
            let is_claude_wt = external::is_claude_worktree(wt);
            let parent_repos: Vec<PathBuf> = self
                .snapshot
                .trees
                .iter()
                .filter(|&(repo, trees)| repo == wt || trees.iter().any(|t| t == wt))
                .map(|(repo, _)| repo.clone())
                .collect();
            for repo in parent_repos {
                if self.collapsed.remove(&repo) {
                    uncollapsed = true;
                }
                if is_claude_wt && self.shown_external.insert(repo) {
                    uncollapsed = true;
                }
            }
        }
        if target_wt.is_some() && self.selected_worktree != target_wt {
            self.selected_worktree.clone_from(&target_wt);
        }
        if uncollapsed {
            self.persist(cx);
        } else if target_wt.is_some() {
            self.write_state();
        }
        let idx = self.rows.iter().position(|r| match r {
            Row::Worktree(p) | Row::Folder(p) => target_wt.as_ref() == Some(p),
            Row::Session(s, _) => s.id == focused_id,
            _ => false,
        });
        if let Some(i) = idx {
            self.cursor = Some(i);
            self.scroll.scroll_to_item(i, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    /// 워크트리 카드의 3행 에이전트 알약 바 클릭 시 하위 세션 목록을 펼치거나 접는다.
    /// 워크트리 카드의 3행 에이전트 알약 바 클릭 시 하위 세션 목록을 펼치거나 접는다.
    /// 펼침 집합은 `UiExpansionState`에 저장해 재시작 후에도 유지한다(OWE-845).
    pub fn toggle_card_sessions(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.expanded_cards.remove(path) {
            if let Some(c) = self.cursor
                && matches!(
                    self.rows.get(c),
                    Some(Row::Session(s, true)) if s.worktree.as_deref() == Some(path)
                )
            {
                self.cursor = self
                    .rows
                    .iter()
                    .position(|r| matches!(r, Row::Worktree(p) if p == path));
            }
            self.write_state();
            cx.notify();
        } else {
            self.expanded_cards.insert(path.to_path_buf());
            self.collapsed.remove(path);
        }
        self.persist(cx);
    }

    /// Open in·새 에이전트의 폴더. git 세션은 워크트리 루트, 일반 세션은 작업 디렉터리.
    pub fn folder(&self, session: SessionId) -> Option<&Path> {
        let session = self.session(session)?;
        session.worktree.as_deref().or(session.cwd.as_deref())
    }

    /// 지정한 호스트에서 세션의 폴더를 찾는다.
    pub fn folder_on(&self, host: &HostId, session: SessionId) -> Option<&Path> {
        let session = self.session_on(host, session)?;
        session.worktree.as_deref().or(session.cwd.as_deref())
    }

    /// OWE-854: 셸 탭의 사용자 지정 제목을 세션별로 동기화한다. 바뀔 때만 다시 그린다.
    pub fn set_custom_titles(
        &mut self,
        titles: BTreeMap<SessionId, String>,
        cx: &mut Context<Self>,
    ) {
        if self.custom_titles != titles {
            self.custom_titles = titles;
            cx.notify();
        }
    }

    /// 셸이 칸의 터미널 제목을 알린다. 바뀔 때만 다시 그린다.
    pub fn set_title(&mut self, session: SessionId, title: &str, cx: &mut Context<Self>) {
        if self.titles.get(&session).map(String::as_str) != Some(title) {
            self.titles.insert(session, title.to_owned());
            cx.notify();
        }
    }

    /// OWE-737: 첫 프롬프트 기반 자동 세션 제목을 직접 설정한다(단위·통합 테스트 및 즉시 반영용).
    pub fn set_auto_title(
        &mut self,
        session: SessionId,
        title: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let title = title.into();
        if self.auto_titles.get(&session) != Some(&title) {
            self.auto_titles.insert(session, title);
            cx.notify();
        }
    }

    /// OWE-737: 테스트에서 프롬프트 캐시 카운트다운 기준 시각(ns)을 고정한다.
    pub fn set_now_unix_ns_for_test(&mut self, now_ns: Option<u64>, cx: &mut Context<Self>) {
        if self.now_override_ns != now_ns {
            self.now_override_ns = now_ns;
            cx.notify();
        }
    }

    pub(super) fn now_unix_ns(&self) -> u64 {
        self.now_override_ns.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        })
    }

    fn has_active_cache_timer(&self, now_ns: u64, cx: &App) -> bool {
        let settings = crate::settings::current(cx);
        if !settings.prompt_cache_timer_enabled {
            return false;
        }
        let ttl = settings.prompt_cache_ttl_secs;
        self.snapshot.sessions.iter().any(|s| {
            crate::ui::status::session_cache_remaining_secs(
                &s.status.agent,
                s.status.state,
                s.status.since_unix_ns,
                now_ns,
                ttl,
            )
            .is_some()
        })
    }

    /// MK-093: 워크트리에 설정된 CLI 코멘트.
    pub fn comment(&self, path: &Path) -> Option<&str> {
        external::comment_for(&self.comments, path)
    }

    /// MK-093: 저장소의 `.claude/worktrees` 숨김 카드를 펼치거나 접는다(OWE-845: 영속화).
    pub fn toggle_external(&mut self, repo: &Path, cx: &mut Context<Self>) {
        if !self.shown_external.remove(repo) {
            self.shown_external.insert(repo.to_path_buf());
        }
        self.persist(cx);
    }

    /// 호스트 머리줄을 그리면(원격 호스트가 있으면) 그 아래 행 전체를 한 단계 민다.
    fn host_indent(&self) -> f32 {
        if self.remote_hosts.is_empty() {
            0.
        } else {
            HOST_CHILD_INDENT
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        if self.state_dir().is_some() {
            self.parents = self.merged_parents();
        }
        if self.remote_hosts.is_empty() {
            self.rows = if self.options.filters.contains_host(&HostId::Local) {
                self.local_rows()
            } else {
                Vec::new()
            };
            self.row_hosts = vec![HostId::Local; self.rows.len()];
        } else {
            let mut rows = Vec::new();
            let mut row_hosts = Vec::new();
            if self.options.filters.contains_host(&HostId::Local) {
                rows.push(Row::HostHeader {
                    host: HostId::Local,
                    label: text::HOST_LOCAL
                        .mac_or(crate::i18n::catalog::other_os::HOST_LOCAL)
                        .get()
                        .to_owned(),
                    status: self.local_status,
                    sessions: self.snapshot.sessions.len(),
                });
                row_hosts.push(HostId::Local);
                if !self.collapsed_hosts.contains(&HostId::Local) {
                    let local_rows = self.local_rows();
                    row_hosts.extend(std::iter::repeat_n(HostId::Local, local_rows.len()));
                    rows.extend(local_rows);
                }
            }
            for remote in &self.remote_hosts {
                let host = HostId::Remote(remote.id.clone());
                if !self.options.filters.contains_host(&host) {
                    continue;
                }
                rows.push(Row::HostHeader {
                    host: host.clone(),
                    label: remote.label.clone(),
                    status: remote.status,
                    sessions: remote.snapshot.sessions.len(),
                });
                row_hosts.push(host.clone());
                if !self.collapsed_hosts.contains(&host) {
                    let r_rows = self.remote_rows(remote);
                    row_hosts.extend(std::iter::repeat_n(host, r_rows.len()));
                    rows.extend(r_rows);
                }
            }
            self.rows = rows;
            self.row_hosts = row_hosts;
        }
        if self.cursor.is_none()
            && let Some(sel) = &self.selected_worktree
        {
            self.cursor = self.rows.iter().position(|r| match r {
                Row::Worktree(p) | Row::Folder(p) | Row::Repository(p) => p == sel,
                _ => false,
            });
        }
        self.cursor = self
            .cursor
            .map(|i| i.min(self.rows.len().saturating_sub(1)))
            .filter(|_| !self.rows.is_empty());
        cx.notify();
    }

    #[cfg(test)]
    #[cfg(unix)]
    fn apply(
        &mut self,
        result: io::Result<(Snapshot, Meta)>,
        comments: BTreeMap<PathBuf, String>,
        cx: &mut Context<Self>,
    ) {
        let auto_titles = self.auto_titles.clone();
        self.apply_with_auto_titles(result, comments, auto_titles, cx);
    }

    fn apply_with_auto_titles(
        &mut self,
        result: io::Result<(Snapshot, Meta)>,
        comments: BTreeMap<PathBuf, String>,
        auto_titles: BTreeMap<SessionId, String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((snapshot, meta)) => {
                // 워크트리 발견·메타 IO는 connect_source의 백그라운드 단계에서 끝낸다.
                let status_changed = self.local_status != HostStatus::Connected;
                self.local_status = HostStatus::Connected;
                let parents_changed =
                    self.state_dir().is_some() && self.merged_parents() != self.parents;
                if snapshot != self.snapshot
                    || meta != self.meta
                    || comments != self.comments
                    || auto_titles != self.auto_titles
                    || parents_changed
                    || self.poll_error.is_some()
                    || status_changed
                    || crate::alert::needs_snapshot(cx)
                {
                    track_unread(
                        &mut self.unread,
                        &self.snapshot.sessions,
                        &snapshot.sessions,
                        self.focused,
                    );
                    self.snapshot = snapshot;
                    self.meta = meta;
                    self.comments = comments;
                    self.auto_titles = auto_titles;
                    let (_, auto_changed) = self.sync_imported_projects_from_disk();
                    self.poll_error = None;
                    let live = &self.snapshot.sessions;
                    self.titles.retain(|id, _| live.iter().any(|s| s.id == *id));
                    self.auto_titles
                        .retain(|id, _| live.iter().any(|s| s.id == *id));
                    // OWE-898: 사라진 워크트리의 파생 상태가 폴링마다 누적되지 않게
                    // 라이브 집합에 없는 키를 정리한다(사용자 접힘·핀·부모 설정은 유지).
                    self.prune_stale_worktree_state();
                    if !crate::menu_bar::is_monitoring(cx) {
                        crate::alert::sync_snapshot(&self.snapshot, cx);
                    }
                    let now_ns = self.now_unix_ns();
                    self.last_cache_tick_sec = self
                        .has_active_cache_timer(now_ns, cx)
                        .then_some(now_ns / 1_000_000_000);
                    if auto_changed {
                        self.persist(cx);
                    } else {
                        self.rebuild(cx);
                    }
                } else {
                    let now_ns = self.now_unix_ns();
                    let next_tick = self
                        .has_active_cache_timer(now_ns, cx)
                        .then_some(now_ns / 1_000_000_000);
                    if self.last_cache_tick_sec != next_tick {
                        self.last_cache_tick_sec = next_tick;
                        cx.notify();
                    }
                }
            }
            Err(e) => {
                crate::alert::disconnected(cx);
                let status_changed = self.local_status != HostStatus::Disconnected;
                self.local_status = HostStatus::Disconnected;
                let msg = e.to_string();
                if self.poll_error.as_ref() != Some(&msg) || status_changed {
                    self.poll_error = Some(msg);
                    if status_changed && !self.remote_hosts.is_empty() {
                        self.rebuild(cx);
                    } else {
                        cx.notify();
                    }
                }
            }
        }
    }

    pub fn connect(&mut self, target: impl Into<HostTarget>, cx: &mut Context<Self>) {
        self.connect_source(Arc::new(ipc::Socket::new(target)), cx);
        self.connect_pr(crate::pull_request::shared(), cx);
    }

    pub fn connect_source(&mut self, source: Arc<dyn Source>, cx: &mut Context<Self>) {
        self.source = Some(source);
        crate::alert::listen(&cx.weak_entity(), cx);
        self.spawn_local_polling(false, cx);
    }

    fn spawn_local_polling(&mut self, force_once: bool, cx: &mut Context<Self>) {
        if (!self.is_visible() && !force_once) || self.task.is_some() || self.source.is_none() {
            return;
        }
        let sidebar_path = self.path.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            let mut forced = force_once;
            loop {
                let Ok(Some(source)) = this.read_with(cx, |s, _| {
                    (s.is_visible() || forced)
                        .then(|| s.source.clone())
                        .flatten()
                }) else {
                    return;
                };
                let sp = sidebar_path.clone();
                let (result, comments, auto_titles) = bg
                    .spawn(async move {
                        let mut comments = source.comments();
                        if comments.is_empty() {
                            comments = external::load_comments_from_sidebar_path(sp.as_deref());
                        }
                        let auto_titles = load_auto_titles_from_sidebar_path(sp.as_deref());
                        let state_dir = sp
                            .as_deref()
                            .and_then(Path::parent)
                            .filter(|p| !p.as_os_str().is_empty())
                            .map(Path::to_path_buf);
                        let result = source.fetch().map(|mut snapshot| {
                            external::include_claude_worktrees(&mut snapshot.trees);
                            let mut meta = source.meta(&snapshot.trees);
                            if let Some(ref dir) = state_dir {
                                external::include_imported_projects(
                                    dir,
                                    &mut snapshot.trees,
                                    &mut meta,
                                );
                            }
                            (snapshot, meta)
                        });
                        (result, comments, auto_titles)
                    })
                    .await;
                let next_delay = this.update(cx, |s, cx| {
                    if !s.is_visible() && !forced {
                        return None;
                    }
                    s.apply_with_auto_titles(result, comments, auto_titles, cx);
                    s.is_visible().then(|| s.poll_interval())
                });
                forced = false;
                let Ok(Some(delay)) = next_delay else {
                    return;
                };
                // No frame requests or notifications on a stable snapshot.
                bg.timer(delay).await;
            }
        }));
    }

    fn write_state(&mut self) {
        let persisted_parents = if self.state_dir().is_some() {
            BTreeMap::new()
        } else {
            self.parents.clone()
        };
        if let Some(path) = &self.path {
            let lin = lineage::LineageState {
                parents: persisted_parents,
                collapsed_lineage: self.collapsed_lineage.clone(),
                sleeping_worktrees: self.sleeping_worktrees.clone(),
            };
            let ui = UiExpansionState {
                collapsed: self.collapsed.clone(),
                collapsed_groups: self.collapsed_groups.clone(),
                collapsed_hosts: self.collapsed_hosts.clone(),
                expanded_cards: self.expanded_cards.clone(),
                selected_worktree: self.selected_worktree.clone(),
                shown_external: self.shown_external.clone(),
                removed_projects: self.removed_projects.clone(),
            };
            if let Err(e) = save_state(path, &self.prefs, &self.options, &lin, &ui) {
                self.error = Some(e.to_string());
            }
        }
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        self.write_state();
        self.rebuild(cx);
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.task = None;
        self.remote_task = None;
        self.spawn_local_polling(true, cx);
        self.spawn_remote_polling(true, cx);
    }

    pub fn new_worktree(&mut self, repo: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_worktree_dialog(repo, None, window, cx);
    }

    /// Used by keyboard navigation and the native frame-budget benchmark.
    pub fn scroll_to_row(&mut self, index: usize, cx: &mut Context<Self>) {
        self.scroll.scroll_to_item(index, ScrollStrategy::Top);
        cx.notify();
    }

    /// 셸 단축키로 사이드바에 들어온다. Enter로 세션을 열거나 Esc를 누르면 `back`으로 돌아간다.
    pub fn focus(&mut self, back: FocusHandle, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        self.back = Some(back);
        if self.cursor.is_none() && !self.rows.is_empty() {
            self.cursor = self.rows.iter().position(|r| matches!(r, Row::Session(..)));
        }
        window.focus(&handle, cx);
        cx.notify();
    }

    fn leave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(back) = &self.back {
            window.focus(back, cx);
        }
        cx.notify();
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(last) = self.rows.len().checked_sub(1) else {
            return;
        };
        let next = self
            .cursor
            .map_or(0, |i| i.saturating_add_signed(delta).min(last));
        self.cursor = Some(next);
        if let Some(folder) = self.cursor_folder()
            && self.selected_worktree.as_ref() != Some(&folder)
        {
            self.selected_worktree = Some(folder);
            self.write_state();
        }
        self.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn cursor_row(&self) -> Option<&Row> {
        self.rows.get(self.cursor?)
    }

    fn cursor_worktree(&self) -> Option<PathBuf> {
        match self.cursor_row()? {
            Row::Worktree(path) => Some(path.clone()),
            Row::Session(s, _) => s.worktree.clone(),
            _ => None,
        }
    }

    fn cursor_folder(&self) -> Option<PathBuf> {
        match self.cursor_row()? {
            Row::Worktree(path) | Row::Repository(path) | Row::Folder(path) => Some(path.clone()),
            Row::Session(s, _) => s.worktree.clone().or_else(|| s.cwd.clone()),
            _ => None,
        }
    }

    pub(crate) fn select_session(&mut self, index: usize, id: SessionId, cx: &mut Context<Self>) {
        let host = self.row_host(index).clone();
        self.select_session_on(index, host, id, cx);
    }

    pub(crate) fn select_session_on(
        &mut self,
        index: usize,
        host: HostId,
        id: SessionId,
        cx: &mut Context<Self>,
    ) {
        self.cursor = Some(index);
        if let Some(wt) = self.folder_on(&host, id).map(Path::to_path_buf)
            && self.selected_worktree.as_ref() != Some(&wt)
        {
            self.selected_worktree = Some(wt);
            self.write_state();
        }
        self.action_host = Some(host);
        cx.emit(Selected(id));
        cx.notify();
    }

    /// OWE-854: 세션 행 메뉴의 '세션 종료…'. 호스트를 남기고 셸에 확인·종료를 맡긴다.
    pub(crate) fn request_close_session(
        &mut self,
        index: usize,
        id: SessionId,
        cx: &mut Context<Self>,
    ) {
        self.cursor = Some(index);
        self.action_host = Some(self.row_host(index).clone());
        cx.emit(CloseSession(id));
        cx.notify();
    }

    /// OWE-854: 세션 행 메뉴의 '새 세션에 이어하기…'. 호스트를 남기고 셸에 모달을 맡긴다.
    pub(crate) fn request_continue_session(
        &mut self,
        index: usize,
        id: SessionId,
        cx: &mut Context<Self>,
    ) {
        self.cursor = Some(index);
        self.action_host = Some(self.row_host(index).clone());
        cx.emit(ContinueSession(id));
        cx.notify();
    }

    /// OWE-854: 세션 행 메뉴의 '이름 변경'. 호스트를 남기고 셸에 탭 이름 변경을 맡긴다.
    pub(crate) fn request_rename_session(
        &mut self,
        index: usize,
        id: SessionId,
        cx: &mut Context<Self>,
    ) {
        self.cursor = Some(index);
        self.action_host = Some(self.row_host(index).clone());
        cx.emit(RenameSession(id));
        cx.notify();
    }

    pub(crate) fn activate_worktree_card(
        &mut self,
        index: usize,
        host: HostId,
        path: PathBuf,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.cursor = Some(index);
        if self.selected_worktree.as_ref() != Some(&path) {
            self.selected_worktree = Some(path.clone());
            self.write_state();
        }
        if let Some(sess_id) = self
            .snapshot_for(&host)
            .sessions
            .iter()
            .find(|sess| {
                sess.worktree.as_ref() == Some(&path)
                    || (sess.worktree.is_none() && sess.cwd.as_ref() == Some(&path))
            })
            .map(|sess| sess.id)
        {
            self.select_session_on(index, host, sess_id, cx);
        } else {
            self.new_session_on(host, path, Program::Login, window, cx);
        }
    }

    pub(crate) fn new_session_on(
        &mut self,
        host: HostId,
        cwd: PathBuf,
        program: Program,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.action_host = Some(host);
        cx.emit(NewSession { cwd, program });
        if let Some(window) = window {
            self.leave(window, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn new_session(
        &mut self,
        cwd: PathBuf,
        program: Program,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let host = self
            .cursor
            .map_or(HostId::Local, |i| self.row_host(i).clone());
        self.new_session_on(host, cwd, program, window, cx);
    }

    fn new_agent_cursor(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = self.cursor_folder() else {
            return;
        };
        let Some(id) = self.agents.get(index).map(|p| p.id.clone()) else {
            return;
        };
        self.new_session(folder, Program::Agent { id }, Some(window), cx);
    }

    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cursor) = self.cursor else {
            return;
        };
        match self.cursor_row().cloned() {
            Some(Row::HostHeader { host, .. }) => self.fold_host(&host, None, cx),
            Some(Row::GroupHeader { key, .. }) => self.fold_group(key, None, cx),
            Some(Row::Repository(path)) => {
                self.cursor = Some(cursor);
                self.fold(&path, None, cx);
            }
            Some(Row::Session(session, _)) => {
                self.action_host = Some(self.row_host(cursor).clone());
                cx.emit(Selected(session.id));
                self.leave(window, cx);
            }
            Some(Row::Worktree(path) | Row::Folder(path)) => {
                let host = self.row_host(cursor).clone();
                self.new_session_on(host, path, Program::Login, Some(window), cx);
            }
            Some(Row::HiddenWorktrees(repo, _, _)) => self.toggle_external(&repo, cx),
            _ => {}
        }
    }

    /// `open`: `None`이면 뒤집고, `Some(true)`면 펼치고, `Some(false)`면 접는다.
    /// 저장소·폴더·워크트리의 접힘은 `UiExpansionState`에 저장해 재시작 후에도 유지한다(OWE-845).
    pub fn fold(&mut self, path: &Path, open: Option<bool>, cx: &mut Context<Self>) {
        let closed = self.collapsed.contains(path);
        if open.unwrap_or(closed) {
            self.collapsed.remove(path);
            self.expanded_cards.insert(path.to_path_buf());
        } else {
            self.collapsed.insert(path.to_path_buf());
            self.expanded_cards.remove(path);
        }
        self.persist(cx);
    }

    fn fold_cursor(&mut self, open: bool, cx: &mut Context<Self>) {
        match self.cursor_row().cloned() {
            Some(Row::HostHeader { host, .. }) => self.fold_host(&host, Some(open), cx),
            Some(Row::GroupHeader { key, .. }) => self.fold_group(key, Some(open), cx),
            Some(Row::Worktree(path) | Row::Folder(path) | Row::Repository(path)) => {
                self.fold(&path, Some(open), cx);
            }
            _ => {}
        }
    }

    fn toggle_pin(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let p = self.prefs.entry(path).or_default();
        p.pinned = !p.pinned;
        self.persist(cx);
    }

    fn toggle_pin_cursor(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.cursor_worktree() {
            self.toggle_pin(path, cx);
        }
    }

    fn open_in_cursor(&mut self, target: Target, cx: &mut Context<Self>) {
        if let Some(folder) = self.cursor_folder() {
            self.open_in(target, folder, cx);
        }
    }

    /// 폴더를 외부 앱으로 연다(MK-055). 실패는 알림으로 남긴다.
    fn open_in(&mut self, target: Target, folder: PathBuf, cx: &mut Context<Self>) {
        self.notice = None;
        #[cfg(test)]
        {
            self.last_open_in = Some((target, folder));
            cx.notify();
        }
        #[cfg(not(test))]
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { target.open(&folder) })
                .await;
            if let Err(e) = result {
                this.update(cx, |s, cx| {
                    s.notice = Some(e.to_string());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// OWE-851: 에이전트명이 비면 워크트리 이름, 그것마저 비면 세션 번호를 보여
    /// `1 ·  · 알 수 없음` 같은 빈 칸 라벨이 나가지 않게 한다.
    pub fn session_who(&self, session: &Session) -> String {
        let agent = session.status.agent.trim();
        if !agent.is_empty() {
            return agent.to_owned();
        }
        let tree = session
            .worktree
            .as_deref()
            .map(|p| self.name(p))
            .unwrap_or_default();
        if !tree.trim().is_empty() {
            return tree;
        }
        format!("{} {}", text::SESSION.get(), session.id.0)
    }

    /// MK-057 Jump 팔레트 항목: 세션마다 제목·에이전트·상태·경과 시간·현재 탭/워크트리·저장소/워크트리 칩을 채운다.
    pub fn jump_items(&self) -> Vec<Item> {
        let focused_id = self.focused;
        let focused_folder = focused_id
            .and_then(|id| self.folder(id))
            .map(Path::to_path_buf);
        let now_ns = self.now_unix_ns();
        let host_label = crate::i18n::catalog::jump::LOCAL_MAC.get().to_owned();
        let mut items: Vec<(u64, Item)> = self
            .snapshot
            .sessions
            .iter()
            .map(|s| {
                let (title, detail) = self.session_label(s);
                let agent =
                    crate::ui::status::infer_agent_from_title(&s.status.agent, &title).to_owned();
                let folder_path = s.worktree.as_deref().or(s.cwd.as_deref());
                let (repo_name, worktree_name) = if let Some(wt) = s.worktree.as_deref() {
                    let repo_path = self
                        .snapshot
                        .trees
                        .iter()
                        .find(|(repo, trees)| {
                            repo.as_path() == wt || trees.iter().any(|t| t.as_path() == wt)
                        })
                        .map_or(wt, |(repo, _)| repo.as_path());
                    (basename(repo_path), self.name(wt))
                } else if let Some(cwd) = s.cwd.as_deref() {
                    (basename(cwd), self.name(cwd))
                } else {
                    (String::new(), String::new())
                };
                let is_current_tab = focused_id == Some(s.id);
                let is_current_worktree = !is_current_tab
                    && folder_path.is_some()
                    && folder_path == focused_folder.as_deref();
                let age = if s.status.since_unix_ns > 0 {
                    crate::jump::format_age_ns(s.status.since_unix_ns, now_ns)
                } else if let Some(&ts) = folder_path.and_then(|p| self.worktree_updated_at.get(p))
                {
                    crate::jump::format_age_secs(ts, now_ns / 1_000_000_000)
                } else {
                    crate::i18n::catalog::jump::AGE_UNDER_MIN.get().to_owned()
                };
                // STYLEGUIDE §7.4: 사이드바 세션 행과 같은 라벨 규칙(`session_label`)을 공유한다.
                let label = if detail.is_empty() {
                    format!("{title} · {}", state_text(s.status.state))
                } else {
                    format!("{title} · {detail} · {}", state_text(s.status.state))
                };
                let mut item = Item::new(s.id, label, needs_you(&s.status));
                item.title = title;
                item.agent = agent;
                item.state = Some(s.status.state);
                item.unread = self.unread.contains(&s.id);
                item.age = age;
                item.is_current_tab = is_current_tab;
                item.is_current_worktree = is_current_worktree;
                item.host_label.clone_from(&host_label);
                item.repo_name = repo_name;
                item.worktree_name = worktree_name;
                item.rebuild_search_chars();
                (s.status.since_unix_ns, item)
            })
            .collect();
        items.sort_by_key(|(since_ns, item)| {
            (
                !item.is_current_tab,
                !item.is_current_worktree,
                !item.needs_you,
                std::cmp::Reverse(*since_ns),
            )
        });
        items.into_iter().map(|(_, item)| item).collect()
    }

    /// Jump 팔레트 '최근 작업 공간' 섹션 항목(git 워크트리 + 열린 폴더 워크스페이스).
    pub fn jump_worktrees(&self) -> Vec<crate::jump::WorktreeItem> {
        let focused_id = self.focused;
        let focused_folder = focused_id
            .and_then(|id| self.folder(id))
            .map(Path::to_path_buf);
        let now_ns = self.now_unix_ns();
        let now_secs = now_ns / 1_000_000_000;
        let host_label = crate::i18n::catalog::jump::LOCAL_MAC.get().to_owned();
        let mut seen = BTreeSet::new();
        let mut entries: Vec<(u64, crate::jump::WorktreeItem)> = Vec::new();

        let mut push_path = |path: &Path, repo_path: &Path, is_primary: bool| {
            if !seen.insert(path.to_path_buf()) {
                return;
            }
            let pref_name = self
                .prefs
                .get(path)
                .filter(|v| !v.name.is_empty())
                .map(|v| v.name.as_str())
                .filter(|n| !krill_proto::git::is_default_branch_or_worktree_name(n));
            let name = pref_name.map_or_else(|| basename(path), str::to_owned);
            let repo_name = basename(repo_path);
            let branch = self.branch(path).unwrap_or_default().to_owned();
            let is_current = focused_folder.as_deref() == Some(path);
            let sessions_in_wt: Vec<&Session> = self
                .snapshot
                .sessions
                .iter()
                .filter(|s| {
                    s.worktree.as_deref() == Some(path)
                        || (s.worktree.is_none() && s.cwd.as_deref() == Some(path))
                })
                .collect();
            let session_id = focused_id
                .filter(|id| sessions_in_wt.iter().any(|s| s.id == *id))
                .or_else(|| sessions_in_wt.first().map(|s| s.id));
            let unread = sessions_in_wt.iter().any(|s| self.unread.contains(&s.id));
            let pairs: Vec<_> = sessions_in_wt
                .iter()
                .filter(|s| {
                    !s.status.agent.is_empty()
                        || !matches!(s.status.state, AgentState::Exited | AgentState::Unknown)
                })
                .map(|s| (s.status.state, s.status.agent.as_str()))
                .collect();
            let state = crate::ui::status::summarize(&pairs)
                .groups
                .first()
                .map(|g| g.state)
                .or_else(|| (!sessions_in_wt.is_empty()).then_some(AgentState::Done));
            let latest_session_ns = sessions_in_wt
                .iter()
                .map(|s| s.status.since_unix_ns)
                .max()
                .unwrap_or(0);
            let updated_secs = if latest_session_ns > 0 {
                latest_session_ns / 1_000_000_000
            } else if let Some(&ts) = self.worktree_updated_at.get(path) {
                ts
            } else {
                fs::metadata(path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs())
            };
            let age = if latest_session_ns > 0 {
                crate::jump::format_age_ns(latest_session_ns, now_ns)
            } else if updated_secs > 0 {
                crate::jump::format_age_secs(updated_secs, now_secs)
            } else if !sessions_in_wt.is_empty() {
                crate::i18n::catalog::jump::AGE_UNDER_MIN.get().to_owned()
            } else {
                String::new()
            };
            let sort_ts = if latest_session_ns > 0 {
                latest_session_ns
            } else {
                updated_secs.saturating_mul(1_000_000_000)
            };
            entries.push((
                sort_ts,
                crate::jump::WorktreeItem::new(
                    path.to_path_buf(),
                    session_id,
                    name,
                    branch,
                    repo_name,
                    host_label.clone(),
                    age,
                    state,
                    unread,
                    is_primary,
                    is_current,
                ),
            ));
        };

        for (repo, trees) in &self.snapshot.trees {
            let show_ext = self.shown_external.contains(repo);
            for wt in trees {
                if !show_ext
                    && external::is_claude_worktree(wt)
                    && focused_folder.as_ref() != Some(wt)
                {
                    continue;
                }
                push_path(wt, repo, wt == repo);
            }
        }
        for s in &self.snapshot.sessions {
            if s.worktree.is_none()
                && let Some(cwd) = s.cwd.as_deref()
            {
                push_path(cwd, cwd, false);
            }
        }

        entries.sort_by(|(ts_a, a), (ts_b, b)| {
            (!a.is_current)
                .cmp(&(!b.is_current))
                .then_with(|| a.session_id.is_none().cmp(&b.session_id.is_none()))
                .then_with(|| ts_b.cmp(ts_a))
                .then_with(|| (!a.is_main).cmp(&(!b.is_main)))
                .then_with(|| a.repo_name.cmp(&b.repo_name))
                .then_with(|| a.name.cmp(&b.name))
        });
        entries.into_iter().map(|(_, wt)| wt).collect()
    }

    /// OWE-865: Jump 팔레트용 워크트리 목록(세션 유무와 무관, 로컬만).
    /// 세션이 있는 워크트리는 열기로, 없는 워크트리·폴더는 새 세션 만들기로 이어진다.
    /// 순서는 저장소 경로순(결정적)이며, 원격 호스트 워크트리는 포함하지 않는다.
    pub fn worktree_entries(&self) -> Vec<crate::jump::WorktreeEntry> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for (repo, trees) in &self.snapshot.trees {
            let repo_name = basename(repo);
            for tree in trees {
                seen.insert(tree.clone());
                let session = self
                    .snapshot
                    .sessions
                    .iter()
                    .find(|s| s.worktree.as_deref() == Some(tree.as_path()))
                    .map(|s| s.id);
                out.push(crate::jump::WorktreeEntry::new(
                    tree.clone(),
                    self.name(tree),
                    Some(repo_name.clone()),
                    session,
                ));
            }
        }
        // git 밖 폴더(세션 작업 디렉터리): 세션이 있으므로 열기로 이어진다.
        for s in self
            .snapshot
            .sessions
            .iter()
            .filter(|s| s.worktree.is_none())
        {
            if let Some(cwd) = &s.cwd
                && seen.insert(cwd.clone())
            {
                out.push(crate::jump::WorktreeEntry::new(
                    cwd.clone(),
                    self.name(cwd),
                    None,
                    Some(s.id),
                ));
            }
        }
        out
    }

    fn rename_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.cursor_worktree() {
            self.rename(path, window, cx);
        }
    }

    fn rename(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name(&path);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        input.update(cx, |input, cx| input.focus(window, cx));
        self.edit_subscription =
            Some(
                cx.subscribe_in(&input, window, |s, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        s.finish_rename(Some(window), cx);
                    }
                }),
            );
        self.editing = Some((path, input));
        cx.notify();
    }

    fn finish_rename(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if let Some((path, input)) = self.editing.take() {
            let value = input.read(cx).value();
            let name: String = value
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(128)
                .collect();
            self.prefs.entry(path).or_default().name = name;
            self.edit_subscription = None;
            if let (Some(window), Some(focus)) = (window, &self.focus) {
                window.focus(focus, cx);
            }
            self.persist(cx);
        }
    }

    /// 이름 변경 편집을 저장하지 않고 닫는다(OWE-847: Esc·취소 버튼).
    fn cancel_rename(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.editing = None;
            self.edit_subscription = None;
            if let (Some(window), Some(focus)) = (window, &self.focus) {
                window.focus(focus, cx);
            }
            cx.notify();
        }
    }

    /// 저장소·폴더 프로젝트를 목록에서 제거한다(OWE-845).
    /// `projects.toml`에 기록된 이관 프로젝트는 파일에서도 지우고,
    /// 그 외에는 제거 집합에 넣어 행을 숨긴다(재시작 후에도 유지).
    pub fn remove_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(dir) = self.state_dir().map(Path::to_path_buf) {
            external::remove_imported_project(&dir, &path);
            self.sync_imported_projects_from_disk();
        }
        self.collapsed.remove(&path);
        self.expanded_cards.remove(&path);
        self.shown_external.remove(&path);
        self.removed_projects.insert(path);
        self.persist(cx);
    }

    /// 워크트리 삭제 확인 모달을 연다(OWE-847). 메뉴 클릭 시 즉시 삭제하지 않는다.
    pub(super) fn request_delete_worktree(
        &mut self,
        path: PathBuf,
        with_descendants: bool,
        cx: &mut Context<Self>,
    ) {
        let host = HostId::Local;
        let descendant_count = if with_descendants {
            self.lineage_descendants(&path).len()
        } else {
            0
        };
        let dirty = self
            .meta_for(&host)
            .get(&path)
            .and_then(|m| m.dirty)
            .unwrap_or(0);
        self.delete_confirm = Some(DeleteConfirm {
            path,
            with_descendants,
            descendant_count,
            dirty,
            force: false,
        });
        cx.notify();
    }

    /// 삭제 확인 모달을 닫는다(OWE-847: 취소 버튼·Esc).
    pub(super) fn cancel_delete(&mut self, cx: &mut Context<Self>) {
        if self.delete_confirm.take().is_some() {
            cx.notify();
        }
    }

    /// 삭제 확인 모달의 강제 삭제 토글(OWE-847: 미커밋 변경 포함).
    pub(super) fn toggle_delete_force(&mut self, cx: &mut Context<Self>) {
        if let Some(confirm) = self.delete_confirm.as_mut() {
            confirm.force = !confirm.force;
            cx.notify();
        }
    }

    /// 삭제 확인 모달에서 삭제를 확정한다(OWE-847).
    pub(super) fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.delete_confirm.take() else {
            return;
        };
        cx.notify();
        if confirm.with_descendants {
            self.delete_with_descendants(confirm.path, confirm.force, cx);
        } else {
            self.delete_worktree(confirm.path, confirm.force, cx);
        }
    }

    /// 바꾼 이름 → 브랜치 → 첫 프롬프트 자동 제목 → 폴더 이름.
    fn name(&self, path: &Path) -> String {
        let pref_name = self
            .prefs
            .get(path)
            .filter(|v| !v.name.is_empty())
            .map(|v| v.name.as_str());
        if let Some(custom) =
            pref_name.filter(|n| !krill_proto::git::is_default_branch_or_worktree_name(n))
        {
            return custom.to_owned();
        }
        if let Some(branch) = self.branch(path) {
            if !krill_proto::git::is_default_branch_or_worktree_name(branch) {
                return branch.to_owned();
            }
            if let Some(auto) = self.worktree_auto_title(path) {
                return auto.to_owned();
            }
            if pref_name.is_none() {
                return branch.to_owned();
            }
        }
        if let Some(auto) = self.worktree_auto_title(path) {
            return auto.to_owned();
        }
        pref_name.map_or_else(|| basename(path), str::to_owned)
    }

    pub(super) fn worktree_auto_title(&self, path: &Path) -> Option<&str> {
        if self.auto_titles.is_empty() {
            return None;
        }
        self.snapshot
            .sessions
            .iter()
            .filter(|s| s.worktree.as_deref() == Some(path) || s.cwd.as_deref() == Some(path))
            .find_map(|s| self.auto_titles.get(&s.id).map(String::as_str))
    }

    fn branch(&self, path: &Path) -> Option<&str> {
        self.meta.get(path)?.branch.as_deref()
    }

    fn branch_on(&self, host: &HostId, path: &Path) -> Option<&str> {
        self.meta_for(host).get(path)?.branch.as_deref()
    }

    /// 에이전트 대시보드에 표시할 모든 호스트(로컬 + 원격)의 세션 목록.
    #[must_use]
    pub fn dashboard_sessions(&self) -> Vec<(HostId, Session)> {
        let mut out = Vec::new();
        if self.options.filters.contains_host(&HostId::Local) {
            out.extend(
                self.snapshot
                    .sessions
                    .iter()
                    .cloned()
                    .map(|s| (HostId::Local, s)),
            );
        }
        for remote in &self.remote_hosts {
            let host = HostId::Remote(remote.id.clone());
            if !self.options.filters.contains_host(&host) {
                continue;
            }
            out.extend(
                remote
                    .snapshot
                    .sessions
                    .iter()
                    .cloned()
                    .map(|s| (host.clone(), s)),
            );
        }
        out
    }

    pub fn session_label(&self, session: &Session) -> (String, String) {
        let custom_title = self
            .custom_titles
            .get(&session.id)
            .map(String::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty());
        let explicit_title = self
            .titles
            .get(&session.id)
            .map(String::as_str)
            .filter(|t| !krill_proto::git::is_default_branch_or_worktree_name(t));
        let title = custom_title
            .or(explicit_title)
            .or_else(|| self.auto_titles.get(&session.id).map(String::as_str))
            .or_else(|| self.titles.get(&session.id).map(String::as_str));
        session_label(session, title)
    }

    #[must_use]
    pub fn session_label_on(&self, host: &HostId, session: &Session) -> (String, String) {
        if host.is_local() {
            self.session_label(session)
        } else {
            session_label(session, None)
        }
    }

    /// OWE-662: 로컬 호스트 워크트리 세션의 상태 묶음과 미확인 세션이 있는지.
    #[cfg(test)]
    #[cfg(unix)]
    fn tree_summary(&self, tree: &Path) -> (crate::ui::status::Summary<'_>, bool) {
        self.tree_summary_on(&HostId::Local, tree)
    }

    /// OWE-662: 워크트리 세션의 상태 묶음과 미확인 세션이 있는지.
    /// 에이전트도 상태 근거도 없는 셸(종료·알 수 없음)은 묶음에서 뺀다.
    #[cfg(test)]
    fn tree_summary_on(
        &self,
        host: &HostId,
        tree: &Path,
    ) -> (crate::ui::status::Summary<'_>, bool) {
        let sessions: Vec<_> = self
            .snapshot_for(host)
            .sessions
            .iter()
            .filter(|s| s.worktree.as_deref() == Some(tree))
            .collect();
        let unread = host.is_local() && sessions.iter().any(|s| self.unread.contains(&s.id));
        let pairs: Vec<_> = sessions
            .iter()
            .filter(|s| {
                !s.status.agent.is_empty()
                    || !matches!(s.status.state, AgentState::Exited | AgentState::Unknown)
            })
            .map(|s| (s.status.state, s.status.agent.as_str()))
            .collect();
        (crate::ui::status::summarize(&pairs), unread)
    }
}

#[cfg(test)]
#[cfg(unix)]
use krill_proto::daemon::Paths;

#[cfg(test)]
#[cfg(unix)]
#[path = "sidebar/tests.rs"]
mod tests;
