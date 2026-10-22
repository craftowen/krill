//! 앱 셸 뷰(MK-030): 3열(사이드바 · 탭 바 + 분할 트리 · 오른쪽 패널), 비율 드래그, 변경마다 레이아웃 저장.
//!
//! gpui 셸 의존은 이 파일·`sidebar`·`jump`·`main.rs`에 있다(MK-002 잠정 결정). 모델·저장은 `layout`이 맡는다.
//!
//! # 칸 ↔ 세션 (OWE-490)
//!
//! 데몬에 붙으면 보이는 칸(활성 탭의 잎)마다 [`Live`] 하나를 두고 IO 태스크가 세션을 정해(`pane::ensure_session`)
//! 화면 스트림에 붙는다(`pane::attach`). 프레임 읽기는 백그라운드에서 막고 읽고, 적용·입력·그리기는 UI 스레드에서 한다.
//! 끊기면 칸 상태는 그대로 두고 다시 붙는다(같은 krilld 세대면 스냅샷 생략). 탭을 바꿔 칸이 안 보이면 [`Live`]를
//! 지워 소켓을 닫는다(MK-035). 칸의 세션이 사라졌으면(재부팅 등) 디스크 기록의 마지막 화면을 읽기 전용으로 띄우고
//! "다시 시작" 버튼으로 새 세션을 붙인다(MK-032 콜드 복원). 키는 `keys`(특수 키)와 IME 확정 텍스트(`krill_term::ime`)로 `Pane::input`에 간다.
//!
//! # 기본 상호작용 (MK-033)
//!
//! 프로그램이 마우스 보고를 켜지 않았거나 Shift를 누르면 왼쪽 끌기가 선택(`krill_term::select`)이 되고 ⌘C가 복사한다.
//! ⌘V·파일 드롭은 `krill_term::paste`로 bracketed paste·셸 이스케이프를 거쳐 입력으로 간다. 우클릭 메뉴(복사·붙여넣기)는
//! 마우스 보고가 꺼져 있을 때만. 프로그램의 OSC 52 쓰기는 krilld가 알려 오면 시스템 클립보드에 쓴다(읽기는 krilld가 막는다).

mod chat;
mod close;
mod commands;
mod diff;
pub(crate) mod files;
mod fit;
mod folder;
mod handoff;
mod liveness;
mod new_session;
mod overlays;
mod persist;
pub(crate) mod popout;
mod release;
pub(crate) mod security;
mod tabs;
mod workspaces;

// 앱이 꺼져 있던 동안의 변화 요약.
mod away;

use krill_proto::os::ffi::OsStringExt as _;
use krill_proto::os::net::UnixStream;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::net::Shutdown;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, ClipboardItem, Context, DragMoveEvent, ElementId,
    ElementInputHandler, Empty, EntityInputHandler, ExternalPaths, FocusHandle, KeyBinding,
    KeyDownEvent, KeyUpEvent, Modifiers, MouseButton, Pixels, Point, PromptLevel, ScrollWheelEvent,
    SharedString, Subscription, Task, UTF16Selection, Window, actions, canvas, div, point,
    prelude::*, px, relative,
};
#[cfg(windows)]
use gpui_component::ActiveTheme as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::ContextMenuExt as _;

use gpui_component::{Disableable as _, IconName, Sizable as _};
use krill_app::detached;
use krill_app::echo::EchoDraw;
use krill_app::link::{BannerTone, Link, notice_text};
use krill_app::pane::{self, Pane};
use krill_app::terminal::{
    Painted, Preedit, TerminalElement, cell_at, cell_size, file_link_target, link_target,
};
use krill_app::{keys, platform};
use krill_proto::agent::Permission;
use krill_proto::control::{AgentLaunch, AgentSettings, AgentState};
use krill_proto::daemon::{self, Paths};
use krill_proto::input::{Notice, Now};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::theme::{CellSize, Push, Theme};
use krill_term::ime::{CellPoint, Ime, Output};
use krill_term::select::{self, Selection};

use krill_app::layout::{self, Axis, Branch, Layout, PaneId, PaneTree};

use krill_app::{jump, sidebar};
use krill_proto::control::{Create, Program};
use krill_proto::os::ffi::OsStrExt as _;

const KEY_CONTEXT: &str = "Shell";
/// 커서 깜박임 반주기(OWE-658, macOS 기본 텍스트 커서와 비슷한 값 — 추정).
const CURSOR_BLINK: std::time::Duration = std::time::Duration::from_millis(530);
/// 분할선의 잡는 폭(pt). 보이는 선은 1pt.
const DIVIDER_GRAB: Pixels = krill_app::ui::DIVIDER_HIT_PX;
/// 재연결 확인 배너가 보이는 시간.
const RECONNECTED_FOR: Duration = Duration::from_millis(2500);

/// 사용자에게 보이는 ko/en 문자열 키(MK-094).
use krill_app::i18n::catalog::shell as text;

/// 칸 연결 재시도 간격: 50ms에서 두 배씩, 상한 500ms(`krill_proto::daemon` 재연결과 같은 설계값).
fn retry_delay(attempt: u32) -> Duration {
    Duration::from_millis(50)
        .saturating_mul(1u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX))
        .min(Duration::from_millis(500))
}

/// 칸 IO 태스크가 붙기를 마친 뒤 할 일.
enum Next {
    /// 붙었다. 이 소켓을 읽는다.
    Read(UnixStream),
    /// 못 붙었다. 잠시 뒤 다시.
    Retry,
    /// 칸이 안 보이게 됐거나 콜드 화면을 띄웠다(태스크 끝).
    Gone,
}

/// 칸 IO 태스크가 백그라운드에서 정한 것.
enum Got {
    Live {
        session: krill_proto::SessionId,
        linked: std::io::Result<pane::Linked>,
        prior_cold: Option<(krill_proto::SessionId, krill_proto::control::Cold)>,
    },
    Cold(krill_proto::SessionId, krill_proto::control::Cold),
}

pub(crate) fn saved_window_bounds_from_gpui(wb: gpui::WindowBounds) -> layout::SavedWindowBounds {
    let (mode, bounds) = match wb {
        gpui::WindowBounds::Windowed(b) => (layout::SavedWindowMode::Windowed, b),
        gpui::WindowBounds::Maximized(b) => (layout::SavedWindowMode::Maximized, b),
        gpui::WindowBounds::Fullscreen(b) => (layout::SavedWindowMode::Fullscreen, b),
    };
    layout::SavedWindowBounds {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        width: f32::from(bounds.size.width),
        height: f32::from(bounds.size.height),
        mode,
    }
}

#[must_use]
pub fn gpui_window_bounds_from_saved(saved: layout::SavedWindowBounds) -> gpui::WindowBounds {
    let b = gpui::Bounds::new(
        gpui::point(gpui::px(saved.x), gpui::px(saved.y)),
        gpui::size(gpui::px(saved.width), gpui::px(saved.height)),
    );
    match saved.mode {
        layout::SavedWindowMode::Windowed => gpui::WindowBounds::Windowed(b),
        layout::SavedWindowMode::Maximized => gpui::WindowBounds::Maximized(b),
        layout::SavedWindowMode::Fullscreen => gpui::WindowBounds::Fullscreen(b),
    }
}

/// 마우스 선택 단위(OWE-866): 더블클릭=단어, 트리플클릭=줄. 드래그 중에도 단위를 유지한다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SelectKind {
    #[default]
    Char,
    Word,
    Line,
}

/// 보이는 칸 하나의 세션 연결과 입력 상태. 지우면 소켓을 닫아 IO 태스크도 끝난다.
struct Live {
    pane: Pane<pane::Writer>,
    /// 연결을 끊을 때 쓰는 소켓 사본(IO 태스크가 읽는 소켓과 같다).
    sock: Option<UnixStream>,
    ime: Ime,
    echo: EchoDraw,
    /// 마지막으로 그린 자리와 칸 크기(IME 후보창).
    painted: Painted,
    /// Live와 함께 해제하는 행 계획(계정 입력 보존 중인 숨은 Live는 예외).
    row_cache: krill_app::terminal::RowPlanCache,
    /// 한 행이 안 되는 스크롤 양(px).
    scroll_rem: f32,
    /// 붙지 못한 이유(연결 전까지 칸에 보인다).
    error: Option<String>,
    launch: Option<AgentLaunch>,
    /// 마우스 선택(MK-033, 보이는 화면 좌표). 입력·스크롤하면 지운다.
    selection: Option<Selection>,
    /// 선택을 마쳤을 때 그 자리 글자(AS-17: 출력 스크롤·리사이즈·재접속으로 내용이
    /// 바뀌면 선택 좌표가 다른 글자를 가리키므로 선택을 지운다).
    selection_text: Option<String>,
    /// 왼쪽 버튼으로 선택을 끄는 중.
    selecting: bool,
    /// 끄는 중인 선택 단위(OWE-866).
    selecting_kind: SelectKind,
    task: Task<()>,
    creation: Arc<std::sync::Mutex<close::Creation>>,
}

impl Live {
    fn new(task: Task<()>) -> Self {
        Self {
            pane: Pane::default(),
            sock: None,
            ime: Ime::default(),
            echo: EchoDraw::default(),
            painted: Rc::default(),
            row_cache: krill_app::terminal::RowPlanCache::default(),
            scroll_rem: 0.,
            error: None,
            launch: None,
            selection: None,
            selection_text: None,
            selecting: false,
            selecting_kind: SelectKind::Char,
            task,
            creation: Arc::default(),
        }
    }

    fn cut(&mut self) {
        if let Some(s) = self.sock.take() {
            s.shutdown(Shutdown::Both).ok();
        }
    }

    /// 선택을 지운다(입력·휠 스크롤·출력으로 내용이 바뀔 때).
    fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_text = None;
        self.selecting_kind = SelectKind::Char;
    }

    /// 확정된 선택의 그 자리 글자를 기록한다(AS-17: 출력으로 내용이 바뀌면 지우기 위해).
    fn snapshot_selection(&mut self) {
        self.selection_text = self.selection.map(|s| s.text(&self.pane.shown()));
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.cut();
    }
}

actions!(
    shell,
    [
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        NextRecentTab,
        PrevRecentTab,
        SplitRight,
        SplitDown,
        ClosePane,
        FocusNextPane,
        FocusPrevPane,
        FocusPaneLeft,
        FocusPaneRight,
        FocusPaneUp,
        FocusPaneDown,
        ToggleZoomPane,
        ClearPane,
        SelectAll,
        NewAgentTab,
        ReopenClosedTab,
        PrevSession,
        NextSession,
        NavigateBack,
        NavigateForward,
        NewWorktree,
        ActivateTab1,
        ActivateTab2,
        ActivateTab3,
        ActivateTab4,
        ActivateTab5,
        ActivateTab6,
        ActivateTab7,
        ActivateTab8,
        ActivateTab9,
        ToggleSidebar,
        FocusSidebar,
        TogglePanel,
        ToggleChanges,
        ToggleExplorer,
        ToggleSearch,
        Quit,
        Jump,
        CommandPalette,
        Find,
        FindNext,
        FindPrev,
        Copy,
        Paste,
        ForkSession,
        ContinueInNewSession
    ]
);

const MAX_NAV_HISTORY: usize = 50;

/// OWE-872: 실행 중인 탭·칸 닫기 확인 대기 대상.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PendingRunningClose {
    Tab(usize),
    Pane(PaneId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum NavEntry {
    Workspace {
        pane: PaneId,
        session: Option<krill_proto::SessionId>,
        worktree: Option<PathBuf>,
    },
    Settings(krill_app::settings::AdminTab),
}

#[derive(Clone, Debug, Default)]
pub(super) struct NavHistory {
    entries: Vec<NavEntry>,
    index: usize,
    navigating: bool,
}

impl NavHistory {
    fn push(&mut self, entry: NavEntry) {
        if self.navigating {
            return;
        }
        if let (
            Some(NavEntry::Workspace {
                pane: cur_pane,
                session: cur_session,
                worktree: cur_wt,
            }),
            NavEntry::Workspace {
                pane,
                session,
                worktree,
            },
        ) = (self.entries.get_mut(self.index), &entry)
            && cur_pane == pane
            && (cur_wt == worktree || cur_wt.is_none() || worktree.is_none())
        {
            if cur_session.is_none() && session.is_some() {
                *cur_session = *session;
            }
            if cur_wt.is_none() && worktree.is_some() {
                cur_wt.clone_from(worktree);
            }
            return;
        }
        if self.entries.get(self.index) == Some(&entry) {
            return;
        }
        if self.index + 1 < self.entries.len() {
            self.entries.truncate(self.index + 1);
        }
        if self.entries.last() == Some(&entry) {
            self.index = self.entries.len().saturating_sub(1);
            return;
        }
        self.entries.push(entry);
        if self.entries.len() > MAX_NAV_HISTORY {
            let overflow = self.entries.len() - MAX_NAV_HISTORY;
            self.entries.drain(0..overflow);
        }
        self.index = self.entries.len().saturating_sub(1);
    }
}

pub fn bind_keys(cx: &mut App) {
    let ctx = Some(KEY_CONTEXT);
    cx.bind_keys([
        krill_app::keys::bind("cmd-t", NewTab, ctx),
        krill_app::keys::bind("cmd-alt-t", NewAgentTab, ctx),
        krill_app::keys::bind("cmd-shift-w", CloseTab, ctx),
        krill_app::keys::bind("cmd-shift-t", ReopenClosedTab, ctx),
        krill_app::keys::bind("cmd-shift-]", NextTab, ctx),
        krill_app::keys::bind("cmd-shift-[", PrevTab, ctx),
        krill_app::keys::bind("cmd-alt-]", NextTab, ctx),
        krill_app::keys::bind("cmd-alt-[", PrevTab, ctx),
        KeyBinding::new("ctrl-tab", NextRecentTab, ctx),
        KeyBinding::new("ctrl-shift-tab", PrevRecentTab, ctx),
        KeyBinding::new("ctrl-pagedown", NextTab, ctx),
        KeyBinding::new("ctrl-pageup", PrevTab, ctx),
        krill_app::keys::bind("cmd-d", SplitRight, ctx),
        krill_app::keys::bind("cmd-shift-d", SplitDown, ctx),
        krill_app::keys::bind("cmd-w", ClosePane, ctx),
        krill_app::keys::bind("cmd-]", FocusNextPane, ctx),
        krill_app::keys::bind("cmd-[", FocusPrevPane, ctx),
        krill_app::keys::bind("cmd-alt-down", FocusNextPane, ctx),
        krill_app::keys::bind("cmd-alt-up", FocusPrevPane, ctx),
        krill_app::keys::bind("cmd-alt-left", NavigateBack, None),
        krill_app::keys::bind("cmd-alt-right", NavigateForward, None),
        // OWE-871: ⌘⌥화살표는 히스토리·순환에 그대로 두고, 기하 방향 이동은 ⌘⌥⇧+화살표다.
        krill_app::keys::bind("cmd-alt-shift-left", FocusPaneLeft, ctx),
        krill_app::keys::bind("cmd-alt-shift-right", FocusPaneRight, ctx),
        krill_app::keys::bind("cmd-alt-shift-up", FocusPaneUp, ctx),
        krill_app::keys::bind("cmd-alt-shift-down", FocusPaneDown, ctx),
        krill_app::keys::bind("cmd-shift-enter", ToggleZoomPane, ctx),
        krill_app::keys::bind("cmd-k", ClearPane, ctx),
        krill_app::keys::bind("cmd-a", SelectAll, ctx),
        krill_app::keys::bind("cmd-shift-up", PrevSession, ctx),
        krill_app::keys::bind("cmd-shift-down", NextSession, ctx),
        krill_app::keys::bind("cmd-n", NewWorktree, ctx),
        krill_app::keys::bind("cmd-1", ActivateTab1, ctx),
        krill_app::keys::bind("cmd-2", ActivateTab2, ctx),
        krill_app::keys::bind("cmd-3", ActivateTab3, ctx),
        krill_app::keys::bind("cmd-4", ActivateTab4, ctx),
        krill_app::keys::bind("cmd-5", ActivateTab5, ctx),
        krill_app::keys::bind("cmd-6", ActivateTab6, ctx),
        krill_app::keys::bind("cmd-7", ActivateTab7, ctx),
        krill_app::keys::bind("cmd-8", ActivateTab8, ctx),
        krill_app::keys::bind("cmd-9", ActivateTab9, ctx),
        KeyBinding::new("ctrl-1", ActivateTab1, ctx),
        KeyBinding::new("ctrl-2", ActivateTab2, ctx),
        KeyBinding::new("ctrl-3", ActivateTab3, ctx),
        KeyBinding::new("ctrl-4", ActivateTab4, ctx),
        KeyBinding::new("ctrl-5", ActivateTab5, ctx),
        KeyBinding::new("ctrl-6", ActivateTab6, ctx),
        KeyBinding::new("ctrl-7", ActivateTab7, ctx),
        KeyBinding::new("ctrl-8", ActivateTab8, ctx),
        KeyBinding::new("ctrl-9", ActivateTab9, ctx),
        krill_app::keys::bind("cmd-b", ToggleSidebar, ctx),
        krill_app::keys::bind("cmd-shift-0", FocusSidebar, ctx),
        krill_app::keys::bind("cmd-shift-e", ToggleExplorer, ctx),
        krill_app::keys::bind("cmd-shift-f", ToggleSearch, ctx),
        krill_app::keys::bind("cmd-alt-b", TogglePanel, ctx),
        krill_app::keys::bind("cmd-l", TogglePanel, ctx),
        krill_app::keys::bind("cmd-shift-g", ToggleChanges, ctx),
        krill_app::keys::bind("cmd-q", Quit, None),
        krill_app::keys::bind("cmd-,", krill_app::settings::OpenSettings, None),
        krill_app::keys::bind("cmd-j", Jump, ctx),
        krill_app::keys::bind("cmd-shift-p", CommandPalette, ctx),
        krill_app::keys::bind("cmd-f", Find, ctx),
        krill_app::keys::bind("cmd-g", FindNext, ctx),
        // OWE-868: ⇧⌘G는 검색 바가 열려 있을 때만 이전 찾기다. 바깥에서는 기존 TogglePanel(우측 패널)을 유지한다.
        krill_app::keys::bind("cmd-shift-g", FindPrev, Some(krill_app::search::CONTEXT)),
        krill_app::keys::bind("cmd-c", Copy, ctx),
        krill_app::keys::bind("cmd-v", Paste, ctx),
    ]);
    // macOS gpui는 ⇧+[ / ⇧+]를 { / }로 정규화하며 Shift 플래그를 지운다.
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-}", NextTab, ctx),
        KeyBinding::new("cmd-{", PrevTab, ctx),
    ]);
    new_session::bind_keys(cx);
    files::bind_keys(cx);
    popout::bind_keys(cx);
    sidebar::bind_keys(cx);
    cx.on_action(|_: &Quit, cx| files::quit_app(cx));
    security::bind_keys(cx);
}

/// 드래그 중인 분할선. 분할은 활성 탭 루트에서의 경로로 가리킨다.
#[derive(Clone, Debug, PartialEq)]
enum Drag {
    Sidebar,
    Panel,
    Split(Vec<Branch>),
}

/// 드래그 중 커서를 따라다니는 미리보기. 분할선은 보여줄 것이 없다.
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

