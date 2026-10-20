//! 앱 셸 레이아웃 모델(MK-030): 탭·분할 트리·열 너비·칸마다 붙은 세션(OWE-490)과 디스크 저장.
//!
//! gpui에 의존하지 않는다. UI 프레임워크가 바뀌어도(MK-002 잠정 결정) 이 모듈과 저장 파일은 그대로 쓴다.

use krill_proto::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use krill_proto::SessionId;
use krill_proto::control::{Create, Program};
use serde::{Deserialize, Serialize};

use crate::hosts::HostId;

/// 분할 칸·옆 열의 최소 크기(pt). T3 #11 기준.
pub const MIN_PANE_PX: f32 = 50.0;

const FILE_NAME: &str = "layout.bin";
/// 파일 첫 바이트. 형식을 바꾸면 올리고, 모르는 버전은 기본 레이아웃으로 시작한다.
/// 2: 칸 ↔ 세션(OWE-490). 3: 칸별 폴더·실행 프로그램(MK-055), v2는 세션을 보존해 읽는다.
/// 4: 파일 칸(MK-119 에디터 탭), v2·v3은 파일 칸 없이 읽는다.
/// 5: 실행 정보의 계정(OWE-712 `Create.account`), v2~v4는 [`LegacyLayout`]으로 읽는다.
/// 6: 탭 핀 고정·9색 태그(OWE-734 `Tab.pinned`, `Tab.color`), v2~v5는 [`LegacyLayout`]으로 읽는다.
/// 7: 탭 사용자 지정 제목(OWE-870 `Tab.custom_title`), v6은 [`LegacyLayoutV6`]으로 읽는다.
const FORMAT_VERSION: u8 = 7;
/// 손상·악성 파일의 깊은 중첩이 역직렬화 재귀로 스택을 넘지 않게 하는 상한.
/// 탭 10개 × 분할 4개는 수백 바이트다.
const MAX_FILE_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PaneId(u32);

impl PaneId {
    pub fn get(self) -> u32 {
        self.0
    }
}

/// 분할 방향. `Horizontal`은 좌우로 나란히, `Vertical`은 위아래.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// 루트에서 분할 노드까지의 경로 한 단계.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    First,
    Second,
}

/// OWE-871: 기하 방향 포커스 이동(⌘⌥⇧+화살표)에 쓰는 4방향.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// `[x, y, w, h]` 사각형의 중심점.
fn center(rect: [f32; 4]) -> (f32, f32) {
    (rect[0] + rect[2] / 2., rect[1] + rect[3] / 2.)
}

/// 탭 색상 태그(MK-180, Orca 9색 패리티).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TabColor {
    Blue,
    Purple,
    Pink,
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Gray,
}

impl TabColor {
    pub const ALL: [Self; 9] = [
        Self::Blue,
        Self::Purple,
        Self::Pink,
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Teal,
        Self::Gray,
    ];

    pub const fn slug(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Pink => "pink",
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Teal => "teal",
            Self::Gray => "gray",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PaneTree {
    Leaf(PaneId),
    Split {
        axis: Axis,
        /// 첫 칸이 차지하는 비율(0~1 사이, 양 끝 제외).
        ratio: f32,
        first: Box<PaneTree>,
        second: Box<PaneTree>,
    },
}

impl PaneTree {
    fn first_leaf(&self) -> PaneId {
        match self {
            Self::Leaf(id) => *id,
            Self::Split { first, .. } => first.first_leaf(),
        }
    }

    pub fn contains(&self, target: PaneId) -> bool {
        match self {
            Self::Leaf(id) => *id == target,
            Self::Split { first, second, .. } => first.contains(target) || second.contains(target),
        }
    }

    fn leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            Self::Leaf(id) => out.push(*id),
            Self::Split { first, second, .. } => {
                first.leaves(out);
                second.leaves(out);
            }
        }
    }

    /// `target` 잎을 `f`의 결과로 바꾼다. 찾았으면 true.
    fn replace_leaf(&mut self, target: PaneId, f: &mut dyn FnMut(PaneId) -> PaneTree) -> bool {
        match self {
            Self::Leaf(id) if *id == target => {
                *self = f(*id);
                true
            }
            Self::Leaf(_) => false,
            Self::Split { first, second, .. } => {
                first.replace_leaf(target, f) || second.replace_leaf(target, f)
            }
        }
    }

    fn last_leaf(&self) -> PaneId {
        match self {
            Self::Leaf(id) => *id,
            Self::Split { second, .. } => second.last_leaf(),
        }
    }

    /// `target` 잎을 지우고 그 부모 분할을 형제로 바꾼다. 닫힌 잎과 맞닿은 형제 쪽 이웃 잎을 돌려준다(#30326).
    fn remove_leaf(&mut self, target: PaneId) -> Option<PaneId> {
        let Self::Split { first, second, .. } = self else {
            return None;
        };
        let (sibling, choose_last) = if **first == Self::Leaf(target) {
            (second, false)
        } else if **second == Self::Leaf(target) {
            (first, true)
        } else {
            return first
                .remove_leaf(target)
                .or_else(|| second.remove_leaf(target));
        };
        let sibling = std::mem::replace(&mut **sibling, Self::Leaf(target));
        *self = sibling;
        Some(if choose_last {
            self.last_leaf()
        } else {
            self.first_leaf()
        })
    }

    fn node_mut(&mut self, path: &[Branch]) -> Option<&mut Self> {
        let Some((step, rest)) = path.split_first() else {
            return Some(self);
        };
        match self {
            Self::Leaf(_) => None,
            Self::Split { first, second, .. } => match step {
                Branch::First => first.node_mut(rest),
                Branch::Second => second.node_mut(rest),
            },
        }
    }

