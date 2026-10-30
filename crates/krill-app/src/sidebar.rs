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
    Subscription, Task, Window, actions,
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

mod async_state;
pub mod external;
mod hooks;
mod ipc;
pub mod lineage;
pub mod options;
mod path_identity;
mod pr;
mod profiles;
mod session_order;
mod startup;
mod view;
mod viewport;

pub use async_state::flush_file_io;
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
/// 기본 세션·프로젝트 행 높이. 상세 카드는 가변 높이 목록에서 별도로 측정한다.
const ROW_HEIGHT: f32 = 32.;
/// 프로젝트(저장소·폴더·그룹) 아래 워크트리·세션 행의 들여쓰기(px).
const CHILD_INDENT: f32 = 12.;
/// 호스트 머리줄이 있을 때(원격 호스트 등록) 그 아래 프로젝트 행의 들여쓰기(px).
const HOST_CHILD_INDENT: f32 = 8.;
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

impl Session {
    /// 에이전트 `SessionEnd` hook과 달리 루트 프로세스 종료가 확인된 세션.
    #[must_use]
    pub fn is_exited(&self) -> bool {
        self.status.state == AgentState::Exited
            && self.status.source == Some(krill_proto::control::Source::Process)
    }
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

pub type PromptHistories = BTreeMap<SessionId, krill_proto::git::SessionPromptHistory>;

/// IPC boundary: gpui tests supply snapshots without waiting for external IO.
/// [`Source::poll_snapshot`]의 결과를 기다리는 future.
pub type PollSnapshot =
    std::pin::Pin<Box<dyn std::future::Future<Output = io::Result<(Snapshot, Meta)>> + Send>>;

pub trait Source: Send + Sync {
    fn fetch(&self) -> io::Result<Snapshot>;
    /// 등록 세대가 바뀐 원격 연결의 캐시·폴링 작업을 교체하기 위한 타깃.
    fn target(&self) -> Option<&HostTarget> {
        None
    }
    fn fetch_shared(&self) -> io::Result<Arc<Snapshot>> {
        self.fetch().map(Arc::new)
    }
    /// 마지막으로 읽은 호스트 스냅샷의 프롬프트 기록.
    fn prompt_histories(&self) -> PromptHistories {
        PromptHistories::new()
    }
    /// 워크트리 행의 git 정보. 모르면 비워 둔다(UI가 숨긴다).
    fn meta(&self, _trees: &BTreeMap<PathBuf, Vec<PathBuf>>) -> Meta {
        Meta::new()
    }
    /// 폴링 한 번: `fetch` 뒤 그 트리로 `meta`. 기본은 호출한 스레드에서 바로 돈다.
    /// 소켓 소스는 호스트 전용 스레드에서 돌려 원격 ssh·IPC 대기가 공유 실행기 스레드를 붙잡지 않게 한다.
    fn poll_snapshot(&self) -> PollSnapshot {
        let polled = self.fetch().map(|snap| {
            let meta = self.meta(&snap.trees);
            (snap, meta)
        });
        Box::pin(std::future::ready(polled))
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
    /// 저장소가 있는 호스트의 실제 브랜치·태그. 미지원 데몬은 `Unsupported`.
    fn worktree_refs(&self, _repo: &Path) -> io::Result<Vec<String>> {
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
    pub host: HostId,
    pub path: PathBuf,
    pub with_descendants: bool,
    pub descendant_count: usize,
    pub dirty: usize,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingWorktree {
    pub host: HostId,
    pub id: u64,
    pub dialog_id: u64,
    pub repo: PathBuf,
    pub parent: Option<PathBuf>,
    pub name: String,
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
    PendingWorktree {
        id: u64,
        repo: PathBuf,
        name: String,
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
    /// 마지막 연결 실패 메시지. 끊긴 이유(꺼짐·인증 거부·Krill 없음)를 보여 주는 데 쓴다.
    pub error: Option<String>,
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
    unread.retain(|id| {
        new.iter()
            .any(|s| s.id == *id && s.status.state != AgentState::Exited)
    });
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
    if windows_style(path) {
        let text = path.to_string_lossy();
        return windows_parts(&text).last().unwrap_or(&text).to_owned();
    }
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Windows 호스트 경로(`C:\x`, `\\server\share`). macOS 앱에서는 `\`가 구분자가 아니라
/// `Path`가 경로 전체를 한 덩어리로 본다 — 이름·포함 판정을 문자열로 따로 한다.
fn windows_style(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    matches!(bytes, [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
        || bytes.starts_with(br"\\")
        || bytes.starts_with(b"//")
}

fn windows_parts(path: &str) -> impl Iterator<Item = &str> {
    path.split(['\\', '/']).filter(|part| !part.is_empty())
}

/// `path`가 `root` 아래(같은 경로 포함)인지. Windows 경로는 구분자(`\`·`/`)와 대소문자를 가리지 않는다.
pub(crate) fn path_within(path: &Path, root: &Path) -> bool {
    krill_proto::path::within(path, root)
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

/// Windows console titles can retain the launcher's executable after an agent
/// starts. Recognize only exact shell labels or drive-absolute shell paths.
fn is_windows_shell_title(title: &str) -> bool {
    let title = title.trim();
    let title = title
        .split_once(':')
        .filter(|(prefix, _)| {
            prefix.trim().eq_ignore_ascii_case("administrator")
                || prefix.trim() == crate::i18n::WINDOWS_ADMIN_TITLE_KO
        })
        .map_or(title, |(_, rest)| rest.trim());
    let title = title
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(title);
    if [
        "Windows PowerShell",
        "PowerShell",
        "powershell.exe",
        "pwsh",
        "pwsh.exe",
        "cmd",
        "cmd.exe",
    ]
    .iter()
    .any(|shell| title.eq_ignore_ascii_case(shell))
    {
        return true;
    }
    matches!(title.as_bytes(), [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
        && title.rsplit(['\\', '/']).next().is_some_and(|leaf| {
            ["powershell.exe", "pwsh.exe", "cmd.exe"]
                .iter()
                .any(|shell| leaf.eq_ignore_ascii_case(shell))
        })
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
const DASHBOARD_READ_FILE: &str = "dashboard-read.json";

/// 에이전트 대시보드의 완료(`Done | Exited`) 카드 읽음 상태 및 읽은 순서.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadDoneEntry {
    pub since_unix_ns: u64,
    pub seq: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedReadDone {
    #[serde(default)]
    entries: Vec<(HostId, SessionId, ReadDoneEntry)>,
}

pub(super) fn load_read_done(path: Option<&Path>) -> BTreeMap<(HostId, SessionId), ReadDoneEntry> {
    let Some(dir) = path
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
    else {
        return BTreeMap::new();
    };
    let file = dir.join(DASHBOARD_READ_FILE);
    let Ok(bytes) = fs::read(file) else {
        return BTreeMap::new();
    };
    serde_json::from_slice::<PersistedReadDone>(&bytes)
        .map(|p| {
            p.entries
                .into_iter()
                .map(|(host, id, entry)| ((host, id), entry))
                .collect()
        })
        .unwrap_or_default()
}

fn save_read_done(
    dir: &Path,
    read_done: &BTreeMap<(HostId, SessionId), ReadDoneEntry>,
) -> io::Result<()> {
    krill_proto::os::fs::DirBuilderExt::mode(fs::DirBuilder::new().recursive(true), 0o700)
        .create(dir)?;
    let path = dir.join(DASHBOARD_READ_FILE);
    let temp = path.with_extension("json.tmp");
    let payload = PersistedReadDone {
        entries: read_done
            .iter()
            .map(|((host, id), entry)| (host.clone(), *id, *entry))
            .collect(),
    };
    let bytes = serde_json::to_vec(&payload).map_err(io::Error::other)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temp, &path)?;
    crate::layout::sync_parent(&path)
}

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

/// 두 `Vec<PathBuf>`는 수동 프로젝트 순서(5번째)와 워크트리 순서(6번째 구간)다. postcard는 구조체 끝 필드 추가를
/// 옛 파일에서 못 읽으므로 구간을 따로 덧붙인다. 옛 앱은 남는 바이트를 무시한다.
type PersistedSidebarState = (
    Preferences,
    WorkspaceOptions,
    lineage::LineageState,
    UiExpansionState,
    Vec<PathBuf>,
    Vec<PathBuf>,
);

/// 저장 파일을 읽는다. 손상·미지원 버전은 덮어쓰지 않도록 `.corrupt`로 보존한 뒤 오류를 돌려준다.
#[cfg(test)]
#[cfg(unix)]
fn load(path: &Path) -> io::Result<Preferences> {
    load_state(path).map(|(prefs, ..)| prefs)
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
        return Ok(sanitize_persisted_sidebar_state((
            prefs,
            WorkspaceOptions::default(),
            lineage::LineageState::default(),
            UiExpansionState::default(),
            Vec::new(),
            Vec::new(),
        )));
    }
    let (opts, rest) =
        postcard::take_from_bytes::<WorkspaceOptions>(rest).map_err(|e| invalid(&e.to_string()))?;
    if rest.is_empty() {
        return Ok(sanitize_persisted_sidebar_state((
            prefs,
            opts,
            lineage::LineageState::default(),
            UiExpansionState::default(),
            Vec::new(),
            Vec::new(),
        )));
    }
    let (lin, rest) = postcard::take_from_bytes::<lineage::LineageState>(rest)
        .map_err(|e| invalid(&e.to_string()))?;
    if rest.is_empty() {
        return Ok(sanitize_persisted_sidebar_state((
            prefs,
            opts,
            lin,
            UiExpansionState::default(),
            Vec::new(),
            Vec::new(),
        )));
    }
    let (ui, rest) =
        postcard::take_from_bytes::<UiExpansionState>(rest).map_err(|e| invalid(&e.to_string()))?;
    let (order, rest) = if rest.is_empty() {
        (Vec::new(), rest)
    } else {
        postcard::take_from_bytes::<Vec<PathBuf>>(rest).map_err(|e| invalid(&e.to_string()))?
    };
    let worktree_order = if rest.is_empty() {
        Vec::new()
    } else {
        postcard::from_bytes::<Vec<PathBuf>>(rest).map_err(|e| invalid(&e.to_string()))?
    };
    Ok(sanitize_persisted_sidebar_state((
        prefs,
        opts,
        lin,
        ui,
        order,
        worktree_order,
    )))
}

fn clean_persisted_path(p: PathBuf) -> PathBuf {
    if let Some(s) = p.to_str()
        && let Some(base) = krill_proto::orchestration::strip_workspace_instance(s)
        && !p.exists()
    {
        return PathBuf::from(base);
    }
    p
}

fn sanitize_persisted_sidebar_state(
    (prefs, mut opts, mut lin, mut ui, order, worktree_order): PersistedSidebarState,
) -> PersistedSidebarState {
    let mut clean_prefs = Preferences::default();
    for (k, mut v) in prefs {
        let ck = clean_persisted_path(k);
        if krill_proto::orchestration::strip_workspace_instance(&v.name).is_some() {
            v.name.clear();
        }
        clean_prefs.entry(ck).or_insert(v);
    }
    opts.filters.repos = opts
        .filters
        .repos
        .into_iter()
        .map(clean_persisted_path)
        .collect();
    let mut clean_parents = BTreeMap::new();
    for (c, p) in lin.parents {
        let cc = clean_persisted_path(c);
        let cp = clean_persisted_path(p);
        if cc != cp {
            clean_parents.entry(cc).or_insert(cp);
        }
    }
    lin.parents = clean_parents;
    lin.collapsed_lineage = lin
        .collapsed_lineage
        .into_iter()
        .map(clean_persisted_path)
        .collect();
    lin.sleeping_worktrees = lin
        .sleeping_worktrees
        .into_iter()
        .map(clean_persisted_path)
        .collect();
    ui.collapsed = ui.collapsed.into_iter().map(clean_persisted_path).collect();
    ui.expanded_cards = ui
        .expanded_cards
        .into_iter()
        .map(clean_persisted_path)
        .collect();
    ui.selected_worktree = ui.selected_worktree.map(clean_persisted_path);
    ui.shown_external = ui
        .shown_external
        .into_iter()
        .map(clean_persisted_path)
        .collect();
    ui.removed_projects = ui
        .removed_projects
        .into_iter()
        .map(clean_persisted_path)
        .collect();
    let mut clean_order = Vec::with_capacity(order.len());
    for p in order {
        let cp = clean_persisted_path(p);
        if !clean_order.contains(&cp) {
            clean_order.push(cp);
        }
    }
    (
        clean_prefs,
        opts,
        lin,
        ui,
        clean_order,
        worktree_order
            .into_iter()
            .map(clean_persisted_path)
            .collect(),
    )
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
        &[],
        &[],
    )
}

fn save_state(
    path: &Path,
    prefs: &Preferences,
    opts: &WorkspaceOptions,
    lin: &lineage::LineageState,
    ui: &UiExpansionState,
    order: &[PathBuf],
    worktree_order: &[PathBuf],
) -> io::Result<()> {
    let mut bytes = postcard::to_stdvec(prefs).map_err(io::Error::other)?;
    let need_ui = !ui.is_empty() || !order.is_empty() || !worktree_order.is_empty();
    let need_lin = !lin.is_empty() || need_ui;
    if opts.is_customized() || need_lin {
        bytes.extend(postcard::to_stdvec(opts).map_err(io::Error::other)?);
        if need_lin {
            bytes.extend(postcard::to_stdvec(lin).map_err(io::Error::other)?);
            if need_ui {
                bytes.extend(postcard::to_stdvec(ui).map_err(io::Error::other)?);
                if !order.is_empty() || !worktree_order.is_empty() {
                    bytes.extend(postcard::to_stdvec(order).map_err(io::Error::other)?);
                    if !worktree_order.is_empty() {
                        bytes
                            .extend(postcard::to_stdvec(worktree_order).map_err(io::Error::other)?);
                    }
                }
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

/// 워크스페이스 선택은 마지막에 사용한 탭·칸을 셸이 복원한다.
#[derive(Debug, Clone)]
pub struct WorkspaceSelected {
    pub host: HostId,
    pub path: PathBuf,
    pub focus_terminal: bool,
}

/// 빈 상태의 '폴더 열기' 버튼. 셸이 기존 폴더 열기 경로(`choose_folder`)로 받는다(OWE-654).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFolder;

/// 워크트리·폴더 행에서 새 세션(로그인 셸·에이전트)을 띄울 때 내보내는 이벤트(N4, PRD 시나리오 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub cwd: PathBuf,
    pub program: Program,
}

/// 라이브 목록이 비어도 저장된 레이아웃의 칸을 먼저 찾아 워크스페이스를 연다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWorkspace {
    pub host: HostId,
    pub cwd: PathBuf,
}

/// OWE-854: 사이드바 세션 행 메뉴의 '세션 종료…'. 셸이 확인 후 데몬에 `Request::Close`를 보낸다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseSession(pub SessionId);

/// OWE-854: 사이드바 세션 행 메뉴의 '새 세션에 이어하기…'. 셸이 이어하기 모달을 연다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinueSession(pub SessionId);

/// The menu owns its clicked target, independently of pane focus and row indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchSessionAccount {
    pub host: HostId,
    pub session: SessionId,
    pub provider: krill_proto::usage::ProviderKind,
    pub account_id: String,
}

/// OWE-854: 사이드바 세션 행 메뉴의 '이름 변경'. 셸이 그 세션의 탭을 열고 탭 이름 변경(OWE-870)을 띄운다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenameSession(pub SessionId);

/// 끊긴 원격 호스트 섹션에서 '다시 연결'을 눌렀을 때 내보내는 이벤트(MK-131).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectHost(pub HostId);

/// 로컬 데몬 재연결 뒤 처음 받은 로컬 세션 목록의 세션 수.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Relisted(pub usize);

/// 셸이 알려 주는 로컬 데몬 감시 연결의 재연결 단계.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relink {
    Steady,
    /// 끊겨 다시 붙는 중. '내 Mac' 끊김을 보존 톤으로 그린다.
    Reconnecting,
    /// 다시 붙었고, 다음 로컬 목록 도착 때 [`Relisted`]를 한 번 내보낸다.
    AwaitingList,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeleteFocus {
    Pending,
    Focused,
}

pub struct Sidebar {
    snapshot: Snapshot,
    meta: Meta,
    local_status: HostStatus,
    relink: Relink,
    remote_hosts: Vec<RemoteHost>,
    remote_task: Option<Task<()>>,
    remote_manager: Option<HostManager>,
    remote_info: BTreeMap<String, krill_hosts::HostConnectionInfo>,
    collapsed_hosts: BTreeSet<HostId>,
    /// 사용자가 끌어다 놓아 정한 프로젝트(저장소·폴더) 순서(`sidebar.bin` 5번째 구간).
    project_order: Vec<PathBuf>,
    worktree_order: Vec<PathBuf>,
    /// Runtime projection of Layout order; Layout remains the persisted source.
    session_order: BTreeMap<HostId, BTreeMap<SessionId, usize>>,
    row_hosts: Vec<HostId>,
    action_host: Option<HostId>,
    prs: BTreeMap<PathBuf, crate::pull_request::Status>,
    pr_guidance_dismissed: bool,
    pr_task: Option<Task<()>>,
    pr_service: Option<Arc<crate::pull_request::Service>>,
    rows: Vec<Row>,
    /// 프로젝트 기준 보기에서 다른 PC의 같은 프로젝트를 합친 머리줄 행 → 합쳐진 (호스트, 경로).
    merged_projects: BTreeMap<usize, Vec<(HostId, PathBuf)>>,
    /// 논리 세션 행 → 카드 머리줄. 렌더 중 계보를 매번 역탐색하지 않는다.
    card_parents: Vec<Option<usize>>,
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
    /// 렌더용 계보 깊이·직계 자식 수. `rebuild`가 행과 함께 다시 계산한다.
    lineage_depths: BTreeMap<PathBuf, usize>,
    lineage_child_counts: BTreeMap<PathBuf, usize>,
    sleeping_worktrees: BTreeSet<PathBuf>,
    shown_external: BTreeSet<PathBuf>,
    collapsed: BTreeSet<PathBuf>,
    /// 카드 하단 요약 알약을 눌러 개별 세션 행을 펼친 워크트리 집합(Orca 기본값: 접힘).
    expanded_cards: BTreeSet<PathBuf>,
    /// 사용자가 요약 알약을 눌러 명시적으로 접은 다중 세션 카드(자동 펼침 예외).
    manually_folded_cards: BTreeSet<PathBuf>,
    /// 마지막으로 선택·활성화된 워크트리/폴더 경로(`sidebar.bin`에 영속화, OWE-842).
    selected_worktree: Option<PathBuf>,
    /// 목록에서 제거한 저장소·폴더 프로젝트(OWE-845). `projects.toml` 정리와 별개로 행을 숨긴다.
    removed_projects: BTreeSet<PathBuf>,
    /// 프로젝트(저장소·폴더) 헤더 아이콘·배지 색상·표시 이름 메타.
    project_visuals: BTreeMap<PathBuf, external::ProjectVisualMeta>,
    /// 원격 호스트 저장소에서 탐지한 아이콘·메타 캐시.
    remote_visuals: BTreeMap<PathBuf, external::ProjectVisualMeta>,
    /// 원격 호스트별로 이미 아이콘 탐지를 요청한 저장소 경로 집합.
    remote_probed_repos: BTreeMap<String, BTreeSet<PathBuf>>,
    disk: async_state::State,
    /// 저장소 훅 IO는 렌더링과 분리해 마지막 완료 결과만 표시한다.
    setup_prompt: hooks::SetupPrompt,
    /// 워크트리 삭제 확인 모달 상태(OWE-847). `Some`일 때 모달을 띄운다.
    delete_confirm: Option<DeleteConfirm>,
    delete_scope_focus: Option<gpui::FocusHandle>,
    delete_cancel_focus: Option<gpui::FocusHandle>,
    delete_previous_focus: Option<gpui::FocusHandle>,
    delete_focus: DeleteFocus,
    path: Option<PathBuf>,
    error: Option<String>,
    /// 연결 오류는 폴링 복구 시 지운다. 저장·손상 오류(`error`)는 사용자가 확인할 때까지 유지한다.
    /// 두 번째 값은 자동 재시도 중인 일시적 연결 오류 여부다.
    poll_error: Option<(String, bool)>,
    /// Open in 실패처럼 사용자가 닫을 때까지 남기는 알림.
    notice: Option<String>,
    task: Option<Task<()>>,
    source: Option<Arc<dyn Source>>,
    card_list: viewport::CardList,
    status_summary: (usize, Arc<Vec<AgentState>>),
    local_rows_cache: std::cell::RefCell<options::LocalRowsCache>,
    hovered_worktree: Option<(HostId, PathBuf)>,
    editing: Option<(PathBuf, Entity<InputState>)>,
    edit_subscription: Option<Subscription>,
    orca_import_subscription: Option<Subscription>,
    pub(crate) creating: Option<(Entity<worktree::Dialog>, Subscription)>,
    pub(super) creating_background: Vec<(u64, Entity<worktree::Dialog>, Subscription)>,
    pub(super) creating_dialog_id: u64,
    pub(super) next_creating_id: u64,
    pub(super) pending_worktrees: Vec<PendingWorktree>,
    pub(super) recently_created_worktrees: BTreeSet<PathBuf>,
    pub(super) pending_reveal_worktree: Option<PathBuf>,
    /// OWE-924: 워크트리 생성 시 예약된 초기 프롬프트(워크트리 경로 → 프롬프트).
    pending_prompts: BTreeMap<PathBuf, String>,
    /// OWE-854: 셸 탭에서 사용자가 직접 지정한 세션별 제목. 세션 행 라벨의 최우선 후보.
    custom_titles: BTreeMap<SessionId, String>,
    /// 셸이 아는 세션별 터미널 제목(보이는 칸만). 세션 이름의 첫 후보.
    titles: BTreeMap<SessionId, String>,
    /// 첫 프롬프트 훅에서 자동 생성된 세션 제목(`session-titles.json`, OWE-737).
    auto_titles: BTreeMap<SessionId, String>,
    /// 세션별 최근 프롬프트 턴 히스토리(`session-prompts.json`).
    prompt_histories: BTreeMap<SessionId, krill_proto::git::SessionPromptHistory>,
    remote_prompt_histories: BTreeMap<String, PromptHistories>,
    last_activity_minute: Option<u64>,
    /// 테스트용 시각 고정(ns, OWE-737).
    now_override_ns: Option<u64>,
    /// 셸의 포커스 칸 세션(OWE-662 미확인 판정).
    focused: Option<SessionId>,
    /// 로컬 스냅샷 행 선택과 이미 동기화된 포커스 세션.
    focused_synced: Option<SessionId>,
    /// 셸의 포커스 칸 세션이 속한 호스트.
    focused_host: HostId,
    /// 보지 않은 사이 완료·대기로 바뀐 세션(OWE-662).
    unread: BTreeSet<SessionId>,
    /// 에이전트 대시보드에서 사용자가 클릭해 확인한 완료(`Done | Exited`) 세션과 읽은 순서.
    read_done: BTreeMap<(HostId, SessionId), ReadDoneEntry>,
    /// 다음 완료 카드 읽음 처리에 부여할 단조 증가 순번.
    next_read_done_seq: u64,
    /// 워크트리 행 메뉴·단축키에서 띄울 에이전트 프로필 목록.
    agents: Vec<AgentProfile>,
    profiles: profiles::State,
    /// 사이드바 열이 펼쳐져 있는지.
    visible: bool,
    /// 창이 활성 상태인지.
    active: bool,
    /// 키보드 탐색 위치(행 인덱스).
    cursor: Option<usize>,
    /// 연결이 잠시 사라져도 동명 경로가 있는 다른 호스트로 커서를 옮기지 않는다.
    cursor_anchor: Option<(HostId, Row)>,
    focus: Option<FocusHandle>,
    /// Enter·Esc 뒤 돌려줄 포커스(셸).
    back: Option<FocusHandle>,
    #[cfg(test)]
    last_open_in: Option<(Target, PathBuf)>,
    #[cfg(test)]
    rendered_rows: usize,
    /// 행 스피너 자리. 회전은 셸이 [`Sidebar::spinner_overlay`]로 그려 틱마다 사이드바를 다시 렌더하지 않는다.
    spinner_slots: crate::ui::status::SpinnerSlots,
    #[cfg(test)]
    renders: usize,
}

impl EventEmitter<Selected> for Sidebar {}
impl EventEmitter<WorkspaceSelected> for Sidebar {}
impl EventEmitter<NewSession> for Sidebar {}
impl EventEmitter<OpenWorkspace> for Sidebar {}
impl EventEmitter<CloseSession> for Sidebar {}
impl EventEmitter<ContinueSession> for Sidebar {}
impl EventEmitter<SwitchSessionAccount> for Sidebar {}
impl EventEmitter<RenameSession> for Sidebar {}
impl EventEmitter<OpenFolder> for Sidebar {}
impl EventEmitter<ReconnectHost> for Sidebar {}
impl EventEmitter<Relisted> for Sidebar {}

impl std::fmt::Debug for Sidebar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sidebar")
            .field("rows", &self.rows.len())
            .field("remote_hosts", &self.remote_hosts.len())
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

/// 작업 폴더 없는 세션 묶음(`Row::Loose`와 그 세션 행, 항상 목록 끝)을 떼어 낸다.
fn split_loose(rows: &mut Vec<Row>) -> Vec<Row> {
    rows.iter()
        .position(|r| matches!(r, Row::Loose))
        .map(|i| rows.split_off(i))
        .unwrap_or_default()
}

/// 프로젝트 기준 보기에서 호스트별 행 목록을 프로젝트 단위로 합친다([`Sidebar::rebuild_by_project`]).
#[derive(Default)]
struct ProjectMerge {
    chunks: Vec<ProjectChunk>,
    /// 프로젝트 열쇠 → (묶음 번호, 머리줄 경로).
    owners: BTreeMap<(bool, String), (usize, PathBuf)>,
    loose: Vec<(HostId, Row)>,
}

/// 머리줄 하나와 그 아래 행들. 다른 PC에서 합쳐진 프로젝트 경로는 `aliases`에 남는다.
struct ProjectChunk {
    rows: Vec<(HostId, Row)>,
    aliases: Vec<(HostId, PathBuf)>,
}

/// PC 사이에는 공통 프로젝트 ID가 없어 종류(git 저장소/폴더)와 폴더 이름(대소문자 무시)으로 맞춘다.
fn project_key(row: &Row) -> Option<((bool, String), &PathBuf)> {
    match row {
        Row::Repository(p) => Some(((true, basename(p).to_lowercase()), p)),
        Row::Folder(p) => Some(((false, basename(p).to_lowercase()), p)),
        _ => None,
    }
}

/// Non-project groups retain their leading position; unordered projects follow
/// saved ones in their existing order.
fn project_rank(row: &Row, order: &[PathBuf]) -> usize {
    project_key(row).map_or(0, |(_, path)| {
        order
            .iter()
            .position(|p| p == path)
            .map_or(usize::MAX, |i| i + 1)
    })
}

/// Sort complete project blocks, keeping worktrees and sessions with their header.
fn order_project_rows(rows: &mut Vec<Row>, order: &[PathBuf]) {
    if order.is_empty() {
        return;
    }
    let Some(first) = rows.iter().position(|r| project_key(r).is_some()) else {
        return;
    };
    let mut chunks: Vec<Vec<Row>> = Vec::new();
    for row in rows.drain(first..) {
        if starts_chunk(&row) || chunks.is_empty() {
            chunks.push(Vec::new());
        }
        if let Some(chunk) = chunks.last_mut() {
            chunk.push(row);
        }
    }
    chunks.sort_by_key(|chunk| chunk.first().map_or(0, |r| project_rank(r, order)));
    rows.extend(chunks.into_iter().flatten());
}

impl ProjectMerge {
    /// 합쳐질 원격 머리줄은 화면에 없으므로 접힘 상태를 주인 머리줄 것으로 맞춘다.
    fn aligned_collapsed<'a>(
        &self,
        collapsed: &'a BTreeSet<PathBuf>,
        snapshot: &Snapshot,
    ) -> std::borrow::Cow<'a, BTreeSet<PathBuf>> {
        let mut out = std::borrow::Cow::Borrowed(collapsed);
        let repos = snapshot.trees.keys().map(|p| (true, p));
        let folders = snapshot
            .sessions
            .iter()
            .filter(|s| s.worktree.is_none())
            .filter_map(|s| s.cwd.as_ref())
            .map(|p| (false, p));
        for (repo, path) in repos.chain(folders) {
            let Some((_, owner)) = self.owners.get(&(repo, basename(path).to_lowercase())) else {
                continue;
            };
            let closed = collapsed.contains(owner);
            if closed != out.contains(path) {
                if closed {
                    out.to_mut().insert(path.clone());
                } else {
                    out.to_mut().remove(path);
                }
            }
        }
        out
    }

    /// 한 호스트의 행을 받아 이미 있는 프로젝트는 그 묶음 끝에 붙인다. 프로젝트가 있었으면 참.
    fn absorb(&mut self, host: &HostId, mut rows: Vec<Row>) -> bool {
        // `Row::Loose` 머리줄은 모든 호스트를 합쳐 한 번만 둔다.
        let loose = split_loose(&mut rows);
        self.loose
            .extend(loose.into_iter().skip(1).map(|r| (host.clone(), r)));
        let mut projects = false;
        let mut rows = rows.into_iter().peekable();
        while let Some(head) = rows.next() {
            let mut body = Vec::new();
            while let Some(row) = rows.next_if(|r| !starts_chunk(r)) {
                body.push((host.clone(), row));
            }
            let Some((key, path)) = project_key(&head) else {
                body.insert(0, (host.clone(), head));
                self.chunks.push(ProjectChunk {
                    rows: body,
                    aliases: Vec::new(),
                });
                continue;
            };
            projects = true;
            let owner = self.owners.get(&key).map(|(i, _)| *i);
            if let Some(chunk) = owner.and_then(|i| self.chunks.get_mut(i)) {
                chunk.aliases.push((host.clone(), path.clone()));
                chunk.rows.extend(body);
            } else {
                self.owners.insert(key, (self.chunks.len(), path.clone()));
                body.insert(0, (host.clone(), head));
                self.chunks.push(ProjectChunk {
                    rows: body,
                    aliases: Vec::new(),
                });
            }
        }
        projects
    }

    /// 프로젝트 → 작업 폴더 모르는 세션 순으로 편다.
    #[expect(
        clippy::type_complexity,
        reason = "rows, row hosts and merged headers are built together"
    )]
    fn finish(
        mut self,
        order: &[PathBuf],
    ) -> (
        Vec<Row>,
        Vec<HostId>,
        BTreeMap<usize, Vec<(HostId, PathBuf)>>,
    ) {
        // Sort whole projects after merging PCs so remote-only projects and folders
        // can move between local repositories without separating their children.
        if !order.is_empty() {
            self.chunks.sort_by_key(|chunk| {
                chunk
                    .rows
                    .first()
                    .map_or(0, |(_, row)| project_rank(row, order))
            });
        }
        let mut rows = Vec::new();
        let mut hosts = Vec::new();
        let mut merged = BTreeMap::new();
        let mut push = |(host, row): (HostId, Row)| {
            hosts.push(host);
            rows.push(row);
        };
        let mut start = 0;
        for chunk in self.chunks {
            if !chunk.aliases.is_empty() {
                merged.insert(start, chunk.aliases);
            }
            start += chunk.rows.len();
            chunk.rows.into_iter().for_each(&mut push);
        }
        if !self.loose.is_empty() {
            push((HostId::Local, Row::Loose));
            self.loose.into_iter().for_each(&mut push);
        }
        (rows, hosts, merged)
    }
}

/// 프로젝트 기준 보기에서 새 묶음을 여는 행.
fn starts_chunk(row: &Row) -> bool {
    matches!(
        row,
        Row::Repository(_)
            | Row::Folder(_)
            | Row::GroupHeader { .. }
            | Row::Loose
            | Row::HostHeader { .. }
            | Row::NeedsYou
    )
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

fn load_prompt_histories_from_sidebar_path(
    sidebar_path: Option<&Path>,
) -> BTreeMap<SessionId, krill_proto::git::SessionPromptHistory> {
    let Some(dir) = sidebar_path.and_then(Path::parent) else {
        return BTreeMap::new();
    };
    krill_proto::git::load_session_prompt_histories(dir)
}

impl Sidebar {
    pub fn new(path: Option<PathBuf>) -> Self {
        // Construction runs inside cx.new: all persisted state is loaded later on the executor.
        let (prefs, options, lin, ui) = (
            Preferences::default(),
            WorkspaceOptions::default(),
            lineage::LineageState::default(),
            UiExpansionState::default(),
        );
        Self {
            snapshot: Snapshot::default(),
            meta: Meta::new(),
            local_status: HostStatus::Connected,
            relink: Relink::Steady,
            remote_hosts: Vec::new(),
            remote_task: None,
            remote_manager: None,
            remote_info: BTreeMap::new(),
            collapsed_hosts: ui.collapsed_hosts,
            project_order: Vec::new(),
            worktree_order: Vec::new(),
            session_order: BTreeMap::new(),
            row_hosts: Vec::new(),
            action_host: None,
            prs: BTreeMap::new(),
            pr_guidance_dismissed: false,
            pr_task: None,
            pr_service: None,
            rows: Vec::new(),
            merged_projects: BTreeMap::new(),
            card_parents: Vec::new(),
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
            comments: BTreeMap::new(),
            parents: lin.parents,
            collapsed_lineage: lin.collapsed_lineage,
            lineage_depths: BTreeMap::new(),
            lineage_child_counts: BTreeMap::new(),
            sleeping_worktrees: lin.sleeping_worktrees,
            shown_external: ui.shown_external,
            collapsed: ui.collapsed,
            expanded_cards: ui.expanded_cards,
            manually_folded_cards: BTreeSet::new(),
            selected_worktree: ui.selected_worktree,
            removed_projects: ui.removed_projects,
            project_visuals: BTreeMap::new(),
            remote_visuals: BTreeMap::new(),
            remote_probed_repos: BTreeMap::new(),
            setup_prompt: hooks::SetupPrompt::default(),
            delete_confirm: None,
            delete_scope_focus: None,
            delete_cancel_focus: None,
            delete_previous_focus: None,
            delete_focus: DeleteFocus::Focused,
            path,
            error: None,
            poll_error: None,
            notice: None,
            task: None,
            source: None,
            card_list: viewport::CardList::default(),
            status_summary: (0, Arc::new(Vec::new())),
            local_rows_cache: std::cell::RefCell::default(),
            editing: None,
            edit_subscription: None,
            orca_import_subscription: None,
            creating: None,
            creating_background: Vec::new(),
            creating_dialog_id: 0,
            next_creating_id: 1,
            pending_worktrees: Vec::new(),
            recently_created_worktrees: BTreeSet::new(),
            pending_reveal_worktree: None,
            pending_prompts: BTreeMap::new(),
            custom_titles: BTreeMap::new(),
            titles: BTreeMap::new(),
            auto_titles: BTreeMap::new(),
            prompt_histories: BTreeMap::new(),
            remote_prompt_histories: BTreeMap::new(),
            last_activity_minute: None,
            now_override_ns: None,
            focused: None,
            focused_synced: None,
            focused_host: HostId::Local,
            unread: BTreeSet::new(),
            read_done: BTreeMap::new(),
            next_read_done_seq: 0,
            agents: Vec::new(),
            profiles: profiles::State::default(),
            disk: async_state::State::default(),
            visible: true,
            active: true,
            cursor: None,
            cursor_anchor: None,
            hovered_worktree: None,
            focus: None,
            back: None,
            #[cfg(test)]
            last_open_in: None,
            #[cfg(test)]
            rendered_rows: 0,
            spinner_slots: crate::ui::status::SpinnerSlots::default(),
            #[cfg(test)]
            renders: 0,
        }
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
    fn auto_collapse_idle_heavy_repos(&mut self, imported: &BTreeSet<String>) -> bool {
        let occupied: BTreeSet<&Path> = self
            .snapshot
            .sessions
            .iter()
            .filter_map(|s| s.worktree.as_deref())
            .collect();
        let mut changed = false;
        let repos: Vec<PathBuf> = self.snapshot.trees.keys().cloned().collect();
        for repo in repos {
            if self.collapsed.contains(&repo) || self.prefs.contains_key(&repo) {
                continue;
            }
            let Some(trees) = self.snapshot.trees.get(&repo) else {
                continue;
            };
            let has_session = trees.iter().any(|t| occupied.contains(t.as_path()));
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

    #[cfg(all(test, unix))]
    fn sync_imported_projects_from_disk(&mut self) -> (bool, bool) {
        let data = async_state::DiskSnapshot::load(
            self.path.as_deref(),
            self.snapshot.clone(),
            self.meta.clone(),
        );
        let auto = self.apply_disk_snapshot(data);
        (true, auto)
    }

    /// OWE-898: 라이브 워크트리 집합에 없는 파생 상태(상태·갱신시각·이슈·포트·호스트 라벨·자동/CLI 표시)를
    /// 정리한다. 디스크 저장소(`WorktreeStore`)가 삭제된 워크트리 항목을 계속 내놓아도 메모리·디스크
    /// 재삽입으로 누적되지 않게 한다. 사용자 접힘·핀·부모·이름 설정은 건드리지 않는다.
    fn prune_stale_worktree_state(&mut self) {
        use std::collections::BTreeSet;
        let mut live: BTreeSet<PathBuf> = BTreeSet::new();
        for wts in self.snapshot.trees.values() {
            for wt in wts {
                live.insert(krill_proto::path::identity(wt));
            }
        }
        for s in &self.snapshot.sessions {
            if let Some(wt) = s.worktree.as_deref() {
                live.insert(krill_proto::path::identity(wt));
            }
        }
        self.worktree_statuses
            .retain(|path, _| live.contains(&krill_proto::path::identity(path)));
        self.worktree_updated_at
            .retain(|path, _| live.contains(&krill_proto::path::identity(path)));
        self.issues
            .retain(|path, _| live.contains(&krill_proto::path::identity(path)));
        self.ports
            .retain(|path, _| live.contains(&krill_proto::path::identity(path)));
        self.worktree_hosts
            .retain(|path, _| live.contains(&krill_proto::path::identity(path)));
        self.automation_worktrees
            .retain(|path| live.contains(&krill_proto::path::identity(path)));
        self.cli_worktrees
            .retain(|path| live.contains(&krill_proto::path::identity(path)));
        self.recently_created_worktrees
            .retain(|path| live.contains(&krill_proto::path::identity(path)));
        self.manually_folded_cards
            .retain(|path| live.contains(&krill_proto::path::identity(path)));
    }

    /// 외부 파일(`projects.toml`, `worktree-comments.toml`) 변경을 즉시 반영해 사이드바 트리를 다시 그린다.
    pub fn reload_external(&mut self, cx: &mut Context<Self>) {
        self.disk.generation = self.disk.generation.wrapping_add(1);
        self.disk.task = None;
        self.ensure_external_polling(cx);
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

    /// 열린 프로젝트(저장소·폴더)나 세션이 하나라도 있는지.
    #[must_use]
    pub fn has_projects(&self) -> bool {
        !self.snapshot.trees.is_empty()
            || !self.snapshot.sessions.is_empty()
            || self.remote_hosts.iter().any(|r| {
                r.status == HostStatus::Connected
                    && (!r.snapshot.trees.is_empty() || !r.snapshot.sessions.is_empty())
            })
    }

    pub(super) fn has_connected_remote_hosts(&self) -> bool {
        self.remote_hosts
            .iter()
            .any(|r| r.status == HostStatus::Connected)
    }

    /// 마지막으로 선택·활성화된 워크트리/폴더 경로.
    #[must_use]
    pub fn selected_worktree(&self) -> Option<&Path> {
        self.selected_worktree.as_deref()
    }

    /// Orca `activeWorktreeId` 패리티: 마지막으로 선택된 워크스페이스(호스트 + 경로).
    /// Cmd+T·새 에이전트는 실행 위치 메뉴가 아니라 여기서 호스트·작업 디렉터리를 가져온다.
    /// 원격은 연결된 호스트만 돌려준다(끊긴 호스트면 예전처럼 실행 위치 메뉴로 폴백한다).
    #[must_use]
    pub fn selected_workspace(&self) -> Option<(HostId, PathBuf)> {
        let path = self.selected_worktree.clone()?;
        // 커서가 선택 경로를 가리키면 그 행의 호스트가 우선(로컬·원격 동명 경로 구분).
        if let Some(i) = self.cursor
            && self.cursor_folder().as_ref() == Some(&path)
        {
            let host = self.row_host(i).clone();
            if self.host_connected(&host) {
                return Some((host, path));
            }
            return None;
        }
        if let Some((host, _)) = &self.cursor_anchor
            && !self.host_connected(host)
        {
            return None;
        }
        // 커서 밖 선택(복원된 상태 등)은 스냅샷에서 소유 호스트를 찾는다.
        // 여러 호스트에 동명 경로가 있으면 판단을 미루고 None(실행 위치 메뉴 폴백).
        let mut found: Option<HostId> = None;
        for remote in &self.remote_hosts {
            if remote.status == HostStatus::Connected
                && Self::snapshot_contains(&remote.snapshot, &path)
            {
                if found.is_some() {
                    return None;
                }
                found = Some(HostId::Remote(remote.id.clone()));
            }
        }
        if let Some(host) = found {
            if Self::snapshot_contains(&self.snapshot, &path) {
                return None;
            }
            return Some((host, path));
        }
        if Self::snapshot_contains(&self.snapshot, &path) {
            return Some((HostId::Local, path));
        }
        None
    }

    #[must_use]
    pub fn host_connected(&self, host: &HostId) -> bool {
        match host {
            HostId::Local => self.local_status == HostStatus::Connected,
            HostId::Remote(id) => self
                .remote_hosts
                .iter()
                .any(|r| &r.id == id && r.status == HostStatus::Connected),
        }
    }

    #[must_use]
    pub fn host_has_trees(&self, host: &HostId) -> bool {
        !self.snapshot_for(host).trees.is_empty()
    }

    /// 현재 연결된 모든 호스트의 세션 목록을 `(HostId, SessionId)`로 순회한다.
    pub fn connected_sessions(&self) -> impl Iterator<Item = (HostId, SessionId)> + '_ {
        let local = (self.local_status == HostStatus::Connected)
            .then(|| self.snapshot.sessions.iter().map(|s| (HostId::Local, s.id)))
            .into_iter()
            .flatten();
        let remote = self
            .remote_hosts
            .iter()
            .filter(|r| r.status == HostStatus::Connected)
            .flat_map(|r| {
                let host = HostId::Remote(r.id.clone());
                r.snapshot
                    .sessions
                    .iter()
                    .map(move |s| (host.clone(), s.id))
            });
        local.chain(remote)
    }

    /// 테스트에서 원격 호스트 스냅샷·상태를 직접 주입한다.
    #[doc(hidden)]
    pub fn set_remote_snapshot_for_test(
        &mut self,
        id: &str,
        status: HostStatus,
        snapshot: Snapshot,
        cx: &mut Context<Self>,
    ) {
        if let Some(remote) = self.remote_hosts.iter_mut().find(|r| r.id == id) {
            remote.status = status;
            remote.snapshot = snapshot;
        } else {
            self.remote_hosts.push(RemoteHost {
                id: id.to_owned(),
                label: id.to_owned(),
                status,
                snapshot,
                meta: Meta::new(),
                source: None,
                error: None,
            });
        }
        self.prune_read_done_for_host(&HostId::Remote(id.to_owned()), cx);
        self.rebuild(cx);
    }

    fn snapshot_contains(snapshot: &Snapshot, path: &Path) -> bool {
        snapshot
            .trees
            .keys()
            .chain(snapshot.trees.values().flatten())
            .any(|root| root == path)
            || snapshot.sessions.iter().any(|s| {
                s.worktree.as_ref() == Some(&path.to_path_buf())
                    || s.cwd.as_ref() == Some(&path.to_path_buf())
            })
    }

    /// 선택된 워크트리/폴더 경로를 갱신하고 영속화한다(OWE-842).
    pub fn select_worktree(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.touch("selected");
        }
        if self.selected_worktree != path || !self.disk.initialized {
            self.cursor_anchor = None;
            self.selected_worktree = path;
            self.persist(cx);
        }
    }

    /// 호스트 섹션 접기/펼치기(OWE-845: `UiExpansionState`에 저장해 재시작 후에도 유지).
    pub fn fold_host(&mut self, host: &HostId, open: Option<bool>, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.ui.collapsed_hosts.insert(host.clone());
        }
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
        self.disk.remote_generation = self.disk.remote_generation.wrapping_add(1);
        if self.disk.remote_refresh.pending {
            self.disk.remote_refresh.again = true;
            return;
        }
        self.disk.remote_refresh.pending = true;
        #[cfg(all(test, unix))]
        {
            self.disk.remote_starts += 1;
        }
        #[cfg(all(test, unix))]
        let gate = self.disk.next_remote_io.take();
        let generation = self.disk.remote_generation;
        let manager = manager.clone();
        cx.spawn(async move |this, cx| {
            let (statuses, info, mut sources) = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(all(test, unix))]
                    if let Some(gate) = gate {
                        gate.await.ok();
                    }
                    let statuses = manager.statuses();
                    let info = statuses
                        .iter()
                        .filter_map(|(spec, _)| {
                            manager
                                .connection_info(&spec.id)
                                .map(|info| (spec.id.clone(), info))
                        })
                        .collect();
                    let sources: BTreeMap<_, _> = statuses
                        .iter()
                        .filter_map(|(spec, _)| {
                            manager
                                .target_for(&HostId::Remote(spec.id.clone()), None)
                                .map(|target| (spec.id.clone(), status_source(target)))
                        })
                        .collect();
                    (statuses, info, sources)
                })
                .await;
            this.update(cx, |s, cx| {
                s.disk.remote_refresh.pending = false;
                if s.disk.remote_generation != generation {
                    if std::mem::take(&mut s.disk.remote_refresh.again)
                        && let Some(manager) = s.remote_manager.clone()
                    {
                        s.sync_remote_hosts(&manager, cx);
                    }
                    return;
                }
                s.disk.remote_refresh.again = false;
                let mut changed = s.remote_info != info || s.remote_hosts.len() != statuses.len();
                s.remote_info = info;
                let previous = std::mem::take(&mut s.remote_hosts);
                let mut new_source = false;
                s.remote_hosts = statuses
                    .into_iter()
                    .map(|(spec, status)| {
                        let mut entry = previous
                            .iter()
                            .find(|r| r.id == spec.id)
                            .cloned()
                            .unwrap_or_else(|| {
                                changed = true;
                                RemoteHost {
                                    error: None,
                                    id: spec.id.clone(),
                                    label: spec.display_name().to_owned(),
                                    status,
                                    snapshot: Snapshot::default(),
                                    meta: Meta::new(),
                                    source: None,
                                }
                            });
                        let label = spec.display_name().to_owned();
                        changed |= entry.label != label || entry.status != status;
                        entry.label = label;
                        entry.status = status;
                        let next = sources.remove(&spec.id);
                        let replaced = entry.source.as_ref().is_some_and(|old| {
                            old.target()
                                .zip(next.as_ref().and_then(|s| s.target()))
                                .is_some_and(|(old, next)| old != next)
                        });
                        if (entry.source.is_none() || replaced) && s.visible {
                            entry.source = next;
                            new_source |= entry.source.is_some();
                        }
                        entry
                    })
                    .collect();
                if changed {
                    s.rebuild(cx);
                }
                if new_source {
                    s.remote_task = None;
                }
                s.start_remote_polling(cx);
            })
            .ok();
        })
        .detach();
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
                error: None,
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

    fn ensure_remote_sources(&mut self, cx: &mut Context<Self>) {
        if let Some(manager) = self.remote_manager.clone() {
            self.sync_remote_hosts(&manager, cx);
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
            self.ensure_remote_sources(cx);
        }
        // A manager may still be resolving sources on a worker after unhide.
        // Do not retain an already-finished empty poll task that would prevent
        // the manager completion from starting polling once sources arrive.
        if self.remote_hosts.iter().all(|host| host.source.is_none()) {
            return;
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
                let mut tasks = Vec::with_capacity(sources.len());
                for (id, source) in sources {
                    let bg = bg.clone();
                    let this = this.clone();
                    tasks.push(cx.spawn(async move |cx| {
                        let (id, source, res, histories) = bg
                            .spawn(async move {
                                let res = source.poll_snapshot().await;
                                let histories = source.prompt_histories();
                                (id, source, res, histories)
                            })
                            .await;
                        this.update(cx, |s, cx| {
                            if (!s.is_visible() && !forced) || s.remote_hosts.is_empty() {
                                return;
                            }
                            let Some(entry) = s.remote_hosts.iter_mut().find(|r| r.id == id) else {
                                return;
                            };
                            if !entry
                                .source
                                .as_ref()
                                .is_some_and(|current| Arc::ptr_eq(current, &source))
                            {
                                return;
                            }
                            if source.target().is_some_and(|target| !target.is_current()) {
                                return;
                            }
                            let mut changed = false;
                            let mut probe_repos = Vec::new();
                            if let Ok((snap, mut meta)) = res {
                                for path in snap.trees.values().flatten() {
                                    if let Some(prev_branch) =
                                        entry.meta.get(path).and_then(|m| m.branch.clone())
                                    {
                                        let tree_meta = meta.entry(path.clone()).or_default();
                                        if tree_meta.branch.is_none() {
                                            tree_meta.branch = Some(prev_branch);
                                        }
                                    }
                                }
                                if entry.status != HostStatus::Connected
                                    || entry.snapshot != snap
                                    || entry.meta != meta
                                    || s.remote_prompt_histories.get(&id) != Some(&histories)
                                {
                                    entry.status = HostStatus::Connected;
                                    entry.snapshot = snap;
                                    entry.meta = meta;
                                    s.remote_prompt_histories.insert(id.clone(), histories);
                                    changed = true;
                                }
                                let repo_keys: Vec<PathBuf> =
                                    entry.snapshot.trees.keys().cloned().collect();
                                let probed = s.remote_probed_repos.entry(id.clone()).or_default();
                                for repo in repo_keys {
                                    if probed.insert(repo.clone()) {
                                        probe_repos.push(repo);
                                    }
                                }
                                let host_id = HostId::Remote(id.clone());
                                s.prune_read_done_for_host(&host_id, cx);
                            } else {
                                s.remote_probed_repos.remove(&id);
                                let status = s
                                    .remote_info
                                    .get(&id)
                                    .map_or(HostStatus::Disconnected, |t| t.status);
                                if entry.status != status {
                                    entry.status = status;
                                    changed = true;
                                }
                            }
                            if !probe_repos.is_empty()
                                && let Some(target) = source.target().cloned()
                            {
                                let state_dir = s.state_dir().map(Path::to_path_buf);
                                let bg_probe = cx.background_executor().clone();
                                cx.spawn(async move |this, cx| {
                                    let resolved = bg_probe
                                        .spawn(async move {
                                            external::probe_remote_project_visuals(
                                                &target,
                                                state_dir.as_deref(),
                                                &probe_repos,
                                            )
                                        })
                                        .await;
                                    if !resolved.is_empty() {
                                        this.update(cx, |s, cx| {
                                            s.remote_visuals.extend(resolved);
                                            cx.notify();
                                        })
                                        .ok();
                                    }
                                })
                                .detach();
                            }
                            if changed {
                                s.rebuild(cx);
                            }
                        })
                        .ok();
                    }));
                }
                for t in tasks {
                    t.await;
                }
                let next_delay = this.update(cx, |s, _| {
                    ((s.is_visible() || forced) && !s.remote_hosts.is_empty())
                        .then(|| s.is_visible().then(|| s.poll_interval()))
                        .flatten()
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
                self.ensure_remote_sources(cx);
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
        self.snapshot_for(host)
            .sessions
            .iter()
            .find(|s| s.id == id)
            .or_else(|| self.profiles.fallback(host, id))
    }

    /// 화면 스트림의 CLOSED를 폴링보다 먼저 반영해 종료 알림·행을 정리한다.
    pub fn mark_session_exited_on(&mut self, host: &HostId, id: SessionId, cx: &mut Context<Self>) {
        self.profiles.remove_fallback(host, id);
        let snapshot = match host {
            HostId::Local => Some(&mut self.snapshot),
            HostId::Remote(host) => self
                .remote_hosts
                .iter_mut()
                .find(|remote| remote.id == *host)
                .map(|remote| &mut remote.snapshot),
        };
        if let Some(session) = snapshot.and_then(|snapshot| {
            snapshot
                .sessions
                .iter_mut()
                .find(|session| session.id == id)
        }) {
            session.status.state = AgentState::Exited;
            session.status.source = Some(krill_proto::control::Source::Process);
            session.status.foreground = None;
        }
        if host.is_local() {
            self.unread.remove(&id);
            crate::alert::dismiss(id, cx);
        }
        self.rebuild(cx);
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

    pub(super) fn source_for(&self, host: &HostId) -> Option<Arc<dyn Source>> {
        match host {
            HostId::Local => self.source.clone(),
            HostId::Remote(id) => self
                .remote_hosts
                .iter()
                .find(|r| r.id == *id)
                .and_then(|r| r.source.clone()),
        }
    }

    #[must_use]
    pub fn can_create_worktree_on(&self, host: &HostId) -> bool {
        match host {
            HostId::Local => self.source.is_some(),
            HostId::Remote(id) => self
                .remote_hosts
                .iter()
                .any(|r| r.id == *id && r.status == HostStatus::Connected && r.source.is_some()),
        }
    }

    /// 사이드바 상단 '에이전트 대시보드' 행에 표시할 `(확인 필요 수, 작업 중 수, 완료 수)`.
    /// 대시보드 모달(`dashboard_sessions`)과 동일한 호스트 필터 및 상태 분류 기준을 따른다.
    #[must_use]
    pub fn agent_counts(&self) -> (usize, usize, usize) {
        let mut waiting = 0usize;
        let mut working = 0usize;
        let mut done = 0usize;
        for (_, s) in self.dashboard_sessions() {
            match s.status.state {
                AgentState::Waiting | AgentState::Blocked => waiting += 1,
                AgentState::Working => working += 1,
                AgentState::Done => done += 1,
                AgentState::Exited | AgentState::Unknown => {}
            }
        }
        (waiting, working, done)
    }

    /// OWE-662: 보지 않은 사이 완료·대기로 바뀐 세션인지.
    #[must_use]
    pub fn is_unread(&self, id: SessionId) -> bool {
        self.unread.contains(&id)
    }

    /// 에이전트 대시보드에서 완료(`Done | Exited`) 카드가 이미 읽음 처리된 상태면 읽은 순번(`Some(seq)`)을 돌려준다.
    /// 아직 확인하지 않은 완료 카드이거나 새로 완료된 카드면 `None`이다.
    #[must_use]
    pub fn dashboard_read_order(&self, host: &HostId, session: &Session) -> Option<u64> {
        if !matches!(session.status.state, AgentState::Done | AgentState::Exited) {
            return None;
        }
        if host.is_local() && self.unread.contains(&session.id) {
            return None;
        }
        let entry = self.read_done.get(&(host.clone(), session.id))?;
        (entry.since_unix_ns == session.status.since_unix_ns || session.status.since_unix_ns == 0)
            .then_some(entry.seq)
    }

    /// 에이전트 대시보드 기준 미확인(안 읽음) 카드인지.
    #[must_use]
    pub fn is_dashboard_unread(&self, host: &HostId, session: &Session) -> bool {
        if matches!(session.status.state, AgentState::Done | AgentState::Exited) {
            self.dashboard_read_order(host, session).is_none()
        } else {
            host.is_local() && self.is_unread(session.id)
        }
    }

    /// 세션을 사용자가 열어 확인했을 때 미확인 표시를 끄고, 완료(`Done | Exited`) 세션이면
    /// 대시보드 읽음 순서(`read_done`)에 기록한다.
    pub fn mark_session_read(
        &mut self,
        host: &HostId,
        id: SessionId,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut changed = host.is_local() && self.unread.remove(&id);
        if let Some(session) = self.session_on(host, id)
            && matches!(session.status.state, AgentState::Done | AgentState::Exited)
        {
            let since_unix_ns = session.status.since_unix_ns;
            let key = (host.clone(), id);
            let already_read = self
                .read_done
                .get(&key)
                .is_some_and(|entry| entry.since_unix_ns == since_unix_ns || since_unix_ns == 0);
            if !already_read {
                self.next_read_done_seq = self.next_read_done_seq.saturating_add(1);
                let seq = self.next_read_done_seq;
                self.read_done
                    .insert(key, ReadDoneEntry { since_unix_ns, seq });
                self.write_read_done(cx);
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
        changed
    }

    pub(super) fn write_read_done(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.state_dir().map(Path::to_path_buf) else {
            return;
        };
        let snapshot = self.read_done.clone();
        self.enqueue_disk_write(move || save_read_done(&dir, &snapshot), cx);
    }

    pub(super) fn prune_read_done_for_host(&mut self, host: &HostId, cx: &mut Context<Self>) {
        let before_len = self.read_done.len();
        let sessions: &[Session] = match host {
            HostId::Local => &self.snapshot.sessions,
            HostId::Remote(id) => self
                .remote_hosts
                .iter()
                .find(|r| r.id == *id)
                .map_or(&[], |r| &r.snapshot.sessions),
        };
        let is_local = host.is_local();
        let unread = &self.unread;
        self.read_done.retain(|(h, id), entry| {
            if h != host {
                return true;
            }
            if is_local && unread.contains(id) {
                return false;
            }
            let Some(session) = sessions.iter().find(|s| s.id == *id) else {
                return false;
            };
            matches!(session.status.state, AgentState::Done | AgentState::Exited)
                && (entry.since_unix_ns == session.status.since_unix_ns
                    || session.status.since_unix_ns == 0)
        });
        if self.read_done.len() != before_len {
            self.write_read_done(cx);
        }
    }

    pub(crate) fn mark_workspace_read(
        &mut self,
        host: &HostId,
        path: &Path,
        cx: &mut Context<Self>,
    ) {
        if !host.is_local() {
            return;
        }
        let ids: Vec<_> = self
            .snapshot_for(host)
            .sessions
            .iter()
            .filter(|s| s.worktree.as_deref().or(s.cwd.as_deref()) == Some(path))
            .map(|s| s.id)
            .collect();
        for id in ids {
            self.mark_session_read(host, id, cx);
        }
        self.rebuild(cx);
    }

    /// OWE-662: 셸이 포커스 칸의 세션을 알린다. 그 세션의 미확인 표시를 끈다.
    pub fn set_focused(&mut self, session: Option<SessionId>, cx: &mut Context<Self>) {
        self.set_focused_on(HostId::Local, session, cx);
    }

    /// 셸이 포커스 칸의 호스트·세션을 알린다. 접혀 있는 저장소·워크트리를 펼치고 해당 세션 행을 선택·노출한다.
    pub fn set_focused_on(
        &mut self,
        host: HostId,
        session: Option<SessionId>,
        cx: &mut Context<Self>,
    ) {
        let next_host = if session.is_some() {
            host
        } else {
            HostId::Local
        };
        let changed =
            self.focused != session || (session.is_some() && self.focused_host != next_host);
        self.focused = session;
        self.focused_host = next_host;
        if !self.focused_host.is_local() {
            self.focused_synced = None;
            if changed {
                self.reveal_focused(cx);
            }
            return;
        }
        let cleared = session.is_some_and(|id| self.unread.remove(&id));
        if let Some(id) = session {
            if changed || self.focused_synced != Some(id) {
                if let Some(wt) = self.folder(id).map(Path::to_path_buf)
                    && self.selected_worktree.as_ref() != Some(&wt)
                {
                    self.selected_worktree = Some(wt);
                    self.write_state(cx);
                }
                self.sync_focused_row(id, changed, cx);
                if changed {
                    self.reveal_focused(cx);
                }
            }
        } else {
            self.focused_synced = None;
            if changed && self.options.filters.hide_sleeping {
                self.rebuild(cx);
            }
        }
        if changed || cleared {
            cx.notify();
        }
    }

    /// 포커스 세션(`self.focused`)이 로컬 스냅샷에 존재할 때 워크트리 선택·펼침·커서·스크롤을 맞춘다.
    fn sync_focused_row(&mut self, id: SessionId, focus_changed: bool, cx: &mut Context<Self>) {
        let Some(path) = self.folder_on(&HostId::Local, id).map(Path::to_path_buf) else {
            self.focused_synced = None;
            if focus_changed && self.options.filters.hide_sleeping {
                self.rebuild(cx);
            }
            return;
        };
        self.focused_synced = Some(id);
        let mut need_rebuild = focus_changed && self.options.filters.hide_sleeping;
        let mut need_persist = false;
        let reveal_created = self.pending_reveal_worktree.as_deref() == Some(path.as_path());
        if reveal_created {
            self.pending_reveal_worktree = None;
            if self.collapsed.remove(&path) {
                need_persist = true;
            }
            if self.expand_lineage_ancestors(&path) {
                need_persist = true;
            }
            let is_claude_wt = external::is_claude_worktree(&path);
            let parent_repos: Vec<PathBuf> = self
                .snapshot
                .trees
                .iter()
                .filter(|&(repo, trees)| repo == &path || trees.iter().any(|t| t == &path))
                .map(|(repo, _)| repo.clone())
                .collect();
            for repo in parent_repos {
                if self.collapsed.remove(&repo) {
                    need_persist = true;
                }
                if is_claude_wt && self.shown_external.insert(repo) {
                    need_persist = true;
                }
            }
        }
        let is_worktree = self
            .snapshot
            .sessions
            .iter()
            .any(|s| s.id == id && s.worktree.as_deref() == Some(path.as_path()));
        if is_worktree {
            let session_count = self
                .snapshot
                .sessions
                .iter()
                .filter(|s| s.worktree.as_deref() == Some(path.as_path()))
                .count();
            if session_count >= 2
                && !self.expanded_cards.contains(&path)
                && !self.manually_folded_cards.contains(&path)
            {
                self.expanded_cards.insert(path.clone());
                self.collapsed.remove(&path);
                need_persist = true;
            }
        }
        let selected_changed = self.selected_worktree.as_ref() != Some(&path);
        if selected_changed {
            self.selected_worktree = Some(path.clone());
        }
        if self.options.filters.hide_sleeping
            && !self.rows.iter().enumerate().any(|(i, row)| {
                self.row_host(i).is_local()
                    && matches!(row, Row::Worktree(p) | Row::Folder(p) if p == &path)
            })
        {
            need_rebuild = true;
        }
        if need_persist {
            self.persist(cx);
        } else {
            if selected_changed {
                self.write_state(cx);
            }
            if need_rebuild {
                self.rebuild(cx);
            }
        }
        let prefer_session_row = !is_worktree || self.expanded_cards.contains(&path);
        let session_idx = prefer_session_row
            .then(|| {
                self.rows.iter().enumerate().position(|(i, row)| {
                    self.row_host(i).is_local() && matches!(row, Row::Session(s, _) if s.id == id)
                })
            })
            .flatten();
        self.cursor = session_idx.or_else(|| {
            self.rows.iter().enumerate().position(|(i, row)| {
                self.row_host(i).is_local()
                    && matches!(row, Row::Worktree(p) | Row::Folder(p) if p == &path)
            })
        });
        self.sync_viewport();
        if let Some(cursor) = self.cursor
            && reveal_created
        {
            self.scroll_row_into_view(cursor, ScrollStrategy::Nearest);
        }
    }

    /// The toolbar bell opens the next unread completion and reveals its project.
    fn open_next_unread(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self
            .snapshot
            .sessions
            .iter()
            .find(|s| self.unread.contains(&s.id) || needs_you(&s.status))
            .map(|s| s.id)
        else {
            return;
        };
        if let Some(path) = self.folder_on(&HostId::Local, id).map(Path::to_path_buf) {
            self.collapsed.remove(&path);
            self.expand_lineage_ancestors(&path);
            for (repo, trees) in &self.snapshot.trees {
                if trees.contains(&path) {
                    self.collapsed.remove(repo);
                    if external::is_claude_worktree(&path) {
                        self.shown_external.insert(repo.clone());
                    }
                }
            }
            self.persist(cx);
        }
        let index = self.rows.iter().enumerate().position(|(i, r)| {
            self.row_host(i).is_local() && matches!(r, Row::Session(s, _) if s.id == id)
        });
        if let Some(index) = index {
            self.select_session_on(index, HostId::Local, id, cx);
            self.scroll_row_into_view(index, ScrollStrategy::Nearest);
        }
    }

    fn focused_row_index(
        &self,
        host: &HostId,
        focused_id: SessionId,
        target_wt: Option<&Path>,
    ) -> Option<usize> {
        self.rows
            .iter()
            .enumerate()
            .position(|(i, r)| {
                self.row_host(i) == host && matches!(r, Row::Session(s, true) if s.id == focused_id)
            })
            .or_else(|| {
                self.rows.iter().enumerate().position(|(i, r)| {
                    self.row_host(i) == host
                        && matches!(r, Row::Session(s, _) if s.id == focused_id)
                })
            })
            .or_else(|| {
                let wt = target_wt?;
                self.rows.iter().enumerate().position(|(i, r)| {
                    self.row_host(i) == host
                        && matches!(r, Row::Worktree(p) | Row::Folder(p) if p == wt)
                })
            })
    }

    /// 현재 포커스된 세션(또는 그 워크트리) 행으로 스크롤·선택한다.
    pub fn reveal_focused(&mut self, cx: &mut Context<Self>) {
        let Some(focused_id) = self.focused else {
            return;
        };
        let focused_host = self.focused_host.clone();
        let target_wt = self
            .folder_on(&focused_host, focused_id)
            .map(Path::to_path_buf);
        let mut uncollapsed = self.collapsed_hosts.remove(&focused_host);
        if let Some(wt) = &target_wt {
            if self.collapsed.remove(wt) {
                uncollapsed = true;
            }
            if focused_host.is_local() {
                if self.expand_lineage_ancestors(wt) {
                    uncollapsed = true;
                }
                if self.prefs.get(wt).is_some_and(|p| p.pinned)
                    && self.collapsed_groups.remove(&GroupKey::Pinned)
                {
                    uncollapsed = true;
                }
            }
            let session_count = self
                .snapshot_for(&focused_host)
                .sessions
                .iter()
                .filter(|s| s.worktree.as_deref() == Some(wt.as_path()))
                .count();
            if session_count > 0 {
                self.manually_folded_cards.remove(wt);
                if self.expanded_cards.insert(wt.clone()) {
                    uncollapsed = true;
                }
            }
            let is_claude_wt = external::is_claude_worktree(wt);
            let parent_repos: Vec<PathBuf> = self
                .snapshot_for(&focused_host)
                .trees
                .iter()
                .filter(|&(repo, trees)| repo == wt || trees.iter().any(|t| t == wt))
                .map(|(repo, _)| repo.clone())
                .collect();
            for repo in &parent_repos {
                if self.collapsed.remove(repo) {
                    uncollapsed = true;
                }
                if is_claude_wt && self.shown_external.insert(repo.clone()) {
                    uncollapsed = true;
                }
            }
            if !focused_host.is_local() && self.options.group_by == GroupBy::Repo {
                let candidate_projects: Vec<&Path> = if parent_repos.is_empty() {
                    vec![wt.as_path()]
                } else {
                    parent_repos.iter().map(PathBuf::as_path).collect()
                };
                for project_path in candidate_projects {
                    let key = basename(project_path).to_ascii_lowercase();
                    let matching_collapsed: Vec<PathBuf> = self
                        .collapsed
                        .iter()
                        .filter(|p| basename(p).to_ascii_lowercase() == key)
                        .cloned()
                        .collect();
                    for path in matching_collapsed {
                        if self.collapsed.remove(&path) {
                            uncollapsed = true;
                        }
                    }
                }
            }
            if !self.collapsed_groups.is_empty() {
                let key = self.group_for_path(&focused_host, wt);
                if key.is_some_and(|key| self.collapsed_groups.remove(&key)) {
                    uncollapsed = true;
                }
            }
            if !focused_host.is_local() && self.options.group_by == GroupBy::Repo {
                let candidate_projects: Vec<&Path> = if parent_repos.is_empty() {
                    vec![wt.as_path()]
                } else {
                    parent_repos.iter().map(PathBuf::as_path).collect()
                };
                for project_path in candidate_projects {
                    let key = basename(project_path).to_ascii_lowercase();
                    let matching_collapsed: Vec<PathBuf> = self
                        .collapsed
                        .iter()
                        .filter(|p| basename(p).to_ascii_lowercase() == key)
                        .cloned()
                        .collect();
                    for path in matching_collapsed {
                        if self.collapsed.remove(&path) {
                            uncollapsed = true;
                        }
                    }
                }
            }
            if !self.collapsed_groups.is_empty() {
                match self.options.group_by {
                    GroupBy::WorkspaceStatus => {
                        let status = if focused_host.is_local() {
                            self.worktree_statuses
                                .get(wt)
                                .copied()
                                .or_else(|| {
                                    self.disk
                                        .store
                                        .statuses
                                        .get(wt.to_string_lossy().as_ref())
                                        .and_then(|v| {
                                            krill_proto::orchestration::WorkspaceStatus::parse(v)
                                                .ok()
                                        })
                                })
                                .unwrap_or(krill_proto::orchestration::WorkspaceStatus::Todo)
                        } else {
                            krill_proto::orchestration::WorkspaceStatus::Todo
                        };
                        if self
                            .collapsed_groups
                            .remove(&GroupKey::WorkspaceStatus(status))
                        {
                            uncollapsed = true;
                        }
                    }
                    GroupBy::PrStatus => {
                        let pr_group = if focused_host.is_local() {
                            let has_cp = self
                                .disk
                                .store
                                .prs
                                .get(wt.to_string_lossy().as_ref())
                                .is_some_and(|&pr| pr > 0);
                            options::PrGroup::from_status(self.prs.get(wt), has_cp)
                        } else {
                            options::PrGroup::from_status(None, false)
                        };
                        if self.collapsed_groups.remove(&GroupKey::PrStatus(pr_group)) {
                            uncollapsed = true;
                        }
                    }
                    GroupBy::Repo | GroupBy::None => {}
                }
            }
        }
        let selected_changed = if target_wt.is_some() && self.selected_worktree != target_wt {
            self.selected_worktree.clone_from(&target_wt);
            true
        } else {
            false
        };
        if uncollapsed {
            self.persist(cx);
        } else {
            if selected_changed {
                self.write_state(cx);
            }
            if target_wt.is_some()
                && self
                    .focused_row_index(&focused_host, focused_id, target_wt.as_deref())
                    .is_none()
            {
                self.rebuild(cx);
            }
        }
        if let Some(i) = self.focused_row_index(&focused_host, focused_id, target_wt.as_deref()) {
            self.cursor = Some(i);
            self.sync_viewport();
            self.scroll_row_into_view(i, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    /// 워크트리 카드의 3행 에이전트 알약 바 클릭 시 하위 세션 목록을 펼치거나 접는다.
    /// 펼침 집합은 `UiExpansionState`에 저장해 재시작 후에도 유지한다(OWE-845).
    pub fn toggle_card_sessions(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.expanded_cards.remove(path) {
            self.manually_folded_cards.insert(path.to_path_buf());
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
            self.write_state(cx);
            cx.notify();
        } else {
            self.manually_folded_cards.remove(path);
            self.expanded_cards.insert(path.to_path_buf());
            self.collapsed.remove(path);
            if let Some(focused_id) = self.focused
                && self.folder_on(&HostId::Local, focused_id) == Some(path)
                && let Some(idx) = self.rows.iter().enumerate().position(|(i, r)| {
                    self.row_host(i).is_local()
                        && matches!(r, Row::Session(s, true) if s.id == focused_id)
                })
            {
                self.cursor = Some(idx);
            }
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

    /// 경로를 포함하는 가장 가까운 워크스페이스 루트. 호스트 간 경로는 섞지 않는다.
    pub fn workspace_for_path_on(&self, host: &HostId, path: &Path) -> Option<PathBuf> {
        let snapshot = self.snapshot_for(host);
        snapshot
            .trees
            .keys()
            .chain(snapshot.trees.values().flatten())
            .chain(snapshot.sessions.iter().filter_map(|s| s.worktree.as_ref()))
            .filter(|root| !root.as_os_str().is_empty() && path_within(path, root))
            .max_by_key(|root| root.as_os_str().len())
            .cloned()
    }

    /// 선택한 호스트·워크스페이스의 현재 세션. 접힘·필터·탭 분리 여부와 무관하다.
    /// 기록 보관 세션은 스냅샷에 없으며, 종료 상태와 끊긴 호스트는 포함하지 않는다.
    #[must_use]
    pub fn live_sessions_in_workspace(&self, host: &HostId, folder: &Path) -> Vec<SessionId> {
        if !self.host_connected(host) {
            return Vec::new();
        }
        self.ordered_snapshot(host, self.snapshot_for(host))
            .sessions
            .iter()
            .filter(|s| {
                if s.is_exited() {
                    return false;
                }
                let Some(path) = s.worktree.as_deref().or(s.cwd.as_deref()) else {
                    return false;
                };
                path_identity::same(
                    self.workspace_for_path_on(host, path)
                        .as_deref()
                        .unwrap_or(path),
                    folder,
                )
            })
            .map(|s| s.id)
            .collect()
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

    #[must_use]
    pub fn now_unix_ns(&self) -> u64 {
        self.now_override_ns.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        })
    }

    fn activity_minute(&self, now_ns: u64) -> Option<u64> {
        self.snapshot
            .sessions
            .iter()
            .chain(self.remote_hosts.iter().flat_map(|r| &r.snapshot.sessions))
            .any(|s| s.status.since_unix_ns > 0)
            .then_some(now_ns / 60_000_000_000)
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

    fn host_indent(&self) -> f32 {
        if !self.has_connected_remote_hosts() || self.options.group_by == GroupBy::Repo {
            0.
        } else {
            HOST_CHILD_INDENT
        }
    }

    fn lookup_visual<'a>(
        map: &'a BTreeMap<PathBuf, external::ProjectVisualMeta>,
        path: &Path,
    ) -> Option<&'a external::ProjectVisualMeta> {
        if let Some(v) = map.get(path) {
            return Some(v);
        }
        let name = basename(path);
        if name.is_empty() {
            return None;
        }
        map.iter()
            .find(|(k, _)| basename(k).eq_ignore_ascii_case(&name))
            .map(|(_, v)| v)
    }

    pub(super) fn project_icon(&self, path: &Path) -> Option<&crate::ui::icons::ProjectIcon> {
        let local = Self::lookup_visual(&self.project_visuals, path).and_then(|v| v.icon.as_ref());
        let remote = Self::lookup_visual(&self.remote_visuals, path).and_then(|v| v.icon.as_ref());
        match (local, remote) {
            (
                Some(crate::ui::icons::ProjectIcon::Image(lp)),
                Some(r @ crate::ui::icons::ProjectIcon::Image(rp)),
            ) if lp
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("url-"))
                && rp
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("remote-")) =>
            {
                Some(r)
            }
            (Some(l), _) => Some(l),
            (None, r) => r,
        }
    }

    pub(super) fn project_badge_color(&self, path: &Path) -> Option<&str> {
        Self::lookup_visual(&self.project_visuals, path)
            .and_then(|v| v.badge_color.as_deref())
            .or_else(|| {
                Self::lookup_visual(&self.remote_visuals, path)
                    .and_then(|v| v.badge_color.as_deref())
            })
    }

    pub(super) fn project_header_name(&self, path: &Path) -> String {
        Self::lookup_visual(&self.project_visuals, path)
            .and_then(|v| v.display_name.as_deref())
            .or_else(|| {
                Self::lookup_visual(&self.remote_visuals, path)
                    .and_then(|v| v.display_name.as_deref())
            })
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(|| basename(path), str::to_owned)
    }

    /// 연결된 원격 호스트만 목록에 띄운다(연결 안 된 호스트와 그 프로젝트는 숨기고, 호스트 관리는 설정에서 한다).
    fn live_remote_hosts(&self) -> impl Iterator<Item = &RemoteHost> {
        self.remote_hosts.iter().filter(|r| {
            r.status == HostStatus::Connected
                && self
                    .options
                    .filters
                    .contains_host(&HostId::Remote(r.id.clone()))
        })
    }

    /// Orca처럼 프로젝트가 맨 위 단계다. 여러 PC에 같은 프로젝트가 있으면 한 머리줄 아래로 합치고
    /// 원격 PC의 워크트리·세션은 호스트 배지로 구분한다. 작업 폴더를 모르는 세션은 끝에 한 묶음으로 둔다.
    fn rebuild_by_project(&mut self) {
        let mut merge = ProjectMerge::default();
        if self.options.filters.contains_host(&HostId::Local) {
            merge.absorb(&HostId::Local, self.local_rows());
        }
        for remote in self.live_remote_hosts() {
            let host = HostId::Remote(remote.id.clone());
            let collapsed = merge.aligned_collapsed(&self.collapsed, &remote.snapshot);
            merge.absorb(&host, self.remote_rows(remote, &collapsed));
        }
        let (rows, row_hosts, merged) = merge.finish(&self.project_order);
        self.rows = rows;
        self.row_hosts = row_hosts;
        self.merged_projects = merged;
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.reconcile_path_identity();
        let cursor_item = self
            .cursor
            .and_then(|i| {
                self.rows
                    .get(i)
                    .map(|row| (self.row_host(i).clone(), row.clone()))
            })
            .or_else(|| self.cursor_anchor.take());
        self.status_summary = (
            self.snapshot
                .sessions
                .iter()
                .filter(|s| needs_you(&s.status))
                .count(),
            Arc::new(
                self.snapshot
                    .sessions
                    .iter()
                    .filter(|s| !s.is_exited())
                    .map(|s| s.status.state)
                    .collect(),
            ),
        );
        self.ensure_external_polling(cx);
        self.merged_projects.clear();
        if !self.has_connected_remote_hosts() {
            self.rows = if self.options.filters.contains_host(&HostId::Local) {
                self.local_rows()
            } else {
                Vec::new()
            };
            self.row_hosts = vec![HostId::Local; self.rows.len()];
        } else if self.options.group_by == GroupBy::Repo {
            self.rebuild_by_project();
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
                    sessions: self
                        .snapshot
                        .sessions
                        .iter()
                        .filter(|s| !s.is_exited())
                        .count(),
                });
                row_hosts.push(HostId::Local);
                if !self.collapsed_hosts.contains(&HostId::Local) {
                    let local_rows = self.local_rows();
                    row_hosts.extend(std::iter::repeat_n(HostId::Local, local_rows.len()));
                    rows.extend(local_rows);
                }
            }
            for remote in self.live_remote_hosts() {
                let host = HostId::Remote(remote.id.clone());
                rows.push(Row::HostHeader {
                    host: host.clone(),
                    label: remote.label.clone(),
                    status: remote.status,
                    sessions: remote
                        .snapshot
                        .sessions
                        .iter()
                        .filter(|s| !s.is_exited())
                        .count(),
                });
                row_hosts.push(host.clone());
                if !self.collapsed_hosts.contains(&host) {
                    let r_rows = self.remote_rows(remote, &self.collapsed);
                    row_hosts.extend(std::iter::repeat_n(host, r_rows.len()));
                    rows.extend(r_rows);
                }
            }
            self.rows = rows;
            self.row_hosts = row_hosts;
        }
        if let Some((host, row)) = &cursor_item {
            self.cursor = self.rows.iter().enumerate().find_map(|(i, new_row)| {
                (self.row_host(i) == host && viewport::same_item(row, new_row)).then_some(i)
            });
        }
        if self.cursor.is_none()
            && let Some(sel) = &self.selected_worktree
        {
            self.cursor = self.rows.iter().enumerate().find_map(|(i, r)| {
                (cursor_item.as_ref().is_none_or(|(host, _)| self.row_host(i) == host)
                    && matches!(r, Row::Worktree(p) | Row::Folder(p) | Row::Repository(p) if p == sel))
                .then_some(i)
            });
        }
        self.cursor = self
            .cursor
            .map(|i| i.min(self.rows.len().saturating_sub(1)))
            .filter(|_| !self.rows.is_empty());
        self.cursor_anchor = cursor_item.filter(|_| self.cursor.is_none());
        self.index_lineage();
        self.sync_viewport();
        self.card_list.invalidate_heights();
        cx.notify();
    }

    /// Build lightweight row membership once per model/navigation change. Card headers and
    /// expanded sessions are separate list items, so even one huge card is virtualized.
    fn sync_viewport(&mut self) {
        let cursor_session = self.cursor.and_then(|i| match self.rows.get(i)? {
            Row::Session(session, true) => Some((self.row_host(i), session.worktree.as_ref()?)),
            _ => None,
        });
        let mut parent = None;
        let mut parents = Vec::with_capacity(self.rows.len());
        let mut visible = Vec::new();
        for (i, row) in self.rows.iter().enumerate() {
            let host = self.row_host(i);
            let card = match row {
                Row::Worktree(path) => {
                    parent = Some((i, host, path));
                    None
                }
                Row::Session(session, true) => parent.filter(|(_, parent_host, path)| {
                    *parent_host == host && session.worktree.as_ref() == Some(*path)
                }),
                _ => {
                    parent = None;
                    None
                }
            };
            parents.push(card.map(|(index, _, _)| index));
            if card.is_none_or(|(_, host, path)| {
                self.expanded_cards.contains(path) || cursor_session == Some((host, path))
            }) {
                visible.push((host.clone(), row.clone(), i));
            }
        }
        self.card_parents = parents;
        self.card_list.update(visible);
    }

    fn scroll_row_into_view(&self, index: usize, strategy: ScrollStrategy) {
        self.card_list.reveal(index, strategy);
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
        let prompt_histories = self.prompt_histories.clone();
        self.apply_with_auto_titles(result, comments, auto_titles, prompt_histories, cx);
    }

    fn apply_with_auto_titles(
        &mut self,
        result: io::Result<(Snapshot, Meta)>,
        comments: BTreeMap<PathBuf, String>,
        auto_titles: BTreeMap<SessionId, String>,
        prompt_histories: BTreeMap<SessionId, krill_proto::git::SessionPromptHistory>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((snapshot, meta)) => {
                let profiles_changed = self.profiles.observe(&HostId::Local, &snapshot);
                if self.relink == Relink::AwaitingList {
                    self.relink = Relink::Steady;
                    cx.emit(Relisted(snapshot.sessions.len()));
                }
                // 워크트리 발견·메타 IO는 connect_source의 백그라운드 단계에서 끝낸다.
                let status_changed = self.local_status != HostStatus::Connected;
                self.local_status = HostStatus::Connected;

                if profiles_changed
                    || snapshot != self.snapshot
                    || meta != self.meta
                    || comments != self.comments
                    || auto_titles != self.auto_titles
                    || prompt_histories != self.prompt_histories
                    || self.poll_error.is_some()
                    || status_changed
                    || crate::alert::needs_snapshot(cx)
                {
                    track_unread(
                        &mut self.unread,
                        &self.snapshot.sessions,
                        &snapshot.sessions,
                        self.focused.filter(|_| self.focused_host.is_local()),
                    );
                    // Screen streams are dropped for hidden tabs. Once the
                    // daemon publishes a changed title, an older screen cache
                    // must not keep masking it indefinitely.
                    self.titles.retain(|id, _| {
                        let previous = self.snapshot.sessions.iter().find(|s| s.id == *id);
                        let current = snapshot.sessions.iter().find(|s| s.id == *id);
                        previous.map(|s| &s.status.title) == current.map(|s| &s.status.title)
                    });
                    self.snapshot = snapshot;
                    self.prune_read_done_for_host(&HostId::Local, cx);
                    self.meta = meta;
                    self.comments = comments;
                    self.auto_titles = auto_titles;
                    self.prompt_histories = prompt_histories;
                    let auto_changed = self.include_imported_snapshot();
                    self.poll_error = None;
                    let live = &self.snapshot.sessions;
                    self.titles.retain(|id, _| live.iter().any(|s| s.id == *id));
                    self.auto_titles
                        .retain(|id, _| live.iter().any(|s| s.id == *id));
                    self.prompt_histories
                        .retain(|id, _| live.iter().any(|s| s.id == *id));
                    // OWE-898: 사라진 워크트리의 파생 상태가 폴링마다 누적되지 않게
                    // 라이브 집합에 없는 키를 정리한다(사용자 접힘·핀·부모 설정은 유지).
                    self.prune_stale_worktree_state();
                    if !crate::menu_bar::is_monitoring(cx) {
                        crate::alert::sync_snapshot(&self.snapshot, cx);
                    }
                    let now_ns = self.now_unix_ns();
                    self.last_activity_minute = self.activity_minute(now_ns);
                    if auto_changed {
                        self.persist(cx);
                    } else {
                        self.rebuild(cx);
                    }
                    if let Some(focused_id) = self.focused.filter(|_| self.focused_host.is_local())
                        && let Some(wt) = self
                            .folder_on(&HostId::Local, focused_id)
                            .map(Path::to_path_buf)
                    {
                        let multi_needs_expand = self
                            .snapshot
                            .sessions
                            .iter()
                            .filter(|s| s.worktree.as_deref() == Some(wt.as_path()))
                            .count()
                            >= 2
                            && !self.expanded_cards.contains(&wt)
                            && !self.manually_folded_cards.contains(&wt);
                        if self.focused_synced != Some(focused_id)
                            || self.pending_reveal_worktree.as_deref() == Some(wt.as_path())
                            || multi_needs_expand
                        {
                            self.sync_focused_row(focused_id, false, cx);
                            cx.notify();
                        }
                    }
                } else {
                    let now_ns = self.now_unix_ns();
                    let activity_minute = self.activity_minute(now_ns);
                    if self.last_activity_minute != activity_minute {
                        self.last_activity_minute = activity_minute;
                        cx.notify();
                    }
                }
            }
            Err(e) => {
                crate::alert::disconnected(cx);
                let status_changed = self.local_status != HostStatus::Disconnected;
                self.local_status = HostStatus::Disconnected;
                // 읽기 시한 만료(macOS는 EAGAIN, os error 35)는 날것의 OS 문구 대신 재시도 안내로 보인다.
                let msg = if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) {
                    text::DAEMON_SLOW.get().to_owned()
                } else {
                    e.to_string()
                };
                let transient = matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::Interrupted
                );
                let next_error = Some((msg, transient));
                if self.poll_error != next_error || status_changed {
                    self.poll_error = next_error;
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
        self.ensure_external_polling(cx);
        self.disk.poll_generation = self.disk.poll_generation.wrapping_add(1);
        self.task = None;
        self.source = Some(source);
        crate::alert::listen(&cx.weak_entity(), cx);
        self.spawn_local_polling(false, cx);
    }

    fn spawn_local_polling(&mut self, force_once: bool, cx: &mut Context<Self>) {
        if (!self.is_visible() && !force_once) || self.task.is_some() || self.source.is_none() {
            return;
        }
        let sidebar_path = self.path.clone();
        let generation = self.disk.poll_generation;
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
                let (result, comments, auto_titles, prompt_histories, fallback) = bg
                    .spawn(async move {
                        let mut comments = source.comments();
                        if comments.is_empty() {
                            comments = external::load_comments_from_sidebar_path(sp.as_deref());
                        }
                        let auto_titles = load_auto_titles_from_sidebar_path(sp.as_deref());
                        let prompt_histories =
                            load_prompt_histories_from_sidebar_path(sp.as_deref());
                        let state_dir = sp
                            .as_deref()
                            .and_then(Path::parent)
                            .filter(|p| !p.as_os_str().is_empty())
                            .map(Path::to_path_buf);
                        let result = source.fetch().map(|mut snapshot| {
                            external::include_claude_worktrees(&mut snapshot.trees);
                            let mut imported_meta = Meta::default();
                            if let Some(ref dir) = state_dir {
                                external::include_imported_projects(
                                    dir,
                                    &mut snapshot.trees,
                                    &mut imported_meta,
                                );
                            }
                            let mut meta = source.meta(&snapshot.trees);
                            for (key, value) in imported_meta {
                                let entry = meta.entry(key).or_default();
                                if entry.branch.is_none() {
                                    entry.branch = value.branch;
                                }
                                if entry.dirty.is_none() {
                                    entry.dirty = value.dirty;
                                }
                            }
                            (snapshot, meta)
                        });
                        // 데몬이 응답하지 않아도 등록된 로컬 프로젝트는 목록에 남긴다.
                        let fallback = if result.is_err()
                            && let Some(ref dir) = state_dir
                        {
                            let mut snapshot = Snapshot::default();
                            let mut meta = Meta::default();
                            external::include_imported_projects(
                                dir,
                                &mut snapshot.trees,
                                &mut meta,
                            );
                            Some((snapshot.trees, meta))
                        } else {
                            None
                        };
                        (result, comments, auto_titles, prompt_histories, fallback)
                    })
                    .await;
                let next_delay = this.update(cx, |s, cx| {
                    if s.disk.poll_generation != generation || (!s.is_visible() && !forced) {
                        return None;
                    }
                    if let Some((trees, meta)) = fallback
                        && s.snapshot.trees.is_empty()
                        && !trees.is_empty()
                    {
                        s.snapshot.trees = trees;
                        s.meta.extend(meta);
                        s.rebuild(cx);
                    }
                    s.apply_with_auto_titles(result, comments, auto_titles, prompt_histories, cx);
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

    fn write_state(&mut self, cx: &mut Context<Self>) {
        self.disk.generation = self.disk.generation.wrapping_add(1);
        if !self.disk.initialized {
            let current = startup::capture(self);
            self.disk.startup.record(current);
            self.disk.dirty = true;
            self.ensure_external_polling(cx);
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let prefs = self.prefs.clone();
        let opts = self.options.clone();
        let lin = lineage::LineageState {
            parents: if self.state_dir().is_some() {
                BTreeMap::new()
            } else {
                self.parents.clone()
            },
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
        let order = self.project_order.clone();
        let worktree_order = self.worktree_order.clone();
        self.enqueue_disk_write(
            move || save_state(&path, &prefs, &opts, &lin, &ui, &order, &worktree_order),
            cx,
        );
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        self.write_state(cx);
        self.rebuild(cx);
    }

    /// 로컬 데몬 감시 연결이 끊겨 다시 붙는 중인지. 재연결 중에서 풀리면 다음 로컬 목록 도착을
    /// [`Relisted`]로 한 번 알린다(재연결 확인은 목록이 실제로 온 뒤에만).
    pub fn set_local_reconnecting(&mut self, reconnecting: bool, cx: &mut Context<Self>) {
        let next = match (reconnecting, self.relink) {
            (true, _) => Relink::Reconnecting,
            (false, Relink::Reconnecting) => Relink::AwaitingList,
            (false, other) => other,
        };
        if next != self.relink {
            self.relink = next;
            cx.notify();
        }
    }

    fn local_reconnecting(&self) -> bool {
        self.relink == Relink::Reconnecting
    }

    /// 로컬 세션들의 에이전트 상태(상태바 떼 미터).
    pub fn status_summary(&self) -> (usize, Arc<Vec<AgentState>>) {
        self.status_summary.clone()
    }

    pub fn session_states(&self) -> Vec<krill_proto::control::AgentState> {
        self.snapshot
            .sessions
            .iter()
            .map(|s| s.status.state)
            .collect()
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.disk.poll_generation = self.disk.poll_generation.wrapping_add(1);
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
        self.scroll_row_into_view(index, ScrollStrategy::Top);
        cx.notify();
    }

    /// Keep the list stationary while the shell opens a selection made in this sidebar.
    #[must_use]
    pub fn preserve_scroll(&self) -> impl Drop + use<> {
        self.card_list.preserve_scroll()
    }

    /// 셸 단축키로 사이드바에 들어온다. Enter로 세션을 열거나 Esc를 누르면 `back`으로 돌아간다.
    pub fn focus(&mut self, back: FocusHandle, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        self.back = Some(back);
        if self.cursor.is_none() && !self.rows.is_empty() {
            self.cursor = self.rows.iter().position(|r| matches!(r, Row::Session(..)));
            self.sync_viewport();
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
        self.sync_viewport();
        if let Some(folder) = self.cursor_folder()
            && self.selected_worktree.as_ref() != Some(&folder)
        {
            self.selected_worktree = Some(folder);
            self.write_state(cx);
        }
        self.scroll_row_into_view(next, ScrollStrategy::Nearest);
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
        self.sync_viewport();
        self.mark_session_read(&host, id, cx);
        if let Some(wt) = self.folder_on(&host, id).map(Path::to_path_buf)
            && self.selected_worktree.as_ref() != Some(&wt)
        {
            self.selected_worktree = Some(wt);
            self.write_state(cx);
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
        focus_terminal: bool,
        cx: &mut Context<Self>,
    ) {
        if self.editing.as_ref().is_some_and(|(p, _)| p == &path) {
            return;
        }
        self.cursor = Some(index);
        if self.selected_worktree.as_ref() != Some(&path) {
            self.selected_worktree = Some(path.clone());
            self.write_state(cx);
        }
        cx.emit(WorkspaceSelected {
            host,
            path,
            focus_terminal,
        });
        cx.notify();
    }

    /// 필터·접힘을 적용한 화면 순서. 호스트가 달라도 같은 경로를 구분한다.
    pub fn session_for_workspace(&self, host: &HostId, folder: &Path) -> Option<SessionId> {
        self.snapshot_for(host)
            .sessions
            .iter()
            .find(|s| {
                s.worktree
                    .as_deref()
                    .or(s.cwd.as_deref())
                    .is_some_and(|p| path_identity::same(p, folder))
            })
            .map(|s| s.id)
    }

    pub fn visible_workspaces(&self) -> Vec<(usize, HostId, PathBuf)> {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| match row {
                Row::Worktree(path) | Row::Folder(path) => {
                    Some((index, self.row_host(index).clone(), path.clone()))
                }
                _ => None,
            })
            .collect()
    }

    pub fn activate_workspace_index(&mut self, position: usize, cx: &mut Context<Self>) {
        if let Some((index, host, path)) = self.visible_workspaces().get(position).cloned() {
            self.cursor = Some(index);
            self.selected_worktree = Some(path.clone());
            self.write_state(cx);
            cx.emit(WorkspaceSelected {
                host,
                path,
                focus_terminal: true,
            });
            cx.notify();
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
                let host = self.row_host(cursor).clone();
                self.mark_session_read(&host, session.id, cx);
                self.action_host = Some(host);
                cx.emit(Selected(session.id));
                self.leave(window, cx);
            }
            Some(Row::Worktree(path) | Row::Folder(path)) => {
                let host = self.row_host(cursor).clone();
                self.activate_worktree_card(cursor, host, path, true, cx);
                self.leave(window, cx);
            }
            Some(Row::HiddenWorktrees(repo, _, _)) => self.toggle_external(&repo, cx),
            _ => {}
        }
    }

    /// 같은 표시 그룹·계보의 형제 카드 순서를 고정한다. 자동 정렬을 다시 선택하면 그 정렬을 따른다.
    pub fn move_worktree(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        let positions = |path: &Path| {
            self.rows.iter().enumerate().find_map(|(i, row)| {
                (matches!(row, Row::Worktree(p) if p == path) && self.row_host(i).is_local())
                    .then_some(i)
            })
        };
        let (Some(from_index), Some(to_index)) = (positions(from), positions(to)) else {
            return;
        };
        let group = |index: usize| {
            self.rows.iter().take(index).rposition(|row| {
                matches!(
                    row,
                    Row::Repository(_) | Row::GroupHeader { .. } | Row::HiddenWorktrees(..)
                )
            })
        };
        let parents = self.merged_parents();
        if from_index == to_index
            || group(from_index) != group(to_index)
            || parents.get(from) != parents.get(to)
        {
            return;
        }
        let bucket = group(from_index);
        let mut order: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| match row {
                Row::Worktree(path)
                    if self.row_host(i).is_local()
                        && group(i) == bucket
                        && parents.get(path) == parents.get(from) =>
                {
                    Some(path.clone())
                }
                _ => None,
            })
            .collect();
        let (Some(i), Some(j)) = (
            order.iter().position(|p| p == from),
            order.iter().position(|p| p == to),
        ) else {
            return;
        };
        let moved = order.remove(i);
        order.insert(j, moved);
        // Retain hidden/filtered cards and the manual order in other groups.
        for path in &self.worktree_order {
            if !order.contains(path) {
                order.push(path.clone());
            }
        }
        self.worktree_order = order;
        self.options.sort_by = SortBy::Created;
        self.persist(cx);
    }

    /// 프로젝트(저장소·폴더) `from`을 `to` 자리로 옮기고 보이는 순서를 저장한다.
    pub fn move_project(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        let mut order: Vec<PathBuf> = self
            .rows
            .iter()
            .filter_map(|row| project_key(row).map(|(_, path)| path.clone()))
            .collect();
        let (Some(i), Some(j)) = (
            order.iter().position(|p| p == from),
            order.iter().position(|p| p == to),
        ) else {
            return;
        };
        if i == j {
            return;
        }
        let moved = order.remove(i);
        order.insert(j, moved);
        // 지금 안 보이는(필터·접힌 호스트) 프로젝트의 예전 자리도 뒤에 남겨 둔다.
        for p in &self.project_order {
            if !order.contains(p) {
                order.push(p.clone());
            }
        }
        self.project_order = order;
        self.persist(cx);
    }

    /// `open`: `None`이면 뒤집고, `Some(true)`면 펼치고, `Some(false)`면 접는다.
    /// 저장소·폴더·워크트리의 접힘은 `UiExpansionState`에 저장해 재시작 후에도 유지한다(OWE-845).
    pub fn fold(&mut self, path: &Path, open: Option<bool>, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.ui.collapsed.insert(path.to_path_buf());
            self.disk
                .startup
                .ui
                .expanded_cards
                .insert(path.to_path_buf());
        }
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

    /// Jump는 표시 행·필터와 무관하게 이미 받은 연결된 호스트 스냅샷만 읽는다.
    fn jump_snapshots(&self) -> impl Iterator<Item = (HostId, &str, &Snapshot)> {
        std::iter::once((
            HostId::Local,
            crate::i18n::catalog::jump::LOCAL_MAC.get(),
            &self.snapshot,
        ))
        .chain(
            self.remote_hosts
                .iter()
                .filter(|r| r.status == HostStatus::Connected)
                .map(|r| (HostId::Remote(r.id.clone()), r.label.as_str(), &r.snapshot)),
        )
    }

    /// MK-057: 연결된 모든 호스트의 세션을 호스트 식별자·검색 인덱스와 함께 만든다.
    pub fn jump_items(&self) -> Vec<Item> {
        let focused_id = self.focused;
        let focused_host = &self.focused_host;
        let focused_folder = focused_id.and_then(|id| self.folder_on(focused_host, id));
        let now_ns = self.now_unix_ns();
        let mut items = Vec::new();
        for (host, host_label, snapshot) in self.jump_snapshots() {
            // 세션마다 전체 워크트리 목록을 다시 훑지 않는다.
            let mut tree_repos = BTreeMap::new();
            for (repo, trees) in &snapshot.trees {
                tree_repos.entry(repo.as_path()).or_insert(repo.as_path());
                for tree in trees {
                    tree_repos.entry(tree.as_path()).or_insert(repo.as_path());
                }
            }
            for s in &snapshot.sessions {
                let (title, detail) = self.session_label_on(&host, s);
                let agent =
                    crate::ui::status::infer_agent_from_status(&s.status, &title).to_owned();
                let folder_path = s.worktree.as_deref().or(s.cwd.as_deref());
                let (repo_name, worktree_name) = if let Some(wt) = s.worktree.as_deref() {
                    let repo_path = tree_repos.get(wt).copied().unwrap_or(wt);
                    (basename(repo_path), self.name_on(&host, wt))
                } else if let Some(cwd) = s.cwd.as_deref() {
                    (basename(cwd), self.name_on(&host, cwd))
                } else {
                    (String::new(), String::new())
                };
                let is_current_tab = &host == focused_host && focused_id == Some(s.id);
                let is_current_worktree = &host == focused_host
                    && !is_current_tab
                    && folder_path.is_some()
                    && folder_path == focused_folder;
                let age = if s.status.since_unix_ns > 0 {
                    crate::jump::format_age_ns(s.status.since_unix_ns, now_ns)
                } else if let Some(&ts) = folder_path
                    .filter(|_| host.is_local())
                    .and_then(|p| self.worktree_updated_at.get(p))
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
                item.unread = host.is_local() && self.unread.contains(&s.id);
                item.age = age;
                item.is_current_tab = is_current_tab;
                item.is_current_worktree = is_current_worktree;
                item.host = host.clone();
                host_label.clone_into(&mut item.host_label);
                item.cwd = s
                    .cwd
                    .as_deref()
                    .or(s.worktree.as_deref())
                    .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
                item.worktree_path = s
                    .worktree
                    .as_deref()
                    .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
                item.repo_name = repo_name;
                item.worktree_name = worktree_name;
                item.rebuild_search_chars();
                items.push((s.status.since_unix_ns, item));
            }
        }
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
        let focused_host = &self.focused_host;
        let focused_folder = focused_id
            .and_then(|id| self.folder_on(focused_host, id))
            .map(Path::to_path_buf);
        let now_ns = self.now_unix_ns();
        let now_secs = now_ns / 1_000_000_000;
        let mut entries: Vec<(u64, crate::jump::WorktreeItem)> = Vec::new();

        for (host, host_label, snapshot) in self.jump_snapshots() {
            let mut seen = BTreeSet::new();
            let mut sessions_by_path: BTreeMap<&Path, Vec<&Session>> = BTreeMap::new();
            for session in &snapshot.sessions {
                if let Some(path) = session.worktree.as_deref().or(session.cwd.as_deref()) {
                    sessions_by_path.entry(path).or_default().push(session);
                }
            }
            let mut push_path = |path: &Path, repo_path: &Path, is_primary: bool| {
                if !seen.insert(path.to_path_buf()) {
                    return;
                }
                let pref_name = self
                    .prefs
                    .get(path)
                    .filter(|v| host.is_local() && !v.name.is_empty())
                    .map(|v| v.name.as_str())
                    .filter(|n| !krill_proto::git::is_default_branch_or_worktree_name(n));
                let name = pref_name.map_or_else(|| basename(path), str::to_owned);
                let repo_name = basename(repo_path);
                let branch = if host.is_local() {
                    self.branch(path)
                } else {
                    self.branch_on(&host, path)
                }
                .unwrap_or_default()
                .to_owned();
                let is_current = &host == focused_host && focused_folder.as_deref() == Some(path);
                let sessions_in_wt = sessions_by_path.get(path).map_or(&[][..], Vec::as_slice);
                let session_id = focused_id
                    .filter(|_| &host == focused_host)
                    .filter(|id| sessions_in_wt.iter().any(|s| s.id == *id))
                    .or_else(|| sessions_in_wt.first().map(|s| s.id));
                let unread =
                    host.is_local() && sessions_in_wt.iter().any(|s| self.unread.contains(&s.id));
                let pairs: Vec<_> = sessions_in_wt
                    .iter()
                    .filter(|s| {
                        !crate::ui::status::infer_agent_from_status(
                            &s.status,
                            s.status.title.as_deref().unwrap_or_default(),
                        )
                        .is_empty()
                            || !matches!(s.status.state, AgentState::Exited | AgentState::Unknown)
                    })
                    .map(|s| {
                        (
                            s.status.state,
                            crate::ui::status::infer_agent_from_status(
                                &s.status,
                                s.status.title.as_deref().unwrap_or_default(),
                            ),
                        )
                    })
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
                } else if let Some(&ts) = self
                    .worktree_updated_at
                    .get(path)
                    .filter(|_| host.is_local())
                {
                    ts
                } else if host.is_local() {
                    self.disk.folder_updated_at.get(path).copied().unwrap_or(0)
                } else {
                    0
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
                let mut item = crate::jump::WorktreeItem::new(
                    path.to_path_buf(),
                    session_id,
                    name,
                    branch,
                    repo_name,
                    host_label.to_owned(),
                    age,
                    state,
                    unread,
                    is_primary,
                    is_current,
                );
                item.host = host.clone();
                entries.push((sort_ts, item));
            };

            for (repo, trees) in &snapshot.trees {
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
            for s in &snapshot.sessions {
                if s.worktree.is_none()
                    && let Some(cwd) = s.cwd.as_deref()
                {
                    push_path(cwd, cwd, false);
                }
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
        if self.editing.as_ref().is_some_and(|(p, _)| p == &path) {
            self.finish_rename(Some(window), cx);
            return;
        }
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
            if !self.disk.initialized {
                self.disk.startup.names.insert(path.clone());
            }
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

    /// Register a project through the same durable queue used by sidebar edits.
    pub fn register_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(dir) = self.state_dir().map(Path::to_path_buf) else {
            return;
        };
        self.enqueue_disk_write(
            move || external::register_project(&dir, &path).map(|_| ()),
            cx,
        );
    }

    /// 저장소·폴더 프로젝트를 목록에서 제거한다(OWE-845).
    /// `projects.toml`에 기록된 이관 프로젝트는 파일에서도 지우고,
    /// 그 외에는 제거 집합에 넣어 행을 숨긴다(재시작 후에도 유지).
    pub fn remove_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(dir) = self.state_dir().map(Path::to_path_buf) {
            let removed = path.clone();
            self.enqueue_disk_write(
                move || {
                    external::remove_imported_project(&dir, &removed);
                    Ok(())
                },
                cx,
            );
        }
        self.collapsed.remove(&path);
        self.expanded_cards.remove(&path);
        self.shown_external.remove(&path);
        self.removed_projects.insert(path);
        self.persist(cx);
    }

    /// 워크트리 삭제 확인 모달을 연다(OWE-847). 메뉴 클릭 시 즉시 삭제하지 않는다.
    #[cfg(all(test, unix))]
    pub(super) fn request_delete_worktree(
        &mut self,
        path: PathBuf,
        with_descendants: bool,
        cx: &mut Context<Self>,
    ) {
        self.request_delete_worktree_on(HostId::Local, path, with_descendants, cx);
    }

    /// 워크트리 삭제 확인 모달을 연다(OWE-847). 메뉴 클릭 시 즉시 삭제하지 않는다.
    pub(super) fn request_delete_worktree_on(
        &mut self,
        host: HostId,
        path: PathBuf,
        with_descendants: bool,
        cx: &mut Context<Self>,
    ) {
        let descendant_count = if with_descendants {
            self.lineage_descendants_on(&host, &path).len()
        } else {
            0
        };
        let dirty = self
            .meta_for(&host)
            .get(&path)
            .and_then(|m| m.dirty)
            .unwrap_or(0);
        self.delete_focus = DeleteFocus::Pending;
        self.delete_confirm = Some(DeleteConfirm {
            host,
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
            self.delete_with_descendants_on(confirm.host, confirm.path, confirm.force, cx);
        } else {
            self.delete_worktree_on(confirm.host, confirm.path, confirm.force, cx);
        }
    }

    /// 바꾼 이름 → 브랜치 → 첫 프롬프트 자동 제목 → 폴더 이름.
    fn name(&self, path: &Path) -> String {
        self.name_on(&HostId::Local, path)
    }

    pub(super) fn name_on(&self, host: &HostId, path: &Path) -> String {
        let pref_name = host
            .is_local()
            .then(|| self.custom_worktree_name(path))
            .flatten();
        if let Some(custom) =
            pref_name.filter(|n| !krill_proto::git::is_default_branch_or_worktree_name(n))
        {
            return custom.to_owned();
        }
        if self.snapshot_for(host).trees.contains_key(path) {
            return self
                .branch_on(host, path)
                .map_or_else(|| basename(path), str::to_owned);
        }
        if let Some(branch) = self.branch_on(host, path) {
            if !krill_proto::git::is_default_branch_or_worktree_name(branch) {
                return branch.to_owned();
            }
            if host.is_local()
                && let Some(auto) = self.worktree_auto_title(path)
            {
                return auto.to_owned();
            }
            if pref_name.is_none() {
                return branch.to_owned();
            }
        }
        if host.is_local()
            && let Some(auto) = self.worktree_auto_title(path)
        {
            return auto.to_owned();
        }
        pref_name.map_or_else(|| basename(path), str::to_owned)
    }

    /// 워크트리에 직접 지정한 이름. 기본 워크트리(`trees` 루트)에 프로젝트 표시 이름·폴더 이름이
    /// 그대로 들어 있는 경우(`projects.toml` 동기화분 포함)는 프로젝트 머리줄과 중복이므로 제외한다.
    pub(super) fn custom_worktree_name(&self, path: &Path) -> Option<&str> {
        let name = self
            .prefs
            .get(path)
            .map(|v| v.name.trim())
            .filter(|s| !s.is_empty())?;
        let folder = basename(path);
        if name.eq_ignore_ascii_case(&folder) {
            return None;
        }
        if self.snapshot.trees.contains_key(path) || self.disk.imported.trees.contains_key(path) {
            let header = self.project_header_name(path);
            if name.eq_ignore_ascii_case(&header) {
                return None;
            }
        }
        Some(name)
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

    /// 사이드바 설정 저장/손상 오류 문구(셸 우하단 토스트 동기화용).
    #[must_use]
    pub fn preference_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn clear_preference_error(&mut self, cx: &mut Context<Self>) {
        if self.error.take().is_some() {
            cx.notify();
        }
    }

    /// 데몬 스냅샷 폴링 실패 문구(셸 우하단 토스트 동기화용).
    #[must_use]
    pub fn poll_error(&self) -> Option<&str> {
        self.poll_error.as_ref().map(|(msg, _)| msg.as_str())
    }

    /// 자동 재시도 중인 연결 오류는 팝업 없이 복구하고, 조치가 필요한 오류만 알린다.
    #[must_use]
    pub fn poll_error_notice(&self) -> Option<&str> {
        self.poll_error
            .as_ref()
            .and_then(|(msg, transient)| (!transient).then_some(msg.as_str()))
    }

    pub fn set_poll_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        let next = error.map(|msg| (msg, false));
        if self.poll_error != next {
            self.poll_error = next;
            cx.notify();
        }
    }

    pub fn clear_poll_error(&mut self, cx: &mut Context<Self>) {
        self.set_poll_error(None, cx);
    }

    /// 사이드바 단발성 알림 문구(셸 우하단 토스트 동기화용).
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn clear_notice(&mut self, cx: &mut Context<Self>) {
        if self.notice.take().is_some() {
            cx.notify();
        }
    }

    /// PR CLI(gh) 미설치/인증 안내 문구(셸 우하단 토스트 동기화용).
    #[must_use]
    pub fn pr_guidance(&self) -> Option<&'static str> {
        (!self.pr_guidance_dismissed)
            .then(|| {
                self.prs
                    .values()
                    .find_map(crate::pull_request::Status::guidance)
            })
            .flatten()
    }

    pub fn dismiss_pr_guidance(&mut self, cx: &mut Context<Self>) {
        if !self.pr_guidance_dismissed {
            self.pr_guidance_dismissed = true;
            cx.notify();
        }
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
                    .filter(|s| s.status.state != AgentState::Exited)
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
                    .filter(|s| s.status.state != AgentState::Exited)
                    .cloned()
                    .map(|s| (host.clone(), s)),
            );
        }
        out
    }

    pub fn session_label(&self, session: &Session) -> (String, String) {
        self.session_label_on_with_title(&HostId::Local, session, None)
    }

    #[must_use]
    pub fn session_label_on(&self, host: &HostId, session: &Session) -> (String, String) {
        self.session_label_on_with_title(host, session, None)
    }

    /// Tabs and sidebar rows share title precedence, including a pane's newest
    /// OSC title. Local custom/auto titles never cross into another host's IDs.
    #[must_use]
    pub fn session_label_on_with_title(
        &self,
        host: &HostId,
        session: &Session,
        pane_title: Option<&str>,
    ) -> (String, String) {
        let local = host.is_local();
        let custom_title = local
            .then(|| self.custom_titles.get(&session.id))
            .flatten()
            .map(String::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty());
        let cached_title = local
            .then(|| self.titles.get(&session.id))
            .flatten()
            .map(String::as_str);
        let live_title = pane_title
            .or(cached_title)
            .and_then(krill_proto::control::display_title);
        let auto_title = local
            .then(|| self.auto_titles.get(&session.id))
            .flatten()
            .map(String::as_str)
            .filter(|title| !title.trim().is_empty());
        let has_agent_prompt = !session.status.agent.trim().is_empty() && auto_title.is_some();
        let specific = |title: &&str| {
            !(krill_proto::git::is_default_branch_or_worktree_name(title)
                || title.eq_ignore_ascii_case("claude code")
                || has_agent_prompt && is_windows_shell_title(title))
        };
        let explicit_title = live_title
            .as_deref()
            .filter(specific)
            .or_else(|| session.status.title.as_deref().filter(specific));
        let title = custom_title
            .or(explicit_title)
            .or(auto_title)
            .or(live_title.as_deref())
            .or(session.status.title.as_deref());
        let (mut name, detail) = session_label(session, title);
        // Derive provenance from dispatch metadata so OSC/custom title changes and
        // already-settled workers keep the marker. Session IDs are host-local.
        if local
            && self.disk.orchestration.dispatches.iter().any(|dispatch| {
                dispatch.session_id == Some(session.id.0)
                    && dispatch.session_origin
                        == krill_proto::orchestration::DispatchSessionOrigin::Created
            })
            && !name.starts_with("[orchestration]")
        {
            name = format!("[orchestration] {name}");
        }
        (name, detail)
    }

    /// 로컬 세션의 최근 프롬프트 턴 히스토리를 돌려준다.
    #[must_use]
    pub fn session_prompt_history(
        &self,
        id: SessionId,
    ) -> Option<&krill_proto::git::SessionPromptHistory> {
        self.prompt_histories.get(&id)
    }

    /// 호스트 구분을 포함해 세션의 최근 프롬프트 턴 히스토리를 돌려준다.
    #[must_use]
    pub fn session_prompt_history_on(
        &self,
        host: &HostId,
        id: SessionId,
    ) -> Option<&krill_proto::git::SessionPromptHistory> {
        match host {
            HostId::Local => self.prompt_histories.get(&id),
            HostId::Remote(host) => self
                .remote_hosts
                .iter()
                .any(|remote| remote.id == *host)
                .then(|| self.remote_prompt_histories.get(host)?.get(&id))
                .flatten(),
        }
    }

    /// 하이브리드 채팅 등 앱 내부에서 프롬프트를 전송했을 때 즉시 메모리·디스크 히스토리에 반영한다.
    pub fn record_local_prompt(
        &mut self,
        session: SessionId,
        prompt: &str,
        cx: &mut Context<Self>,
    ) {
        let now_ns = self.now_unix_ns();
        let changed = self
            .prompt_histories
            .entry(session)
            .or_default()
            .record_turn(prompt, now_ns);
        if !changed {
            return;
        }
        if let Some(dir) = self.state_dir().map(Path::to_path_buf) {
            let owned = prompt.to_owned();
            cx.background_executor()
                .spawn(async move {
                    krill_proto::git::record_session_prompt(&dir, session, &owned, now_ns).ok();
                })
                .detach();
        }
        cx.notify();
    }

    /// 백그라운드 폴링(사이드바 접힘 상태 포함)에서 읽어 온 프롬프트 히스토리를 동기화한다.
    pub fn sync_prompt_histories(
        &mut self,
        mut histories: BTreeMap<SessionId, krill_proto::git::SessionPromptHistory>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.snapshot.sessions.is_empty() {
            let live = &self.snapshot.sessions;
            histories.retain(|id, _| live.iter().any(|s| s.id == *id));
        }
        if self.prompt_histories == histories {
            return false;
        }
        self.prompt_histories = histories;
        cx.notify();
        true
    }

    /// OWE-662: 로컬 호스트 워크트리 세션의 상태 묶음과 미확인 세션이 있는지.
    #[cfg(test)]
    #[cfg(unix)]
    fn tree_summary(&self, tree: &Path) -> (crate::ui::status::Summary<'_>, bool) {
        self.tree_summary_on(&HostId::Local, tree)
    }

    /// OWE-662: 워크트리 세션의 상태 묶음과 미확인 세션이 있는지.
    /// 에이전트도 상태 근거도 없는 셸(종료·알 수 없음)은 묶음에서 뺀다.
    #[cfg(all(test, unix))]
    fn tree_summary_on(
        &self,
        host: &HostId,
        tree: &Path,
    ) -> (crate::ui::status::Summary<'_>, bool) {
        let sessions: Vec<_> = self
            .snapshot_for(host)
            .sessions
            .iter()
            .filter(|s| !s.is_exited() && s.worktree.as_deref() == Some(tree))
            .collect();
        let unread = host.is_local() && sessions.iter().any(|s| self.unread.contains(&s.id));
        let pairs: Vec<_> = sessions
            .iter()
            .filter(|s| {
                !crate::ui::status::infer_agent_from_status(
                    &s.status,
                    s.status.title.as_deref().unwrap_or_default(),
                )
                .is_empty()
                    || !matches!(s.status.state, AgentState::Exited | AgentState::Unknown)
            })
            .map(|s| {
                (
                    s.status.state,
                    crate::ui::status::infer_agent_from_status(
                        &s.status,
                        s.status.title.as_deref().unwrap_or_default(),
                    ),
                )
            })
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

#[cfg(test)]
mod project_order_tests;