#[derive(Clone, PartialEq)]
struct HostSource {
    specs: Vec<krill_app::hosts::RemoteHostSpec>,
    external: Vec<(
        krill_app::hosts::RemoteHostSpec,
        PathBuf,
        krill_hosts::HostConnectionInfo,
    )>,
    paths: Option<Paths>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent window, panel, layout restore and service lifecycle flags"
)]
pub struct Shell {
    language: krill_app::i18n::Language,
    sidebar: gpui::Entity<sidebar::Sidebar>,
    _sidebar_selection: Subscription,
    _sidebar_new_session: Subscription,
    _sidebar_close_session: Subscription,
    _sidebar_continue_session: Subscription,
    _sidebar_reconnect: Subscription,
    _sidebar_disconnect: Subscription,
    _sidebar_relisted: Subscription,
    _sidebar_tabs: Subscription,
    layout: Layout,
    sidebar_collapsed: bool,
    panel_collapsed: bool,
    path: Option<PathBuf>,
    saver: persist::Saver,
    layout_loading: bool,
    restore_session: Option<(krill_app::hosts::HostId, krill_proto::SessionId)>,
    local_generation: u64,
    ssh_generation: u64,
    orch_snapshot: Arc<krill_proto::orchestration::OrchestrationStore>,
    orch_poll: Option<Task<()>>,
    fits: fit::Pending,
    focus: FocusHandle,
    /// 세션 데몬 감시 연결 상태(MK-025·MK-026). 끊겼거나 붙지 못했으면 가운데 열 위에 배너로 보인다.
    link: Link,
    daemon_task: Option<Task<()>>,
    /// 데몬 경로. [`Shell::connect_daemon`] 전에는 칸을 세션에 붙이지 않는다(레이아웃 테스트).
    paths: Option<Paths>,
    recording: krill_app::recording::Status,
    update_ready: Option<PathBuf>,
    /// 보이는 칸의 세션 연결(OWE-490).
    live: HashMap<PaneId, Live>,
    pane_liveness: Option<Task<()>>,
    creations: HashMap<PaneId, Arc<std::sync::Mutex<close::Creation>>>,
    /// 칸마다 마지막으로 본 창 제목(탭 이름). 숨긴 칸도 남긴다.
    titles: HashMap<PaneId, String>,
    agents: Option<AgentSettings>,
    agents_busy: bool,
    agent_task: Option<Task<()>>,
    agent_client: Arc<dyn pane::AgentClient>,
    /// 마지막 입력 알림(누르면 닫힘).
    notice: Option<String>,
    /// 데몬 재연결 뒤 실제 세션 목록이 도착했다는 확인. [`RECONNECTED_FOR`] 뒤 사라진다.
    reconnected: Option<String>,
    reconnected_task: Option<Task<()>>,
    /// 자리 비운 사이 요약의 기준 시각. 이 프로세스의 첫 메인 창이 데몬에 처음 붙을 때 한 번 쓴다.
    away_since: Option<u64>,
    /// 자리 비운 사이 요약 한 줄(누르면 닫힘).
    away: Option<String>,
    away_task: Option<Task<()>>,
    /// 마지막 생존 시각 주기 저장([`away::HEARTBEAT`]).
    heartbeat: Option<Task<()>>,
    app_quit: Option<Subscription>,
    window: Option<AnyWindowHandle>,
    activation: Option<Subscription>,
    bounds_observer: Option<Subscription>,
    diff: gpui::Entity<krill_app::diff::Panel>,
    _diff_collapse: Subscription,
    _diff_send: Subscription,
    /// 어느 칸에도 없는 세션(OWE-492). 칸·탭을 닫아도 세션은 여기 남는다.
    detached: gpui::Entity<detached::Panel>,
    _detached_open: Subscription,
    /// 열린 Jump 팔레트(MK-057)와 그 이벤트 구독.
    jump: Option<(gpui::Entity<jump::Palette>, Subscription)>,
    /// OWE-723 프로젝트 추가 모달.
    add_project: Option<(gpui::Entity<krill_app::add_project::Dialog>, Subscription)>,
    /// MK-182 새 세션에 요약 이어하기 모달.
    continue_session: Option<(
        gpui::Entity<krill_app::continue_session::Dialog>,
        Subscription,
    )>,
    /// OWE-903 온보딩 모달(메인 창 안 `ui::modal_backdrop`).
    onboarding: Option<overlays::OnboardingModal>,
    /// OWE-903 짝짓기 모달(메인 창 안 `ui::modal_backdrop`).
    /// OWE-903 렌더에서 지연 생성할 모달 요청.
    pending: overlays::PendingOverlays,
    /// 인앱 에이전트 대시보드 모달.
    pub(crate) dashboard: Option<gpui::Entity<popout::Dashboard>>,
    /// 열린 터미널 검색 바(MK-059)와 그 이벤트 구독.
    search: Option<(gpui::Entity<krill_app::search::Bar>, Subscription)>,
    /// Resource Manager(MK-071).
    resources: gpui::Entity<krill_app::resources::Panel>,
    _resources_open: Subscription,
    /// OWE-851: 열려 있는 상태바 드롭다운 메뉴 수. 0이 아니면 호버 툴팁을 숨긴다.
    status_menus_open: usize,
    history: gpui::Entity<krill_app::agent_history::Panel>,
    _history_open: Subscription,
    appearance: Option<Box<dyn std::io::Write>>,
    terminal_theme: Theme,
    cell_pixels: Option<CellSize>,
    /// 포커스 칸 커서 깜박임 타이머와 꺼진 순간 여부(OWE-658). 깜박일 커서가 없으면 `None`(유휴 프레임 0).
    cursor_blink: Option<(Task<()>, bool)>,
    /// 데몬 연결 중 계정 사용량 주기 갱신(OWE-702). 링크가 끊기면 `None`(취소).
    accounts_poll: Option<Task<()>>,
    /// 기동 시점 및 4시간 주기 백그라운드 앱 업데이트 점검(MK-156).
    update_poll: Option<Task<()>>,
    _theme_observer: Subscription,
    _config_observer: Subscription,
    system_appearance: Option<Subscription>,
    security: krill_app::security::State,
    paste_queue: security::PasteQueue,
    zoomed_pane: Option<PaneId>,
    /// 가운데 영역 파일(에디터) 탭(MK-119).
    files: files::Files,
    #[cfg(test)]
    close_call: Option<krill_app::resources::Call>,
    #[cfg(test)]
    ime_discards: Rc<std::cell::Cell<usize>>,
    /// 닫기로 데몬 종료를 요청한 세션(테스트 확인용).
    #[cfg(test)]
    closed_sessions: Vec<krill_proto::SessionId>,
    /// 닫은 탭·칸의 (호스트, 폴더). 세션은 닫을 때 끝나므로 ⌘⇧T는 이 폴더에 새 탭을 연다.
    closed_dirs: Vec<(krill_app::hosts::HostId, PathBuf)>,
    pub(super) usage_refresh: Option<Task<()>>,
    /// 상태바 사용량 팝업의 "사용량 초기화" 1단계(무장). 메뉴가 닫혔다 다시 열리면 확인 행을 보인다.
    pub(super) reset_armed: Option<(krill_proto::usage::ProviderKind, String)>,
    account_handoff_poll: Option<Task<()>>,
    pub(super) accounts_snapshot: krill_proto::usage::AccountsSnapshot,
    pub(super) ssh_hosts: Vec<krill_app::ssh_hosts::SshHost>,
    pub(super) host_manager: krill_app::hosts::HostManager,
    /// 같은 기기를 가리키는 중복 등록 id → 대표 id(`ssh -G` 기준, 백그라운드에서 구한다).
    host_aliases: BTreeMap<String, String>,
    host_statuses: Vec<(
        krill_app::hosts::RemoteHostSpec,
        krill_app::hosts::HostStatus,
    )>,
    host_targets: HashMap<krill_app::hosts::HostId, krill_app::hosts::HostTarget>,
    host_generation: u64,
    #[cfg(test)]
    next_host_io: Option<tokio::sync::oneshot::Receiver<()>>,
    host_source: Option<HostSource>,
    host_refresh_running: bool,
    host_refresh_again: bool,
    host_pending_reconnects: Vec<String>,
    external_hosts: Vec<krill_hosts::ExternalHost>,
    link_hosts: Option<Arc<krill_app::hosts::link::LinkHosts>>,
    link_poll: Option<Task<()>>,
    pub(super) launch_host: krill_app::hosts::HostId,
    /// 상태바 포트 감지 메모(AS-24): 세션별 마지막 화면 텍스트와 감지 결과.
    /// 렌더마다 모든 칸의 화면 텍스트에 정규식을 돌리지 않고 텍스트가 바뀔 때만 다시 감지한다.
    ports_cache: std::cell::RefCell<HashMap<u64, popout::PortCache>>,
    /// 인앱 설정 페이지(`SettingsView`, Orca `activeView === 'settings'` 패리티).
    pub(super) settings: Option<gpui::Entity<krill_app::settings::SettingsView>>,
    /// 플로팅 자식 터미널 창 여부(설정 호스트에서 제외).
    pub(super) role: WindowRole,
    pending_focus: PendingFocus,
    /// OWE-734: 탭 우클릭 컨텍스트 메뉴 `(탭 인덱스, 클릭 위치)`.
    pub(super) tab_context_menu: Option<(usize, Point<Pixels>)>,
    /// OWE-734: 고정된 탭을 닫으려 할 때 확인 대화상자 대상 탭 인덱스.
    pub(super) confirm_close_tab: Option<usize>,
    /// OWE-872: 실행 중인 탭·칸 닫기 확인 대기(대상 + '다시 묻지 않기' 체크 상태).
    pub(super) pending_running_close: Option<(PendingRunningClose, bool)>,
    /// OWE-870: 탭 이름 변경 대화상자(인라인 편집 대신 모달 + `InputState`).
    tab_rename: Option<tabs::TabRename>,
    /// 워크트리·탭·설정 뷰 탐색 히스토리(MK-176 / OWE-730).
    pub(super) nav_history: NavHistory,
    /// OWE-946 / MK-243: 데스크톱 Hybrid Chat `[Chat | Term]` 오버레이 상태.
    pub(super) chat: chat::ChatState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WindowRole {
    Main,
    Floating,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PendingFocus {
    None,
    Settings,
    Workspace,
}

impl Shell {
    /// 저장된 레이아웃으로 시작한다. 없거나 읽을 수 없으면 기본 레이아웃.
    pub fn new(path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        if cx.has_global::<gpui_component::Theme>() {
            let mode = match krill_app::settings::current(cx).theme {
                krill_app::settings::ThemeChoice::Light => gpui_component::ThemeMode::Light,
                krill_app::settings::ThemeChoice::Dark => gpui_component::ThemeMode::Dark,
                krill_app::settings::ThemeChoice::System => cx.window_appearance().into(),
            };
            krill_app::ui::apply(mode, cx);
        }
        let notice = None;
        let layout = Layout::default();
        let restore_path = path.clone();
        let sidebar_collapsed = layout.sidebar_collapsed;
        let panel_collapsed = layout.panel_collapsed;
        let panel_tab = layout.panel_tab;
        let sidebar_path = path.as_deref().map(|p| p.with_file_name("sidebar.bin"));
        let sidebar = cx.new(|cx| {
            let mut s = sidebar::Sidebar::new(sidebar_path);
            if sidebar_collapsed {
                s.set_visible(false, cx);
            }
            s
        });
        let sidebar_tabs = cx.observe(&sidebar, |this, sidebar, cx| {
            this.sync_live_cwds(sidebar.read(cx));
            cx.notify();
        });
        let selection = cx.subscribe(&sidebar, |this, sidebar, e: &sidebar::Selected, cx| {
            let host = sidebar
                .update(cx, |s, _| s.take_action_host())
                .unwrap_or(krill_app::hosts::HostId::Local);
            this.open_session_on(host, e.0, cx);
        });
        let new_session = cx.subscribe(&sidebar, |this, sidebar, e: &sidebar::NewSession, cx| {
            let host = sidebar
                .update(cx, |s, _| s.take_action_host())
                .unwrap_or(krill_app::hosts::HostId::Local);
            let initial_prompt = sidebar.update(cx, |s, _| s.take_initial_prompt(&e.cwd));
            this.new_folder_tab_on(host, &e.cwd, e.program.clone(), cx);
            if let Some(prompt) = initial_prompt
                && let Some(pane) = this.layout.focused()
            {
                let agent_id = match &e.program {
                    krill_proto::control::Program::Agent { id } => id.clone(),
                    _ => "shell".to_owned(),
                };
                this.start_handoff_delivery(
                    pane,
                    agent_id,
                    prompt.into_bytes(),
                    handoff::BOOT_TIMEOUT,
                    cx,
                );
            }
            if let Some(win) = this.window {
                let focus = this.focus.clone();
                win.update(cx, |_, window, cx| window.focus(&focus, cx))
                    .ok();
            }
        });
        let sidebar_reconnect =
            cx.subscribe(&sidebar, |this, _, e: &sidebar::ReconnectHost, cx| {
                if let Some(id) = e.0.remote_id() {
                    this.sync_hosts_after_reconnect(Some(id.to_owned()), cx);
                }
                this.sync_hosts(cx);
                this.sync(cx);
                cx.notify();
            });
        let sidebar_disconnect =
            cx.subscribe(&sidebar, |this, _, e: &sidebar::DisconnectHost, cx| {
                if let Some(id) = e.0.remote_id() {
                    if let Some(raw) = id.strip_prefix("iroh:")
                        && let Some(service) = &this.link_hosts
                    {
                        service.disconnect(raw);
                    }
                    this.host_manager.disconnect_now(id);
                }
                this.sync_hosts(cx);
                this.sync(cx);
                cx.notify();
            });
        let sidebar_relisted = cx.subscribe(&sidebar, |this, _, e: &sidebar::Relisted, cx| {
            this.show_reconnected(e.0, cx);
        });
        let close_session =
            cx.subscribe(&sidebar, |this, sidebar, e: &sidebar::CloseSession, cx| {
                let host = sidebar
                    .update(cx, |s, _| s.take_action_host())
                    .unwrap_or(krill_app::hosts::HostId::Local);
                this.request_close_sidebar_session(host, e.0, cx);
            });
        let continue_session = cx.subscribe(
            &sidebar,
            |this, sidebar, e: &sidebar::ContinueSession, cx| {
                let host = sidebar
                    .update(cx, |s, _| s.take_action_host())
                    .unwrap_or(krill_app::hosts::HostId::Local);
                this.continue_sidebar_session(&host, e.0, cx);
            },
        );
        // OWE-854: 사이드바 세션 '이름 변경'은 그 세션 탭을 열고(없으면 새로) 탭 이름 변경(OWE-870)을 띄운다.
        cx.subscribe(&sidebar, |this, sidebar, e: &sidebar::RenameSession, cx| {
            let host = sidebar
                .update(cx, |s, _| s.take_action_host())
                .unwrap_or(krill_app::hosts::HostId::Local);
            this.open_session_on(host, e.0, cx);
            let (Some(win), tab) = (this.window, this.layout.active()) else {
                return;
            };
            let shell = cx.entity().downgrade();
            cx.defer(move |cx| {
                win.update(cx, |_, window, cx| {
                    shell
                        .update(cx, |s, cx| s.begin_tab_rename(tab, window, cx))
                        .ok();
                })
                .ok();
            });
        })
        .detach();
        // OWE-654·723: 사이드바 폴더 아이콘·빈 상태 '폴더 열기'는 Orca처럼 프로젝트 추가 모달을 연다.
        cx.subscribe(&sidebar, |this, _, _: &sidebar::OpenFolder, cx| {
            let Some(win) = this.window else {
                Self::choose_folder(cx);
                return;
            };
            let shell = cx.entity().downgrade();
            cx.defer(move |cx| {
                win.update(cx, |_, window, cx| {
                    shell
                        .update(cx, |s, cx| s.open_add_project(window, cx))
                        .ok();
                })
                .ok();
            });
        })
        .detach();
        let detached = cx.new(|_| detached::Panel::default());
        let detached_open = cx.subscribe(&detached, |this, _, &detached::Open(s), cx| {
            this.open_session(s, cx);
        });
        // 바이너리 단위 테스트는 라이브러리를 cfg(test) 없이 링크하므로, 기본 Panel이면 이 Mac의 실제 krilld에 붙어
        // 사용자 기록 볼트 결과가 팔레트에 섞인다. 테스트에서는 데몬 호출을 끊는다.
        #[cfg(not(test))]
        let history = cx.new(|_| krill_app::agent_history::Panel::default());
        #[cfg(test)]
        let history = cx.new(|_| {
            krill_app::agent_history::Panel::new(std::sync::Arc::new(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "daemon disabled in unit tests",
                ))
            }))
        });
        let history_open = cx.subscribe(
            &history,
            |this, _, &krill_app::agent_history::Open(s), cx| {
                if !this.panel_collapsed
                    && this.diff.read(cx).active_tab() == krill_app::diff::PanelTab::History
                {
                    this.toggle_panel(cx);
                }
                this.open_session(s, cx);
            },
        );
        cx.subscribe(
            &history,
            |this, _, _: &krill_app::agent_history::Toggle, cx| {
                this.toggle_history(cx);
            },
        )
        .detach();
        cx.observe(&history, |_, _, cx| cx.notify()).detach();
        // OWE-851: 리소스 행 클릭도 메인 창 해당 세션으로 이동한다.
        let resources_sidebar = sidebar.clone();
        let resources = cx.new(|_| {
            let mut panel = krill_app::resources::Panel::default();
            panel.set_sidebar(&resources_sidebar);
            panel
        });
        let resources_open =
            cx.subscribe(&resources, |this, _, &krill_app::resources::Open(s), cx| {
                this.open_session(s, cx);
            });
        let folder = layout
            .focused()
            .and_then(|p| layout.launch(p))
            .map_or_else(folder::default_folder, |c| {
                PathBuf::from(std::ffi::OsString::from_vec(c.cwd.clone()))
            });
        let theme_observer = cx.observe_global::<gpui_component::Theme>(|this, cx| {
            this.push_appearance(cx);
        });
        let config_observer = cx.observe_global::<krill_app::settings::Config>(|this, cx| {
            this.on_config_changed(cx);
        });
        // 점검 스냅샷이 바뀌면 상태바 경고도 다시 그린다(구독은 앱 수명 동안 둔다).
        cx.observe_global::<krill_app::onboarding::ChecksSnapshot>(|_, cx| cx.notify())
            .detach();
        // 필요한 순간에만 뜨는 조치 배너(OWE-714).
        cx.observe_global::<krill_app::onboarding::actions::FixNeeds>(|_, cx| cx.notify())
            .detach();
        cx.observe_global::<krill_app::accounts::installed::Installed>(|_, cx| cx.notify())
            .detach();
        let diff = cx.new(|cx| {
            let mut panel = krill_app::diff::Panel::unconnected(folder, cx);
            panel.set_split_center(true);
            panel.set_tab(panel_tab, cx);
            if panel_collapsed {
                panel.set_visible(false, cx);
            }
            panel.set_history(history.clone(), cx);
            panel
        });
        cx.observe(&diff, |this, diff, cx| {
            let tab = diff.read(cx).active_tab();
            if this.layout.set_panel_tab(tab) {
                this.save();
            }
        })
        .detach();
        let diff_send = Self::subscribe_diff(&diff, cx);
        cx.subscribe(&diff, |this, _, e: &krill_app::diff::OpenFile, cx| {
            // 단일 클릭 미리보기면 같은 칸을 재사용하고, 고정 열기면 탭을 확정한다(OWE-849).
            let preview = this.diff.update(cx, |d, _| d.take_open_file_preview());
            if preview {
                this.open_file_preview(e.0.clone(), cx);
            } else {
                this.open_file(e.0.clone(), cx);
            }
        })
        .detach();
        cx.subscribe(&diff, |this, _, e: &krill_app::diff::OpenFileAt, cx| {
            this.open_file_at(e.path.clone(), e.line, e.column, cx);
        })
        .detach();
        let diff_collapse =
            cx.subscribe(&diff, |this, _, _: &krill_app::diff::ToggleCollapse, cx| {
                this.toggle_panel(cx);
            });
        let terminal_theme = krill_app::ui::terminal_theme(cx);
        let accounts_snapshot = krill_proto::usage::AccountsSnapshot::default();
        let ssh_hosts = Vec::new();
        let host_manager = krill_app::hosts::HostManager::new();