    fn ratios_valid(&self) -> bool {
        match self {
            Self::Leaf(_) => true,
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => *ratio > 0.0 && *ratio < 1.0 && first.ratios_valid() && second.ratios_valid(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub root: PaneTree,
    pub focused: PaneId,
    pub pinned: bool,
    pub color: Option<TabColor>,
    /// 탭 더블클릭·우클릭 메뉴로 정한 사용자 지정 제목(OWE-870). `None`이면 자동 제목을 쓴다.
    #[serde(default)]
    pub custom_title: Option<String>,
}

impl Tab {
    fn single(pane: PaneId) -> Self {
        Self {
            root: PaneTree::Leaf(pane),
            focused: pane,
            pinned: false,
            color: None,
            custom_title: None,
        }
    }
}

/// 최근 포커스 탭 순서(MRU, MK-180). 런타임 전용이라 디스크 저장·`PartialEq` 비교에는 포함하지 않는다.
#[derive(Clone, Debug, Default)]
struct MruState {
    order: Vec<usize>,
    cursor: Option<usize>,
}

impl PartialEq for MruState {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

/// 창 위치·크기·전체화면 상태(OWE-842).
pub const MIN_WINDOW_WIDTH: f32 = 360.0;
pub const MIN_WINDOW_HEIGHT: f32 = 240.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SavedWindowMode {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWindowBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub mode: SavedWindowMode,
}

impl SavedWindowBounds {
    #[must_use]
    pub fn is_valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width >= MIN_WINDOW_WIDTH
            && self.height >= MIN_WINDOW_HEIGHT
    }
}

/// 파일(에디터) 칸의 커서 위치와 미리보기/편집 모드 상태(OWE-842).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedFileState {
    pub line: u32,
    pub column: u32,
    pub preview: bool,
}

/// OWE-842: v6 본문(`hosts` 뒤)에 붙는 확장 상태(창 크기·위치·전체화면, 좌·우 사이드바 열림/탭,
/// 파일 커서·미리보기, 시나리오 B 재스폰용 최신 cwd).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct LayoutTail {
    window_bounds: Option<SavedWindowBounds>,
    sidebar_collapsed: bool,
    panel_collapsed: bool,
    panel_tab: crate::diff::PanelTab,
    file_states: BTreeMap<PaneId, SavedFileState>,
    /// 셸 내부 `cd` 포함 최신 라이브 cwd(칸별). `launches`의 폴더(워크트리 루트·Open in 기준)와 분리 보관한다.
    respawn_cwds: BTreeMap<PaneId, Vec<u8>>,
    /// 앱이 마지막으로 살아 있던 시각(UNIX ns, 0 = 모름). 정상 종료 때와 주기적으로 갱신한다.
    /// 끝에 붙인 필드라 구버전 앱은 이 꼬리를 읽고 남는 바이트를 무시한다.
    last_seen_unix_ns: u64,
}

/// `last_seen_unix_ns`가 붙기 전 6-필드 꼬리.
#[derive(Deserialize)]
struct TailV6 {
    window_bounds: Option<SavedWindowBounds>,
    sidebar_collapsed: bool,
    panel_collapsed: bool,
    panel_tab: crate::diff::PanelTab,
    file_states: BTreeMap<PaneId, SavedFileState>,
    respawn_cwds: BTreeMap<PaneId, Vec<u8>>,
}

impl From<TailV6> for LayoutTail {
    fn from(old: TailV6) -> Self {
        Self {
            window_bounds: old.window_bounds,
            sidebar_collapsed: old.sidebar_collapsed,
            panel_collapsed: old.panel_collapsed,
            panel_tab: old.panel_tab,
            file_states: old.file_states,
            respawn_cwds: old.respawn_cwds,
            last_seen_unix_ns: 0,
        }
    }
}

/// `respawn_cwds`가 붙기 전 OWE-842 중간 형식의 5-필드 꼬리. 새 꼬리 디코딩에 실패할 때만 구 꼬리로 읽는다.
#[derive(Deserialize)]
struct LegacyTail {
    window_bounds: Option<SavedWindowBounds>,
    sidebar_collapsed: bool,
    panel_collapsed: bool,
    panel_tab: crate::diff::PanelTab,
    file_states: BTreeMap<PaneId, SavedFileState>,
}

impl From<LegacyTail> for LayoutTail {
    fn from(old: LegacyTail) -> Self {
        Self {
            window_bounds: old.window_bounds,
            sidebar_collapsed: old.sidebar_collapsed,
            panel_collapsed: old.panel_collapsed,
            panel_tab: old.panel_tab,
            file_states: old.file_states,
            respawn_cwds: BTreeMap::new(),
            last_seen_unix_ns: 0,
        }
    }
}

/// 창 하나의 레이아웃. 불변식: 탭이 있으면 `active < tabs.len()`(없으면 `active == 0`), 각 탭의 `focused`는 그 탭의 잎.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    tabs: Vec<Tab>,
    active: usize,
    pub sidebar_width: f32,
    pub panel_width: f32,
    next_pane: u32,
    /// 칸에 붙은 krilld 세션. 앱을 다시 띄우면 같은 세션에 붙는다. 닫힌 칸의 항목은 지운다(세션 자체는 닫지 않는다).
    sessions: BTreeMap<PaneId, SessionId>,
    launches: BTreeMap<PaneId, Create>,
    /// 칸이 속한 호스트(`HostId::Remote`만 저장하며 기본값은 `HostId::Local`).
    #[serde(skip)]
    hosts: BTreeMap<PaneId, HostId>,
    /// 파일 칸(MK-119): 세션 대신 이 파일을 에디터로 보인다. 세션을 붙이지 않는다.
    files: BTreeMap<PaneId, PathBuf>,
    /// 단일 클릭 미리보기 탭 슬롯(OWE-849): 다음 단일 클릭이 같은 칸을 재사용한다.
    /// 고정 열기·칸 닫기로 무효화된다. 런타임 전용이라 저장하지 않는다.
    #[serde(skip)]
    preview: Option<(PaneId, PathBuf)>,
    #[serde(skip)]
    window_bounds: Option<SavedWindowBounds>,
    #[serde(skip)]
    pub sidebar_collapsed: bool,
    #[serde(skip)]
    pub panel_collapsed: bool,
    #[serde(skip)]
    pub panel_tab: crate::diff::PanelTab,
    #[serde(skip)]
    file_states: BTreeMap<PaneId, SavedFileState>,
    /// 시나리오 B 재스폰용 최신 라이브 cwd(칸별, 디스크에는 꼬리로 영속). `launches`와 분리.
    #[serde(skip)]
    respawn_cwds: BTreeMap<PaneId, Vec<u8>>,
    /// [`LayoutTail::last_seen_unix_ns`]. 디스크에는 꼬리로만 영속한다.
    #[serde(skip)]
    last_seen_unix_ns: u64,
    #[serde(skip)]
    mru: MruState,
}

/// v5까지의 디스크 `Tab`(OWE-734 `pinned`·`color` 이전).
#[derive(Deserialize)]
struct LegacyTab {
    root: PaneTree,
    focused: PaneId,
}

/// v6 디스크 `Tab`(`pinned`·`color`는 있고 OWE-870 `custom_title`은 없음).
#[derive(Deserialize)]
struct LegacyTabV6 {
    root: PaneTree,
    focused: PaneId,
    pinned: bool,
    color: Option<TabColor>,
}

/// v4까지의 디스크 `Create`(OWE-712 `account` 이전). postcard는 `#[serde(default)]`를 모르므로 따로 읽는다.
#[derive(Deserialize)]
struct LegacyCreate {
    program: Program,
    cwd: Vec<u8>,
    cols: u16,
    rows: u16,
}

impl From<LegacyCreate> for Create {
    fn from(c: LegacyCreate) -> Self {
        Self {
            program: c.program,
            cwd: c.cwd,
            cols: c.cols,
            rows: c.rows,
            account: None,
        }
    }
}

/// v2~v5 디스크 레이아웃. 필드 순서는 [`Layout`]과 같고 `tabs`는 [`LegacyTab`], `launches`는 버전별 실행 정보다.
#[derive(Deserialize)]
struct LegacyLayout<C> {
    tabs: Vec<LegacyTab>,
    active: usize,
    sidebar_width: f32,
    panel_width: f32,
    next_pane: u32,
    sessions: BTreeMap<PaneId, SessionId>,
    launches: BTreeMap<PaneId, C>,
    files: BTreeMap<PaneId, PathBuf>,
}

/// v6 디스크 레이아웃. `tabs`만[`LegacyTabV6`], 실행 정보는 v5와 같은 `Create`다.
#[derive(Deserialize)]
struct LegacyLayoutV6 {
    tabs: Vec<LegacyTabV6>,
    active: usize,
    sidebar_width: f32,
    panel_width: f32,
    next_pane: u32,
    sessions: BTreeMap<PaneId, SessionId>,
    launches: BTreeMap<PaneId, Create>,
    files: BTreeMap<PaneId, PathBuf>,
}

impl From<LegacyLayoutV6> for Layout {
    fn from(old: LegacyLayoutV6) -> Self {
        let tabs = old
            .tabs
            .into_iter()
            .map(|t| Tab {
                root: t.root,
                focused: t.focused,
                pinned: t.pinned,
                color: t.color,
                custom_title: None,
            })
            .collect();
        let mut layout = Self {
            tabs,
            active: old.active,
            sidebar_width: old.sidebar_width,
            panel_width: old.panel_width,
            next_pane: old.next_pane,
            sessions: old.sessions,
            launches: old.launches,
            hosts: BTreeMap::new(),
            files: old.files,
            preview: None,
            window_bounds: None,
            sidebar_collapsed: false,
            panel_collapsed: false,
            panel_tab: crate::diff::PanelTab::default(),
            file_states: BTreeMap::new(),
            respawn_cwds: BTreeMap::new(),
            last_seen_unix_ns: 0,
            mru: MruState::default(),
        };
        layout.record_mru(layout.active);
        layout
    }
}

impl<C: Into<Create>> From<LegacyLayout<C>> for Layout {
    fn from(old: LegacyLayout<C>) -> Self {
        let tabs = old
            .tabs
            .into_iter()
            .map(|t| Tab {
                root: t.root,
                focused: t.focused,
                pinned: false,
                color: None,
                custom_title: None,
            })
            .collect();
        let launches = old
            .launches
            .into_iter()
            .map(|(pane, c)| (pane, c.into()))
            .collect();
        let mut layout = Self {
            tabs,
            active: old.active,
            sidebar_width: old.sidebar_width,
            panel_width: old.panel_width,
            next_pane: old.next_pane,
            sessions: old.sessions,
            launches,
            hosts: BTreeMap::new(),
            files: old.files,
            preview: None,
            window_bounds: None,
            sidebar_collapsed: false,
            panel_collapsed: false,
            panel_tab: crate::diff::PanelTab::default(),
            file_states: BTreeMap::new(),
            respawn_cwds: BTreeMap::new(),
            last_seen_unix_ns: 0,
            mru: MruState::default(),
        };
        layout.record_mru(layout.active);
        layout
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            tabs: vec![Tab::single(PaneId(0))],
            active: 0,
            sidebar_width: 240.0,
            panel_width: 280.0,
            next_pane: 1,
            sessions: BTreeMap::new(),
            launches: BTreeMap::new(),
            hosts: BTreeMap::new(),
            files: BTreeMap::new(),
            preview: None,
            window_bounds: None,
            sidebar_collapsed: false,
            panel_collapsed: false,
            panel_tab: crate::diff::PanelTab::default(),
            file_states: BTreeMap::new(),
            respawn_cwds: BTreeMap::new(),
            last_seen_unix_ns: 0,
            mru: MruState {
                order: vec![0],
                cursor: None,
            },
        }
    }
}