        let weak = cx.weak_entity();
        krill_app::settings::register_host(cx, move |cmd, cx| {
            let entity = weak.upgrade()?;
            if entity.read(cx).role == WindowRole::Floating {
                return Some(false);
            }
            Some(entity.update(cx, |this, cx| match cmd {
                krill_app::settings::HostCommand::Login { session } => {
                    this.open_session(session, cx);
                    true
                }
                krill_app::settings::HostCommand::Open { tab, window } => {
                    if window.is_some_and(|w| this.window != Some(w)) {
                        return false;
                    }
                    this.open_settings(tab, None, cx);
                    if window.is_none()
                        && let Some(host_window) = this.window
                    {
                        cx.defer(move |cx| {
                            host_window
                                .update(cx, |_, window, _| window.activate_window())
                                .ok();
                        });
                    }
                    true
                }
                krill_app::settings::HostCommand::Close { window, view } => {
                    let Some(active_view) = &this.settings else {
                        return false;
                    };
                    if let Some(view_id) = view {
                        if active_view.entity_id() != view_id {
                            return false;
                        }
                    } else if this.window != Some(window) {
                        return false;
                    }
                    this.close_settings(None, cx);
                    true
                }
            }))
        });
        let mut shell = Self {
            language: krill_app::i18n::language(),
            sidebar,
            appearance: None,
            terminal_theme,
            cell_pixels: None,
            cursor_blink: None,
            _theme_observer: theme_observer,
            _config_observer: config_observer,
            system_appearance: None,
            _sidebar_selection: selection,
            _sidebar_new_session: new_session,
            _sidebar_close_session: close_session,
            _sidebar_continue_session: continue_session,
            _sidebar_reconnect: sidebar_reconnect,
            _sidebar_disconnect: sidebar_disconnect,
            _sidebar_relisted: sidebar_relisted,
            _sidebar_tabs: sidebar_tabs,
            layout,
            sidebar_collapsed,
            panel_collapsed,
            layout_loading: path.is_some(),
            restore_session: None,
            path,
            saver: persist::Saver::new(cx),
            local_generation: 0,
            ssh_generation: 0,
            orch_snapshot: Arc::default(),
            orch_poll: None,
            fits: fit::Pending::default(),
            focus: cx.focus_handle(),
            link: Link::Connecting,
            daemon_task: None,
            paths: None,
            recording: krill_app::recording::Status::default(),
            update_ready: None,
            live: HashMap::new(),
            pane_liveness: None,
            creations: HashMap::new(),
            titles: HashMap::new(),
            agents: None,
            agents_busy: false,
            agent_task: None,
            agent_client: Arc::new(pane::DaemonAgents),
            notice,
            reconnected: None,
            reconnected_task: None,
            away_since: None,
            away: None,
            away_task: None,
            heartbeat: None,
            app_quit: None,
            window: None,
            activation: None,
            bounds_observer: None,
            diff,
            _diff_collapse: diff_collapse,
            _diff_send: diff_send,
            detached,
            _detached_open: detached_open,
            jump: None,
            add_project: None,
            continue_session: None,
            onboarding: None,
            pending: overlays::PendingOverlays::default(),
            dashboard: None,
            search: None,
            history,
            _history_open: history_open,
            resources,
            _resources_open: resources_open,
            status_menus_open: 0,
            security: krill_app::security::State::default(),
            paste_queue: security::PasteQueue::default(),
            zoomed_pane: None,
            files: files::Files::default(),
            #[cfg(test)]
            close_call: None,
            #[cfg(test)]
            ime_discards: Rc::default(),
            #[cfg(test)]
            closed_sessions: Vec::new(),
            closed_dirs: Vec::new(),
            accounts_snapshot,
            usage_refresh: None,
            reset_armed: None,
            account_handoff_poll: None,
            accounts_poll: None,
            update_poll: None,
            ssh_hosts,
            host_aliases: BTreeMap::new(),
            host_manager,
            host_statuses: Vec::new(),
            host_targets: HashMap::new(),
            host_generation: 0,
            #[cfg(test)]
            next_host_io: None,
            host_source: None,
            host_refresh_running: false,
            host_refresh_again: false,
            host_pending_reconnects: Vec::new(),
            external_hosts: Vec::new(),
            link_hosts: None,
            link_poll: None,
            launch_host: krill_app::hosts::HostId::Local,
            ports_cache: std::cell::RefCell::default(),
            settings: None,
            role: WindowRole::Main,
            pending_focus: PendingFocus::None,
            tab_context_menu: None,
            confirm_close_tab: None,
            pending_running_close: None,
            tab_rename: None,
            nav_history: NavHistory::default(),
            chat: chat::ChatState::default(),
        };
        shell.sync_sidebar_custom_titles(cx);
        shell.record_workspace_nav(cx);
        Self::register_overlay_hosts(cx);
        shell.refresh_local_state(cx);
        shell.orch_poll = Some(Self::poll_orchestration(cx));
        if let Some(path) = restore_path {
            shell.restore_layout(path, cx);
        }
        shell
    }

    /// Restore without holding the UI executor over filesystem reads or migration.
    fn restore_layout(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let baseline = self.layout.clone();
        cx.spawn(async move |this, cx| {
            let (loaded, writable) = cx
                .background_executor()
                .spawn(async move {
                    let loaded = Layout::load(&path);
                    let writable = (loaded.is_ok()
                        || loaded
                            .as_ref()
                            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound))
                        || matches!(path.try_exists(), Ok(false));
                    (loaded, writable)
                })
                .await;
            this.update(cx, |this, cx| {
                this.layout_loading = false;
                if this.role != WindowRole::Main {
                    return;
                }
                if !writable {
                    this.path = None;
                }
                let bounds = this.layout.window_bounds();
                let mut baseline = baseline;
                if let Some(bounds) = bounds {
                    baseline.set_window_bounds(bounds);
                }
                let edited_while_loading = this.layout != baseline;
                match loaded {
                    Ok(mut layout) if this.layout == baseline => {
                        if let Some(bounds) = bounds {
                            layout.set_window_bounds(bounds);
                        }
                        this.sidebar_collapsed = layout.sidebar_collapsed;
                        this.panel_collapsed = layout.panel_collapsed;
                        let panel_tab = layout.panel_tab;
                        this.layout = layout;
                        this.sidebar
                            .update(cx, |s, cx| s.set_visible(!this.sidebar_collapsed, cx));
                        this.diff.update(cx, |d, cx| {
                            d.set_visible(!this.panel_collapsed, cx);
                            d.set_tab(panel_tab, cx);
                        });
                        this.sync_focused_session(cx);
                        this.sync_sidebar_custom_titles(cx);
                        this.record_workspace_nav(cx);
                    }
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        let message = if writable {
                            text::LAYOUT_RECOVERED.get()
                        } else {
                            text::LAYOUT_UNREADABLE.get()
                        };
                        this.notice = Some(format!("{message}: {e}"));
                    }
                    _ => {}
                }
                this.sync(cx);
                if let Some((host, session)) = this.restore_session.take() {
                    this.open_session_on(host, session, cx);
                }
                if edited_while_loading {
                    this.save();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Snapshot reads and file locks happen only on this serial background loop.
    fn poll_orchestration(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok((dir, screens, mut previous)) = this.read_with(cx, |this, cx| {
                    let mut cache = this.ports_cache.borrow_mut();
                    let mut previous = HashMap::new();
                    let screens = this
                        .live
                        .values()
                        .filter_map(|live| {
                            let id = live.pane.session()?.0;
                            let screen = live.pane.shown();
                            if cache.get(&id).is_some_and(|entry| entry.matches(&screen)) {
                                return None;
                            }
                            if let Some(old) = cache.remove(&id) {
                                previous.insert(id, old.into_data());
                            }
                            Some((id, screen))
                        })
                        .collect::<Vec<_>>();
                    (this.state_dir(cx), screens, previous)
                }) else {
                    return;
                };
                let copies = screens
                    .iter()
                    .map(|(id, screen)| (*id, (**screen).clone()))
                    .collect::<Vec<_>>();
                let (snapshot, ports) = cx
                    .background_executor()
                    .spawn(async move {
                        let snapshot = dir.map(|dir| {
                            Arc::new(krill_proto::orchestration::OrchestrationStore::load_cached(
                                &dir,
                            ))
                        });
                        let ports = copies
                            .into_iter()
                            .map(|(id, screen)| {
                                (id, popout::detect_ports(id, &screen, previous.remove(&id)))
                            })
                            .collect::<HashMap<_, _>>();
                        (snapshot, ports)
                    })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        if let Some(snapshot) = snapshot {
                            this.orch_snapshot = snapshot;
                        }
                        let mut cache = this.ports_cache.borrow_mut();
                        cache.retain(|id, _| {
                            this.live
                                .values()
                                .any(|live| live.pane.session().is_some_and(|sid| sid.0 == *id))
                        });
                        cache.extend(ports.into_iter().filter_map(|(id, (text, entries))| {
                            let (_, screen) = screens.iter().find(|(sid, _)| *sid == id)?;
                            this.live
                                .values()
                                .any(|live| {
                                    live.pane.session().is_some_and(|sid| sid.0 == id)
                                        && Rc::ptr_eq(screen, &live.pane.shown())
                                })
                                .then(|| {
                                    (id, popout::PortCache::from_result(screen, text, entries))
                                })
                        }));
                        drop(cache);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        })
    }

    /// OWE-854: 탭의 사용자 지정 제목을 사이드바 세션 행 라벨에 동기화한다.
    pub(super) fn sync_sidebar_custom_titles(&self, cx: &mut Context<Self>) {
        let mut custom = std::collections::BTreeMap::new();
        for (i, tab) in self.layout.tabs().iter().enumerate() {
            let Some(title) = self.layout.tab_title(i) else {
                continue;
            };
            if let Some(sid) = self.layout.session(tab.focused) {
                custom.insert(sid, title.to_owned());
            }
            for pane in self.layout.tab_panes(i) {
                if let Some(sid) = self.layout.session(pane) {
                    custom.entry(sid).or_insert_with(|| title.to_owned());
                }
            }
        }
        self.sidebar
            .update(cx, |s, cx| s.set_custom_titles(custom, cx));
    }

    /// 사이드바가 데몬에서 읽은 라이브 세션들의 최신 `cwd`를 재스폰용으로 따로 보관한다(OWE-842 시나리오 B).
    /// `layout.launches`의 폴더(워크트리 루트·`folder()`·Open in·새 에이전트 기준)는 건드리지 않는다.
    /// 재부팅 뒤에도 살아남도록 꼬리(`LayoutTail.respawn_cwds`)로 영속한다.
    pub(super) fn sync_live_cwds(&mut self, sidebar: &sidebar::Sidebar) {
        let mut changed = false;
        let panes: Vec<PaneId> = (0..self.layout.tabs().len())
            .flat_map(|i| self.layout.tab_panes(i))
            .collect();
        for pane in panes {
            let Some(sid) = self.layout.session(pane) else {
                continue;
            };
            let host = self.layout.host(pane);
            let Some(live_cwd) = sidebar
                .session_on(host, sid)
                .and_then(|s| s.cwd.as_deref())
                .filter(|p| !p.as_os_str().is_empty())
            else {
                continue;
            };
            let cwd_bytes = live_cwd.as_os_str().as_bytes();
            if self.layout.respawn_cwd(pane) != Some(cwd_bytes) {
                changed |= self.layout.set_respawn_cwd(pane, cwd_bytes.to_vec());
            }
        }
        if changed {
            self.save();
        }
    }

    /// 칸이 사이드바의 로컬 git 워크트리에 속할 때만 그 워크트리 루트 경로를 돌려준다.
    /// 비-git 폴더 탭·에디터 파일 탭·원격 호스트 탭은 `None`이다(OWE-757).
    pub(super) fn worktree_for_pane(&self, pane: PaneId, cx: &App) -> Option<PathBuf> {
        if self.layout.file(pane).is_some() || self.layout.host(pane).is_remote() {
            return None;
        }
        let sidebar = self.sidebar.read(cx);
        if let Some(sid) = self.layout.session(pane)
            && let Some(folder) = sidebar.folder_on(&krill_app::hosts::HostId::Local, sid)
            && let Some(wt) = sidebar.worktree_for_path(folder)
        {
            return Some(wt);
        }
        let launch = self.layout.launch(pane)?;
        let cwd = std::path::Path::new(std::ffi::OsStr::from_bytes(&launch.cwd));
        sidebar.worktree_for_path(cwd)
    }

    fn find_pane_for_worktree(&self, wt: &std::path::Path, cx: &App) -> Option<(usize, PaneId)> {
        for i in 0..self.layout.tabs().len() {
            for p in self.layout.tab_panes(i) {
                if self.worktree_for_pane(p, cx).as_deref() == Some(wt) {
                    return Some((i, p));
                }
            }
        }
        None
    }

    /// 현재 포커스된 워크스페이스 칸(탭·세션·워크트리)을 탐색 히스토리에 기록한다.
    pub(super) fn record_workspace_nav(&mut self, cx: &App) {
        if self.settings.is_some() {
            return;
        }
        let Some(pane) = self.layout.focused() else {
            return;
        };
        let session = self.layout.session(pane);
        let worktree = self.worktree_for_pane(pane, cx);
        self.nav_history.push(NavEntry::Workspace {
            pane,
            session,
            worktree,
        });
    }

    fn is_entry_reachable(&self, entry: &NavEntry, cx: &App) -> bool {
        match entry {
            NavEntry::Settings(_) => true,
            NavEntry::Workspace {
                pane,
                session,
                worktree,
            } => {
                if self.layout.pane_exists(*pane) {
                    return true;
                }
                let sidebar = self.sidebar.read(cx);
                if let Some(wt) = worktree {
                    if sidebar.has_trees() && !sidebar.contains_worktree(wt) {
                        return false;
                    }
                    if self.find_pane_for_worktree(wt, cx).is_some() {
                        return true;
                    }
                }
                session.is_some_and(|sid| sidebar.session(sid).is_some())
            }
        }
    }

    fn is_same_nav_target(cur: Option<&NavEntry>, cand: &NavEntry) -> bool {
        match (cur, cand) {
            (
                Some(NavEntry::Workspace { pane: cur_pane, .. }),
                NavEntry::Workspace {
                    pane: cand_pane, ..
                },
            ) => cur_pane == cand_pane,
            (Some(cur), cand) => cur == cand,
            (None, _) => false,
        }
    }

    fn prev_nav_index(&self, cx: &App) -> Option<usize> {
        if self.nav_history.index >= self.nav_history.entries.len() {
            return None;
        }
        let cur = self.nav_history.entries.get(self.nav_history.index);
        let end = self.nav_history.index.min(self.nav_history.entries.len());
        (0..end).rev().find(|&i| {
            let Some(cand) = self.nav_history.entries.get(i) else {
                return false;
            };
            !Self::is_same_nav_target(cur, cand) && self.is_entry_reachable(cand, cx)
        })
    }

    fn next_nav_index(&self, cx: &App) -> Option<usize> {
        if self.nav_history.index >= self.nav_history.entries.len() {
            return None;
        }
        let cur = self.nav_history.entries.get(self.nav_history.index);
        ((self.nav_history.index + 1)..self.nav_history.entries.len()).find(|&i| {
            let Some(cand) = self.nav_history.entries.get(i) else {
                return false;
            };
            !Self::is_same_nav_target(cur, cand) && self.is_entry_reachable(cand, cx)
        })
    }

    #[must_use]
    pub fn can_navigate_back(&self, cx: &App) -> bool {
        self.prev_nav_index(cx).is_some()
    }

    #[must_use]
    pub fn can_navigate_forward(&self, cx: &App) -> bool {
        self.next_nav_index(cx).is_some()
    }

    /// 탐색 히스토리에서 이전 워크트리·탭·설정 뷰로 이동한다(`⌘⌥←`).
    pub fn navigate_back(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) -> bool {
        let Some(target_idx) = self.prev_nav_index(cx) else {
            return false;
        };
        let Some(entry) = self.nav_history.entries.get(target_idx).cloned() else {
            return false;
        };
        self.nav_history.index = target_idx;
        self.apply_nav_entry(entry, window, cx);
        true
    }

    /// 탐색 히스토리에서 다음 워크트리·탭·설정 뷰로 이동한다(`⌘⌥→`).
    pub fn navigate_forward(
        &mut self,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target_idx) = self.next_nav_index(cx) else {
            return false;
        };
        let Some(entry) = self.nav_history.entries.get(target_idx).cloned() else {
            return false;
        };
        self.nav_history.index = target_idx;
        self.apply_nav_entry(entry, window, cx);
        true
    }

    fn apply_nav_entry(
        &mut self,
        entry: NavEntry,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.nav_history.navigating = true;
        match entry {
            NavEntry::Settings(tab) => {
                self.open_settings(tab, window, cx);
            }
            NavEntry::Workspace {
                pane,
                session,
                worktree,
            } => {
                let mut window = window;
                if self.dashboard.is_some() {
                    self.close_dashboard(window.as_deref_mut(), cx);
                }
                if self.settings.is_some() {
                    self.close_settings(window.as_deref_mut(), cx);
                }
                let mut target_tab_pane = self
                    .layout
                    .tabs()
                    .iter()
                    .enumerate()
                    .find_map(|(i, t)| t.root.contains(pane).then_some((i, pane)));
                if target_tab_pane.is_none()
                    && let Some(wt) = &worktree
                {
                    target_tab_pane = self.find_pane_for_worktree(wt, cx);
                }
                if let Some((tab_idx, target_pane)) = target_tab_pane {
                    self.change(true, cx, |l| {
                        l.activate(tab_idx);
                        l.focus(target_pane);
                    });
                    if let Some(w) = window {
                        w.focus(&self.focus, cx);
                    }
                } else if let Some(sid) = session {
                    let host = self
                        .sidebar
                        .read(cx)
                        .host_for_session(sid)
                        .unwrap_or(krill_app::hosts::HostId::Local);
                    self.open_session_on(host, sid, cx);
                }
                self.sidebar.update(cx, sidebar::Sidebar::reveal_focused);
            }
        }
        self.nav_history.navigating = false;
        cx.notify();
    }

    /// 인앱 설정 페이지(`SettingsView`)를 열거나 지정한 탭으로 전환한다.
    pub(super) fn open_settings(
        &mut self,
        tab: krill_app::settings::AdminTab,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if !cx.has_global::<krill_app::settings::Config>() {
            let dir = self.state_dir(cx);
            krill_app::settings::init_at(dir, cx);
        }
        if let Some(pane) = self.layout.focused() {
            self.cancel_pending_paste_for(pane, cx);
            self.commit_ime(pane, window.as_deref(), cx);
        }
        if self.role == WindowRole::Floating {
            cx.defer(move |cx| krill_app::settings::open_window_at_tab(tab, cx));
            return;
        }
        self.jump = None;
        self.continue_session = None;
        self.dashboard = None;
        self.sync_sidebar_visibility(cx);
        self.files.close_quick_open();
        if self.search.take().is_some()
            && let Some(l) = self.focused_live()
        {
            l.pane.clear_search();
        }
        let view = if let Some(existing) = &self.settings {
            existing.update(cx, |v, cx| v.set_tab(tab, cx));
            existing.clone()
        } else {
            let created = cx.new(|cx| {
                let mut v = krill_app::settings::SettingsView::new(cx);
                v.set_tab(tab, cx);
                krill_app::settings::SettingsView::sync_recording_paused(cx);
                v
            });
            self.settings = Some(created.clone());
            created
        };
        self.nav_history.push(NavEntry::Settings(tab));
        self.sync_focused_session(cx);
        if let Some(window) = window {
            self.window = Some(window.window_handle());
            let focus = view.read(cx).focus_handle();
            window.focus(&focus, cx);
            self.pending_focus = PendingFocus::None;
        } else {
            self.pending_focus = PendingFocus::Settings;
        }
        cx.notify();
    }

    /// 인앱 설정 페이지를 닫고 터미널 워크스페이스로 돌아간다.
    pub(super) fn close_settings(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if self.settings.take().is_none() {
            return;
        }
        self.record_workspace_nav(cx);
        self.sync_focused_session(cx);
        self.refresh_local_state(cx);
        self.files.refocus = true;
        if let Some(window) = window {
            window.focus(&self.focus, cx);
            self.pending_focus = PendingFocus::None;
        } else {
            self.pending_focus = PendingFocus::Workspace;
        }
        cx.notify();
    }

    /// 인앱 에이전트 대시보드 모달을 닫고 이전 화면(설정 또는 워크스페이스)으로 포커스를 돌려준다.
    pub(super) fn close_dashboard(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if self.dashboard.take().is_none() {
            return;
        }
        self.sync_sidebar_visibility(cx);
        self.sync_focused_session(cx);
        if self.settings.is_none() {
            self.files.refocus = true;
        }
        if let Some(window) = window {
            let focus = self
                .settings
                .as_ref()
                .map_or_else(|| self.focus.clone(), |s| s.read(cx).focus_handle());
            window.focus(&focus, cx);
            self.pending_focus = PendingFocus::None;
        } else if self.settings.is_some() {
            self.pending_focus = PendingFocus::Settings;
        } else {
            self.pending_focus = PendingFocus::Workspace;
        }
        cx.notify();
    }

    /// Cmd+J: Jump 팔레트를 열거나(사이드바 스냅샷으로) 닫는다. 설정이 열려 있으면 먼저 닫고 연다(OWE-860).
    /// 열 때 칸의 조합 중 글자는 확정한다(포커스 이탈).
    /// OWE-862: ⌘J·⌘P·⇧⌘P는 단일 모달이라 열 때 다른 팔레트를 닫는다.
    fn toggle_jump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            self.close_settings(Some(&mut *window), cx);
        }
        if self.jump.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        self.files.close_quick_open();
        if self.dashboard.take().is_some() {
            self.sync_sidebar_visibility(cx);
            self.sync_focused_session(cx);
        }
        if let Some(pane) = self.layout.focused() {
            self.commit_ime(pane, Some(window), cx);
        }
        let (items, worktrees) = {
            let sb = self.sidebar.read(cx);
            (sb.jump_items(), sb.jump_worktrees())
        };
        let ws = Some(self.folder().display().to_string());
        self.history
            .update(cx, |h, cx| h.update_scope_paths(ws.clone(), Vec::new(), cx));
        let history = self.history.clone();
        let (actions, quick_commands) = Self::palette_entries(cx);
        let palette = cx.new(|cx| jump::Palette::new(items, window, cx));
        palette.update(cx, |p, cx| {
            p.set_worktrees(worktrees, cx);
            p.set_actions(actions, cx);
            p.set_quick_commands(quick_commands, cx);
            p.set_scope_paths(ws, Vec::new(), cx);
            p.connect_history(&history, cx);
            p.focus(window, cx);
        });
        let events = cx.subscribe_in(
            &palette,
            window,
            |this, palette, e: &jump::Event, window, cx| {
                this.on_palette_event(palette, e, window, cx);
            },
        );
        self.jump = Some((palette, events));
        cx.notify();
    }

    /// Cmd+G/⇧Cmd+G: 열린 터미널 검색 바의 검색어로 다음/이전 일치를 찾는다(OWE-868). 바가 닫혀 있으면 무시한다.
    fn step_search(&mut self, direction: krill_proto::search::Direction, cx: &mut Context<Self>) {
        let query = self
            .search
            .as_ref()
            .map(|(bar, _)| bar.read(cx).query().to_owned());
        if let Some(query) = query
            && !query.is_empty()
            && let Some(l) = self.focused_live()
        {
            l.pane.search(&query, direction);
        }
    }

    /// OWE-865: 팔레트 빠른 액션(내장 8개)과 설정의 빠른 명령 목록.
    fn palette_entries(
        cx: &gpui::App,
    ) -> (
        Vec<jump::ActionEntry>,
        Vec<krill_app::commands::QuickCommand>,
    ) {
        use jump::ActionKind as Kind;
        use krill_app::i18n::catalog::jump as jtext;
        let actions = [
            (Kind::NewWorktree, jtext::ACT_NEW_WORKTREE.get()),
            (Kind::NewTab, jtext::ACT_NEW_TAB.get()),
            (Kind::SplitRight, jtext::ACT_SPLIT_RIGHT.get()),
            (Kind::SplitDown, jtext::ACT_SPLIT_DOWN.get()),
            (Kind::OpenSettings, jtext::ACT_OPEN_SETTINGS.get()),
            (Kind::ToggleSidebar, jtext::ACT_TOGGLE_SIDEBAR.get()),
            (Kind::ToggleExplorer, jtext::ACT_TOGGLE_EXPLORER.get()),
            (Kind::ToggleSearch, jtext::ACT_TOGGLE_SEARCH.get()),
        ]
        .into_iter()
        .map(|(kind, label)| jump::ActionEntry::new(kind, label.to_owned()))
        .collect();
        (actions, krill_app::settings::current(cx).quick_commands)
    }

    /// ⇧⌘P: 명령 팔레트(OWE-865). Jump와 같은 슬롯을 쓰되 액션부터 보여주고,
    /// 파일 빠른 열기가 열려 있으면 닫고 시작한다.
    fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.jump.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        self.files.close_quick_open();
        if self.dashboard.take().is_some() {
            self.sync_sidebar_visibility(cx);
            self.sync_focused_session(cx);
        }
        if let Some(pane) = self.layout.focused() {
            self.commit_ime(pane, Some(window), cx);
        }
        let (items, worktrees) = {
            let sb = self.sidebar.read(cx);
            (sb.jump_items(), sb.jump_worktrees())
        };
        let ws = Some(self.folder().display().to_string());
        self.history
            .update(cx, |h, cx| h.update_scope_paths(ws.clone(), Vec::new(), cx));
        let history = self.history.clone();
        let (actions, quick_commands) = Self::palette_entries(cx);
        let palette = cx.new(|cx| jump::Palette::new(items, window, cx));
        palette.update(cx, |p, cx| {
            p.set_mode(jump::Mode::Commands, cx);
            p.set_worktrees(worktrees, cx);
            p.set_actions(actions, cx);
            p.set_quick_commands(quick_commands, cx);
            p.set_scope_paths(ws, Vec::new(), cx);
            p.connect_history(&history, cx);
            p.input().update(cx, |input, cx| {
                input.set_placeholder(
                    krill_app::i18n::catalog::jump::PLACEHOLDER_COMMANDS.get(),
                    window,
                    cx,
                );
            });
            p.focus(window, cx);
        });
        let events = cx.subscribe_in(
            &palette,
            window,
            |this, palette, e: &jump::Event, window, cx| {
                this.on_palette_event(palette, e, window, cx);
            },
        );
        self.jump = Some((palette, events));
        cx.notify();
    }

    /// Jump·명령 팔레트 이벤트를 한곳에서 처리한다(OWE-865).
    fn on_palette_event(
        &mut self,
        palette: &gpui::Entity<jump::Palette>,
        event: &jump::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vault_target = match *event {
            jump::Event::ResumeVault(idx) => palette
                .read(cx)
                .vault_hit(idx)
                .map(krill_proto::agent_history::VaultHit::target),
            _ => None,
        };
        let worktree = match *event {
            jump::Event::OpenWorktree(idx) => palette.read(cx).worktree_entry(idx).cloned(),
            _ => None,
        };
        self.jump = None;
        match *event {
            jump::Event::Confirm(id) => self.open_session(id, cx),
            jump::Event::OpenWorktree(_) => {
                if let Some(entry) = worktree {
                    self.open_palette_worktree(&entry, cx);
                }
            }
            jump::Event::Action(kind) => self.run_palette_action(kind, window, cx),
            jump::Event::ResumeVault(_) => {
                if let Some(target) = vault_target {
                    self.history.update(cx, |h, cx| h.resume_vault(target, cx));
                }
            }
            jump::Event::Dismiss => {}
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// OWE-865: 팔레트에서 고른 워크트리 열기. 세션이 있으면 그 세션으로,
    /// 없으면 새 탭에 새 세션을 만든다(사이드바 카드 클릭과 같다).
    fn open_palette_worktree(&mut self, entry: &jump::WorktreeItem, cx: &mut Context<Self>) {
        if let Some(id) = entry.session_id {
            self.open_session(id, cx);
        } else {
            self.new_folder_tab_on(
                krill_app::hosts::HostId::Local,
                &entry.path,
                Program::Login,
                cx,
            );
        }
    }

    /// OWE-865: 팔레트 빠른 액션 실행. 모두 이미 구현된 기능만 둔다.
    fn run_palette_action(
        &mut self,
        kind: jump::ActionKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match kind {
            jump::ActionKind::NewWorktree => self.open_new_worktree_dialog(window, cx),
            jump::ActionKind::NewTab => self.new_tab_on_launch_host(cx),
            jump::ActionKind::SplitRight => self.split_active(Axis::Horizontal, cx),
            jump::ActionKind::SplitDown => self.split_active(Axis::Vertical, cx),
            jump::ActionKind::OpenSettings => {
                self.open_settings(krill_app::settings::AdminTab::General, Some(window), cx);
            }
            jump::ActionKind::ToggleSidebar => self.toggle_sidebar(cx),
            jump::ActionKind::ToggleExplorer => self.toggle_explorer(window, cx),
            jump::ActionKind::ToggleSearch => self.toggle_workspace_search(window, cx),
            jump::ActionKind::QuickCommand(index) => {
                if let Some(command) = krill_app::settings::current(cx)
                    .quick_commands
                    .get(index)
                    .cloned()
                {
                    self.inject_command(&command, window, cx);
                }
            }
        }
    }

    /// OWE-865: Jump 팔레트가 열려 있으면 ⌘1~9를 탭 전환 대신 N번째 행 즉시
    /// 실행으로 쓴다. 팔레트가 없으면 false(기존 탭 전환이 이어진다).
    fn jump_to_palette_row(&mut self, row: usize, cx: &mut Context<Self>) -> bool {
        let Some((palette, _)) = &self.jump else {
            return false;
        };
        let palette = palette.clone();
        palette.update(cx, |p, cx| {
            p.jump_to_row(row, cx);
        });
        true
    }

    /// Cmd+F: 설정 페이지가 열려 있으면 설정 검색 입력으로 포커스를 옮기고, 파일 탭이 포커스면
    /// 에디터 내장 검색을 열며(OWE-874), 아니면 터미널 검색 바(MK-059)를 열거나 닫는다.
    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dashboard.is_some() {
            return;
        }
        if let Some(settings) = self.settings.clone() {
            settings.update(cx, |v, cx| v.focus_search(window, cx));
            return;
        }
        if self.search.take().is_some() {
            if let Some(l) = self.focused_live() {
                l.pane.clear_search();
            }
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        if let Some(pane) = self.layout.focused()
            && self.open_editor_search(pane, window, cx)
        {
            return;
        }
        // OWE-868: 빈 탭에서는 터미널 검색 바를 열지 않는다(열어도 동작하지 않는다).
        // 파일 탭은 위에서 에디터 내장 검색으로 처리된다(OWE-874).
        if self.focused_live().is_none() {
            return;
        }
        if let Some(pane) = self.layout.focused() {
            self.commit_ime(pane, Some(window), cx);
        }
        let bar = cx.new(|cx| krill_app::search::Bar::new(window, cx));
        bar.update(cx, |b, cx| b.focus(window, cx));
        let events = cx.subscribe_in(
            &bar,
            window,
            |this, bar, e: &krill_app::search::Event, window, cx| {
                match e {
                    krill_app::search::Event::Query(q) => {
                        if let Some(l) = this.focused_live() {
                            if q.is_empty() {
                                l.pane.clear_search();
                            } else {
                                l.pane.search(q, krill_proto::search::Direction::Next);
                            }
                        }
                    }
                    krill_app::search::Event::Step(dir) => {
                        let q = bar.read(cx).query().to_owned();
                        if !q.is_empty()
                            && let Some(l) = this.focused_live()
                        {
                            l.pane.search(&q, *dir);
                        }
                    }
                    krill_app::search::Event::Dismiss => {
                        this.search = None;
                        if let Some(l) = this.focused_live() {
                            l.pane.clear_search();
                        }
                        window.focus(&this.focus, cx);
                    }
                }
                cx.notify();
            },
        );
        self.search = Some((bar, events));
        cx.notify();
    }

    /// krilld에 붙어(없으면 띄운다) 감시 연결을 들고 있는다. 끊기면 바로 "재연결 중" 배너를 띄우고 조작 없이 다시
    /// 붙는다(MK-026, 정책은 `krill_proto::daemon` "끊김과 재연결"). ping/pong heartbeat(OWE-586)로 EOF 없이
    /// 멈춘 데몬도 감지한다. 소켓·프로세스 대기는 백그라운드에서 한다.
    // ponytail: 감시 연결의 막히는 read가 백그라운드 스레드 하나를 앱 수명 동안 쥔다. 세션 연결(OWE-490)이 생기면
    // 그 연결들의 EOF로 대신할 수 있다.
    pub fn connect_daemon(&mut self, cx: &mut Context<Self>) {
        if self.daemon_task.is_some() {
            return;
        }
        self.daemon_task = Some(cx.spawn(async move |this, cx| {
            let paths = cx.background_executor().spawn(async { Paths::dev() }).await;
            this.update(cx, |this, cx| match paths {
                Ok(paths) => this.connect_daemon_with_paths(paths, cx),
                Err(e) => {
                    this.daemon_task = None;
                    this.link = Link::Failed(daemon::Error::Io(e).to_string());
                    krill_app::orca_import::set_daemon_link_up(false, cx);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn connect_daemon_with_paths(&mut self, paths: Paths, cx: &mut Context<Self>) {
        let target = krill_app::hosts::HostTarget::local(paths.clone());
        self.paths = Some(paths);
        self.start_away_tracking(cx);
        self.sidebar
            .update(cx, |s, cx| s.connect(target.clone(), cx));
        self.diff.update(cx, |d, cx| d.connect(target.clone(), cx));
        self.sync_hosts(cx);
        self.start_link_hosts(cx);
        self.daemon_task = Some(cx.spawn(async move |this, cx| {
            let hello = daemon::heartbeat_hello(ClientRole::App, "krill-app");
            let bg = cx.background_executor().clone();
            let (t, h) = (target.clone(), hello.clone());
            let mut conn = match bg.spawn(async move { t.connect(&h) }).await {
                Ok(c) => c,
                Err(e) => {
                    let link = Link::Failed(e.to_string());
                    if this.update(cx, |this, cx| this.set_link(link, cx)).is_err() {
                        return;
                    }
                    let (t, h) = (target.clone(), hello.clone());
                    bg.spawn(async move { t.reconnect_until(&h) }).await
                }
            };
            // MK-062: 앱이 새 빌드면 구 krilld를 교체한다. 구 krilld가 끝나며 감시 연결이 EOF가 되고 아래 루프가 다시 붙는다.
            let (t, h) = (target.clone(), hello.clone());
            bg.spawn(async move {
                if let Err(error) = t.upgrade(&h) {
                    tracing::warn!(kind = ?error.kind(), "daemon upgrade failed");
                }
            })
            .detach();
            loop {
                let t = target.clone();
                let appearance = bg
                    .spawn(async move { krill_app::appearance::connect(&t) })
                    .await;
                let recording = bg.spawn(async { krill_app::recording::check() }).await;
                if this
                    .update(cx, |this, cx| {
                        this.recording = recording;
                        this.appearance = appearance
                            .unwrap_or_else(|error| {
                                tracing::warn!(kind = ?error.kind(), "theme connection failed");
                                None
                            })
                            .map(|writer| Box::new(writer) as Box<dyn std::io::Write>);
                        this.push_appearance(cx);
                        this.set_link(Link::Up, cx);
                    })
                    .is_err()
                {
                    return;
                }
                let features = conn.negotiated.features;
                let mut stream = conn.stream;
                if bg
                    .spawn(async move { daemon::wait_responsive(&mut stream, features) })
                    .await
                    .is_err()
                {
                    tracing::warn!("daemon stopped responding, reconnecting");
                }
                if this
                    .update(cx, |this, cx| this.set_link(Link::Down, cx))
                    .is_err()
                {
                    return;
                }
                let (t, h) = (target.clone(), hello.clone());
                conn = bg.spawn(async move { t.reconnect_until(&h) }).await;
            }
        }));
    }

    fn start_link_hosts(&mut self, cx: &mut Context<Self>) {
        if self.link_poll.is_some() {
            return;
        }
        let Some(state) = self.state_dir(cx) else {
            return;
        };
        self.link_poll = Some(cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            let source_state = state.clone();
            let service = bg
                .spawn(async move {
                    let executable = krill_app::hosts::link::executable();
                    krill_app::hosts::link::LinkHosts::shared(state, executable)
                })
                .await;
            let installed = this
                .update(cx, |this, cx| {
                    if this.state_dir(cx).as_ref() != Some(&source_state) {
                        return false;
                    }
                    this.link_hosts = Some(service.clone());
                    true
                })
                .unwrap_or(false);
            if !installed {
                return;
            }
            loop {
                let service = service.clone();
                let registrations = bg.spawn(async move { service.registrations() }).await;
                if this
                    .update(cx, |shell, cx| {
                        if let Ok(hosts) = registrations {
                            shell.external_hosts = hosts;
                            shell.sync_hosts(cx);
                        }
                    })
                    .is_err()
                {
                    return;
                }
                bg.timer(Duration::from_secs(1)).await;
            }
        }));
    }

    pub(crate) fn sync_hosts(&mut self, cx: &mut Context<Self>) {
        self.sync_hosts_after_reconnect(None, cx);
    }

    /// Every manager API may lock a slot owned by an SSH connection attempt.
    /// UI code reads only these accepted snapshots; backend updates are ordered.
    fn sync_hosts_after_reconnect(&mut self, reconnect: Option<String>, cx: &mut Context<Self>) {
        let hosts = self.ssh_hosts.clone();
        let specs = krill_app::hosts::specs_from_ssh_hosts(&hosts);
        let external = self.external_hosts.clone();
        let allowed = specs.iter().chain(external.iter().map(|host| &host.spec));
        let allowed = allowed.cloned().collect::<Vec<_>>();
        self.host_statuses
            .retain(|(spec, _)| allowed.contains(spec));
        self.host_targets.retain(|host, _| {
            self.host_statuses
                .iter()
                .any(|(spec, _)| *host == krill_app::hosts::HostId::Remote(spec.id.clone()))
        });
        if let krill_app::hosts::HostId::Remote(id) = &self.launch_host
            && !allowed.iter().any(|spec| &spec.id == id)
        {
            self.launch_host = krill_app::hosts::HostId::Local;
        }
        let source = HostSource {
            specs: specs.clone(),
            external: external
                .iter()
                .map(|host| (host.spec.clone(), host.socket.clone(), host.info.clone()))
                .collect(),
            paths: self.paths.clone(),
        };
        if self.host_source.as_ref() != Some(&source) {
            self.host_generation = self.host_generation.wrapping_add(1);
            self.host_source = Some(source);
        }
        if let Some(id) = reconnect {
            if let Some(raw_id) = id.strip_prefix("iroh:")
                && let Some(service) = &self.link_hosts
            {
                service.paired(raw_id);
            }
            self.host_generation = self.host_generation.wrapping_add(1);
            self.host_pending_reconnects.push(id);
        }
        if self.host_refresh_running {
            // One active operation and one latest refresh, independent of tick count.
            self.host_refresh_again = true;
            return;
        }
        self.host_refresh_running = true;
        self.host_refresh_again = false;
        let generation = self.host_generation;
        let reconnects = std::mem::take(&mut self.host_pending_reconnects);
        let manager = self.host_manager.clone();
        let paths = self.paths.clone();
        let (complete, result) = tokio::sync::oneshot::channel();
        #[cfg(test)]
        let gate = self.next_host_io.take();
        cx.background_executor()
            .spawn(async move {
                #[cfg(test)]
                if let Some(gate) = gate {
                    gate.await.ok();
                }
                let aliases: BTreeMap<String, String> =
                    krill_app::ssh_hosts::duplicate_hosts(&hosts)
                        .into_iter()
                        .collect();
                let specs = specs
                    .into_iter()
                    .filter(|s| !aliases.contains_key(&s.id))
                    .collect::<Vec<_>>();
                manager.set_aliases(aliases.clone());
                manager.sync_specs(&specs);
                manager.sync_external_hosts(external);
                for id in reconnects {
                    manager.reconnect_now(&id);
                }
                let statuses = manager.statuses();
                let targets = statuses
                    .iter()
                    .filter_map(|(spec, _)| {
                        let host = krill_app::hosts::HostId::Remote(spec.id.clone());
                        manager
                            .target_for(&host, paths.as_ref())
                            .map(|target| (host, target))
                    })
                    .collect();
                complete.send((statuses, targets, paths, aliases)).ok();
            })
            .detach();
        cx.spawn(async move |this, cx| {
            let result = result.await;
            this.update(cx, |this, cx| {
                this.host_refresh_running = false;
                if let Ok((statuses, targets, paths, aliases)) = result
                    && this.host_generation == generation
                    && this.paths == paths
                {
                    this.host_aliases = aliases;
                    this.host_statuses = statuses;
                    this.host_targets = targets;
                    if this.paths.is_some() {
                        let manager = this.host_manager.clone();
                        this.sidebar
                            .update(cx, |sidebar, cx| sidebar.sync_remote_hosts(&manager, cx));
                    }
                    this.sync(cx);
                    cx.notify();
                }
                if this.host_refresh_again || !this.host_pending_reconnects.is_empty() {
                    this.sync_hosts_after_reconnect(None, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn local_target(&self) -> Option<krill_app::hosts::HostTarget> {
        self.paths.clone().map(krill_app::hosts::HostTarget::local)
    }

    pub(super) fn target_for_host(
        &self,
        host: &krill_app::hosts::HostId,
    ) -> Option<krill_app::hosts::HostTarget> {
        match host {
            krill_app::hosts::HostId::Local => self.local_target(),
            krill_app::hosts::HostId::Remote(_) => self.host_targets.get(host).cloned(),
        }
    }

    pub(super) fn target_for_pane(&self, pane: PaneId) -> Option<krill_app::hosts::HostTarget> {
        let host = self.layout.host(pane);
        self.target_for_host(host)
    }

    pub(super) fn new_tab_on_launch_host(&mut self, cx: &mut Context<Self>) {
        let host = self.launch_host.clone();
        self.change(true, cx, |l| {
            l.new_tab();
            if host.is_remote()
                && let Some(pane) = l.focused()
            {
                l.set_host(pane, host);
            }
        });
    }

    fn push_appearance(&mut self, cx: &mut Context<Self>) {
        let theme = krill_app::ui::terminal_theme(cx);
        self.terminal_theme = theme;
        let push = Push {
            theme,
            cell: self.cell_pixels,
        };
        if let Err(error) = krill_app::appearance::push(self.appearance.as_mut(), push) {
            tracing::warn!(kind = ?error.kind(), "theme push failed");
        }
        cx.notify();
    }

    pub(super) fn state_dir(&self, cx: &App) -> Option<PathBuf> {
        self.path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(PathBuf::from)
            .or_else(|| {
                let parent = self.paths.as_ref()?.socket.parent()?;
                if parent.file_name().is_some_and(|n| n == "run") {
                    parent.parent().map(PathBuf::from)
                } else {
                    Some(parent.to_path_buf())
                }
            })
            .or_else(|| krill_app::settings::state_dir(cx))
    }

    fn on_config_changed(&mut self, cx: &mut Context<Self>) {
        self.refresh_local_state(cx);
        self.sidebar
            .update(cx, krill_app::sidebar::Sidebar::reload_external);
        if let Some(host) = krill_app::settings::take_pending_ssh_connect(cx) {
            self.close_settings(None, cx);
            self.connect_ssh_host(&host, None, cx);
            if let Some(win) = self.window {
                let focus = self.focus.clone();
                cx.defer(move |cx| {
                    win.update(cx, |_, window, cx| {
                        window.activate_window();
                        window.focus(&focus, cx);
                    })
                    .ok();
                });
            }
        }
        let mut ran_terminal_cmd = false;
        while let Some(cmd) = krill_app::settings::take_pending_terminal_command(cx) {
            if !ran_terminal_cmd {
                self.close_settings(None, cx);
                ran_terminal_cmd = true;
            }
            self.run_terminal_command(&cmd, None, cx);
        }
        if ran_terminal_cmd && let Some(win) = self.window {
            let focus = self.focus.clone();
            cx.defer(move |cx| {
                win.update(cx, |_, window, cx| {
                    window.activate_window();
                    window.focus(&focus, cx);
                })
                .ok();
            });
        }
        if let Some(permission) = krill_app::settings::take_pending_permission(cx) {
            self.load_agents(Some(permission), cx);
        }
        // 설정 창이 닫힌 뒤에도 보여야 할 안내(예: ssh-copy-id 비밀번호 입력)를 메인 창 배너로 잇는다(OWE-884).
        if let Some(msg) = krill_app::settings::take_pending_notice(cx) {
            self.notice = Some(msg);
        }
        cx.notify();
    }

    /// AS-10: 계정 스냅샷·SSH 호스트 읽기를 백그라운드로 미룬다. 설정 토글·1초 폴링·
    /// 외부 변경 때마다 호출되므로 동기 IO(`~/.codex/sessions` 재귀 stat 등)면 매번
    /// UI 스레드가 멈춘다. 결과는 UI 스레드에서 반영한다.
    /// OWE-697: 데몬이 연결돼 있으면 계정은 데몬 `ListAccounts` 응답만 쓴다. 로컬 파일은 데몬이
    /// 없을 때 읽기 전용으로만 읽는다(자동전환 판정은 krilld 단독).
    fn refresh_local_state(&mut self, cx: &mut Context<Self>) {
        self.local_generation = self.local_generation.wrapping_add(1);
        let generation = self.local_generation;
        if self.link == Link::Up {
            self.refresh_accounts(krill_proto::usage::UsageRequest::ListAccounts, cx);
            self.refresh_ssh_hosts(cx);
            return;
        }
        let home = krill_app::orca_import::state(cx)
            .and_then(|s| s.home_override.clone())
            .unwrap_or_else(krill_app::accounts::default_home);
        let dir = self.state_dir(cx);
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let hosts = krill_app::ssh_hosts::load_hosts(dir.as_deref());
                    let snap = krill_app::accounts::refresh_local(&home, dir.as_deref());
                    (hosts, snap)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.local_generation != generation || this.link == Link::Up {
                    return;
                }
                let (hosts, snap) = loaded;
                this.ssh_hosts = hosts;
                this.sync_hosts(cx);
                this.apply_accounts_snapshot(snap, cx);
            })
            .ok();
        })
        .detach();
    }

    /// AS-10: SSH 호스트 목록만 백그라운드에서 다시 읽는다(재연결 때마다 UI를 막지 않게).
    fn refresh_ssh_hosts(&mut self, cx: &mut Context<Self>) {
        self.ssh_generation = self.ssh_generation.wrapping_add(1);
        let generation = self.ssh_generation;
        let dir = self.state_dir(cx);
        cx.spawn(async move |this, cx| {
            let hosts = cx
                .background_executor()
                .spawn(async move { krill_app::ssh_hosts::load_hosts(dir.as_deref()) })
                .await;
            this.update(cx, |this, cx| {
                if this.ssh_generation != generation {
                    return;
                }
                this.ssh_hosts = hosts;
                this.sync_hosts(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_link(&mut self, link: Link, cx: &mut Context<Self>) {
        if link != Link::Up {
            krill_app::alert::disconnected(cx);
            self.appearance = None;
            self.reconnected = None;
            self.reconnected_task = None;
        }
        // 재연결 확인은 다시 붙은 뒤 실제 세션 목록이 도착해야 보인다(결과 동사 규칙, STYLEGUIDE §7.1.1).
        let reconnecting = link == Link::Down;
        self.sidebar
            .update(cx, |s, cx| s.set_local_reconnecting(reconnecting, cx));
        self.link = link;
        krill_app::orca_import::set_daemon_link_up(self.link == Link::Up, cx);
        self.accounts_poll = (self.link == Link::Up).then(|| Self::poll_accounts(cx));
        if self.link == Link::Up {
            if let Some(since) = self.away_since.take() {
                self.load_away_summary(since, cx);
            }
            self.load_agents(None, cx);
            self.refresh_accounts(krill_proto::usage::UsageRequest::ListAccounts, cx);
            self.refresh_ssh_hosts(cx);
            if self.role == WindowRole::Main
                && self.update_poll.is_none()
                && let Some(bg_dir) = self.state_dir(cx)
            {
                self.update_poll = Some(Self::poll_updates(bg_dir, cx));
            }
            self.sync(cx);
            self.refresh_detached(cx);
        }
        cx.notify();
    }

    /// 메인 창이면 지난번 마지막 생존 시각을 요약 기준으로 잡고, 이후 주기·종료 때 생존 시각을 레이아웃에 쓴다.
    /// 창을 닫았다 다시 연 같은 프로세스에서는 요약하지 않는다(앱은 계속 돌고 있었다).
    pub(super) fn refresh_usage_manually(&mut self, cx: &mut Context<Self>) {
        if self.usage_refresh.is_some() {
            return;
        }
        let Some(paths) = self.paths.clone().filter(|_| self.link == Link::Up) else {
            self.refresh_accounts(krill_proto::usage::UsageRequest::ListAccounts, cx);
            return;
        };
        cx.notify();
        self.usage_refresh = Some(cx.spawn(async move |this, cx| {
            let first = cx
                .background_executor()
                .spawn({
                    let paths = paths.clone();
                    async move {
                        krill_app::accounts::request_ipc(
                            &paths,
                            &krill_proto::usage::UsageRequest::ListAccounts,
                        )
                    }
                })
                .await;
            let ok = this
                .update(cx, |this, cx| match first {
                    Ok(snap) => {
                        this.apply_accounts_snapshot(snap, cx);
                        true
                    }
                    Err(e) => {
                        this.usage_refresh = None;
                        this.notice = Some(e);
                        cx.notify();
                        false
                    }
                })
                .unwrap_or(false);
            if !ok {
                return;
            }
            for delay_ms in [700_u64, 1_100] {
                cx.background_executor()
                    .timer(Duration::from_millis(delay_ms))
                    .await;
                let follow = cx
                    .background_executor()
                    .spawn({
                        let paths = paths.clone();
                        async move {
                            krill_app::accounts::request_ipc(
                                &paths,
                                &krill_proto::usage::UsageRequest::ListAccounts,
                            )
                        }
                    })
                    .await;
                if let Ok(snap) = follow {
                    this.update(cx, |this, cx| {
                        this.apply_accounts_snapshot(snap, cx);
                    })
                    .ok();
                }
            }
            this.update(cx, |this, cx| {
                this.usage_refresh = None;
                cx.notify();
            })
            .ok();
        }));
    }

    fn start_away_tracking(&mut self, cx: &mut Context<Self>) {
        static FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
        if self.path.is_none() || self.role != WindowRole::Main || self.heartbeat.is_some() {
            return;
        }
        if FIRST.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.away_since = self.layout.last_seen_unix_ns();
        }
        self.heartbeat = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(away::HEARTBEAT).await;
                if this.update(cx, |this, _| this.touch_last_seen()).is_err() {
                    return;
                }
            }
        }));
        // 정상 종료·업데이트 재시작(`cx.quit`)은 종료 직전 시각을 디스크까지 쓴다. kill -9는 마지막 주기 저장값이 남는다.
        self.app_quit = Some(cx.on_app_quit(|this, cx| {
            this.touch_last_seen();
            let barrier = this.saver.barrier(cx);
            async move {
                let _ = barrier.await;
            }
        }));
    }

    fn touch_last_seen(&mut self) {
        self.layout.set_last_seen_unix_ns(away::now_ns());
        self.save();
    }

    /// 실제 상태 목록을 받은 뒤에만 요약을 세운다. 읽지 못하면 아무것도 보이지 않는다.
    fn load_away_summary(&mut self, since: u64, cx: &mut Context<Self>) {
        let Some(paths) = self.paths.clone() else {
            return;
        };
        let target = krill_app::hosts::HostTarget::local(paths);
        self.away_task = Some(cx.spawn(async move |this, cx| {
            let statuses = cx
                .background_executor()
                .spawn(async move { away::fetch_statuses(&target) })
                .await;
            let Ok(statuses) = statuses else {
                return;
            };
            this.update(cx, |this, cx| {
                if let Some((away_for, counts)) =
                    away::count(&statuses, Some(since), away::now_ns())
                {
                    this.away = Some(away::summary(away_for, counts));
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn show_reconnected(&mut self, sessions: usize, cx: &mut Context<Self>) {
        if self.link != Link::Up {
            return;
        }
        self.reconnected = Some(
            krill_app::i18n::catalog::link::RECONNECTED.format(&[("n", &sessions.to_string())]),
        );
        self.reconnected_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RECONNECTED_FOR).await;
            this.update(cx, |this, cx| {
                this.reconnected = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn poll_updates(state_dir: PathBuf, cx: &mut Context<Self>) -> Task<()> {
        // 4시간이면 앱을 켠 뒤 낸 릴리스가 반나절 가까이 안 보였다(사용자 지적). 매니페스트 한 번 받는 일이라 30분마다 본다.
        const UPDATE_POLL_INTERVAL: Duration = Duration::from_mins(30);
        cx.spawn(async move |this, cx| {
            loop {
                let dir = state_dir.clone();
                let res = cx
                    .background_executor()
                    .spawn(async move { krill_app::updater::poll_env_update(&dir) })
                    .await;
                if let Ok(Some(staged)) = res {
                    let automatic = this
                        .read_with(cx, |_, cx| {
                            krill_app::settings::current(cx).auto_restart_update
                        })
                        .unwrap_or(false);
                    let relaunch = staged.clone();
                    let relaunched = staged.relaunched
                        || (automatic
                            && cx
                                .background_executor()
                                .spawn(async move { relaunch.spawn_relauncher().is_ok() })
                                .await);
                    let keep_going = this
                        .update(cx, |_, cx| {
                            if relaunched {
                                files::quit_app(cx);
                                return;
                            }
                            let mut u = cx
                                .try_global::<krill_app::updater::UpdateState>()
                                .cloned()
                                .unwrap_or_default();
                            u.staged = Some(staged.clone());
                            u.status = krill_app::updater::CheckStatus::Ready(staged);
                            u.card_collapsed = false;
                            cx.set_global(u);
                            cx.refresh_windows();
                            cx.notify();
                        })
                        .is_ok();
                    if !keep_going {
                        break;
                    }
                }
                cx.background_executor().timer(UPDATE_POLL_INTERVAL).await;
            }
        })
    }

    pub(super) fn refresh_accounts(
        &mut self,
        req: krill_proto::usage::UsageRequest,
        cx: &mut Context<Self>,
    ) {
        let mutating = !matches!(req, krill_proto::usage::UsageRequest::ListAccounts);
        let Some(paths) = self.paths.clone().filter(|_| self.link == Link::Up) else {
            let state_dir = self.state_dir(cx);
            // OWE-697: 실제 상태 디렉터리에서 데몬 없이 계정을 바꾸지 않는다(읽기 전용).
            // 로컬 변이는 데몬 없는 임시 상태 디렉터리(테스트)에서만 쓴다.
            if mutating && state_dir == krill_proto::daemon::state_dir() {
                self.notice = Some(
                    krill_app::i18n::catalog::settings::ACCOUNTS_DAEMON_REQUIRED
                        .get()
                        .into(),
                );
                cx.notify();
                return;
            }
            self.local_generation = self.local_generation.wrapping_add(1);
            let generation = self.local_generation;
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        krill_app::accounts::request_local(
                            &krill_app::accounts::default_home(),
                            state_dir.as_deref(),
                            &req,
                        )
                    })
                    .await;
                this.update(cx, |this, cx| {
                    if this.local_generation != generation || this.link == Link::Up {
                        return;
                    }
                    if let Ok(snap) = result {
                        this.apply_accounts_snapshot(snap, cx);
                        if mutating {
                            cx.defer(krill_app::settings::notify_external_change);
                        }
                    }
                })
                .ok();
            })
            .detach();
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { krill_app::accounts::request_ipc(&paths, &req) })
                .await;
            this.update(cx, |this, cx| match res {
                Ok(snap) => {
                    this.apply_accounts_snapshot(snap, cx);
                    if mutating {
                        cx.defer(krill_app::settings::notify_external_change);
                    }
                }
                Err(e) => {
                    this.notice = Some(e);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Codex banked reset 크레딧 소진(상태바 사용량 팝업의 "사용량 초기화" 확인 단계).
    /// 데몬 전용이다. 성공하면 스냅샷을 적용하고 소진 결과를 배너로 알린 뒤,
    /// 백그라운드 재조회가 끝날 때까지 숫자를 당겨오게 새로고침을 예약한다.
    pub(super) fn consume_reset_credit(
        &mut self,
        provider: krill_proto::usage::ProviderKind,
        account_id: String,
        cx: &mut Context<Self>,
    ) {
        use krill_proto::usage::{ResetConsumeOutcome, UsageRequest};
        self.reset_armed = None;
        let req = UsageRequest::ConsumeResetCredit {
            provider,
            account_id,
        };
        let Some(paths) = self.paths.clone().filter(|_| self.link == Link::Up) else {
            self.notice = Some(
                krill_app::i18n::catalog::settings::ACCOUNTS_DAEMON_REQUIRED
                    .get()
                    .into(),
            );
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { krill_app::accounts::consume_reset_credit_ipc(&paths, &req) })
                .await;
            this.update(cx, |this, cx| match res {
                Ok(consumed) => {
                    this.apply_accounts_snapshot(consumed.snapshot, cx);
                    this.notice = Some(match consumed.outcome {
                        ResetConsumeOutcome::Reset | ResetConsumeOutcome::AlreadyRedeemed => {
                            krill_app::i18n::catalog::status::PROVIDER_RESET_USAGE_DONE
                                .format(&[("n", &consumed.windows_reset.to_string())])
                        }
                        ResetConsumeOutcome::NothingToReset => {
                            krill_app::i18n::catalog::status::PROVIDER_RESET_USAGE_NOTHING
                                .get()
                                .to_owned()
                        }
                        ResetConsumeOutcome::NoCredit => {
                            krill_app::i18n::catalog::status::PROVIDER_RESET_USAGE_NO_CREDIT
                                .get()
                                .to_owned()
                        }
                    });
                    cx.defer(krill_app::settings::notify_external_change);
                    this.refresh_usage_manually(cx);
                }
                Err(e) => {
                    this.notice = Some(e);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn apply_accounts_snapshot(
        &mut self,
        snap: krill_proto::usage::AccountsSnapshot,
        cx: &mut Context<Self>,
    ) {
        if let Some(ev) = snap.switched.first() {
            self.notice = Some(
                krill_app::i18n::catalog::popout::AUTO_SWITCH_NOTICE.format(&[
                    ("provider", ev.provider.display_name()),
                    ("from", &ev.from_label),
                    ("pct", &ev.reason_percent.to_string()),
                    ("to", &ev.to_label),
                ]),
            );
        }
        krill_app::alert::sync_account_switches(&snap.advanced, cx);
        let mut input_notices = Vec::new();
        for (pane, live) in &mut self.live {
            if !self.layout.host(*pane).is_local() || !snap.details.supported {
                continue;
            }
            let handoff = self
                .layout
                .session(*pane)
                .and_then(|session| snap.details.handoffs.iter().find(|h| h.from == session));
            match handoff {
                Some(h) if h.to.is_none() && h.error.is_none() => live.pane.pause_account_input(),
                Some(h) if h.to.is_some() => {} // moved below, retaining unsent input
                other => input_notices.extend(
                    live.pane
                        .resume_account_input(other.is_some_and(|h| h.error.is_some()), Now::get()),
                ),
            }
        }
        let mut rebound = false;
        let mut reconnect = std::collections::BTreeSet::new();
        // Durable handoffs can form A -> B -> C while this window is closed.
        for _ in 0..snap.details.handoffs.len().min(128) {
            let mut changed = false;
            for handoff in &snap.details.handoffs {
                if let Some(to) = handoff.to.filter(|to| *to != handoff.from) {
                    let panes = self.layout.rebind_session(
                        &krill_app::hosts::HostId::Local,
                        handoff.from,
                        to,
                    );
                    if !panes.is_empty() && self.chat.active_sessions.remove(&handoff.from) {
                        self.chat.active_sessions.insert(to);
                        self.chat.logs.remove(&handoff.from);
                    }
                    changed |= !panes.is_empty();
                    for pane in panes {
                        if let Some(live) = self.live.get_mut(&pane) {
                            live.cut();
                            input_notices.extend(live.pane.handoff_account(to));
                            live.ime.clear_pending();
                            live.echo = EchoDraw::default();
                            live.painted = Rc::default();
                            live.error = None;
                            live.clear_selection();
                            reconnect.insert(pane);
                        }
                    }
                } else if let Some(error) = &handoff.error
                    && self.layout.sessions().contains(&handoff.from)
                {
                    self.notice = Some(
                        if self.live.values().any(|live| {
                            live.pane.session() == Some(handoff.from)
                                && live.pane.account_switching()
                        }) {
                            format!(
                                "{} ({error})",
                                krill_app::i18n::catalog::settings::ACCOUNT_INPUT_HELD.get()
                            )
                        } else {
                            error.clone()
                        },
                    );
                }
            }
            rebound |= changed;
            if !changed {
                break;
            }
        }
        let pending = snap
            .details
            .handoffs
            .iter()
            .any(|h| h.to.is_none() && h.error.is_none());
        if let Some(settings) = &self.settings {
            settings.update(cx, |settings, cx| {
                settings.update_accounts_snapshot(snap.clone(), cx);
            });
        }
        self.accounts_snapshot = snap;
        if rebound {
            for pane in reconnect {
                if let Some(target) = self.target_for_pane(pane) {
                    let task = Self::spawn_pane(pane, target, None, cx);
                    if let Some(live) = self.live.get_mut(&pane) {
                        live.task = task;
                    }
                }
            }
            self.sync(cx);
            self.sync_focused_session(cx);
            self.sync_sidebar_custom_titles(cx);
            self.save();
            self.refresh_detached(cx);
        }
        for notice in &input_notices {
            self.show(Some(notice), cx);
        }
        if pending
            && self.account_handoff_poll.is_none()
            && let Some(paths) = self.paths.clone().filter(|_| self.link == Link::Up)
        {
            self.account_handoff_poll = Some(cx.spawn(async move |this, cx| {
                for _ in 0..120 {
                    cx.background_executor()
                        .timer(Duration::from_millis(500))
                        .await;
                    let result = cx
                        .background_executor()
                        .spawn({
                            let paths = paths.clone();
                            async move {
                                krill_app::accounts::request_ipc(
                                    &paths,
                                    &krill_proto::usage::UsageRequest::ListAccounts,
                                )
                            }
                        })
                        .await;
                    let pending = this
                        .update(cx, |this, cx| {
                            if let Ok(snapshot) = result {
                                this.apply_accounts_snapshot(snapshot, cx);
                            }
                            this.accounts_snapshot
                                .details
                                .handoffs
                                .iter()
                                .any(|h| h.to.is_none() && h.error.is_none())
                        })
                        .unwrap_or(false);
                    if !pending {
                        break;
                    }
                }
                this.update(cx, |this, _| {
                    this.account_handoff_poll = None;
                })
                .ok();
            }));
        }
        cx.notify();
    }

    pub(super) fn connect_ssh_host(
        &mut self,
        host: &krill_app::ssh_hosts::SshHost,
        window: Option<&Window>,
        cx: &mut Context<Self>,
    ) {
        let cmd = krill_app::ssh_hosts::build_connect_command(host);
        self.run_terminal_command(&cmd, window, cx);
    }

    /// Orca 스타일 설정/온보딩 터미널 자동 실행: 현재 포커스된 패인이 유휴 로컬 셸이 아니면 새 탭을 열고,
    /// 명령어와 개행(`\n`)을 PTY에 바로 입력해 실행한다.
    pub(super) fn run_terminal_command(
        &mut self,
        command: &str,
        window: Option<&Window>,
        cx: &mut Context<Self>,
    ) {
        let trimmed = command.trim();
        if trimmed.is_empty() {
            return;
        }
        let cmd = format!("{trimmed}\n");
        let focused = self.layout.focused();
        let was_remote = focused.is_some_and(|p| !self.layout.host(p).is_local());
        let open_new_tab = self.focused_live().is_none()
            || focused.is_none_or(|focused| {
                was_remote
                    || (self.link == Link::Up && self.layout.session(focused).is_some())
                    || matches!(
                        self.layout.launch(focused).map(|c| &c.program),
                        Some(Program::Agent { .. } | Program::Custom { .. })
                    )
                    || self.live.get(&focused).is_some_and(|l| {
                        l.launch.is_some()
                            || l.pane.is_closed()
                            || l.pane.is_cold()
                            || l.pane.modes().mouse()
                            || l.pane.modes().kitty != 0
                    })
            });
        if open_new_tab {
            let cwd = (!was_remote)
                .then(|| {
                    focused
                        .and_then(|p| self.layout.launch(p))
                        .map(|c| c.cwd.clone())
                        .filter(|cwd| !cwd.is_empty())
                })
                .flatten()
                .unwrap_or_else(|| folder::default_folder().as_os_str().as_bytes().to_vec());
            self.change(true, cx, |l| {
                l.new_tab();
                if let Some(pane) = l.focused() {
                    l.set_launch(
                        pane,
                        Create {
                            account: None,
                            program: Program::Login,
                            cwd,
                            cols: pane::SIZE.0,
                            rows: pane::SIZE.1,
                        },
                    );
                }
            });
        }
        let Some(l) = self.focused_live() else {
            return;
        };
        let mut out = l.ime.commit();
        out.pty = keys::tui::text(&out.pty, l.pane.modes());
        out.pty.extend_from_slice(cmd.as_bytes());
        self.send(&out, window, cx);
    }

    fn record_closed_pane(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        #[cfg(test)]
        if let Some(session) = self.layout.session(pane) {
            self.closed_sessions.push(session);
        }
        self.remember_closed_dir(pane, cx);
    }

    fn remember_closed_dir(&mut self, pane: PaneId, cx: &App) {
        if let Some(dir) = self.pane_cwd(pane, cx) {
            self.closed_dirs.push((self.layout.host(pane).clone(), dir));
        }
    }

    /// 칸이 아직 레이아웃에 있으면 true(확인 대화상자를 기다리는 동안 닫혔을 수 있다).
    fn has_pane(&self, pane: PaneId) -> bool {
        self.layout.tabs().iter().any(|t| t.root.contains(pane))
    }

    /// OWE-872: 칸을 닫기 전에 실행 중인지 본다. 실행 중이면 확인 대화상자를 띄우고,
    /// 아니면 바로 닫는다.
    pub(super) fn maybe_close_pane(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if self.has_pane(pane)
            && self.pane_is_running(pane, cx)
            && krill_app::settings::confirm_running_close(cx)
        {
            self.pending_running_close = Some((PendingRunningClose::Pane(pane), false));
            cx.notify();
            return;
        }
        self.do_close_pane(pane, cx);
    }

    /// OWE-872: 칸을 실제로 닫는다(줌 해제·닫은 기록 포함).
    pub(super) fn do_close_pane(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if !self.has_pane(pane) {
            cx.notify();
            return;
        }
        self.zoomed_pane = None;
        if let Some(index) = self
            .layout
            .tabs()
            .iter()
            .position(|t| t.root == PaneTree::Leaf(pane))
        {
            self.do_close_tab(index, cx);
            return;
        }
        self.record_closed_pane(pane, cx);
        self.close_layout(cx, |l| {
            l.focus(pane);
            l.close_pane();
        });
    }

    pub(super) fn record_closed_tab(&mut self, tab_idx: usize, cx: &mut Context<Self>) {
        #[cfg(test)]
        self.closed_sessions.extend(
            self.layout
                .tab_panes(tab_idx)
                .iter()
                .filter_map(|pane| self.layout.session(*pane)),
        );
        if let Some(tab) = self.layout.tabs().get(tab_idx) {
            let focused = tab.focused;
            self.remember_closed_dir(focused, cx);
        }
    }

    fn open_new_worktree_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // OWE-860: 설정에서 ⌘N을 누르면 설정을 닫고 대화상자를 연다.
        if self.settings.is_some() {
            self.close_settings(Some(&mut *window), cx);
        }
        let repo = self.folder();
        if self.sidebar_collapsed {
            self.toggle_sidebar(cx);
        }
        self.sidebar
            .update(cx, |s, cx| s.new_worktree(repo, window, cx));
    }

    fn activate_tab_index(&mut self, index: usize, cx: &mut Context<Self>) {
        // OWE-860: 설정에서 ⌘1~9를 누르면 설정을 닫고 해당 탭으로 전환한다.
        if self.settings.is_some() {
            self.close_settings(None, cx);
        }
        let tabs = self.workspace_tabs(cx);
        let len = tabs.len();
        if len == 0 {
            return;
        }
        let target = if index == 8 {
            len.saturating_sub(1)
        } else if index < len {
            index
        } else {
            return;
        };
        let Some(&tab) = tabs.get(target) else {
            return;
        };
        self.change(true, cx, |l| l.activate(tab));
    }

    fn cycle_session_focus(&mut self, delta: isize, cx: &mut Context<Self>) {
        // OWE-860: 설정에서 ⌘⇧↑/↓를 누르면 설정을 닫고 세션을 전환한다.
        if self.settings.is_some() {
            self.close_settings(None, cx);
        }
        let items = self.sidebar.read(cx).jump_items();
        if items.is_empty() {
            self.cycle_workspace_tab(delta, cx);
            return;
        }
        let cur = self.layout.focused().and_then(|p| self.layout.session(p));
        let pos = cur
            .and_then(|id| items.iter().position(|i| i.id == id))
            .unwrap_or(0);
        let len = items.len().cast_signed();
        let next = (pos.cast_signed() + delta).rem_euclid(len).cast_unsigned();
        if let Some(item) = items.get(next) {
            self.open_session(item.id, cx);
        }
    }

    /// 전역 설정은 데몬 응답을 받은 뒤 반영한다. 실패하면 현재 값을 유지하고 이유를 보인다.
    fn load_agents(&mut self, permission: Option<Permission>, cx: &mut Context<Self>) {
        if self.agents_busy {
            return;
        }
        let Some(target) = self.local_target() else {
            return;
        };
        self.agents_busy = true;
        let client = Arc::clone(&self.agent_client);
        self.agent_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { client.settings_for(&target, permission) })
                .await;
            this.update(cx, |this, cx| {
                this.agents_busy = false;
                match result {
                    Ok(settings) => {
                        let profiles = settings.profiles.clone();
                        this.agents = Some(settings);
                        this.sidebar.update(cx, |s, cx| s.set_agents(profiles, cx));
                    }
                    Err(e) => this.notice = Some(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 새 탭에서 에이전트를 지금 보이는 폴더(MK-055)로 띄운다. 선택된 실행 위치가 원격이면 원격 기본 디렉터리에서 시작한다.
    fn new_agent(&mut self, id: String, cx: &mut Context<Self>) {
        self.new_agent_with_optional_account(id, None, cx);
    }

    pub(super) fn new_agent_with_optional_account(
        &mut self,
        id: String,
        account: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let host = self.launch_host.clone();
        let cwd = if host.is_remote()
            && self
                .layout
                .focused()
                .is_none_or(|focused| *self.layout.host(focused) != host)
        {
            PathBuf::new()
        } else {
            self.folder()
        };
        krill_app::star_nag::record_agent_start(cx);
        self.change(true, cx, |l| {
            l.new_tab();
            let pane = l.active_tab().focused;
            if host.is_remote() {
                l.set_host(pane, host);
            }
            l.set_launch(
                pane,
                Create {
                    account,
                    program: Program::Agent { id },
                    cwd: cwd.as_os_str().as_bytes().to_vec(),
                    cols: pane::SIZE.0,
                    rows: pane::SIZE.1,
                },
            );
        });
    }

    /// 첫 Attached 성공 때만 업데이트 health 신호를 보낸다.
    pub fn arm_update_ready(&mut self, state: PathBuf) {
        self.update_ready = Some(state);
    }

    /// 보이는 칸만 세션에 붙어 있게 한다: 안 보이는 칸의 연결은 닫고, 새로 보이는 칸은 붙이기 시작한다.
    fn sync(&mut self, cx: &mut Context<Self>) {
        if self.layout_loading {
            return;
        }
        let visible = self.layout.visible();
        let layout = &self.layout;
        self.creations.retain(|p, _| layout.pane_exists(*p));
        self.live.retain(|p, live| {
            visible.contains(p) || layout.pane_exists(*p) && live.pane.keep_account_input()
        });
        self.security.retain_panes(&visible);
        if self.paths.is_none() && self.host_targets.is_empty() {
            return;
        }
        let mut any_terminal = false;
        for p in visible {
            if self.layout.file(p).is_some() {
                continue; // 파일 칸은 세션에 붙지 않는다(MK-119)
            }
            // 업데이트 health는 로컬 칸 연결로 판정한다. 원격 칸만 보이면 호스트가 꺼졌거나 SSH가 느릴 때
            // 30초 안에 붙지 못해 업데이트가 매번 롤백됐다(설 실기, v0.1.10→0.1.11).
            any_terminal |= self.layout.host(p).is_local();
            if self.layout.launch(p).is_none() {
                self.layout.set_launch(p, self.launch_for(p));
                self.save();
            }
            let Some(target) = self.target_for_pane(p) else {
                continue;
            };
            if let Entry::Vacant(e) = self.live.entry(p) {
                let mut live = Live::new(Self::spawn_pane(p, target, None, cx));
                live.creation = Arc::clone(self.creations.entry(p).or_default());
                e.insert(live);
            }
        }
        if !any_terminal && let Some(state) = self.update_ready.take() {
            cx.background_spawn(async move {
                krill_app::updater::mark_ready(&state, env!("CARGO_PKG_VERSION"));
            })
            .detach();
        }
    }

    /// 칸 하나의 IO 태스크: 세션을 정해 붙고, 프레임을 읽어 UI에 넘기고, 끊기면 다시 붙는다. 칸이 안 보이게 되면
    /// ([`Live`]가 지워짐) 끝난다.
    // ponytail: 프레임마다 백그라운드 작업 하나(소켓을 옮겨 다님)와 막힌 read 하나가 칸마다 스레드 하나를 쥔다.
    // 보이는 칸은 많아야 수십이라 그대로 둔다. 늘면 연결 하나에 세션 여러 개(MK-023 오웬 확인 항목)로.
    fn spawn_pane(
        pane: PaneId,
        target: krill_app::hosts::HostTarget,
        resume: Option<krill_proto::SessionId>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            let mut attempt = 0u32;
            let is_remote = target.host().is_remote();
            loop {
                let Ok(Some((want, known, launch, creation))) = this.read_with(cx, |t, _| {
                    let want = t.layout.session(pane);
                    let l = t.live.get(&pane)?;
                    let mut launch = t.launch_for(pane);
                    if let Some((cols, rows)) = l.pane.grid() {
                        (launch.cols, launch.rows) = (cols, rows); // 새 세션도 칸 크기로(OWE-534)
                    }
                    // OWE-842: 죽은 세션 자리 재스폰은 셸 내부 `cd` 포함 최신 cwd로.
                    // 폴더(`launches`)는 워크트리 루트를 유지하므로 세션 없는 새 칸에는 덮지 않는다.
                    if want.is_some()
                        && let Some(respawn) = t.layout.respawn_cwd(pane)
                    {
                        launch.cwd = respawn.to_vec();
                    }
                    Some((
                        want,
                        want.and_then(|w| l.pane.known(w)),
                        launch,
                        l.creation.clone(),
                    ))
                }) else {
                    return;
                };
                let p = target.clone();
                let old = resume.filter(|r| want.is_none() || want == Some(*r));
                let got = bg
                    .spawn(async move {
                        let (session, prior_cold) = match old {
                            Some(old) => (
                                pane::resume_session_in(&p, Some(old), &launch)?,
                                pane::load_cold(&p, old).map(|c| (old, c)),
                            ),
                            None => match pane::ensure_session_in(&p, want, &launch)? {
                                pane::Found::Live(session, _) => (session, None),
                                // OWE-842(시나리오 B): 재부팅 등으로 이전 세션이 콜드가 되었으면
                                // 같은 cwd·셸 설정으로 새 세션을 즉시 자동 재스폰하고,
                                // 이전 세션의 콜드 화면·스크롤백을 새 세션 위쪽 히스토리로 보존한다.
                                // 재개에 실패하면(예: holder 없음) 기존 콜드 읽기 전용 화면으로 폴백한다.
                                pane::Found::Cold(cold_session, cold) => {
                                    match pane::resume_session_in(&p, Some(cold_session), &launch) {
                                        Ok(new_session) => {
                                            (new_session, Some((cold_session, cold)))
                                        }
                                        Err(_) => return Ok(Got::Cold(cold_session, cold)),
                                    }
                                }
                            },
                        };
                        // 명시적 닫기가 Create 응답보다 먼저 와도 새 세션을 남기지 않는다.
                        if !close::Creation::publish(&creation, session) {
                            close::send(&krill_app::resources::daemon_call_for(Some(p)), session)?;
                            return Err(std::io::ErrorKind::Interrupted.into());
                        }
                        let known = known.filter(|_| want == Some(session));
                        match pane::attach(&p, session, known) {
                            Ok(linked) => Ok(Got::Live {
                                session,
                                linked: Ok(linked),
                                prior_cold,
                            }),
                            // AS-27: attach 직전 세션이 끝나도(`Refused`→`NotFound`) 디스크 마지막
                            // 화면을 보여준다. 백그라운드에서 읽어 UI 스레드를 막지 않는다.
                            Err(e) if is_permanent_error(&e) => {
                                Ok(match pane::load_cold(&p, session) {
                                    Some(cold) => Got::Cold(session, cold),
                                    None => Got::Live {
                                        session,
                                        linked: Err(e),
                                        prior_cold,
                                    },
                                })
                            }
                            Err(e) => Ok(Got::Live {
                                session,
                                linked: Err(e),
                                prior_cold,
                            }),
                        }
                    })
                    .await;
                let next = this.update(cx, |t, cx| t.settled(pane, got, cx));
                let mut reader = match next {
                    Ok(Next::Read(r)) => r,
                    Ok(Next::Retry) => {
                        let delay = if is_remote {
                            krill_app::hosts::HostManager::backoff_delay(attempt.saturating_add(1))
                        } else {
                            retry_delay(attempt)
                        };
                        bg.timer(delay).await;
                        attempt = attempt.saturating_add(1);
                        continue;
                    }
                    Ok(Next::Gone) | Err(_) => return,
                };
                attempt = 0;
                loop {
                    let (r, frame) = bg
                        .spawn(async move {
                            let f = ipc::read_frame(&mut reader);
                            (reader, f)
                        })
                        .await;
                    reader = r;
                    let Ok(Some((k, body))) = frame else { break };
                    match this.update(cx, |t, cx| t.on_frame(pane, k, &body, cx)) {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(_) => return,
                    }
                }
                let alive = this.update(cx, |t, cx| {
                    let l = t.live.get_mut(&pane)?;
                    l.cut();
                    if l.pane.is_closed() {
                        return None;
                    }
                    let n = l.pane.unlink();
                    t.show(n.as_ref(), cx);
                    Some(())
                });
                if !matches!(alive, Ok(Some(()))) {
                    return;
                }
                bg.timer(retry_delay(0)).await;
            }
        })
    }

    /// 붙기 시도가 끝났다. `got`은 세션 정하기의 결과와, 정했다면 그 세션에 붙기의 결과. 정한 세션은 붙기가 실패해도
    /// 레이아웃에 적는다 — 안 적으면 다음 시도가 세션을 또 만든다(OWE-506). 콜드 기록이면 읽기 전용 화면을 띄운다(MK-032).
    fn settled(&mut self, pane: PaneId, got: std::io::Result<Got>, cx: &mut Context<Self>) -> Next {
        let got = match got {
            Ok(Got::Cold(session, cold)) => return self.cold(pane, session, cold, cx),
            Ok(Got::Live {
                session,
                linked,
                prior_cold,
            }) => Ok((session, linked, prior_cold)),
            Err(e) => Err(e),
        };
        let got = got.and_then(|(session, linked, prior_cold)| {
            if let Some((cold_session, cold)) = prior_cold
                && let Some(l) = self.live.get_mut(&pane)
                && !l.pane.is_cold()
            {
                l.pane.show_cold(cold_session, cold).ok();
            }
            if self.layout.session(pane) != Some(session) {
                if self.layout.host(pane).is_local() {
                    self.layout.clear_session(pane);
                }
                self.layout.set_session(pane, session);
                self.sync_sidebar_custom_titles(cx);
                self.record_workspace_nav(cx);
                self.save();
                self.refresh_detached(cx);
            }
            linked.map(|l| (session, l))
        });
        match got {
            Ok((session, linked)) => self.linked(pane, session, linked, cx),
            Err(e) => {
                let permanent = is_permanent_error(&e);
                let old = self.layout.session(pane);
                match self.live.get_mut(&pane) {
                    Some(l) => {
                        if permanent
                            && !l.pane.is_cold()
                            && !l.pane.is_closed()
                            && let Some(session) = l.pane.session().or(old)
                        {
                            l.pane
                                .show_cold(
                                    session,
                                    krill_proto::control::Cold {
                                        screen: Vec::new(),
                                        history: Vec::new(),
                                    },
                                )
                                .ok();
                        }
                        l.error = Some(e.to_string());
                        cx.notify();
                        if permanent { Next::Gone } else { Next::Retry }
                    }
                    None => Next::Gone,
                }
            }
        }
    }

    /// 칸의 세션이 사라졌다: 디스크 기록의 마지막 화면을 읽기 전용으로 띄운다. 레이아웃의 세션은 그대로 둔다
    /// ("다시 시작"을 누를 때까지 앱을 다시 띄워도 같은 콜드 화면).
    fn cold(
        &mut self,
        pane: PaneId,
        session: krill_proto::SessionId,
        cold: krill_proto::control::Cold,
        cx: &mut Context<Self>,
    ) -> Next {
        let show = self.live.get_mut(&pane).map(|l| {
            let result = l.pane.show_cold(session, cold);
            (result, l.pane.screen().title.clone())
        });
        let Some((show, title)) = show else {
            return Next::Gone;
        };
        if let Err(e) = show {
            if let Some(l) = self.live.get_mut(&pane) {
                l.error = Some(e.to_string());
            }
            cx.notify();
            return Next::Retry;
        }
        // AS-27: attach 실패 뒤 콜드로 온 세션도 레이아웃에 적는다(다음 시도가 또 만들지 않게).
        if self.layout.session(pane) != Some(session) {
            self.layout.clear_session(pane);
            self.layout.set_session(pane, session);
            self.sync_sidebar_custom_titles(cx);
            self.record_workspace_nav(cx);
            self.save();
            self.refresh_detached(cx);
        }
        // 읽기 전용 옛 화면이라 낙관적 확정 글자를 지운다(OWE-615 원인 1).
        if let Some(l) = self.live.get_mut(&pane) {
            l.ime.clear_pending();
            l.error = None;
        }
        if let Some(state) = self.update_ready.take() {
            cx.background_spawn(async move {
                krill_app::updater::mark_ready(&state, env!("CARGO_PKG_VERSION"));
            })
            .detach();
        }
        self.titles.insert(pane, title);
        cx.notify();
        Next::Gone
    }

    /// 콜드·종료 화면의 "다시 시작"(MK-032·MK-052): 칸에서 옛 세션을 떼고 에이전트 재개(`claude --resume`,
    /// `codex resume`) 또는 새 셸에 붙인다. 옛 기록은 보존 기간 동안 디스크에 남는다.
    ///
    /// 잠정 결정 의존: OWE-397 [MK-032] 콜드 복원 뷰. 거부되면 이 함수를 새 세션 생성으로 되돌린다.
    fn restart(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if let Some(target) = self
            .target_for_pane(pane)
            .filter(|_| self.layout.visible().contains(&pane))
        {
            let old = self
                .live
                .get(&pane)
                .and_then(|l| l.pane.session())
                .or_else(|| self.layout.session(pane));
            let task = Self::spawn_pane(pane, target, old, cx);
            if let Some(l) = self.live.get_mut(&pane) {
                l.error = None;
                l.ime.clear_pending();
                l.task = task;
            } else {
                let mut live = Live::new(task);
                live.creation = Arc::clone(self.creations.entry(pane).or_default());
                self.live.insert(pane, live);
            }
            cx.notify();
        } else {
            self.live.remove(&pane);
            self.change(true, cx, |l| l.clear_session(pane));
        }
    }

    /// 칸이 세션에 붙었다. 보류 입력을 보낸다. 읽을 소켓을 돌려준다.
    fn linked(
        &mut self,
        pane: PaneId,
        session: krill_proto::SessionId,
        linked: pane::Linked,
        cx: &mut Context<Self>,
    ) -> Next {
        self.start_pane_liveness(cx);
        let Some(l) = self.live.get_mut(&pane) else {
            return Next::Gone;
        };
        l.error = None;
        if let Some(launch) = &linked.launch {
            krill_app::onboarding::actions::note_agent_session(&launch.profile, cx);
        }
        l.launch = linked.launch;
        l.sock = linked.reader.try_clone().ok();
        let notices = l.pane.link(
            session,
            linked.instance,
            linked.writer,
            linked.features,
            Now::get(),
        );
        l.pane.set_searchable(linked.search);
        // 새 화면이 오면 낙관적 확정 글자는 옛 세션의 잔상이라 지운다(OWE-615 원인 1).
        l.ime.clear_pending();
        if let Some(state) = self.update_ready.take() {
            cx.background_spawn(async move {
                krill_app::updater::mark_ready(&state, env!("CARGO_PKG_VERSION"));
            })
            .detach();
        }
        for n in &notices {
            self.show(Some(n), cx);
        }
        cx.notify();
        Next::Read(linked.reader)
    }

    /// krilld 프레임 하나. 연결을 이어 가면 `true`.
    fn on_frame(&mut self, pane: PaneId, k: u8, body: &[u8], cx: &mut Context<Self>) -> bool {
        let Some(l) = self.live.get_mut(&pane) else {
            return false;
        };
        let Ok(ev) = l.pane.frame(k, body) else {
            l.cut();
            return false;
        };
        if ev.closed {
            l.cut();
            l.error = None;
            l.ime = Ime::default();
            l.echo = EchoDraw::default();
            l.clear_selection();
        }
        if ev.redraw {
            let s = l.pane.screen();
            let c = s.cursor;
            l.ime.set_cursor(
                CellPoint {
                    line: usize::from(c.line),
                    column: usize::from(c.column),
                },
                c.visible,
            );
            // 확정 에코 만료(숨은 커서면 `set_cursor`이 무시하므로 시간으로 지운다, OWE-615 원인 1).
            l.ime.expire_pending(Instant::now());
            let title = krill_app::security::sanitize_title(&s.title);
            let search_res = l.pane.search_result().cloned();
            if let Some(w) = self.window {
                l.echo.screen_changed(w, cx);
            }
            if self.layout.host(pane).is_local()
                && let Some(session) = self.layout.session(pane)
            {
                self.sidebar
                    .update(cx, |s, cx| s.set_title(session, &title, cx));
            }
            self.titles.insert(pane, title);
            if self.layout.focused() == Some(pane)
                && let Some((bar, _)) = &self.search
            {
                bar.update(cx, |b, cx| b.set_result(search_res.as_ref(), cx));
            }
            // AS-17: 출력 스크롤·리사이즈·재접속으로 선택 자리 글자가 바뀌면 선택을 지운다.
            // 끌기 중이거나 글자가 그대로면(커서만 움직임 등) 유지한다.
            if !l.selecting && l.selection.is_some() {
                let current = l.selection.map(|s| s.text(&l.pane.shown()));
                if current != l.selection_text {
                    l.clear_selection();
                }
            }
            if self.layout.focused() == Some(pane) {
                self.maybe_poll_active_chat(cx);
            }
            cx.notify();
        }
        if ev.closed {
            cx.notify();
        }
        if ev.bell && krill_app::settings::notification_sound(cx) {
            platform::beep();
        }
        if ev.bell
            && let Some(id) = self.layout.session(pane)
        {
            let title = self.titles.get(&pane).cloned().unwrap_or_default();
            krill_app::alert::notify_bell(id, &title, cx);
        }
        if let Some(text) = ev.clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        self.show(ev.notice.as_ref(), cx);
        !ev.closed
    }

    fn show(&mut self, notice: Option<&Notice>, cx: &mut Context<Self>) {
        if let Some(n) = notice {
            self.notice = Some(notice_text(n));
            cx.notify();
        }
    }

    /// 보이는 칸 중 보낸 입력의 ack가 PROBE 넘게 오지 않은 칸이 있으면 배너 문구(OWE-586).
    /// ack·재연결이 오면 다음 그리기에서 사라진다.
    fn stalled_banner(&self) -> Option<String> {
        let now = Now::get();
        let any = self
            .layout
            .visible()
            .iter()
            .filter_map(|p| self.live.get(p))
            .any(|l| l.pane.stalled(now));
        krill_app::link::stalled_banner(any)
    }

    /// 포커스 칸에 PTY 바이트를 보낸다(IME 결과). 조합을 끊었으면 플랫폼 IME 쪽 조합도 버린다.
    /// IME 바깥 요인(포커스 칸 변경·탭 전환·창 비활성화·붙여넣기 확정)에서만 부른다. 키 이벤트 안에서는
    /// `handleEvent:` 재진입 중이라 `discardMarkedText`를 부르면 IMK 오토마타가 꼬여 다음 글자가
    /// 분리되므로 [`Shell::send_in_key`]를 쓴다(OWE-615 원인 2).
    fn send(&mut self, out: &Output, window: Option<&Window>, cx: &mut Context<Self>) -> bool {
        self.send_inner(out, true, window, cx)
    }

    /// 키 이벤트(`on_key_down`·`on_key_up`) 안에서 PTY 바이트를 보낸다. IME 자신이 그 키를 처리
    /// 중이므로 `discardMarkedText`를 동기 재진입으로 부르지 않는다(OWE-615 원인 2). 위젯이 직접
    /// 확정한 글자의 늦은 `insertText:` 중복은 `krill_term::ime`의 `committed`로 버린다.
    fn send_in_key(
        &mut self,
        out: &Output,
        window: Option<&Window>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.send_inner(out, false, window, cx)
    }

    fn send_inner(
        &mut self,
        out: &Output,
        allow_discard: bool,
        window: Option<&Window>,
        cx: &mut Context<Self>,
    ) -> bool {
        if out.discard_ime
            && allow_discard
            && let Some(w) = window
        {
            platform::discard_marked_text(w);
        }
        let Some(pane) = self.layout.focused() else {
            return false;
        };
        if out.pty.is_empty() {
            cx.notify();
            return false;
        }
        if self.queue_input_behind_paste(pane, &out.pty) {
            cx.notify();
            return true;
        }
        let Some(l) = self.live.get_mut(&pane) else {
            return false;
        };
        l.clear_selection();
        l.echo.key_sent(Instant::now());
        let n = l.pane.input(&out.pty, Now::get());
        self.show(n.as_ref(), cx);
        cx.notify();
        n.is_none()
    }

    fn focused_live(&mut self) -> Option<&mut Live> {
        let pane = self.layout.focused()?;
        self.live.get_mut(&pane)
    }

    fn on_key_down(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() || !self.focus.is_focused(window) {
            return;
        }
        if self.confirm_close_tab.is_some() {
            match e.keystroke.key.as_str() {
                "escape" => self.cancel_close_pinned_tab(cx),
                "enter" => self.confirm_close_pinned_tab(cx),
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        // OWE-872: 실행 중 닫기 확인 대화상자가 열려 있으면 Enter로 닫고 Esc로 취소한다.
        if self.pending_running_close.is_some() {
            match e.keystroke.key.as_str() {
                "escape" => self.cancel_running_close(cx),
                "enter" => self.confirm_running_close(cx),
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        // OWE-870: 이름 변경 대화상자가 열려 있으면 Esc로 닫는다(Enter 저장은 입력창 구독이 맡는다).
        if self.tab_rename.is_some() && e.keystroke.key == "escape" {
            self.cancel_tab_rename(cx);
            cx.stop_propagation();
            return;
        }
        if self.tab_context_menu.is_some() && e.keystroke.key == "escape" {
            self.tab_context_menu = None;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.layout.mru_switching() {
            if e.keystroke.key == "escape" {
                self.commit_mru_switcher(cx);
                cx.stop_propagation();
                return;
            }
            if !e.keystroke.modifiers.control {
                self.commit_mru_switcher(cx);
            }
        }
        // MK-156: 우하단 플로팅 UpdateCard가 열려 있을 때 Esc 키를 누르면 상태바 배지로 축소한다.
        // 리소스 팝오버·세션 기록 패널이 열려 있을 때도 Esc로 닫는다.
        // 닫지 않으면 `\x1b`가 터미널 PTY로 전달된다.
        if e.keystroke.key == "escape"
            && !e.keystroke.modifiers.modified()
            && (self.resources.read(cx).is_open() || self.history.read(cx).is_open())
        {
            self.resources
                .update(cx, krill_app::resources::Panel::close);
            if self.history.read(cx).is_open()
                && !self.panel_collapsed
                && self.diff.read(cx).active_tab() == krill_app::diff::PanelTab::History
            {
                self.toggle_panel(cx);
            } else {
                self.history
                    .update(cx, krill_app::agent_history::Panel::close);
            }
            window.focus(&self.focus, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.role == WindowRole::Main
            && e.keystroke.key == "escape"
            && !e.keystroke.modifiers.modified()
            && cx
                .try_global::<krill_app::updater::UpdateState>()
                .is_some_and(|u| u.staged.is_some() && !u.card_collapsed)
        {
            let mut u = cx
                .try_global::<krill_app::updater::UpdateState>()
                .cloned()
                .unwrap_or_default();
            u.card_collapsed = true;
            cx.set_global(u);
            cx.stop_propagation();
            cx.refresh_windows();
            cx.notify();
            return;
        }
        // 가운데 diff가 터미널을 덮고 있으면 Esc로 닫고, 다른 키는 가려진 터미널로 보내지 않는다.
        if self.diff.read(cx).has_center_diff() {
            if e.keystroke.key == "escape" && !e.keystroke.modifiers.modified() {
                self.close_center_diff(cx);
                cx.stop_propagation();
            }
            return;
        }
        // Cmd 키는 `tui::key`가 단축키(액션)로 보고 `None`을 돌려준다. Cmd+Backspace만 Ctrl+U로 보낸다.
        let ks = &e.keystroke;
        let Some(l) = self.focused_live() else { return };
        ime_trace(format_args!(
            "key_down {ks:?} held={} composing={}",
            e.is_held,
            l.ime.is_composing()
        ));
        let event = if e.is_held {
            keys::tui::Event::Repeat
        } else {
            keys::tui::Event::Press
        };
        let meta = krill_app::settings::option_as_meta(cx);
        let Some(bytes) =
            keys::tui::key(ks, l.pane.modes(), event).or_else(|| keys::tui::meta(ks, meta))
        else {
            return; // 글자 키는 IME 확정 텍스트(replace_text_in_range)로 온다
        };
        // 조합 중 Backspace·방향키 등은 IME가 `handleEvent:` 안에서 먼저 처리하고, 처리했으면
        // `doCommandBySelector:`가 오지 않아 여기까지 오지 않는다. 여기 온 키는 IME가 처리하지
        // 않은 키라 조합을 먼저 확정하고 보낸다. `discardMarkedText`는 재진입이라 부르지 않는다.
        let mut out = l.ime.commit();
        out.pty = keys::tui::text(&out.pty, l.pane.modes());
        out.pty.extend(bytes);
        self.send_in_key(&out, Some(window), cx);
        cx.stop_propagation();
    }

    fn on_key_up(&mut self, e: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        // AS-12: `on_key_down`과 같이 포커스를 검사한다. 검색 바·Jump·사이드바 입력 중에
        // 올라온 Release가 버블링돼 터미널 PTY로 새는 것을 막는다(kitty 플래그 2 보고 모드).
        if self.settings.is_some() || !self.focus.is_focused(window) {
            return;
        }
        let Some(l) = self.focused_live() else { return };
        ime_trace(format_args!(
            "key_up {:?} composing={}",
            e.keystroke,
            l.ime.is_composing()
        ));
        // 조합 중 일반 문자 키의 Release는 IME 자모 키를 뗀 것인데 영문 키 Release 시퀀스가 되어
        // `echo.key_sent`를 유발한다. 보내지 않고 삼킨다(OWE-615 원인 4).
        if l.ime.is_composing() && is_plain_char_key(&e.keystroke) {
            cx.stop_propagation();
            return;
        }
        if let Some(pty) = keys::tui::key(&e.keystroke, l.pane.modes(), keys::tui::Event::Release) {
            self.send_in_key(
                &Output {
                    pty,
                    discard_ime: false,
                },
                Some(window),
                cx,
            );
            cx.stop_propagation();
        }
    }

    fn mouse_input(
        &mut self,
        pane: PaneId,
        event: keys::tui::Mouse,
        position: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        // 파일 드롭(gpui가 왼쪽 버튼 이동·놓기로 바꿔 보낸다)은 프로그램에 마우스 보고로 보내지 않는다.
        if cx.has_active_drag() {
            return;
        }
        let Some(l) = self.live.get_mut(&pane) else {
            return;
        };
        let Some((column, line)) = cell_at(&l.painted, l.pane.screen(), position) else {
            return;
        };
        let Some(bytes) = keys::tui::mouse(l.pane.modes(), event, column, line, modifiers) else {
            return;
        };
        let n = l.pane.input(&bytes, Now::get());
        self.show(n.as_ref(), cx);
        cx.stop_propagation();
    }

    fn on_scroll(
        &mut self,
        pane: PaneId,
        e: &ScrollWheelEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let line = cell_size(window, cx).height;
        let Some(l) = self.live.get_mut(&pane) else {
            return;
        };
        let dy = f32::from(e.delta.pixel_delta(line).y) + l.scroll_rem;
        let lines = (dy / f32::from(line)).trunc();
        l.scroll_rem = dy - lines * f32::from(line);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "휠 한 번은 수백 행 안이고 abs 뒤에만 usize로 바꾼다"
        )]
        if lines != 0. {
            if e.modifiers.platform || (cfg!(target_os = "windows") && e.modifiers.control) {
                krill_app::settings::step_terminal_font_size(if lines > 0. { 1 } else { -1 }, cx);
                cx.stop_propagation();
                return;
            }
            if let Some((column, line)) = cell_at(&l.painted, l.pane.screen(), e.position)
                && let Some(bytes) = keys::tui::mouse(
                    l.pane.modes(),
                    keys::tui::Mouse::Wheel(lines > 0.),
                    column,
                    line,
                    e.modifiers,
                )
            {
                let mut report = Vec::new();
                for _ in 0..(lines.abs() as usize).min(100) {
                    report.extend_from_slice(&bytes);
                }
                let n = l.pane.input(&report, Now::get());
                self.show(n.as_ref(), cx);
                cx.stop_propagation();
            } else if l.pane.scroll(lines as isize) {
                l.clear_selection();
                cx.notify();
            }
        }
    }

    /// 마우스 선택을 쓸지: 프로그램이 마우스 보고를 켜지 않았거나 Shift를 눌렀다(MK-033).
    fn selects(&self, pane: PaneId, modifiers: Modifiers) -> bool {
        self.live
            .get(&pane)
            .is_some_and(|l| modifiers.shift || !l.pane.modes().mouse())
    }

    /// 왼쪽 버튼 누름(OWE-866): Shift+클릭은 기존 anchor를 유지한 채 head만 옮기고,
    /// 더블클릭은 단어, 트리플클릭은 줄을 선택한다. 그리드 밖이면 그대로.
    fn select_begin(
        &mut self,
        pane: PaneId,
        position: Point<Pixels>,
        click_count: usize,
        shift: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(l) = self.live.get_mut(&pane) else {
            return;
        };
        let screen = l.pane.shown();
        let Some((column, line)) = cell_at(&l.painted, &screen, position) else {
            return;
        };
        let at = select::Point { line, column };
        if shift && l.selection.is_some() {
            let anchor = l.selection.map_or(at, |sel| sel.anchor);
            l.selection = Some(Selection { anchor, head: at });
            l.selection_text = None;
            l.selecting = true;
            l.selecting_kind = SelectKind::Char;
            cx.notify();
            return;
        }
        let (selection, kind) = if click_count >= 3 {
            (Selection::line_at(&screen, at), SelectKind::Line)
        } else if click_count == 2 {
            (Selection::word_at(&screen, at), SelectKind::Word)
        } else {
            (Selection::new(at), SelectKind::Char)
        };
        l.selection = Some(selection);
        l.selection_text = None;
        l.selecting = true;
        l.selecting_kind = kind;
        cx.notify();
    }

    /// 선택 끌기 중 끝점 이동: 누름 때 정한 단위(글자·단어·줄)를 유지한다(OWE-866).
    fn select_move(&mut self, pane: PaneId, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(l) = self.live.get_mut(&pane) else {
            return;
        };
        let screen = l.pane.shown();
        let Some((column, line)) = cell_at(&l.painted, &screen, position) else {
            return;
        };
        let at = select::Point { line, column };
        let (Some(mut sel), selecting, kind) = (l.selection, l.selecting, l.selecting_kind) else {
            return;
        };
        if !selecting {
            return;
        }
        match kind {
            SelectKind::Char => sel.head = at,
            SelectKind::Word => {
                let word = Selection::word_at(&screen, at);
                sel.head = if at >= sel.anchor {
                    word.head
                } else {
                    word.anchor
                };
            }
            SelectKind::Line => {
                let cols = screen.cols;
                sel.head = if at.line >= sel.anchor.line {
                    select::Point {
                        line: at.line,
                        column: cols.saturating_sub(1),
                    }
                } else {
                    select::Point {
                        line: at.line,
                        column: 0,
                    }
                };
            }
        }
        l.selection = Some(sel);
        cx.notify();
    }

    /// 선택 끌기를 마친다. 움직이지 않은 클릭이면 선택을 지운다(OWE-866: 단어·줄
    /// 선택은 한 칸짜리라도 유지한다). 끌던 중이었으면 `true`.
    fn select_end(&mut self, pane: PaneId, cx: &mut Context<Self>) -> bool {
        let Some(l) = self.live.get_mut(&pane).filter(|l| l.selecting) else {
            return false;
        };
        l.selecting = false;
        let keep_single = l.selecting_kind != SelectKind::Char;
        l.selecting_kind = SelectKind::Char;
        if l.selection.is_some_and(|s| s.anchor == s.head) && !keep_single {
            l.clear_selection();
        } else {
            l.snapshot_selection();
        }
        cx.notify();
        true
    }

    /// 포커스 칸 커서가 깜박여야 하면(프로그램 지정 또는 설정 기본값, OWE-658) 타이머로 숨김을 뒤집는다.
    /// 깜박일 커서가 없으면 타이머를 버린다.
    fn sync_cursor_blink(&mut self, active: bool, cx: &mut Context<Self>) {
        let default = krill_app::settings::terminal_cursor(cx);
        let blink = active
            && self.settings.is_none()
            && self.dashboard.is_none()
            && !self.is_active_chat_view()
            && self
                .layout
                .focused()
                .filter(|pane| self.layout.file(*pane).is_none())
                .and_then(|pane| self.live.get(&pane))
                .is_some_and(|l| {
                    let screen = l.pane.shown();
                    screen.cursor.visible
                        && krill_app::terminal::effective_cursor(screen.cursor_style, default).blink
                });
        if !blink {
            self.cursor_blink = None;
        } else if self.cursor_blink.is_none() {
            let task = cx.spawn(async move |this, cx| {
                let bg = cx.background_executor().clone();
                loop {
                    bg.timer(CURSOR_BLINK).await;
                    let flipped = this.update(cx, |t, cx| {
                        if let Some((_, hidden)) = &mut t.cursor_blink {
                            *hidden = !*hidden;
                            cx.notify();
                        }
                    });
                    if flipped.is_err() {
                        return;
                    }
                }
            });
            self.cursor_blink = Some((task, false));
        }
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if self.settings.is_some() || self.dashboard.is_some() {
            return;
        }
        let Some(l) = self.focused_live() else { return };
        let Some(sel) = l.selection else { return };
        let text = sel.text(&l.pane.shown());
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.settings.is_some() || self.dashboard.is_some() {
            return;
        }
        self.paste_clipboard(window, cx);
    }

    /// 포커스를 잃은 칸의 IME 조합을 확정한다(탭 전환·창 비활성화, MK-028 규칙 4).
    /// AS-02: `window`가 없어도 저장된 창 핸들로 플랫폼 조합을 버린다. 같은 창의
    /// `inputContext` 하나를 쓰므로 버리지 않으면 다음 자모가 새 칸에서 이어지거나
    /// 늦은 `insertText`가 새 칸에 중복된다. 키 이벤트 밖(마우스·사이드바·Jump)이라
    /// `handleEvent:` 재진입이 없어 `discardMarkedText`를 직접 불러도 된다.
    fn commit_ime(&mut self, pane: PaneId, window: Option<&Window>, cx: &mut Context<Self>) {
        let Some(l) = self.live.get_mut(&pane) else {
            return;
        };
        let mut out = l.ime.commit();
        out.pty = keys::tui::text(&out.pty, l.pane.modes());
        if out.discard_ime {
            if let Some(w) = window {
                platform::discard_marked_text(w);
            } else if let Some(handle) = self.window {
                #[cfg(test)]
                let discards = self.ime_discards.clone();
                // 마우스·액션 처리 중 현재 창은 이미 빌려져 있다. 이벤트가 끝난 뒤
                // 다시 빌려야 `window not found`로 조합 폐기가 누락되지 않는다.
                cx.defer(move |cx| {
                    handle
                        .update(cx, |_, window, _| {
                            platform::discard_marked_text(window);
                            #[cfg(test)]
                            discards.set(discards.get() + 1);
                        })
                        .ok();
                });
            }
        }
        if !out.pty.is_empty() {
            if self.queue_input_behind_paste(pane, &out.pty) {
                return;
            }
            let Some(l) = self.live.get_mut(&pane) else {
                return;
            };
            let n = l.pane.input(&out.pty, Now::get());
            self.show(n.as_ref(), cx);
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// 레이아웃을 바꾸고 저장한다. 드래그 중에는 `save = false`로 그리기만 하고 놓을 때 저장한다.
    /// 포커스 칸이 바뀌면 이전 칸의 IME 조합을 확정하고, 보이는 칸이 바뀌면 연결을 맞춘다.
    fn change(&mut self, save: bool, cx: &mut Context<Self>, f: impl FnOnce(&mut Layout)) {
        self.change_with_policy(save, close::Policy::Preserve, cx, f);
    }

    fn change_with_policy(
        &mut self,
        save: bool,
        policy: close::Policy,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Layout),
    ) {
        let folder = self.folder();
        let focused = self.layout.focused();
        let before = self.layout.clone();
        f(&mut self.layout);
        if self.hold_dirty_close(&before, policy, cx) {
            return;
        }
        self.cancel_closed_pastes(cx);
        if policy == close::Policy::Terminate {
            self.close_removed_sessions(&before, cx);
        }
        let now_focused = self.layout.focused();
        if self.zoomed_pane.is_some_and(|z| Some(z) != now_focused) {
            self.zoomed_pane = None;
        }
        if now_focused != focused {
            self.files.refocus = true;
            if let Some(old) = focused {
                self.cancel_pending_paste_for(old, cx);
                self.commit_ime(old, None, cx);
            }
            self.diff
                .update(cx, krill_app::diff::Panel::clear_selection);
        }
        self.sync_focused_session(cx);
        self.sync_sidebar_custom_titles(cx);
        if self.folder() != folder {
            let folder = self.folder();
            self.diff.update(cx, |d, cx| d.set_repo(folder, cx));
        }
        self.sync(cx);
        if now_focused != focused {
            self.poll_active_chat(cx);
        }
        if save && self.layout != before {
            self.close_dashboard(None, cx);
            self.close_settings(None, cx);
            self.record_workspace_nav(cx);
            self.save();
            self.refresh_detached(cx);
        }
        cx.notify();
    }

    pub(super) fn focused_local_session(&self) -> Option<krill_proto::SessionId> {
        if self.settings.is_some() || self.dashboard.is_some() {
            return None;
        }
        let pane = self.layout.focused()?;
        if self.layout.file(pane).is_some() || !self.layout.host(pane).is_local() {
            return None;
        }
        self.layout.session(pane)
    }

    pub(super) fn sync_focused_session(&self, cx: &mut Context<Self>) {
        let session = self.focused_local_session();
        krill_app::alert::focus(session, cx);
        self.sidebar.update(cx, |s, cx| s.set_focused(session, cx));
    }

    /// 세션을 보인다: 어떤 탭의 칸에 있으면 그 탭·칸으로, 없으면(분리된 세션) 새 탭에 붙인다.
    pub(super) fn open_session(&mut self, session: krill_proto::SessionId, cx: &mut Context<Self>) {
        self.open_session_on(krill_app::hosts::HostId::Local, session, cx);
    }

    pub(super) fn open_session_on(
        &mut self,
        host: krill_app::hosts::HostId,
        session: krill_proto::SessionId,
        cx: &mut Context<Self>,
    ) {
        if self.layout_loading {
            self.restore_session = Some((host, session));
            return;
        }
        self.dashboard = None;
        self.sync_sidebar_visibility(cx);
        self.close_settings(None, cx);
        // CLI에서 만든 세션도 워크트리(비-git이면 작업 디렉터리)가 Open in·새 에이전트의 폴더가 된다(MK-055).
        let folder = self
            .sidebar
            .read(cx)
            .folder_on(&host, session)
            .map(std::path::Path::to_path_buf);
        if let Some(old) = self.layout.focused() {
            self.commit_ime(old, None, cx);
        }
        let target = self.tab_pane_for_session_on(&host, session);
        self.change(true, cx, |l| {
            if let Some((tab, pane)) = target {
                l.activate(tab);
                l.focus(pane);
            } else {
                l.new_tab();
                if let Some(pane) = l.focused() {
                    l.set_host(pane, host);
                    l.set_session(pane, session);
                }
            }
            if let Some(folder) = folder
                && let Some(pane) = l.focused()
                && (target.is_none() || l.launch(pane).is_none_or(|launch| launch.cwd.is_empty()))
            {
                let mut launch = l.launch(pane).cloned().unwrap_or(Create {
                    account: None,
                    program: Program::Login,
                    cwd: Vec::new(),
                    cols: pane::SIZE.0,
                    rows: pane::SIZE.1,
                });
                launch.cwd = folder.as_os_str().as_bytes().to_vec();
                l.set_launch(pane, launch);
            }
        });
        self.sidebar.update(cx, sidebar::Sidebar::reveal_focused);
        if let Some(handle) = self.window {
            let focus = self.focus.clone();
            cx.update_window(handle, |_, window, cx| window.focus(&focus, cx))
                .ok();
        }
    }

    /// 지정한 호스트·세션이 붙은 칸을 찾는다(탭 인덱스·칸). OWE-854 사이드바 이어하기가 쓴다.
    fn tab_pane_for_session_on(
        &self,
        host: &krill_app::hosts::HostId,
        session: krill_proto::SessionId,
    ) -> Option<(usize, PaneId)> {
        for i in 0..self.layout.tabs().len() {
            let mut layout = self.layout.clone();
            layout.activate(i);
            if let Some(pane) = layout
                .visible()
                .into_iter()
                .find(|&p| layout.session(p) == Some(session) && layout.host(p) == host)
            {
                return Some((i, pane));
            }
        }
        None
    }

    /// OWE-854: 사이드바 세션 행의 '새 세션에 이어하기…'. 칸에 없으면 먼저 붙이고 모달을 연다.
    fn continue_sidebar_session(
        &mut self,
        host: &krill_app::hosts::HostId,
        session: krill_proto::SessionId,
        cx: &mut Context<Self>,
    ) {
        let mut target = self.tab_pane_for_session_on(host, session);
        if target.is_none() {
            self.open_session_on(host.clone(), session, cx);
            target = self.tab_pane_for_session_on(host, session);
        }
        let Some((_, pane)) = target else {
            return;
        };
        let Some(handle) = self.window.and_then(|h| h.downcast::<Shell>()) else {
            self.notice = Some(text::SIDEBAR_NO_WINDOW.get().to_owned());
            cx.notify();
            return;
        };
        cx.defer(move |cx| {
            handle
                .update(cx, |this, window, cx| {
                    this.open_continue_session(pane, window, cx);
                })
                .ok();
        });
    }

    /// OWE-854: 사이드바 세션 행의 '세션 종료…'. 확인 후 해당 호스트 데몬에 `Request::Close`를 보낸다.
    fn request_close_sidebar_session(
        &mut self,
        host: krill_app::hosts::HostId,
        session: krill_proto::SessionId,
        cx: &mut Context<Self>,
    ) {
        if self.target_for_host(&host).is_none() {
            self.notice = Some(text::DAEMON_FAILED.get().to_owned());
            cx.notify();
            return;
        }
        let Some(handle) = self.window.and_then(|h| h.downcast::<Shell>()) else {
            self.notice = Some(text::SIDEBAR_NO_WINDOW.get().to_owned());
            cx.notify();
            return;
        };
        cx.defer(move |cx| {
            handle
                .update(cx, |this, window, cx| {
                    this.confirm_close_sidebar_session(&host, session, window, cx);
                })
                .ok();
        });
    }

    /// '세션 종료…' 확인 대화상자 뒤에 호스트 데몬에 종료를 요청한다. 결과는 알림에 남긴다.
    fn confirm_close_sidebar_session(
        &mut self,
        host: &krill_app::hosts::HostId,
        session: krill_proto::SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use krill_app::i18n::catalog::resources as rtext;
        if self.target_for_host(host).is_none() {
            self.notice = Some(text::DAEMON_FAILED.get().to_owned());
            cx.notify();
            return;
        }
        let name = {
            let sidebar = self.sidebar.read(cx);
            sidebar.session_on(host, session).map_or_else(
                || format!("{} {}", rtext::SESSION.get(), session.0),
                |s| sidebar.session_label(s).0,
            )
        };
        let title = rtext::KILL_TITLE_NAMED.format(&[("name", &name)]);
        let detail = rtext::KILL_DETAIL.get().to_owned();
        let yes = rtext::KILL_CONFIRM.get().to_owned();
        let no = rtext::CANCEL.get().to_owned();
        let answer = window.prompt(
            PromptLevel::Warning,
            &title,
            Some(&detail),
            &[yes.as_str(), no.as_str()],
            cx,
        );
        let host = host.clone();
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            this.update(cx, |this, cx| {
                this.close_sidebar_session(&host, session, cx);
            })
            .ok();
        })
        .detach();
    }

    fn refresh_detached(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.local_target() {
            let attached = self
                .layout
                .sessions_for_host(&krill_app::hosts::HostId::Local);
            self.detached
                .update(cx, |d, cx| d.refresh(target, attached, cx));
        }
    }

    fn save(&self) {
        if self.layout_loading {
            return;
        }
        if let Some(path) = &self.path {
            self.saver.save(path, &self.layout);
        }
    }

    pub(super) fn sync_sidebar_visibility(&self, cx: &mut Context<Self>) {
        let visible = !self.sidebar_collapsed || self.dashboard.is_some();
        self.sidebar.update(cx, |s, cx| s.set_visible(visible, cx));
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        // OWE-860: 설정에서 ⌘B를 누르면 설정을 닫고 사이드바를 전환한다.
        if self.settings.is_some() {
            self.close_settings(None, cx);
        }
        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.layout.sidebar_collapsed = self.sidebar_collapsed;
        self.save();
        self.sync_sidebar_visibility(cx);
        cx.notify();
    }

    fn toggle_history(&mut self, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            self.close_settings(None, cx);
        }
        if !self.panel_collapsed
            && self.diff.read(cx).active_tab() == krill_app::diff::PanelTab::History
        {
            self.toggle_panel(cx);
            self.pending_focus = PendingFocus::Workspace;
        } else {
            self.panel_collapsed = false;
            self.layout.panel_collapsed = false;
            self.diff.update(cx, |d, cx| {
                d.set_tab(krill_app::diff::PanelTab::History, cx);
                d.set_visible(true, cx);
            });
            self.save();
            cx.notify();
        }
    }

    fn toggle_panel(&mut self, cx: &mut Context<Self>) {
        // OWE-860: 설정에서 ⌘L 등을 누르면 설정을 닫고 패널을 전환한다.
        if self.settings.is_some() {
            self.close_settings(None, cx);
        }
        self.panel_collapsed = !self.panel_collapsed;
        self.layout.panel_collapsed = self.panel_collapsed;
        self.save();
        let visible = !self.panel_collapsed && self.security.is_window_active();
        self.diff.update(cx, |d, cx| d.set_visible(visible, cx));
        cx.notify();
    }

    fn toggle_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            return;
        }
        if self.panel_collapsed {
            self.panel_collapsed = false;
            let visible = self.security.is_window_active();
            self.diff.update(cx, |d, cx| {
                d.set_visible(visible, cx);
                d.show_changes(window, cx);
            });
            cx.notify();
            return;
        }
        if self.diff.read(cx).active_tab() == krill_app::diff::PanelTab::Changes {
            self.panel_collapsed = true;
            self.diff.update(cx, |d, cx| d.set_visible(false, cx));
            window.focus(&self.focus, cx);
        } else {
            self.diff.update(cx, |d, cx| {
                d.show_changes(window, cx);
            });
        }
        cx.notify();
    }

    fn toggle_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // OWE-860: 설정에서 ⌘⇧E를 누르면 설정을 닫고 탐색기로 전환한다.
        if self.settings.is_some() {
            self.close_settings(Some(&mut *window), cx);
        }
        if self.panel_collapsed {
            self.panel_collapsed = false;
            self.layout.panel_collapsed = false;
            let visible = self.security.is_window_active();
            self.diff.update(cx, |d, cx| {
                d.set_visible(visible, cx);
                d.show_files(window, cx);
            });
            let _ = self.layout.set_panel_tab(krill_app::diff::PanelTab::Files);
            self.save();
            cx.notify();
            return;
        }
        if self.diff.read(cx).active_tab() == krill_app::diff::PanelTab::Files {
            self.panel_collapsed = true;
            self.layout.panel_collapsed = true;
            self.save();
            self.diff.update(cx, |d, cx| d.set_visible(false, cx));
            window.focus(&self.focus, cx);
        } else {
            self.diff.update(cx, |d, cx| d.show_files(window, cx));
            if self.layout.set_panel_tab(krill_app::diff::PanelTab::Files) {
                self.save();
            }
        }
        cx.notify();
    }

    fn toggle_workspace_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // OWE-860: 설정에서 ⌘⇧F를 누르면 설정을 닫고 작업공간 검색으로 전환한다.
        if self.settings.is_some() {
            self.close_settings(Some(&mut *window), cx);
        }
        if self.panel_collapsed {
            self.panel_collapsed = false;
            self.layout.panel_collapsed = false;
            let visible = self.security.is_window_active();
            self.diff.update(cx, |d, cx| {
                d.set_visible(visible, cx);
                d.show_search(window, cx);
            });
            let _ = self.layout.set_panel_tab(krill_app::diff::PanelTab::Search);
            self.save();
            cx.notify();
            return;
        }
        if self.diff.read(cx).active_tab() == krill_app::diff::PanelTab::Search {
            self.panel_collapsed = true;
            self.layout.panel_collapsed = true;
            self.save();
            self.diff.update(cx, |d, cx| d.set_visible(false, cx));
            self.files.refocus = true;
            window.focus(&self.focus, cx);
        } else {
            self.diff.update(cx, |d, cx| d.show_search(window, cx));
            if self.layout.set_panel_tab(krill_app::diff::PanelTab::Search) {
                self.save();
            }
        }
        cx.notify();
    }

    fn on_column_drag(&mut self, event: &DragMoveEvent<Drag>, cx: &mut Context<Self>) {
        let drag = event.drag(cx).clone();
        let x = f32::from(event.event.position.x);
        let left = f32::from(event.bounds.left());
        let total = f32::from(event.bounds.size.width);
        let l = &self.layout;
        let sidebar = if self.sidebar_collapsed {
            0.0
        } else {
            l.sidebar_width
        };
        let panel = if self.panel_collapsed {
            0.0
        } else {
            l.panel_width
        };
        match drag {
            Drag::Sidebar => self.change(false, cx, |l| {
                l.sidebar_width = layout::clamp_column(x - left, total, panel);
            }),
            Drag::Panel => self.change(false, cx, |l| {
                l.panel_width = layout::clamp_column(left + total - x, total, sidebar);
            }),
            Drag::Split(_) => {}
        }
    }

    fn render_sidebar_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = krill_app::ui::palette(cx);
        let can_back = self.can_navigate_back(cx);
        let can_forward = self.can_navigate_forward(cx);
        div()
            .id("sidebar-titlebar")
            .h(krill_app::ui::TITLEBAR_HEIGHT)
            .w_full()
            .flex_none()
            .pl(traffic_light_pad(window))
            .pr_2()
            .flex()
            .items_center()
            .gap_1()
            .bg(p.sidebar)
            .border_b_1()
            .border_color(p.sidebar_border)
            .text_color(p.muted_foreground)
            .child(
                div()
                    .id("titlebar-title")
                    // K1 OWE-843: 좁은 사이드바·큰 UI 배율에서도 앱 이름을 찌그러뜨리지 않는다.
                    .flex_none()
                    .px_1()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(krill_app::ui::TEXT_SM)
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(p.sidebar_foreground)
                    .child(
                        krill_app::ui::icons::krill_brand_icon(krill_app::ui::ICON_MD, p)
                            .debug_selector(|| "titlebar-brand-mark".into()),
                    )
                    .child(div().truncate().child(text::APP_TITLE.get())),
            )
            .child(krill_app::ui::button_with_tooltip(
                Button::new("toggle-sidebar")
                    .icon(IconName::PanelLeft)
                    .ghost()
                    .xsmall()
                    .debug_selector(|| "toggle-sidebar".into())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                "toggle-sidebar",
                text::TOGGLE_SIDEBAR.get(),
            ))
            .child(
                div()
                    .id("sidebar-titlebar-drag")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .window_control_area(gpui::WindowControlArea::Drag),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .child(krill_app::ui::button_with_tooltip(
                        Button::new("sidebar-history-prev")
                            .icon(
                                gpui_component::Icon::default()
                                    .data(krill_app::ui::icons::ARROW_LEFT_SVG),
                            )
                            .ghost()
                            .xsmall()
                            .disabled(!can_back)
                            .debug_selector(|| "sidebar-history-prev".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.navigate_back(Some(window), cx);
                            })),
                        "sidebar-history-prev",
                        krill_app::i18n::catalog::settings::SC_NAV_BACK.get(),
                    ))
                    .child(krill_app::ui::button_with_tooltip(
                        Button::new("sidebar-history-next")
                            .icon(
                                gpui_component::Icon::default()
                                    .data(krill_app::ui::icons::ARROW_RIGHT_SVG),
                            )
                            .ghost()
                            .xsmall()
                            .disabled(!can_forward)
                            .debug_selector(|| "sidebar-history-next".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.navigate_forward(Some(window), cx);
                            })),
                        "sidebar-history-next",
                        krill_app::i18n::catalog::settings::SC_NAV_FORWARD.get(),
                    )),
            )
    }

    fn render_tree(
        &mut self,
        node: &PaneTree,
        path: &mut Vec<Branch>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (axis, ratio, first, second) = match node {
            PaneTree::Leaf(id) => return self.render_pane(*id, window, cx),
            PaneTree::Split {
                axis,
                ratio,
                first,
                second,
            } => (*axis, *ratio, first, second),
        };
        let mine = path.clone();
        path.push(Branch::First);
        let first = self.render_tree(first, path, window, cx);
        path.pop();
        path.push(Branch::Second);
        let second = self.render_tree(second, path, window, cx);
        path.pop();

        let divider_id = ElementId::Name(format!("split-{mine:?}").into());
        let drag = Drag::Split(mine.clone());
        let horizontal = axis == Axis::Horizontal;
        div()
            .flex()
            .size_full()
            .when(!horizontal, Styled::flex_col)
            .on_drag_move(cx.listener(move |this, e: &DragMoveEvent<Drag>, _, cx| {
                if *e.drag(cx) != Drag::Split(mine.clone()) {
                    return;
                }
                let (pos, start, len) = if horizontal {
                    (e.event.position.x, e.bounds.left(), e.bounds.size.width)
                } else {
                    (e.event.position.y, e.bounds.top(), e.bounds.size.height)
                };
                let r = layout::drag_ratio(f32::from(pos), f32::from(start), f32::from(len));
                let mine = mine.clone();
                this.change(false, cx, move |l| l.set_ratio(&mine, r));
            }))
            .child(
                div()
                    .flex_basis(relative(ratio))
                    .flex_shrink_0()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(first),
            )
            .child(divider(divider_id, drag, axis, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(second),
            )
            .into_any_element()
    }

    /// YOLO 권한 칸 표시. 단일 칸은 패인 헤더가 숨으므로 탭에도 붙인다(OWE-653).
    fn yolo_badge(&self, pane: PaneId, cx: &App) -> Option<gpui::Div> {
        let p = krill_app::ui::palette(cx);
        self.live
            .get(&pane)
            .and_then(|l| l.launch.as_ref())
            .filter(|launch| launch.permission == Permission::Yolo)
            .map(|_| {
                div()
                    .debug_selector(|| "yolo-badge".into())
                    .flex_none()
                    .px_1p5()
                    .py_0p5()
                    .rounded(krill_app::ui::RADIUS_XS)
                    .bg(p.warning)
                    .text_color(p.warning_foreground)
                    .text_size(krill_app::ui::TEXT_2XS)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(text::YOLO_BADGE.get())
            })
    }

    pub(super) fn host_name_for_pane(&self, pane: PaneId, cx: &App) -> Option<String> {
        let krill_app::hosts::HostId::Remote(id) = self.layout.host(pane) else {
            return None;
        };
        if let Some(h) = self
            .sidebar
            .read(cx)
            .remote_hosts()
            .iter()
            .find(|h| h.id == *id)
        {
            return Some(h.label.clone());
        }
        if let Some((spec, _)) = self
            .host_statuses
            .iter()
            .find(|(s, _)| s.id == *id || s.address == *id)
        {
            return Some(spec.display_name().to_owned());
        }
        if let Some(h) = self
            .ssh_hosts
            .iter()
            .find(|h| h.id == *id || h.destination == *id)
        {
            let label = h.label.trim();
            return Some(if label.is_empty() {
                h.destination.clone()
            } else {
                label.to_owned()
            });
        }
        Some(id.clone())
    }

    /// MK-131: 원격 호스트 세션 패인의 탭·패인 헤더에 표시하는 호스트 이름 배지.
    pub(super) fn host_badge(
        &self,
        pane: PaneId,
        prefix: &'static str,
        cx: &App,
    ) -> Option<gpui::Div> {
        let host_name = self.host_name_for_pane(pane, cx)?;
        let p = krill_app::ui::palette(cx);
        let id = pane.get();
        Some(
            div()
                .debug_selector(move || format!("{prefix}-{id}"))
                .flex_none()
                .px_1p5()
                .py_0p5()
                .rounded(krill_app::ui::RADIUS_XS)
                .bg(p.sidebar_accent)
                .border_1()
                .border_color(p.border)
                .text_color(p.muted_foreground)
                .text_size(krill_app::ui::TEXT_2XS)
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(host_name),
        )
    }

    /// OWE-873: 스크롤 중(바닥 위)일 때 터미널 칸 우측에 스크롤 위치 막대와
    /// '맨 아래로' 배지를 겹쳐 그린다. 바닥에 있으면 아무것도 그리지 않는다.
    fn scroll_chrome(&self, id: PaneId, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(l) = self.live.get(&id) else {
            return Vec::new();
        };
        if l.pane.session().is_none() {
            return Vec::new();
        }
        let fetched = l.pane.scrollback_len();
        let offset = l.pane.scroll_offset();
        if offset == 0 || fetched == 0 {
            return Vec::new();
        }
        let lines = usize::from(l.pane.screen().lines).max(1);
        #[expect(
            clippy::cast_precision_loss,
            reason = "행 수는 수만 이하라 f32로도 순서·비율이 유지된다"
        )]
        let (lines, fetched, shown) = (lines as f32, fetched as f32, offset.min(fetched) as f32);
        // 전체(화면 + 받아 둔 행)에서 화면이 차지하는 비율이 막대 길이, 받아 둔 행 중
        // 아직 위쪽에 남은 비율이 막대 머리 위치다(받는 중인 위쪽 행은 어림값).
        let thumb = (lines / (lines + fetched)).clamp(0.05, 1.);
        let top = ((fetched - shown) / (lines + fetched)).clamp(0., 1. - thumb);
        let p = krill_app::ui::palette(cx);
        let bar = div()
            .debug_selector(|| "scrollbar".into())
            .absolute()
            .top(px(4.))
            .bottom(px(4.))
            .right(px(2.))
            .w(px(6.))
            .flex()
            .flex_col()
            .child(div().w_full().h(gpui::relative(top)))
            .child(
                div()
                    .debug_selector(|| "scrollbar-thumb".into())
                    .w_full()
                    .h(gpui::relative(thumb))
                    .min_h(px(12.))
                    .rounded_full()
                    .bg(p.ring),
            );
        let bottom = div()
            .id(("scroll-to-bottom", id.get()))
            .debug_selector(|| "scroll-to-bottom".into())
            .absolute()
            .bottom(px(10.))
            .right(px(14.))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(l) = this.live.get_mut(&id) {
                    l.clear_selection();
                    l.pane.scroll(isize::MIN);
                    cx.notify();
                }
            }))
            .child(krill_app::ui::badge(
                p,
                krill_app::ui::BadgeTone::Neutral,
                text::SCROLL_TO_BOTTOM.get(),
            ));
        vec![bar.into_any_element(), bottom.into_any_element()]
    }

    fn render_pane(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.layout.file(id).is_some() {
            return self.render_file_pane(id, window, cx);
        }
        let focused = self.layout.focused() == Some(id);
        let is_split = self.layout.visible().len() > 1;
        let show_header = is_split || self.layout.host(id).is_remote();
        let p = krill_app::ui::palette(cx);
        let pane_title = self.pane_label(id, cx);
        let yolo_badge = self.yolo_badge(id, cx);
        let host_badge = self.host_badge(id, "pane-host-badge", cx);
        // OWE-886: 탭 바와 같은 식별자(상태 글리프 + 에이전트 브랜드 아이콘, STYLEGUIDE §6.4·§7.4).
        let host = self.layout.host(id);
        let session = self
            .layout
            .session(id)
            .and_then(|sid| self.sidebar.read(cx).session_on(host, sid));
        let launch_agent = match self.layout.launch(id).map(|l| &l.program) {
            Some(Program::Agent { id }) => tabs::launch_agent_id(id),
            _ => "",
        };
        let agent = session.map_or(launch_agent, |s| {
            if s.status.agent.is_empty() {
                launch_agent
            } else {
                &s.status.agent
            }
        });
        let state = session.map_or(AgentState::Unknown, |s| s.status.state);
        let pane_fg = if focused {
            p.sidebar_foreground
        } else {
            p.muted_foreground
        };
        // OWE-871: 확대(줌) 상태면 헤더에 배지와 해제 버튼을 보인다.
        let zoomed = self.zoomed_pane == Some(id);
        let pane_header = div()
            .debug_selector(move || format!("pane-header-{}", id.get()))
            .h(px(26.))
            .px_2()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .bg(p.sidebar)
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        krill_app::ui::status::status_glyph(
                            state,
                            false,
                            krill_app::ui::status::Size::Md,
                            cx,
                        )
                        .debug_selector(move || format!("pane-state-{}", id.get())),
                    )
                    .child(krill_app::ui::status::agent_icon(
                        agent,
                        krill_app::ui::ICON_SM,
                        pane_fg,
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(krill_app::ui::TEXT_XS)
                            .font_weight(if focused {
                                gpui::FontWeight::SEMIBOLD
                            } else {
                                gpui::FontWeight::NORMAL
                            })
                            .text_color(if focused {
                                p.sidebar_foreground
                            } else {
                                p.muted_foreground
                            })
                            .child(pane_title),
                    )
                    .children(yolo_badge)
                    .children(host_badge)
                    .when(zoomed, |d| {
                        d.child(
                            div()
                                .id(("pane-zoom-badge", id.get()))
                                .debug_selector(move || format!("pane-zoom-badge-{}", id.get()))
                                .aria_label(text::ZOOMED_BADGE.get())
                                .flex_none()
                                .px_1()
                                .rounded(krill_app::ui::RADIUS_XS)
                                .bg(p.muted)
                                .font_family(krill_app::ui::FONT_UI)
                                .text_size(krill_app::ui::TEXT_2XS)
                                .text_color(p.muted_foreground)
                                .child(text::ZOOMED_BADGE.get()),
                        )
                        .child(krill_app::ui::button_with_tooltip(
                            krill_app::ui::ghost(
                                ("pane-unzoom", id.get()),
                                text::PANE_UNZOOM.get(),
                            )
                            .debug_selector(move || format!("pane-unzoom-{}", id.get()))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.zoomed_pane = None;
                                    cx.notify();
                                },
                            )),
                            format!("pane-unzoom-{}", id.get()),
                            text::PANE_UNZOOM.get(),
                        ))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_0p5()
                    .child(krill_app::ui::button_with_tooltip(
                        Button::new(("pane-split-right", id.get()))
                            .icon(IconName::PanelRight)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                // OWE-871: 분할하면 확대 상태는 푼다(분할 액션과 같은 규칙).
                                this.zoomed_pane = None;
                                this.change(true, cx, |l| {
                                    l.focus(id);
                                    l.split(Axis::Horizontal);
                                });
                            })),
                        format!("pane-split-right-{}", id.get()),
                        text::SPLIT_RIGHT.get(),
                    ))
                    .child(krill_app::ui::button_with_tooltip(
                        Button::new(("pane-split-down", id.get()))
                            .icon(IconName::PanelBottom)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                // OWE-871: 분할하면 확대 상태는 푼다(분할 액션과 같은 규칙).
                                this.zoomed_pane = None;
                                this.change(true, cx, |l| {
                                    l.focus(id);
                                    l.split(Axis::Vertical);
                                });
                            })),
                        format!("pane-split-down-{}", id.get()),
                        text::SPLIT_DOWN.get(),
                    ))
                    .when(is_split, |d| {
                        d.child(krill_app::ui::button_with_tooltip(
                            Button::new(("pane-close", id.get()))
                                .debug_selector(move || format!("pane-close-{}", id.get()))
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    // OWE-871: ⌘W와 같이 닫은 기록을 남기고 확대 상태를 푼다.
                                    // OWE-872: 실행 중이면 확인 대화상자를 먼저 띄운다.
                                    this.maybe_close_pane(id, cx);
                                })),
                            format!("pane-close-{}", id.get()),
                            text::CLOSE_PANE.get(),
                        ))
                    }),
            );
        let body = match self.live.get(&id) {
            Some(l) if l.pane.session().is_some() => {
                let mut el = TerminalElement::new(l.pane.shown(), self.terminal_theme)
                    .with_row_cache(l.row_cache.clone());
                el.opacity = krill_app::settings::terminal_opacity_alpha(cx);
                el.cursor = krill_app::terminal::effective_cursor(
                    el.screen.cursor_style,
                    krill_app::settings::terminal_cursor(cx),
                );
                el.cursor_hidden = focused && self.cursor_blink.as_ref().is_some_and(|b| b.1);
                // OWE-871: 비포커스 칸의 블록 커서는 빈 테두리로 그려 포커스 칸과 구별한다.
                el.cursor_hollow = !focused;
                el.painted = Some(Rc::clone(&l.painted));
                el.selection = l.selection;
                el.highlight = l.pane.highlight();
                el.highlights = l.pane.highlights().to_vec();
                el.link = self.path_hover(id);
                el.preedit = focused.then(|| l.ime.overlay()).flatten().map(|o| Preedit {
                    origin: o.origin,
                    text: o.text.to_owned(),
                    columns: o.columns,
                });
                // 확정 뒤 에코 대기 글자는 화면에 아직 없어도 그린다(OWE-615 원인 1).
                el.pending = focused.then(|| l.ime.pending()).flatten().map(|p| Preedit {
                    origin: p.origin,
                    text: p.text.to_owned(),
                    columns: p.columns,
                });
                if l.pane.is_cold() || l.pane.is_closed() {
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        // OWE-653: 배너는 테두리 있는 띠로, 문구는 본문 색·왼쪽, 버튼은 오른쪽 끝(다시 시작=주 버튼).
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .mb_1()
                                .px_2()
                                .py_1()
                                .rounded(krill_app::ui::RADIUS_SM)
                                .border_1()
                                .border_color(p.border)
                                .bg(p.muted)
                                .text_color(p.foreground)
                                .text_size(krill_app::ui::TEXT_XS)
                                .flex_wrap()
                                .debug_selector(|| "session-ended".into())
                                .child(div().min_w_0().child(if l.pane.is_closed() {
                                    text::ENDED.get()
                                } else {
                                    text::COLD.get()
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .flex_none()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            Button::new(ElementId::Name(
                                                format!("restart-{}", id.get()).into(),
                                            ))
                                            .debug_selector(move || format!("restart-{}", id.get()))
                                            .label(text::RESTART.get())
                                            .primary()
                                            .xsmall()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.restart(id, cx);
                                            })),
                                        )
                                        .child(
                                            Button::new(("close-ended-pane", id.get()))
                                                .label(text::CLOSE_PANE.get())
                                                .debug_selector(|| "close-ended-pane".into())
                                                .ghost()
                                                .xsmall()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    // OWE-871: 끝난 칸을 닫을 때도 기록·줌 해제는 패인 헤더 ×와 같다.
                                                    this.zoomed_pane = None;
                                                    this.record_closed_pane(id, cx);
                                                    this.change(true, cx, |layout| {
                                                        layout.focus(id);
                                                        layout.close_pane();
                                                    });
                                                })),
                                        ),
                                )
                                .children(l.error.clone().map(|err| {
                                    div()
                                        .debug_selector(|| "pane-error".into())
                                        .text_color(p.destructive)
                                        .child(krill_app::i18n::cached_notice(&err))
                                })),
                        )
                        .child(div().flex_1().child(el))
                        .into_any_element()
                } else {
                    el.into_any_element()
                }
            }
            Some(l) => div()
                .text_size(krill_app::ui::TEXT_XS)
                .child(l.error.as_deref().map_or_else(
                    || text::CONNECTING.get().to_owned(),
                    krill_app::i18n::cached_notice,
                ))
                .into_any_element(),
            None => div()
                .text_size(krill_app::ui::TEXT_XS)
                .child(format!("{} {}", text::PANE.get(), id.get()))
                .into_any_element(),
        };
        let term_bg = krill_app::ui::terminal_surface_alpha(
            self.terminal_theme,
            krill_app::settings::terminal_opacity_pct(cx),
        );
        let pane = div()
            .id(ElementId::Name(format!("pane-{}", id.get()).into()))
            .size_full()
            .overflow_hidden()
            .bg(term_bg)
            .text_color(p.muted_foreground)
            .flex()
            .flex_col()
            // OWE-653 / MK-131: 단일 로컬 칸은 탭이 이미 제목을 보이므로 헤더 줄을 숨기고, 분할 칸이거나 원격 칸이면 헤더를 보인다.
            .when(show_header, |d| d.child(pane_header))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .p(krill_app::ui::SPACE_2)
                    .child(
                        div()
                            .size_full()
                            .relative()
                            .child(body)
                            .children(self.fit_probe(id, cx))
                            .children(self.scroll_chrome(id, cx)),
                    )
                    .children(self.render_pane_chat_overlay(id, window, cx)),
            )
            .children(self.render_pane_security(id, cx))
            .when(self.files.hover.as_ref().is_some_and(|h| h.0 == id), |d| {
                d.cursor_pointer()
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                    if e.modifiers.platform {
                        let mut file = None;
                        let mut url = None;
                        if let Some(l) = this.live.get(&id) {
                            let screen = l.pane.shown();
                            if let Some(uri) = link_target(&screen, &l.painted, e.position) {
                                url = Some(uri.to_owned());
                            } else if let Some(path) =
                                file_link_target(&screen, &l.painted, e.position)
                            {
                                file = Some(path);
                            } else if let Some((_, uri)) = krill_app::terminal::url_link_target(
                                &screen, &l.painted, e.position,
                            ) {
                                url = Some(uri);
                            }
                        }
                        if let Some(uri) = url {
                            cx.open_url(&uri);
                        } else {
                            // 파일 링크는 가운데 에디터 탭으로(MK-119), 폴더는 Finder로. 출력 속 경로도 에디터로(OWE-663).
                            match file {
                                Some(path) => this.open_terminal_file(path, cx),
                                None => {
                                    if let Some((_, link)) =
                                        this.path_link_at(id, e.position, true, cx)
                                    {
                                        this.open_path_link(link, cx);
                                    }
                                }
                            }
                        }
                        cx.stop_propagation();
                        return;
                    }
                    this.change(true, cx, |l| l.focus(id));
                    if this.selects(id, e.modifiers) {
                        this.select_begin(id, e.position, e.click_count, e.modifiers.shift, cx);
                    } else {
                        this.mouse_input(
                            id,
                            keys::tui::Mouse::Press(0),
                            e.position,
                            e.modifiers,
                            cx,
                        );
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                    this.mouse_input(id, keys::tui::Mouse::Press(1), e.position, e.modifiers, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                    // 우클릭 메뉴의 복사·붙여넣기가 이 칸에 가도록 포커스를 옮긴다.
                    this.change(true, cx, |l| l.focus(id));
                    this.mouse_input(id, keys::tui::Mouse::Press(2), e.position, e.modifiers, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseUpEvent, _, cx| {
                    if this.select_end(id, cx) {
                        // 선택 시 복사(OWE-658): 남은 선택이 있으면 ⌘C와 같은 경로로 복사한다.
                        if krill_app::settings::copy_on_select(cx) {
                            this.copy(cx);
                        }
                        return;
                    }
                    this.mouse_input(
                        id,
                        keys::tui::Mouse::Release(0),
                        e.position,
                        e.modifiers,
                        cx,
                    );
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, e: &gpui::MouseUpEvent, _, cx| {
                    this.mouse_input(
                        id,
                        keys::tui::Mouse::Release(1),
                        e.position,
                        e.modifiers,
                        cx,
                    );
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(move |this, e: &gpui::MouseUpEvent, _, cx| {
                    this.mouse_input(
                        id,
                        keys::tui::Mouse::Release(2),
                        e.position,
                        e.modifiers,
                        cx,
                    );
                }),
            )
            .on_mouse_move(cx.listener(move |this, e: &gpui::MouseMoveEvent, _, cx| {
                this.update_hover_link(id, e.position, cx);
                this.update_path_hover(id, e.position, e.modifiers.platform, cx);
                if e.pressed_button == Some(MouseButton::Left)
                    && this.live.get(&id).is_some_and(|l| l.selecting)
                {
                    this.select_move(id, e.position, cx);
                    return;
                }
                let b = match e.pressed_button {
                    Some(MouseButton::Left) => Some(0),
                    Some(MouseButton::Middle) => Some(1),
                    Some(MouseButton::Right) => Some(2),
                    _ => None,
                };
                this.mouse_input(id, keys::tui::Mouse::Move(b), e.position, e.modifiers, cx);
            }))
            .on_scroll_wheel(cx.listener(move |this, e: &ScrollWheelEvent, window, cx| {
                this.on_scroll(id, e, window, cx);
            }))
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                use krill_proto::os::ffi::OsStrExt as _;
                this.change(true, cx, |l| l.focus(id));
                let text = krill_term::paste::dropped(
                    paths.paths().iter().map(|p| p.as_os_str().as_bytes()),
                );
                this.paste_bytes(&text, window, cx);
            }));
        // 우클릭 메뉴는 프로그램이 마우스 보고를 켜지 않았을 때만(켰으면 우클릭은 프로그램 것이다).
        let peer_supports_fork = self.peer_supports_fork(id);
        let can_fork = self.can_fork_pane(id, cx);
        let can_continue = self.can_continue_pane(id);
        let menu = self
            .live
            .get(&id)
            .filter(|l| !l.pane.modes().mouse())
            .map(|l| (l.selection.is_some(), self.focus.clone()));
        match menu {
            Some((has_selection, focus)) => pane
                .context_menu(move |m, _, _| {
                    // OWE-871: 복사·붙여넣기만 있던 메뉴에 선택·분할·줌·닫기·지우기를 연결한다.
                    let m = m
                        .action_context(focus.clone())
                        .menu_with_disabled(text::COPY.get(), Box::new(Copy), !has_selection)
                        .menu(text::PASTE.get(), Box::new(Paste))
                        .separator()
                        .menu(text::PANE_SELECT_ALL.get(), Box::new(SelectAll))
                        .menu(text::PANE_CLEAR.get(), Box::new(ClearPane))
                        .separator()
                        .menu(text::SPLIT_RIGHT.get(), Box::new(SplitRight))
                        .menu(text::SPLIT_DOWN.get(), Box::new(SplitDown))
                        .menu(text::PANE_ZOOM_TOGGLE.get(), Box::new(ToggleZoomPane));
                    if peer_supports_fork {
                        m.separator()
                            .menu_with_disabled(
                                text::FORK_SESSION.get(),
                                Box::new(ForkSession),
                                !can_fork,
                            )
                            .menu_with_disabled(
                                text::CONTINUE_IN_NEW_SESSION.get(),
                                Box::new(ContinueInNewSession),
                                !can_continue,
                            )
                            .separator()
                            .menu(text::CLOSE_PANE.get(), Box::new(ClosePane))
                    } else {
                        m.separator()
                            .menu(text::CLOSE_PANE.get(), Box::new(ClosePane))
                    }
                })
                .into_any_element(),
            None => pane.into_any_element(),
        }
    }
}

/// 보존 톤 배너(STYLEGUIDE §3.3): `preserve` 12% 바탕·30% 테두리, 앞에 모노 크릴 마크 12px.
fn preserve_banner(
    id: &'static str,
    msg: impl Into<SharedString>,
    p: &krill_app::ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .px_3()
        .py_1p5()
        .flex()
        .items_center()
        .gap_1p5()
        .bg(p.preserve.opacity(0.12))
        .border_b_1()
        .border_color(p.preserve.opacity(0.3))
        .text_color(p.foreground)
        .text_size(krill_app::ui::TEXT_XS)
        .child(krill_app::ui::icons::svg_box(
            krill_app::ui::icons::KRILL_SVG,
            krill_app::ui::ICON_XS,
            p.preserve,
        ))
        .child(div().min_w_0().child(msg.into()))
}

/// 분할선: 1pt 선 + 잡는 폭 5pt. 드래그 값은 `Drag`, 실제 크기 계산은 부모의 `on_drag_move`가 한다.
/// OWE-871: 기본 선 색은 `p.split_divider`(STYLEGUIDE §3.1)이며, 분할 트리의 선(`reset` 있음)을
/// 더블클릭하면 50:50(`ratio = 0.5`)으로 되돌린다.
/// 타이틀바 첫 컨트롤의 왼쪽 여백. 전체화면에서는 신호등이 숨으므로 비우지 않는다(K11).
fn traffic_light_pad(window: &Window) -> Pixels {
    if window.is_fullscreen() {
        krill_app::ui::SPACE_2
    } else {
        krill_app::ui::TRAFFIC_LIGHT_PAD
    }
}

/// 분할선. 더블클릭하면 분할은 50:50, 사이드바·우측 패널은 기본 너비로 되돌린다.
fn divider(id: ElementId, drag: Drag, axis: Axis, cx: &mut Context<Shell>) -> impl IntoElement {
    let p = krill_app::ui::palette(cx);
    let line = div().bg(p.split_divider);
    div()
        .id(id)
        .flex_none()
        .flex()
        .justify_center()
        .hover(|d| d.bg(p.split_divider_strong))
        .map(|d| match axis {
            Axis::Horizontal => d
                .w(DIVIDER_GRAB)
                .h_full()
                .cursor_col_resize()
                .child(line.w(px(1.)).h_full()),
            Axis::Vertical => d
                .h(DIVIDER_GRAB)
                .w_full()
                .flex_col()
                .cursor_row_resize()
                .child(line.h(px(1.)).w_full()),
        })
        .on_drag(drag.clone(), |_, _, _, cx| cx.new(|_| DragGhost))
        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
            if e.click_count() < 2 {
                return;
            }
            let reset = drag.clone();
            this.change(false, cx, move |l| match reset {
                Drag::Split(path) => l.set_ratio(&path, 0.5),
                Drag::Sidebar => l.sidebar_width = Layout::default().sidebar_width,
                Drag::Panel => l.panel_width = Layout::default().panel_width,
            });
            this.save();
        }))
}