impl Layout {
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active(&self) -> usize {
        self.active
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// 활성 탭의 포커스 칸. 모든 탭이 닫혔으면 `None`.
    pub fn focused(&self) -> Option<PaneId> {
        self.tabs.get(self.active).map(|t| t.focused)
    }

    /// 보이는 칸(활성 탭의 잎).
    pub fn visible(&self) -> Vec<PaneId> {
        self.tab_panes(self.active)
    }

    /// 지정한 탭의 모든 잎 칸을 순서대로 돌려준다.
    pub fn tab_panes(&self, index: usize) -> Vec<PaneId> {
        let mut out = Vec::new();
        if let Some(tab) = self.tabs.get(index) {
            tab.root.leaves(&mut out);
        }
        out
    }

    /// 지정한 칸(`pane`)이 현재 열린 탭 중 하나의 분할 트리에 살아 있는지 돌려준다.
    #[must_use]
    pub fn pane_exists(&self, pane: PaneId) -> bool {
        self.tabs.iter().any(|t| t.root.contains(pane))
    }

    pub fn is_tab_pinned(&self, index: usize) -> bool {
        self.tabs.get(index).is_some_and(|t| t.pinned)
    }

    pub fn toggle_pin_tab(&mut self, index: usize) -> bool {
        let Some(tab) = self.tabs.get_mut(index) else {
            return false;
        };
        tab.pinned = !tab.pinned;
        tab.pinned
    }

    pub fn tab_color(&self, index: usize) -> Option<TabColor> {
        self.tabs.get(index).and_then(|t| t.color)
    }

    pub fn set_tab_color(&mut self, index: usize, color: Option<TabColor>) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.color = color;
        }
    }

    /// OWE-870: 탭의 사용자 지정 제목. 없거나 비어 있으면 `None`(자동 제목을 쓴다).
    pub fn tab_title(&self, index: usize) -> Option<&str> {
        self.tabs
            .get(index)
            .and_then(|t| t.custom_title.as_deref())
            .map(str::trim)
            .filter(|t| !t.is_empty())
    }

    /// OWE-870: 탭의 사용자 지정 제목을 정한다. 비어 있으면 지운다(자동 제목으로 복귀).
    /// 제어 문자는 빼고 128자로 자른다(사이드바 이름 변경과 같은 규칙).
    pub fn set_tab_title(&mut self, index: usize, title: Option<&str>) {
        if let Some(tab) = self.tabs.get_mut(index) {
            let cleaned: String = title
                .unwrap_or_default()
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(128)
                .collect();
            tab.custom_title = (!cleaned.is_empty()).then_some(cleaned);
        }
    }

    /// 지정한 탭의 모든 잎이 에디터(파일) 칸이면 `true`.
    pub fn is_editor_tab(&self, index: usize) -> bool {
        let leaves = self.tab_panes(index);
        !leaves.is_empty() && leaves.iter().all(|p| self.files.contains_key(p))
    }

    pub fn session(&self, pane: PaneId) -> Option<SessionId> {
        self.sessions.get(&pane).copied()
    }

    /// 칸들이 가진 로컬 호스트 세션(OWE-492: 이 밖의 로컬 krilld 세션은 분리된 세션).
    pub fn sessions(&self) -> Vec<SessionId> {
        self.sessions
            .iter()
            .filter(|(p, _)| self.host(**p).is_local())
            .map(|(_, s)| *s)
            .collect()
    }

    /// 지정한 호스트의 칸들이 가진 세션 목록.
    pub fn sessions_for_host(&self, host: &HostId) -> Vec<SessionId> {
        self.sessions
            .iter()
            .filter(|(p, _)| self.host(**p) == host)
            .map(|(_, s)| *s)
            .collect()
    }

    /// 칸이 속한 호스트(지정되지 않았으면 `HostId::Local`).
    pub fn host(&self, pane: PaneId) -> &HostId {
        self.hosts.get(&pane).unwrap_or(&HostId::Local)
    }

    /// 칸의 호스트를 지정한다. 닫힌 칸이면 무시한다.
    pub fn set_host(&mut self, pane: PaneId, host: HostId) {
        if self.panes().contains(&pane) {
            if host.is_local() {
                self.hosts.remove(&pane);
            } else {
                self.hosts.insert(pane, host);
            }
        }
    }

    /// 칸에 세션을 붙인다. 닫힌 칸·파일 칸이면 무시한다.
    pub fn set_session(&mut self, pane: PaneId, session: SessionId) {
        if self.panes().contains(&pane) && !self.files.contains_key(&pane) {
            self.sessions.insert(pane, session);
        }
    }

    /// Rebind every pane on this host, including panes in background tabs.
    pub fn rebind_session(&mut self, host: &HostId, from: SessionId, to: SessionId) -> Vec<PaneId> {
        let panes: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|(&pane, &session)| {
                (session == from && self.host(pane) == host).then_some(pane)
            })
            .collect();
        for &pane in &panes {
            self.sessions.insert(pane, to);
            if let Some(launch) = self.launches.get_mut(&pane) {
                launch.account = None;
            }
        }
        panes
    }

    /// 폴더·실행 프로그램. 기존 세션에 재접속할 때도 Open in의 폴더를 유지한다.
    pub fn launch(&self, pane: PaneId) -> Option<&Create> {
        self.launches.get(&pane)
    }

    pub fn set_launch(&mut self, pane: PaneId, launch: Create) {
        if self.panes().contains(&pane) {
            self.launches.insert(pane, launch);
        }
    }

    /// 파일 칸이면 그 파일.
    pub fn file(&self, pane: PaneId) -> Option<&Path> {
        self.files.get(&pane).map(PathBuf::as_path)
    }

    /// 앱이 마지막으로 살아 있던 시각(UNIX ns). 저장된 적 없으면(구버전 파일 포함) `None`.
    #[must_use]
    pub const fn last_seen_unix_ns(&self) -> Option<u64> {
        if self.last_seen_unix_ns == 0 {
            None
        } else {
            Some(self.last_seen_unix_ns)
        }
    }

    pub const fn set_last_seen_unix_ns(&mut self, ns: u64) {
        self.last_seen_unix_ns = ns;
    }

    /// 시나리오 B 재스폰용 최신 라이브 cwd(셸 내부 `cd` 반영). 없으면 `launches`의 폴더를 쓴다.
    /// `launches`의 cwd(워크트리 루트 유지)와 분리 보관하므로 `folder()`·Open in에 영향을 주지 않는다.
    #[must_use]
    pub fn respawn_cwd(&self, pane: PaneId) -> Option<&[u8]> {
        self.respawn_cwds.get(&pane).map(Vec::as_slice)
    }

    /// 재스폰용 최신 cwd를 갱신한다. 값이 바뀌었을 때만 `true`를 돌려준다.
    pub fn set_respawn_cwd(&mut self, pane: PaneId, cwd: Vec<u8>) -> bool {
        if cwd.is_empty() || !self.panes().contains(&pane) {
            return false;
        }
        if self.respawn_cwds.get(&pane) == Some(&cwd) {
            return false;
        }
        self.respawn_cwds.insert(pane, cwd);
        true
    }

    /// 파일 칸들(모든 탭).
    pub fn files(&self) -> impl Iterator<Item = (PaneId, &Path)> {
        self.files.iter().map(|(p, f)| (*p, f.as_path()))
    }

    /// 파일 칸의 에디터 상태(커서 위치·미리보기 모드, OWE-842).
    #[must_use]
    pub fn file_state(&self, pane: PaneId) -> Option<SavedFileState> {
        self.file_states.get(&pane).copied()
    }

    /// 파일 칸의 에디터 상태를 갱신한다. 값이 바뀌었을 때만 `true`를 돌려준다.
    pub fn set_file_state(&mut self, pane: PaneId, state: SavedFileState) -> bool {
        if !self.files.contains_key(&pane) {
            return false;
        }
        if state == SavedFileState::default() {
            return self.file_states.remove(&pane).is_some();
        }
        if self.file_states.get(&pane) == Some(&state) {
            return false;
        }
        self.file_states.insert(pane, state);
        true
    }

    /// 마지막으로 저장된 창 위치·크기·전체화면 상태(OWE-842).
    #[must_use]
    pub fn window_bounds(&self) -> Option<SavedWindowBounds> {
        self.window_bounds
    }

    /// 창 위치·크기·전체화면 상태를 갱신한다. 유효하고 값이 바뀌었을 때만 `true`.
    pub fn set_window_bounds(&mut self, bounds: SavedWindowBounds) -> bool {
        if !bounds.is_valid() || self.window_bounds == Some(bounds) {
            return false;
        }
        self.window_bounds = Some(bounds);
        true
    }

    #[must_use]
    pub fn panel_tab(&self) -> crate::diff::PanelTab {
        self.panel_tab
    }

    /// 우측 패널의 활성 탭을 갱신한다. 값이 바뀌었을 때만 `true`.
    pub fn set_panel_tab(&mut self, tab: crate::diff::PanelTab) -> bool {
        if self.panel_tab == tab {
            return false;
        }
        self.panel_tab = tab;
        true
    }

    /// 파일을 연다: 이미 어느 탭의 칸에 열려 있으면 그 탭·칸으로, 아니면 새 탭(폴더는 지금 칸을 물려받는다).
    pub fn open_file(&mut self, path: PathBuf) -> PaneId {
        if let Some((tab, pane)) = self.find_file_pane(&path) {
            self.active = tab;
            self.tabs[tab].focused = pane;
            self.record_mru(tab);
            // 미리보기로 열려 있던 파일을 고정으로 여는 것이므로 슬롯을 비운다.
            if self.preview == Some((pane, path)) {
                self.preview = None;
            }
            return pane;
        }
        self.clear_stale_preview();
        self.new_tab();
        let pane = self.active_tab().focused;
        self.files.insert(pane, path);
        pane
    }

    /// 파일을 미리보기로 연다(OWE-849): 이미 열려 있으면 그 탭·칸으로 포커스만 옮기고,
    /// 살아 있는 미리보기 칸이 있으면 그 칸을 재사용하며, 아니면 새 탭을 열어 슬롯에 기록한다.
    pub fn open_file_preview(&mut self, path: PathBuf) -> PaneId {
        let tabs: Vec<_> = (0..self.tabs.len()).collect();
        self.open_file_preview_in(path, &tabs)
    }

    /// 다른 워크스페이스의 미리보기 칸은 교체하지 않는다.
    pub fn open_file_preview_in(&mut self, path: PathBuf, tabs: &[usize]) -> PaneId {
        if let Some((tab, pane)) = self.find_file_pane(&path) {
            self.active = tab;
            self.tabs[tab].focused = pane;
            self.record_mru(tab);
            return pane;
        }
        if let Some((pane, _)) = self.preview.clone()
            && self.preview_is_live()
            && let Some(tab) = self.tab_with(pane)
            && tabs.contains(&tab)
        {
            self.files.insert(pane, path.clone());
            self.preview = Some((pane, path));
            self.active = tab;
            self.tabs[tab].focused = pane;
            self.record_mru(tab);
            return pane;
        }
        let pane = self.open_file(path.clone());
        self.preview = Some((pane, path));
        pane
    }

    /// 미리보기 슬롯이 가리키는 칸이 아직 같은 파일을 보이고 있으면 `true`를 반환한다.
    fn preview_is_live(&self) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|(pane, old)| self.files.get(pane) == Some(old))
    }

    /// 기록과 달라진 미리보기 슬롯을 비운다. `prune`에서도 호출된다.
    fn clear_stale_preview(&mut self) {
        if !self.preview_is_live() {
            self.preview = None;
        }
    }

    /// `path`가 열린 (탭 인덱스, 칸)을 찾는다.
    fn find_file_pane(&self, path: &Path) -> Option<(usize, PaneId)> {
        self.tabs.iter().enumerate().find_map(|(i, t)| {
            let mut leaves = Vec::new();
            t.root.leaves(&mut leaves);
            leaves
                .into_iter()
                .find(|p| self.files.get(p).is_some_and(|f| f == path))
                .map(|p| (i, p))
        })
    }

    /// `pane`을 포함한 탭 인덱스를 찾는다.
    fn tab_with(&self, pane: PaneId) -> Option<usize> {
        self.tabs.iter().position(|t| {
            let mut leaves = Vec::new();
            t.root.leaves(&mut leaves);
            leaves.contains(&pane)
        })
    }

    fn inherited_launch(&self) -> Option<Create> {
        self.focused()
            .and_then(|p| self.launch(p))
            .cloned()
            .map(|mut c| {
                c.account = None;
                c.program = Program::Login;
                c
            })
    }

    /// 칸의 세션을 뗀다(콜드 화면의 "다시 시작", MK-032). 다음에 붙을 때 새 세션을 만든다.
    pub fn clear_session(&mut self, pane: PaneId) {
        self.sessions.remove(&pane);
    }

    fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        for tab in &self.tabs {
            tab.root.leaves(&mut out);
        }
        out
    }

    /// 닫힌 칸의 세션 항목을 지운다. 미리보기 슬롯이 가리킨 칸이 사라졌으면 함께 비운다.
    fn prune(&mut self) {
        let live = self.panes();
        self.sessions.retain(|p, _| live.contains(p));
        self.launches.retain(|p, _| live.contains(p));
        self.hosts.retain(|p, _| live.contains(p));
        self.files.retain(|p, _| live.contains(p));
        self.clear_stale_preview();
        self.file_states.retain(|p, _| self.files.contains_key(p));
        self.respawn_cwds.retain(|p, _| live.contains(p));
    }

    fn alloc_pane(&mut self) -> PaneId {
        let id = PaneId(self.next_pane);
        self.next_pane = self.next_pane.saturating_add(1);
        id
    }

    fn sync_mru(&mut self) {
        if self.tabs.is_empty() {
            self.mru.order.clear();
            self.mru.cursor = None;
            return;
        }
        let len = self.tabs.len();
        let mut deduped = Vec::with_capacity(len);
        for &idx in &self.mru.order {
            if idx < len && !deduped.contains(&idx) {
                deduped.push(idx);
            }
        }
        for idx in 0..len {
            if !deduped.contains(&idx) {
                deduped.push(idx);
            }
        }
        self.mru.order = deduped;
        if self.mru.cursor.is_some_and(|c| c >= self.mru.order.len()) {
            self.mru.cursor = None;
        }
    }

    fn record_mru(&mut self, index: usize) {
        self.mru.cursor = None;
        if self.tabs.is_empty() {
            self.mru.order.clear();
            return;
        }
        self.sync_mru();
        if index < self.tabs.len() {
            self.mru.order.retain(|&i| i != index);
            self.mru.order.insert(0, index);
        }
    }

    /// 현재 MRU 순서(0번째가 가장 최근 포커스된 탭 인덱스).
    pub fn mru_order(&self) -> Vec<usize> {
        let len = self.tabs.len();
        let mut out = Vec::with_capacity(len);
        if self.active < len && self.mru.cursor.is_none() {
            out.push(self.active);
        }
        for &idx in &self.mru.order {
            if idx < len && !out.contains(&idx) {
                out.push(idx);
            }
        }
        for idx in 0..len {
            if !out.contains(&idx) {
                out.push(idx);
            }
        }
        out
    }

    /// `Ctrl+Tab` 최근 탭 스위처가 열려 있으면 현재 가리키는 `mru_order` 내 인덱스.
    pub fn mru_cursor(&self) -> Option<usize> {
        self.mru.cursor
    }

    pub fn mru_switching(&self) -> bool {
        self.mru.cursor.is_some()
    }

    /// `Ctrl+Tab` / `Ctrl+Shift+Tab`: 최근 사용한 탭 순서(MRU)대로 활성 탭을 옮긴다.
    pub fn cycle_mru(&mut self, step: isize) {
        let tabs: Vec<_> = (0..self.tabs.len()).collect();
        self.cycle_mru_in(step, &tabs);
    }

    /// 지정된 워크스페이스 탭 안에서만 MRU를 순환한다. 커서는 전체 MRU 순서의 위치다.
    pub fn cycle_mru_in(&mut self, step: isize, tabs: &[usize]) {
        if tabs.len() <= 1 {
            self.mru.cursor = None;
            return;
        }
        self.sync_mru();
        if self.mru.cursor.is_none() && self.mru.order.first().copied() != Some(self.active) {
            self.record_mru(self.active);
        }
        let eligible: Vec<_> = self
            .mru
            .order
            .iter()
            .enumerate()
            .filter_map(|(pos, i)| tabs.contains(i).then_some(pos))
            .collect();
        if eligible.is_empty() {
            return;
        }
        let cur = eligible
            .iter()
            .position(|&pos| self.mru.order[pos] == self.active)
            .unwrap_or(0);
        let next = (cur.cast_signed() + step)
            .rem_euclid(eligible.len().cast_signed())
            .cast_unsigned();
        let cursor = eligible[next];
        self.mru.cursor = Some(cursor);
        self.active = self.mru.order[cursor];
    }

    /// `Ctrl` 키를 놓거나 스위처 선택을 확정할 때 현재 활성 탭을 MRU 맨 앞으로 올린다.
    pub fn commit_mru(&mut self) -> bool {
        if self.mru.cursor.take().is_some() {
            self.record_mru(self.active);
            true
        } else {
            false
        }
    }

    /// 새 탭은 활성 탭 바로 오른쪽에 연다(포크 탭 `new_tab_after`와 같은 위치).
    pub fn new_tab(&mut self) {
        self.new_tab_after(self.active);
    }

    /// `index` 탭 바로 오른쪽(`index + 1`)에 새 탭을 만들고 활성화한다(OWE-736).
    pub fn new_tab_after(&mut self, index: usize) {
        let launch = self.inherited_launch();
        let pane = self.alloc_pane();
        let insert_at = index.saturating_add(1).min(self.tabs.len());
        self.tabs.insert(insert_at, Tab::single(pane));
        if let Some(launch) = launch {
            self.launches.insert(pane, launch);
        }
        // OWE-734 MRU는 탭 인덱스를 담으므로 삽입 위치 뒤 인덱스를 민다.
        for idx in &mut self.mru.order {
            if *idx >= insert_at {
                *idx += 1;
            }
        }
        self.active = insert_at;
        self.record_mru(insert_at);
    }

    /// 탭을 닫는다. 마지막 탭도 닫혀 탭이 없는 빈 상태가 될 수 있다(Orca 패리티).
    pub fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        self.mru.order.retain_mut(|i| {
            if *i == index {
                false
            } else {
                if *i > index {
                    *i -= 1;
                }
                true
            }
        });
        self.mru.cursor = None;
        if self.tabs.is_empty() {
            self.active = 0;
        } else if self.active > index || self.active == self.tabs.len() {
            self.active -= 1;
        }
        self.prune();
        self.record_mru(self.active);
    }

    fn retain_tabs(&mut self, prefer_active: usize, keep: impl Fn(usize, &Tab) -> bool) {
        let mut new_tabs = Vec::with_capacity(self.tabs.len());
        let mut old_to_new = vec![None; self.tabs.len()];
        for (i, tab) in self.tabs.drain(..).enumerate() {
            if keep(i, &tab) {
                old_to_new[i] = Some(new_tabs.len());
                new_tabs.push(tab);
            }
        }
        self.tabs = new_tabs;
        self.mru.order = self
            .mru
            .order
            .iter()
            .filter_map(|&old| old_to_new.get(old).copied().flatten())
            .collect();
        self.mru.cursor = None;
        if self.tabs.is_empty() {
            self.active = 0;
        } else if let Some(mapped) = old_to_new.get(prefer_active).copied().flatten() {
            self.active = mapped;
        } else if let Some(mapped) = old_to_new.get(self.active).copied().flatten() {
            self.active = mapped;
        } else if let Some(&mru_first) = self.mru.order.first() {
            self.active = mru_first;
        } else {
            self.active = self.tabs.len() - 1;
        }
        self.prune();
        self.record_mru(self.active);
    }

    /// 지정한 탭을 제외한 나머지 비고정 탭을 모두 닫는다(핀 고정 탭은 유지).
    pub fn close_other_tabs(&mut self, keep_index: usize) {
        if keep_index >= self.tabs.len() {
            return;
        }
        self.retain_tabs(keep_index, |i, tab| i == keep_index || tab.pinned);
    }

    /// 지정한 탭의 오른쪽에 있는 비고정 탭을 모두 닫는다(핀 고정 탭은 유지).
    pub fn close_tabs_to_right(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let prefer = if self.active <= index {
            self.active
        } else {
            index
        };
        self.retain_tabs(prefer, |i, tab| i <= index || tab.pinned);
    }

    /// OWE-870: 지정한 탭의 왼쪽에 있는 비고정 탭을 모두 닫는다(핀 고정 탭은 유지).
    pub fn close_tabs_to_left(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let prefer = if self.active >= index {
            self.active
        } else {
            index
        };
        self.retain_tabs(prefer, |i, tab| i >= index || tab.pinned);
    }

    /// 비고정 에디터(파일) 탭을 모두 닫는다(핀 고정 탭과 터미널 탭은 유지).
    pub fn close_all_editor_tabs(&mut self) {
        let editor_flags: Vec<bool> = (0..self.tabs.len())
            .map(|i| self.is_editor_tab(i))
            .collect();
        let active = self.active;
        self.retain_tabs(active, |i, tab| !editor_flags[i] || tab.pinned);
    }

    /// 탭을 최종 위치 `to`로 옮긴다. 활성 탭·세션·분할 포커스는 그대로 보존한다.
    pub fn reorder_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        let remap = |idx: usize| -> usize {
            if idx == from {
                to
            } else if from < idx && idx <= to {
                idx - 1
            } else if to <= idx && idx < from {
                idx + 1
            } else {
                idx
            }
        };
        self.active = remap(self.active);
        for idx in &mut self.mru.order {
            *idx = remap(*idx);
        }
        self.mru.cursor = None;
        self.sync_mru();
    }

    pub fn activate(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
            self.record_mru(index);
        }
    }

    /// `step`만큼 탭을 순환 이동한다(1 = 다음, -1 = 이전).
    pub fn cycle(&mut self, step: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let len = self.tabs.len().cast_signed();
        self.active = (self.active.cast_signed() + step)
            .rem_euclid(len)
            .cast_unsigned();
        self.record_mru(self.active);
    }

    /// 활성 탭의 포커스 칸을 반으로 나누고 새 칸에 포커스한다. 모든 탭이 닫혀 있으면 첫 탭을 만든다(OWE-869).
    pub fn split(&mut self, axis: Axis) {
        let Some(focused) = self.focused() else {
            self.new_tab();
            return;
        };
        let launch = self.inherited_launch();
        let host = self.host(focused).clone();
        let new = self.alloc_pane();
        let tab = &mut self.tabs[self.active];
        let target = tab.focused;
        if tab.root.replace_leaf(target, &mut |old| PaneTree::Split {
            axis,
            ratio: 0.5,
            first: Box::new(PaneTree::Leaf(old)),
            second: Box::new(PaneTree::Leaf(new)),
        }) {
            tab.focused = new;
            if let Some(launch) = launch {
                self.launches.insert(new, launch);
            }
            if !host.is_local() {
                self.hosts.insert(new, host);
            }
        }
    }

    /// 활성 탭의 포커스 칸을 닫는다. 탭의 마지막 칸이면 탭을 닫는다.
    pub fn close_pane(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        match tab.root.remove_leaf(tab.focused) {
            Some(next) => tab.focused = next,
            None => self.close_tab(self.active),
        }
        self.prune();
    }

    pub fn focus(&mut self, pane: PaneId) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let mut leaves = Vec::new();
        tab.root.leaves(&mut leaves);
        if leaves.contains(&pane) {
            tab.focused = pane;
        }
    }

    /// 활성 탭 안에서 분할 칸 포커스를 `step`만큼 순환 이동한다(1 = 다음 칸, -1 = 이전 칸, MK-110).
    pub fn cycle_pane(&mut self, step: isize) {
        let leaves = self.visible();
        if leaves.len() <= 1 {
            return;
        }
        let cur = leaves
            .iter()
            .position(|&p| p == self.tabs[self.active].focused)
            .unwrap_or(0);
        let len = leaves.len().cast_signed();
        let next = (cur.cast_signed() + step).rem_euclid(len).cast_unsigned();
        self.tabs[self.active].focused = leaves[next];
    }

    /// OWE-871: 활성 탭 안에서 `dir` 쪽에 가장 가까운 칸으로 포커스를 옮긴다.
    /// 칸 배치는 분할 트리의 비율로 0~1 정사각형에 펼쳐 중심점으로 견준다.
    /// 같은 쪽에 칸이 없으면 그대로 둔다.
    pub fn focus_direction(&mut self, dir: Direction) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let mut rects = Vec::new();
        Self::leaf_rects(&tab.root, [0., 0., 1., 1.], &mut rects);
        let cur = rects
            .iter()
            .find(|(p, _)| *p == tab.focused)
            .map(|(_, r)| center(*r));
        let Some((cur_x, cur_y)) = cur else {
            return;
        };
        let mut best: Option<(PaneId, f32)> = None;
        for (pane, rect) in &rects {
            if *pane == tab.focused {
                continue;
            }
            let (cand_x, cand_y) = center(*rect);
            let (primary, perp) = match dir {
                Direction::Left => (cur_x - cand_x, (cur_y - cand_y).abs()),
                Direction::Right => (cand_x - cur_x, (cur_y - cand_y).abs()),
                Direction::Up => (cur_y - cand_y, (cur_x - cand_x).abs()),
                Direction::Down => (cand_y - cur_y, (cur_x - cand_x).abs()),
            };
            if primary <= f32::EPSILON {
                continue;
            }
            let score = primary + perp * 2.;
            if best.is_none_or(|(_, s)| score < s) {
                best = Some((*pane, score));
            }
        }
        if let Some((pane, _)) = best {
            self.tabs[self.active].focused = pane;
        }
    }

    /// 분할 트리를 0~1 정사각형에 펼쳐 잎마다 `[x, y, w, h]`를 모은다.
    fn leaf_rects(node: &PaneTree, rect: [f32; 4], out: &mut Vec<(PaneId, [f32; 4])>) {
        match node {
            PaneTree::Leaf(id) => out.push((*id, rect)),
            PaneTree::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let [left, top, width, height] = rect;
                let clamped = ratio.clamp(0., 1.);
                let (first_rect, second_rect) = match axis {
                    Axis::Horizontal => (
                        [left, top, width * clamped, height],
                        [left + width * clamped, top, width * (1. - clamped), height],
                    ),
                    Axis::Vertical => (
                        [left, top, width, height * clamped],
                        [left, top + height * clamped, width, height * (1. - clamped)],
                    ),
                };
                Self::leaf_rects(first, first_rect, out);
                Self::leaf_rects(second, second_rect, out);
            }
        }
    }

    /// 활성 탭에서 `path`의 분할 비율을 바꾼다. 범위 밖 값은 무시한다.
    pub fn set_ratio(&mut self, path: &[Branch], value: f32) {
        if !(value > 0.0 && value < 1.0) {
            return;
        }
        if let Some(PaneTree::Split { ratio, .. }) = self
            .tabs
            .get_mut(self.active)
            .and_then(|t| t.root.node_mut(path))
        {
            *ratio = value;
        }
    }

    fn tail(&self) -> LayoutTail {
        LayoutTail {
            window_bounds: self.window_bounds,
            sidebar_collapsed: self.sidebar_collapsed,
            panel_collapsed: self.panel_collapsed,
            panel_tab: self.panel_tab,
            file_states: self.file_states.clone(),
            respawn_cwds: self.respawn_cwds.clone(),
            last_seen_unix_ns: self.last_seen_unix_ns,
        }
    }

    fn apply_tail(&mut self, tail: LayoutTail) {
        self.window_bounds = tail.window_bounds;
        self.sidebar_collapsed = tail.sidebar_collapsed;
        self.panel_collapsed = tail.panel_collapsed;
        self.panel_tab = tail.panel_tab;
        self.file_states = tail.file_states;
        self.respawn_cwds = tail.respawn_cwds;
        self.last_seen_unix_ns = tail.last_seen_unix_ns;
    }

    fn is_valid(&self) -> bool {
        let widths_ok = [self.sidebar_width, self.panel_width]
            .iter()
            .all(|w| w.is_finite() && *w >= 0.0);
        let mut seen = Vec::new();
        let tabs_ok = self.tabs.iter().all(|tab| {
            let start = seen.len();
            tab.root.leaves(&mut seen);
            tab.root.ratios_valid() && seen[start..].contains(&tab.focused)
        });
        let ids_ok = seen.iter().all(|id| id.0 < self.next_pane) && {
            let mut sorted: Vec<u32> = seen.iter().map(|id| id.0).collect();
            sorted.sort_unstable();
            sorted.windows(2).all(|w| w[0] != w[1])
        };
        let sessions_ok = self.sessions.keys().all(|p| seen.contains(p));
        let active_ok = if self.tabs.is_empty() {
            self.active == 0
        } else {
            self.active < self.tabs.len()
        };
        active_ok
            && widths_ok
            && tabs_ok
            && ids_ok
            && sessions_ok
            && self.launches.keys().all(|p| seen.contains(p))
            && self
                .hosts
                .iter()
                .all(|(p, h)| seen.contains(p) && !h.is_local())
            && self.window_bounds.is_none_or(SavedWindowBounds::is_valid)
            && self.file_states.keys().all(|p| self.files.contains_key(p))
            && self.respawn_cwds.keys().all(|p| seen.contains(p))
    }

    /// 손상 백업(`.corrupt`)을 만들지 않고 저장 파일에서 창 위치·크기만 미리 읽는다.
    #[must_use]
    pub fn peek_window_bounds(path: &Path) -> Option<SavedWindowBounds> {
        let mut bytes = Vec::new();
        fs::File::open(path)
            .ok()?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        Self::decode(&bytes).ok()?.window_bounds
    }

    /// 저장 파일을 읽는다. 손상·미지원 버전은 덮어쓰지 않도록 `.corrupt`로 보존한 뒤 오류를 돌려준다.
    pub fn load(path: &Path) -> io::Result<Self> {
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        let result = Self::decode(&bytes);
        if result
            .as_ref()
            .is_err_and(|e| e.kind() == io::ErrorKind::InvalidData)
        {
            preserve_corrupt(path)?;
        }
        result
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        let invalid = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_owned());
        if u64::try_from(bytes.len()).map_or(true, |n| n > MAX_FILE_BYTES) {
            return Err(invalid("layout file too large"));
        }
        let Some((&version, body)) = bytes.split_first() else {
            return Err(invalid("empty layout file"));
        };
        // 뒤에 추가된 map 필드(v3 launches, v4 files)는 빈 map(postcard 길이 0)으로 채운다.
        let missing = match version {
            2 => 2,
            3 => 1,
            4 | 5 | 6 | FORMAT_VERSION => 0,
            _ => return Err(invalid("unknown layout format version")),
        };
        let mut migrated;
        let body = if missing > 0 {
            migrated = body.to_vec();
            migrated.extend(std::iter::repeat_n(0, missing));
            migrated.as_slice()
        } else {
            body
        };
        let mut layout = match version {
            FORMAT_VERSION => {
                let (mut l, rest) =
                    postcard::take_from_bytes::<Self>(body).map_err(|e| invalid(&e.to_string()))?;
                if !rest.is_empty() {
                    let (hosts, rest) = postcard::take_from_bytes::<BTreeMap<PaneId, HostId>>(rest)
                        .map_err(|e| invalid(&e.to_string()))?;
                    l.hosts = hosts;
                    if !rest.is_empty() {
                        // 7-필드 꼬리가 아니면 6-필드(`last_seen` 이전), 그다음 중간 커밋의 5-필드 꼬리로 읽는다.
                        let tail = postcard::from_bytes::<LayoutTail>(rest)
                            .or_else(|_| postcard::from_bytes::<TailV6>(rest).map(LayoutTail::from))
                            .or_else(|_| {
                                postcard::from_bytes::<LegacyTail>(rest).map(LayoutTail::from)
                            })
                            .map_err(|e| invalid(&e.to_string()))?;
                        l.apply_tail(tail);
                    }
                }
                Ok(l)
            }
            // v6: 탭 핀·색상은 있고 OWE-870 `custom_title`은 없음.
            6 => postcard::from_bytes::<LegacyLayoutV6>(body).map(Self::from),
            5 => postcard::from_bytes::<LegacyLayout<Create>>(body).map(Self::from),
            _ => postcard::from_bytes::<LegacyLayout<LegacyCreate>>(body).map(Self::from),
        }
        .map_err(|e: postcard::Error| invalid(&e.to_string()))?;
        if !layout.is_valid() {
            return Err(invalid("layout violates invariants"));
        }
        layout.record_mru(layout.active);
        Ok(layout)
    }

    /// 임시 파일(0600)에 쓰고 fsync한 뒤 rename한다. 중간에 죽어도 이전 파일이나 새 파일 중 하나가 남는다.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut body = postcard::to_stdvec(self).map_err(io::Error::other)?;
        let tail = self.tail();
        if !self.hosts.is_empty() || tail != LayoutTail::default() {
            body.extend(postcard::to_stdvec(&self.hosts).map_err(io::Error::other)?);
            if tail != LayoutTail::default() {
                body.extend(postcard::to_stdvec(&tail).map_err(io::Error::other)?);
            }
        }
        if u64::try_from(body.len()).map_or(true, |n| n >= MAX_FILE_BYTES) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "layout file too large",
            ));
        }
        if let Some(dir) = path.parent() {
            DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        let tmp = path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&[FORMAT_VERSION])?;
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        sync_parent(path)
    }
}