impl Shell {
    /// OWE-855: 메인 창 제목. 포커스 칸 세션의 워크트리가 있으면 "Krill — <워크트리>".
    fn main_window_title(&self, cx: &App) -> String {
        let tree = self.layout.focused().and_then(|pane| {
            let id = self.layout.session(pane)?;
            let session = self
                .sidebar
                .read(cx)
                .session_on(self.layout.host(pane), id)?;
            let name = session.worktree.as_ref()?.file_name()?.to_string_lossy();
            Some(krill_app::security::sanitize_title(&name))
        });
        match tree {
            Some(name) if !name.is_empty() => format!("{} — {name}", text::APP_TITLE.get()),
            _ => text::APP_TITLE.get().to_owned(),
        }
    }
}

/// 어느 메인 창·플로팅 창의 레이아웃에 같은 호스트 세션이 떠 있으면 true.
fn session_shown(
    host: &krill_app::hosts::HostId,
    session: krill_proto::SessionId,
    cx: &App,
) -> bool {
    cx.windows().into_iter().any(|w| {
        let shell = if let Some(h) = w.downcast::<Shell>() {
            h.entity(cx).ok()
        } else if let Some(h) = w.downcast::<popout::Floating>() {
            h.read(cx).ok().map(|f| f.shell.clone())
        } else {
            None
        };
        shell.is_some_and(|e| e.read(cx).layout.sessions_for_host(host).contains(&session))
    })
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(windows)]
        krill_app::platform::sync_window(
            window,
            cx.theme().mode == gpui_component::ThemeMode::Dark,
        );
        self.window = Some(window.window_handle());
        // OWE-903: 라이브러리에서 들어온 온보딩·짝짓기 모달 요청을 엔티티로 만든다.
        self.open_pending_overlays(window, cx);
        // OWE-855: 인앱 설정(SettingsView::render)이 메인 창 제목을 "설정"으로 덮은 뒤,
        // 복귀·워크트리 전환 시에도 복원되지 않는다. 메인 창은 렌더마다 제목을 고정해
        // macOS 창 목록·Mission Control·접근성 트리를 복원한다.
        // 플로팅 창 제목은 Floating::render가 소유하므로 건드리지 않는다.
        if self.role == WindowRole::Main {
            if self.settings.is_some() {
                window.set_window_title(krill_app::i18n::catalog::settings::TITLE.get());
            } else {
                window.set_window_title(&self.main_window_title(cx));
            }
        }
        if self.language != krill_app::i18n::language() {
            self.language = krill_app::i18n::language();
            if let Some((palette, _)) = &self.jump {
                let (items, worktrees) = {
                    let sb = self.sidebar.read(cx);
                    (sb.jump_items(), sb.jump_worktrees())
                };
                palette.update(cx, |palette, cx| {
                    palette.set_items(items, cx);
                    palette.set_worktrees(worktrees, cx);
                });
            }
        }
        if self.system_appearance.is_none() {
            self.system_appearance = Some(cx.observe_window_appearance(window, |_, window, cx| {
                krill_app::settings::system_appearance_changed(window.appearance(), cx);
            }));
        }
        match std::mem::replace(&mut self.pending_focus, PendingFocus::None) {
            PendingFocus::None => {}
            PendingFocus::Workspace => window.focus(&self.focus, cx),
            PendingFocus::Settings => {
                if let Some(settings) = &self.settings {
                    let focus = settings.read(cx).focus_handle();
                    window.focus(&focus, cx);
                }
            }
        }
        // 첫 렌더도 실제 창의 활성 상태로 커서 타이머를 결정한다.
        self.sync_cursor_blink(window.is_window_active(), cx);
        if self.settings.is_none() {
            self.sync_files(window, cx);
        }
        let cell = cell_size(window, cx);
        let pixel = |p: Pixels| -> u16 {
            // 글꼴 측정값은 유한한 양수이며 u16 범위로 제한한다.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "rounded and clamped pixel size"
            )]
            {
                (f32::from(p) * window.scale_factor())
                    .round()
                    .clamp(1., f32::from(u16::MAX)) as u16
            }
        };
        let cell = Some(CellSize {
            width: pixel(cell.width),
            height: pixel(cell.height),
        });
        if self.cell_pixels != cell {
            self.cell_pixels = cell;
            self.push_appearance(cx);
        }
        if self.activation.is_none() {
            self.activation = Some(cx.observe_window_activation(window, |this, window, cx| {
                let active = window.is_window_active();
                this.on_window_active_changed(active, cx);
                if active {
                    krill_app::onboarding::actions::recheck_needed(cx).detach();
                }
                if !active && let Some(pane) = this.layout.focused() {
                    this.commit_ime(pane, Some(window), cx);
                }
            }));
        }
        if self.bounds_observer.is_none() && self.role == WindowRole::Main {
            self.bounds_observer = Some(cx.observe_window_bounds(window, |this, window, _| {
                if this.role == WindowRole::Main {
                    let saved = saved_window_bounds_from_gpui(window.window_bounds());
                    if this.layout.set_window_bounds(saved) {
                        this.save();
                    }
                }
            }));
        }
        if window.is_window_active() {
            // OWE-662: 창으로 돌아와 보고 있는 세션의 미확인 표시를 끈다.
            self.sync_focused_session(cx);
            krill_app::alert::set_active(true, cx);
        }
        let (focus, me) = (self.focus.clone(), cx.entity());
        let input = canvas(
            |_, _, _| {},
            move |bounds, (), window, cx| {
                window.handle_input(&focus, ElementInputHandler::new(bounds, me.clone()), cx);
            },
        )
        .absolute()
        .size_full();
        let settings_view = self.settings.clone();
        let tree = if settings_view.is_none() {
            let root = if let Some(zoomed) = self
                .zoomed_pane
                .filter(|p| self.layout.visible().contains(p))
            {
                Some(PaneTree::Leaf(zoomed))
            } else {
                self.zoomed_pane = None;
                self.layout
                    .tabs()
                    .get(self.layout.active())
                    .map(|t| t.root.clone())
            };
            Some(match root {
                Some(r) => self.render_tree(&r, &mut Vec::new(), window, cx),
                None => Self::render_empty_workspace(cx),
            })
        } else {
            None
        };
        let center_diff = if settings_view.is_none() {
            self.render_center_diff(window, cx)
        } else {
            None
        };
        let p = krill_app::ui::palette(cx);
        let (sidebar_bg, bg, fg) = (p.sidebar, p.background, p.foreground);
        let (warning, warning_fg) = (p.warning, p.warning_foreground);

        div()
            // OWE-860: 설정 화면에서도 Shell 키 문맥을 유지해 전역 탐색 단축키(⌘J·⌘B·⌘P 등)와
            // 커스텀 ClosePane(⌘W) 바인딩이 동작하게 한다. 터미널 편집 액션은 각 핸들러의 settings 가드가 막는다.
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &krill_app::settings::OpenSettings, window, cx| {
                    // OWE-860: ⌘,는 설정 토글이다. 이미 열려 있으면 닫고(현재 탭 초기화 없음),
                    // 닫혀 있을 때만 일반 탭으로 연다.
                    if this.settings.is_some() {
                        this.close_settings(Some(&mut *window), cx);
                    } else {
                        this.open_settings(
                            krill_app::settings::AdminTab::General,
                            Some(window),
                            cx,
                        );
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &NewTab, _, cx| {
                if this.settings.is_some() {
                    this.close_settings(None, cx);
                }
                this.new_tab_on_launch_host(cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, _, cx| {
                if this.settings.is_some() || this.dashboard.is_some() {
                    return;
                }
                this.request_close_tab(this.layout.active(), cx);
            }))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| {
                if this.settings.is_some() {
                    this.close_settings(None, cx);
                }
                this.cycle_workspace_tab(1, cx);
            }))
            .on_action(cx.listener(|this, _: &PrevTab, _, cx| {
                if this.settings.is_some() {
                    this.close_settings(None, cx);
                }
                this.cycle_workspace_tab(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &NextRecentTab, _, cx| {
                if this.settings.is_some() {
                    this.close_settings(None, cx);
                }
                this.on_recent_tab(1, cx);
            }))
            .on_action(cx.listener(|this, _: &PrevRecentTab, _, cx| {
                if this.settings.is_some() {
                    this.close_settings(None, cx);
                }
                this.on_recent_tab(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitRight, _, cx| {
                this.split_active(Axis::Horizontal, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitDown, _, cx| {
                this.split_active(Axis::Vertical, cx);
            }))
            .on_action(cx.listener(|this, _: &ClosePane, window, cx| {
                if this.dashboard.is_some() {
                    this.close_dashboard(Some(window), cx);
                    return;
                }
                if this.settings.is_some() {
                    this.close_settings(Some(window), cx);
                    return;
                }
                if this.diff.read(cx).has_center_diff() {
                    this.close_center_diff(cx);
                    return;
                }
                if this.layout.visible().len() == 1
                    && this.layout.is_tab_pinned(this.layout.active())
                {
                    this.request_close_tab(this.layout.active(), cx);
                    return;
                }
                // OWE-872: 실행 중이면 확인 대화상자를 먼저 띄운다(줌 해제·기록은 확정 뒤에).
                if let Some(pane) = this.layout.focused() {
                    this.maybe_close_pane(pane, cx);
                } else {
                    // K11: 탭이 없는 빈 작업 공간에서는 macOS처럼 ⌘W가 창을 닫는다.
                    // 파일 탭도 탭이라 여기서는 미저장 파일이 없다.
                    window.remove_window();
                }
            }))
            .on_action(cx.listener(|this, _: &FocusNextPane, _, cx| {
                if this.settings.is_some() || this.dashboard.is_some() {
                    return;
                }
                this.change(true, cx, |l| l.cycle_pane(1));
            }))
            .on_action(cx.listener(|this, _: &FocusPrevPane, _, cx| {
                if this.settings.is_some() || this.dashboard.is_some() {
                    return;
                }
                this.change(true, cx, |l| l.cycle_pane(-1));
            }))
            // OWE-871: 2차원 분할에서 상하좌우 기하 방향으로 포커스를 옮긴다.
            .on_action(cx.listener(|this, _: &FocusPaneLeft, _, cx| {
                if this.settings.is_some() {
                    return;
                }
                this.change(true, cx, |l| {
                    l.focus_direction(krill_app::layout::Direction::Left);
                });
            }))
            .on_action(cx.listener(|this, _: &FocusPaneRight, _, cx| {
                if this.settings.is_some() {
                    return;
                }
                this.change(true, cx, |l| {
                    l.focus_direction(krill_app::layout::Direction::Right);
                });
            }))
            .on_action(cx.listener(|this, _: &FocusPaneUp, _, cx| {
                if this.settings.is_some() {
                    return;
                }
                this.change(true, cx, |l| {
                    l.focus_direction(krill_app::layout::Direction::Up);
                });
            }))
            .on_action(cx.listener(|this, _: &FocusPaneDown, _, cx| {
                if this.settings.is_some() {
                    return;
                }
                this.change(true, cx, |l| {
                    l.focus_direction(krill_app::layout::Direction::Down);
                });
            }))
            .on_action(cx.listener(|this, _: &ToggleZoomPane, _, cx| {
                if this.settings.is_some() || this.dashboard.is_some() {
                    return;
                }
                let Some(focused) = this.layout.focused() else {
                    return;
                };
                if this.zoomed_pane == Some(focused) {
                    this.zoomed_pane = None;
                } else if this.layout.visible().len() > 1 {
                    this.zoomed_pane = Some(focused);
                } else {
                    this.zoomed_pane = None;
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ClearPane, _, cx| {
                if this.settings.is_some() || this.dashboard.is_some() {
                    return;
                }
                let unlinked_session = this.layout.focused().and_then(|pane| {
                    let l = this.live.get(&pane)?;
                    (!l.pane.is_linked())
                        .then(|| this.target_for_pane(pane).zip(l.pane.session()))
                        .flatten()
                });
                if let Some(l) = this.focused_live() {
                    // OWE-873 / OWE-873b: 받아 둔 스크롤백·선택·검색 하이라이트를 먼저 버리고 바닥에 고정하며,
                    // 데몬이 FEAT_CLEAR_SCROLLBACK을 협상했으면 핫·콜드 스크롤백도 비운 뒤 셸에 화면 지우기를 요청한다.
                    l.clear_selection();
                    l.pane.clear_search();
                    l.pane.clear_scrollback();
                    let n = l.pane.input(b"\x0c", Now::get());
                    this.show(n.as_ref(), cx);
                    cx.notify();
                }
                if let Some((target, session)) = unlinked_session {
                    cx.background_executor()
                        .spawn(async move {
                            pane::clear_scrollback(target, session).ok();
                        })
                        .detach();
                }
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                if this.settings.is_some() || this.dashboard.is_some() {
                    return;
                }
                if let Some(l) = this.focused_live() {
                    let shown = l.pane.shown();
                    let rows = u16::try_from(shown.rows.len()).unwrap_or(u16::MAX);
                    let cols = shown.cols;
                    if rows > 0 && cols > 0 {
                        l.selection = Some(Selection {
                            anchor: select::Point { line: 0, column: 0 },
                            head: select::Point {
                                line: rows.saturating_sub(1),
                                column: cols.saturating_sub(1),
                            },
                        });
                        l.snapshot_selection();
                        l.selecting = false;
                        cx.notify();
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &NewAgentTab, _, cx| {
                if this.settings.is_some() {
                    this.close_settings(None, cx);
                }
                if this.link == Link::Up && this.agents.is_some() {
                    this.new_default_agent(cx);
                } else {
                    this.new_tab_on_launch_host(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ReopenClosedTab, _, cx| {
                if this.settings.is_some() {
                    this.close_settings(None, cx);
                }
                if let Some((host, dir)) = this.closed_dirs.pop() {
                    this.new_folder_tab_on(host, &dir, Program::Login, cx);
                } else {
                    this.change(true, cx, Layout::new_tab);
                }
            }))
            .on_action(cx.listener(|this, _: &PrevSession, _, cx| {
                this.cycle_session_focus(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &NextSession, _, cx| {
                this.cycle_session_focus(1, cx);
            }))
            .on_action(cx.listener(|this, _: &NavigateBack, window, cx| {
                this.navigate_back(Some(window), cx);
            }))
            .on_action(cx.listener(|this, _: &NavigateForward, window, cx| {
                this.navigate_forward(Some(window), cx);
            }))
            .on_action(cx.listener(|this, _: &NewWorktree, window, cx| {
                this.open_new_worktree_dialog(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivateTab1, _, cx| {
                if !this.jump_to_palette_row(0, cx) {
                    this.activate_tab_index(0, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ActivateTab2, _, cx| {
                if !this.jump_to_palette_row(1, cx) {
                    this.activate_tab_index(1, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ActivateTab3, _, cx| {
                if !this.jump_to_palette_row(2, cx) {
                    this.activate_tab_index(2, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ActivateTab4, _, cx| {
                if !this.jump_to_palette_row(3, cx) {
                    this.activate_tab_index(3, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ActivateTab5, _, cx| {
                if !this.jump_to_palette_row(4, cx) {
                    this.activate_tab_index(4, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ActivateTab6, _, cx| {
                if !this.jump_to_palette_row(5, cx) {
                    this.activate_tab_index(5, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ActivateTab7, _, cx| {
                if !this.jump_to_palette_row(6, cx) {
                    this.activate_tab_index(6, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ActivateTab8, _, cx| {
                if !this.jump_to_palette_row(7, cx) {
                    this.activate_tab_index(7, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ActivateTab9, _, cx| {
                if !this.jump_to_palette_row(8, cx) {
                    this.activate_tab_index(8, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &FocusSidebar, window, cx| {
                // OWE-860: 설정에서 호출하면 설정을 닫고 사이드바에 포커스를 둔다.
                if this.settings.is_some() {
                    this.close_settings(Some(&mut *window), cx);
                }
                let back = this.focus.clone();
                this.sidebar.update(cx, |s, cx| s.focus(back, window, cx));
            }))
            .map(|d| Self::new_session_actions(d, cx))
            .map(|d| Self::file_actions(d, cx))
            .on_action(cx.listener(|this, _: &TogglePanel, _, cx| this.toggle_panel(cx)))
            .on_action(cx.listener(|this, _: &ToggleChanges, window, cx| {
                this.toggle_changes(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleExplorer, window, cx| {
                this.toggle_explorer(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSearch, window, cx| {
                this.toggle_workspace_search(window, cx);
            }))
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Replace, window, cx| {
                    cx.stop_propagation();
                    this.toggle_workspace_search(window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &Jump, window, cx| this.toggle_jump(window, cx)))
            .on_action(cx.listener(|this, _: &CommandPalette, window, cx| {
                this.toggle_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &Find, window, cx| this.toggle_search(window, cx)))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| {
                this.step_search(krill_proto::search::Direction::Next, cx);
            }))
            .on_action(cx.listener(|this, _: &FindPrev, _, cx| {
                this.step_search(krill_proto::search::Direction::Previous, cx);
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &Paste, window, cx| this.paste(window, cx)))
            .on_action(cx.listener(|this, _: &ForkSession, _, cx| {
                if let Some(pane) = this.layout.focused() {
                    this.fork_pane_session(pane, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ContinueInNewSession, window, cx| {
                if let Some(pane) = this.layout.focused() {
                    this.open_continue_session(pane, window, cx);
                }
            }))
            .on_action(cx.listener(Self::on_toggle_secure_input))
            .on_action(cx.listener(Self::on_float_terminal))
            .on_action(cx.listener(Self::on_dashboard))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_modifiers_changed(cx.listener(Self::on_link_modifiers))
            .when(
                settings_view.is_none() && !self.diff.read(cx).has_center_diff(),
                |d| d.child(input),
            )
            .on_drag_move(
                cx.listener(|this, e: &DragMoveEvent<Drag>, _, cx| this.on_column_drag(e, cx)),
            )
            // 드래그는 놓을 때 한 번 저장한다(창 전체가 드롭 대상).
            .on_drop(cx.listener(|this, _: &Drag, _, _| this.save()))
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .font_family(krill_app::ui::FONT_UI)
            .text_size(krill_app::ui::TEXT_MD)
            .child(if let Some(settings) = settings_view {
                div().flex_1().min_h_0().w_full().child(settings)
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .when(!self.sidebar_collapsed, |d| {
                        d.child(
                            div()
                                .id("sidebar-column")
                                .debug_selector(|| "sidebar-column".into())
                                .flex_none()
                                .w(px(self.layout.sidebar_width))
                                .h_full()
                                .bg(sidebar_bg)
                                .flex()
                                .flex_col()
                                .child(self.render_sidebar_header(window, cx))
                                .child(self.render_agents(cx))
                                .child(div().flex_1().min_h_0().child(self.sidebar.clone()))
                                .child(self.detached.clone())
                                .child(Self::render_sidebar_footer(cx)),
                        )
                        .child(divider(
                            "divider-sidebar".into(),
                            Drag::Sidebar,
                            Axis::Horizontal,
                            cx,
                        ))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_col()
                            .bg(bg)
                            .child(self.render_tab_bar(window, cx))
                            .children(self.link.banner().map(|msg| {
                                let msg = krill_app::i18n::cached_notice(&msg);
                                match self.link.banner_tone() {
                                    BannerTone::Preserve => preserve_banner("link-banner", msg, p),
                                    BannerTone::Warning => div()
                                        .id("link-banner")
                                        .px_3()
                                        .py_1p5()
                                        .bg(warning)
                                        .text_color(warning_fg)
                                        .text_size(krill_app::ui::TEXT_XS)
                                        .child(msg),
                                }
                            }))
                            .children(
                                self.reconnected
                                    .clone()
                                    .map(|msg| preserve_banner("reconnected-banner", msg, p)),
                            )
                            .children(self.away.clone().map(|msg| {
                                preserve_banner("away-summary", msg, p)
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.away = None;
                                        cx.notify();
                                    }))
                            }))
                            .children(self.notice.clone().map(|msg| {
                                div()
                                    .id("notice")
                                    .px_3()
                                    .py_1p5()
                                    .bg(warning)
                                    .text_color(warning_fg)
                                    .text_size(krill_app::ui::TEXT_XS)
                                    .child(krill_app::i18n::cached_notice(&msg))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.notice = None;
                                        cx.notify();
                                    }))
                            }))
                            .children(self.stalled_banner().map(|msg| {
                                div()
                                    .id("stalled")
                                    .px_3()
                                    .py_1p5()
                                    .bg(warning)
                                    .text_color(warning_fg)
                                    .text_size(krill_app::ui::TEXT_XS)
                                    .child(msg)
                            }))
                            .children(self.render_security_banner(cx))
                            .children(krill_app::onboarding::banner::render(cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .relative()
                                    .bg(krill_app::ui::terminal_surface_alpha(
                                        self.terminal_theme,
                                        krill_app::settings::terminal_opacity_pct(cx),
                                    ))
                                    .children(tree)
                                    .children(center_diff)
                                    // ⌘F 검색 바: 터미널 영역 기준 우측 정렬 오버레이(OWE-849).
                                    // 창 루트가 아니라 터미널 컨테이너 자식으로 그려 우측 패널
                                    // 너비·접힘과 무관하게 항상 터미널 우측에 붙는다.
                                    .children(self.search.as_ref().map(|(bar, _)| bar.clone())),
                            ),
                    )
                    .when(!self.panel_collapsed, |d| {
                        d.child(divider(
                            "divider-panel".into(),
                            Drag::Panel,
                            Axis::Horizontal,
                            cx,
                        ))
                        .child(
                            div()
                                .id("panel-column")
                                .debug_selector(|| "panel-column".into())
                                .flex_none()
                                .w(px(self.layout.panel_width))
                                .h_full()
                                .flex()
                                .flex_col()
                                .bg(sidebar_bg)
                                .child(div().flex_1().min_h_0().child(self.diff.clone())),
                        )
                    })
            })
            .child(self.render_status_bar(window, cx))
            .children(self.jump.as_ref().map(|(palette, _)| palette.clone()))
            .children(self.add_project.as_ref().map(|(dialog, _)| dialog.clone()))
            .children(
                self.continue_session
                    .as_ref()
                    .map(|(dialog, _)| dialog.clone()),
            )
            // OWE-903: 온보딩·짝짓기는 메인 창 안 모달로 그린다(별도 OS 창 없음).
            .children(self.onboarding.as_ref().map(|modal| modal.view.clone()))
            .children(self.dashboard.clone())
            .children(self.quick_open_palette())
            .children(self.render_tab_context_menu(window, cx))
            .children(self.render_pinned_close_dialog(window, cx))
            .children(self.render_running_close_dialog(window, cx))
            .children(self.render_tab_rename_dialog(window, cx))
            .children(self.render_recent_tab_switcher(window, cx))
            // 우하단 카드는 하나만: 업데이트 카드가 있으면 별 요청 카드(OWE-905)는 숨긴다.
            .children(
                (self.role == WindowRole::Main)
                    .then(|| {
                        Self::render_update_card(cx)
                            .map(IntoElement::into_any_element)
                            .or_else(|| krill_app::star_nag::card(cx))
                    })
                    .flatten(),
            )
    }
}

impl Shell {
    pub(super) fn split_active(&mut self, axis: Axis, cx: &mut Context<Self>) {
        if self.settings.is_some() || self.dashboard.is_some() {
            return;
        }
        self.zoomed_pane = None;
        if self.layout.tabs().is_empty() {
            self.new_tab_on_launch_host(cx);
        } else {
            self.change(true, cx, |l| l.split(axis));
        }
    }

    fn render_empty_workspace(cx: &mut Context<Self>) -> AnyElement {
        let p = krill_app::ui::palette(cx);
        let action_row = |id: &'static str, label: &'static str, shortcut: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .cursor_pointer()
                .px_3()
                .py_1p5()
                .rounded(krill_app::ui::RADIUS_SM)
                .hover(|s| s.bg(p.accent))
                .flex()
                .items_center()
                .justify_between()
                .gap_6()
                .text_size(krill_app::ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(div().text_color(p.foreground).child(label))
                .child(krill_app::ui::keycap(p, shortcut))
        };
        div()
            .id("empty-workspace")
            .debug_selector(|| "empty-workspace".into())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_6()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(
                        krill_app::ui::icons::brand_mark_tile(48.)
                            .debug_selector(|| "empty-brand-tile".into()),
                    )
                    .child(
                        div()
                            .text_size(krill_app::ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(text::APP_TITLE.get()),
                    )
                    .child(
                        div()
                            .debug_selector(|| "empty-tagline".into())
                            .text_size(krill_app::ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(text::EMPTY_TAGLINE.get()),
                    ),
            )
            .child(
                div()
                    .w(px(240.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        action_row(
                            "empty-new-terminal",
                            text::NEW_TERMINAL.get(),
                            text::SHORTCUT_NEW_TAB.get(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.new_tab_on_launch_host(cx);
                        })),
                    )
                    .child(
                        action_row(
                            "empty-new-agent",
                            text::EMPTY_NEW_AGENT.get(),
                            text::SHORTCUT_NEW_AGENT_TAB.get(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.link == Link::Up && this.agents.is_some() {
                                this.new_default_agent(cx);
                            } else {
                                this.new_tab_on_launch_host(cx);
                            }
                        })),
                    )
                    .child(
                        action_row(
                            "empty-open-file",
                            text::EMPTY_OPEN_FILE.get(),
                            text::SHORTCUT_QUICK_OPEN.get(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_quick_open(window, cx);
                        })),
                    ),
            )
            .into_any_element()
    }

    // Design adapted from Orca (.ref/orca/src/renderer/src/components/UpdateCard.tsx,
    // UpdateDownloadCardContent.tsx), MIT License — Copyright (c) 2026 Orca Contributors.
    fn render_update_card(cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        use gpui_component::button::{Button, ButtonVariants as _};
        use krill_app::i18n::catalog::updater_relaunch as utext;
        use krill_app::updater::UpdateState;

        let state = cx.try_global::<UpdateState>().cloned().unwrap_or_default();
        if state.card_collapsed {
            return None;
        }
        let staged = state.staged?;
        let p = krill_app::ui::palette(cx);
        let title = utext::CARD_READY_TITLE.get();
        let body = utext::CARD_READY_BODY.format(&[("version", &staged.to_version)]);
        let subtitle = utext::CARD_READY_SUBTITLE.get();
        let release_notes_url = staged.release_notes_url();

        Some(
            div()
                .id("update-card")
                .debug_selector(|| "update-card".into())
                .occlude()
                .absolute()
                .bottom(px(34.))
                .right(px(12.))
                .w(px(300.))
                .p_3()
                .rounded(krill_app::ui::RADIUS_MD)
                .border_1()
                .border_color(p.border)
                .bg(p.popover)
                .shadow_lg()
                .flex()
                .flex_col()
                .gap_2p5()
                .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                    if e.keystroke.key == "escape" {
                        let mut u = cx.try_global::<UpdateState>().cloned().unwrap_or_default();
                        u.card_collapsed = true;
                        cx.set_global(u);
                        cx.stop_propagation();
                        cx.refresh_windows();
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_size(krill_app::ui::TEXT_SM)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(p.popover_foreground)
                                .child(title),
                        )
                        .child(krill_app::ui::button_with_tooltip(
                            Button::new("update-card-minimize")
                                .icon(gpui_component::IconName::Minus)
                                .debug_selector(|| "update-card-minimize".into())
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|_, _, _, cx| {
                                    let mut u =
                                        cx.try_global::<UpdateState>().cloned().unwrap_or_default();
                                    u.card_collapsed = true;
                                    cx.set_global(u);
                                    cx.refresh_windows();
                                    cx.notify();
                                })),
                            "update-card-minimize",
                            utext::CARD_MINIMIZE_TOOLTIP.get(),
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(krill_app::ui::TEXT_XS)
                                .text_color(p.popover_foreground)
                                .child(body),
                        )
                        .child(
                            div()
                                .text_size(krill_app::ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(subtitle),
                        )
                        .children(release_notes_url.map(|url| {
                            div().flex().items_center().child(
                                Button::new("update-card-release-notes")
                                    .label(utext::CARD_RELEASE_NOTES.get())
                                    .debug_selector(|| "update-card-release-notes".into())
                                    .ghost()
                                    .xsmall()
                                    .on_click(move |_, _, cx| {
                                        cx.open_url(&url);
                                    }),
                            )
                        })),
                )
                .child(
                    div().w_full().child(
                        Button::new("update-card-restart")
                            .label(utext::CARD_RESTART_BTN.get())
                            .debug_selector(|| "update-card-restart".into())
                            .primary()
                            .small()
                            .w_full()
                            .on_click(move |_, _, cx| {
                                let staged = staged.clone();
                                cx.spawn(async move |cx| {
                                    let launched = cx
                                        .background_executor()
                                        .spawn(async move { staged.spawn_relauncher().is_ok() })
                                        .await;
                                    if launched {
                                        cx.update(files::quit_app);
                                    }
                                })
                                .detach();
                            }),
                    ),
                ),
        )
    }
}

#[cfg(test)]
impl Shell {
    /// 디바운스 저장 작업자의 대기분을 디스크에 반영한다(레이아웃 테스트용).
    pub(crate) fn flush_layout(&self) {
        self.saver.flush();
    }

    /// IO 태스크 없이 칸을 `sock`(krilld 쪽 대신)에 붙인다.
    #[cfg(unix)]
    fn attach_test(
        &mut self,
        pane: PaneId,
        session: krill_proto::SessionId,
        sock: UnixStream,
        cx: &mut Context<Self>,
    ) {
        self.start_pane_liveness(cx);
        let mut l = Live::new(Task::ready(()));
        l.pane.link(
            session,
            1,
            pane::Writer::spawn(sock).unwrap(),
            krill_proto::input::FEAT_INPUT,
            Now::get(),
        );
        self.live.insert(pane, l);
        cx.notify();
    }
}

/// IME(`NSTextInputClient`) 메시지를 포커스 칸의 IME 상태기계로 옮긴다(MK-028, `krill_term::ime`).
impl EntityInputHandler for Shell {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        // 문서 = preedit. `markedRange`가 가리키는 글자를 돌려줘야 IME가 조합을 이어 간다(W1).
        let got = self.focused_live()?.ime.text_for_range_utf16(range.clone());
        ime_trace(format_args!("textForRange {range:?} -> {got:?}"));
        let (text, actual) = got?;
        if actual != range {
            *adjusted = Some(actual);
        }
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let range = self.focused_live()?.ime.selected_range_utf16();
        ime_trace(format_args!("selectedRange -> {range:?}"));
        Some(UTF16Selection {
            range,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let pane = self.layout.focused()?;
        let r = self.live.get(&pane)?.ime.marked_range_utf16();
        ime_trace(format_args!("markedRange -> {r:?}"));
        r
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        ime_trace(format_args!("unmarkText"));
        let Some(l) = self.focused_live() else { return };
        let mut out = l.ime.unmark();
        out.pty = keys::tui::text(&out.pty, l.pane.modes());
        self.send(&out, Some(window), cx);
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        ime_trace(format_args!("insertText {text:?} replace={range:?}"));
        let Some(l) = self.focused_live() else { return };
        let mut out = l.ime.insert(text);
        out.pty = keys::tui::text(&out.pty, l.pane.modes());
        self.send(&out, Some(window), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        ime_trace(format_args!(
            "setMarkedText {new_text:?} sel={new_selected_range:?} replace={range:?}"
        ));
        let Some(l) = self.focused_live() else { return };
        l.ime.set_marked(new_text, new_selected_range);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let l = self.focused_live()?;
        let (bounds, cell) = l.painted.get();
        let at = l.ime.candidate_anchor(range_utf16.start);
        let n = |v: usize| f32::from(u16::try_from(v).unwrap_or(u16::MAX));
        Some(Bounds::new(
            point(
                bounds.origin.x + cell.width * n(at.column),
                bounds.origin.y + cell.height * n(at.line),
            ),
            cell,
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

/// `KRILL_IME_TRACE=<파일>`이면 IME 메시지와 키 이벤트를 한 줄씩 덧붙인다(실기 한글 IME 진단용, 기본 꺼짐).
fn ime_trace(line: std::fmt::Arguments<'_>) {
    use std::io::Write as _;
    static SEND: std::sync::OnceLock<Option<std::sync::mpsc::SyncSender<String>>> =
        std::sync::OnceLock::new();
    let Some(send) = SEND.get_or_init(|| {
        let path = std::env::var_os("KRILL_IME_TRACE").map(PathBuf::from)?;
        let (send, recv) = std::sync::mpsc::sync_channel::<String>(1024);
        std::thread::Builder::new()
            .name("ime-trace".into())
            .spawn(move || {
                let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                else {
                    return;
                };
                for line in recv {
                    writeln!(file, "{line}").ok();
                }
            })
            .ok()?;
        Some(send)
    }) else {
        return;
    };
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    // Diagnostics must not apply backpressure to IME/key events.
    send.try_send(format!("{ms} {line}")).ok();
}

/// 일반 문자 키인가(Ctrl·Alt·Cmd·Fn 없음, 한 글자·스페이스). 조합 중 `KeyUp` 차단(OWE-615 원인 4)
/// 대상. `key`는 물리 키 이름이라 한글 자모 키도 한 글자로 온다(`key_char`는 테스트 파싱에 없어 보지 않는다).
fn is_plain_char_key(ks: &gpui::Keystroke) -> bool {
    if ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform || ks.modifiers.function {
        return false;
    }
    if ks.key == "space" {
        return true;
    }
    let mut chars = ks.key.chars();
    matches!(chars.next(), Some(ch) if !ch.is_control()) && chars.next().is_none()
}

fn is_permanent_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::InvalidInput
            | std::io::ErrorKind::InvalidData
            | std::io::ErrorKind::Unsupported
    )
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