/// 기존 백업을 교체하지 않는 hard link로 원본 바이트를 먼저 확보한다.
/// 보존 실패 시 원본 경로는 남고 호출자는 저장을 중단한다.
pub(crate) fn preserve_corrupt(path: &Path) -> io::Result<()> {
    let mut suffix = 0u32;
    loop {
        let backup = path.with_extension(if suffix == 0 {
            "corrupt".to_owned()
        } else {
            format!("{suffix}.corrupt")
        });
        match fs::hard_link(path, &backup) {
            Ok(()) => {
                sync_parent(path)?;
                fs::remove_file(path)?;
                return sync_parent(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                suffix = suffix
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("too many corrupt layout backups"))?;
            }
            Err(e) => return Err(e),
        }
    }
}

/// 이름 바꾸기·삭제를 디스크에 남기려고 부모 디렉터리를 fsync한다. Windows는 디렉터리를 `File::open`으로
/// 열 수 없어(os error 5, OWE-835 실측) 건너뛴다. 모든 호출처(레이아웃·사이드바·편집기 저장)가 이 함수를 지난다.
pub(crate) fn sync_parent(path: &Path) -> io::Result<()> {
    if cfg!(not(unix)) {
        return Ok(());
    }
    fs::File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()
}

/// 레이아웃 파일 경로: [`krill_proto::daemon::state_dir`] 아래.
pub fn default_path() -> Option<PathBuf> {
    krill_proto::daemon::state_dir().map(|d| d.join(FILE_NAME))
}

/// 드래그 위치를 분할 비율로 바꾼다. 양쪽 칸이 `MIN_PANE_PX`보다 작아지지 않게 자른다.
pub fn drag_ratio(pos: f32, start: f32, len: f32) -> f32 {
    if len.is_nan() || len <= 2.0 * MIN_PANE_PX {
        return 0.5;
    }
    let min = MIN_PANE_PX / len;
    ((pos - start) / len).clamp(min, 1.0 - min)
}

/// 옆 열 너비를 자른다. 최소 `MIN_PANE_PX`, 가운데 열도 `MIN_PANE_PX` 이상 남긴다.
pub fn clamp_column(width: f32, total: f32, other: f32) -> f32 {
    let max = (total - other - MIN_PANE_PX).max(MIN_PANE_PX);
    width.clamp(MIN_PANE_PX, max)
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
