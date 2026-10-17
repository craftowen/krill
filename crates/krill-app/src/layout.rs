//! 앱 셸 레이아웃 모델(MK-030): 탭·분할 트리·열 너비·칸마다 붙은 세션(OWE-490)과 디스크 저장.
//!
//! gpui에 의존하지 않는다. UI 프레임워크가 바뀌어도(MK-002 잠정 결정) 이 모듈과 저장 파일은 그대로 쓴다.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
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
const FORMAT_VERSION: u8 = 6;
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

    /// `target` 잎을 지우고 그 부모 분할을 형제로 바꾼다. 형제의 첫 잎을 돌려준다.
    fn remove_leaf(&mut self, target: PaneId) -> Option<PaneId> {
        let Self::Split { first, second, .. } = self else {
            return None;
        };
        let sibling = if **first == Self::Leaf(target) {
            second
        } else if **second == Self::Leaf(target) {
            first
        } else {
            return first
                .remove_leaf(target)
                .or_else(|| second.remove_leaf(target));
        };
        let sibling = std::mem::replace(&mut **sibling, Self::Leaf(target));
        *self = sibling;
        Some(self.first_leaf())
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
}

impl Tab {
    fn single(pane: PaneId) -> Self {
        Self {
            root: PaneTree::Leaf(pane),
            focused: pane,
            pinned: false,
            color: None,
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
    #[serde(skip)]
    mru: MruState,
}

/// v5까지의 디스크 `Tab`(OWE-734 `pinned`·`color` 이전).
#[derive(Deserialize)]
struct LegacyTab {
    root: PaneTree,
    focused: PaneId,
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

    /// 파일 칸들(모든 탭).
    pub fn files(&self) -> impl Iterator<Item = (PaneId, &Path)> {
        self.files.iter().map(|(p, f)| (*p, f.as_path()))
    }

    /// 파일을 연다: 이미 어느 탭의 칸에 열려 있으면 그 탭·칸으로, 아니면 새 탭(폴더는 지금 칸을 물려받는다).
    pub fn open_file(&mut self, path: PathBuf) -> PaneId {
        if let Some((tab, pane)) = self.tabs.iter().enumerate().find_map(|(i, t)| {
            let mut leaves = Vec::new();
            t.root.leaves(&mut leaves);
            leaves
                .into_iter()
                .find(|p| self.files.get(p) == Some(&path))
                .map(|p| (i, p))
        }) {
            self.active = tab;
            self.tabs[tab].focused = pane;
            self.record_mru(tab);
            return pane;
        }
        self.new_tab();
        let pane = self.active_tab().focused;
        self.files.insert(pane, path);
        pane
    }

    fn inherited_launch(&self) -> Option<Create> {
        self.focused()
            .and_then(|p| self.launch(p))
            .cloned()
            .map(|mut c| {
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

    /// 닫힌 칸의 세션 항목을 지운다.
    fn prune(&mut self) {
        let live = self.panes();
        self.sessions.retain(|p, _| live.contains(p));
        self.launches.retain(|p, _| live.contains(p));
        self.hosts.retain(|p, _| live.contains(p));
        self.files.retain(|p, _| live.contains(p));
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
        if self.tabs.len() <= 1 {
            self.mru.cursor = None;
            return;
        }
        self.sync_mru();
        if self.mru.cursor.is_none() && self.mru.order.first().copied() != Some(self.active) {
            self.record_mru(self.active);
        }
        let cur = self.mru.cursor.unwrap_or(0);
        let len = self.mru.order.len().cast_signed();
        let next = (cur.cast_signed() + step).rem_euclid(len).cast_unsigned();
        self.mru.cursor = Some(next);
        self.active = self.mru.order[next];
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

    pub fn new_tab(&mut self) {
        let launch = self.inherited_launch();
        let pane = self.alloc_pane();
        self.tabs.push(Tab::single(pane));
        if let Some(launch) = launch {
            self.launches.insert(pane, launch);
        }
        self.active = self.tabs.len() - 1;
        self.record_mru(self.active);
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

    /// 활성 탭의 포커스 칸을 반으로 나누고 새 칸에 포커스한다.
    pub fn split(&mut self, axis: Axis) {
        if self.tabs.is_empty() {
            return;
        }
        let launch = self.inherited_launch();
        let host = self.host(self.active_tab().focused).clone();
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
                .files
                .keys()
                .all(|p| seen.contains(p) && !self.sessions.contains_key(p))
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
            4 | 5 | FORMAT_VERSION => 0,
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
            FORMAT_VERSION => postcard::from_bytes::<Self>(body),
            5 => postcard::from_bytes::<LegacyLayout<Create>>(body).map(Self::from),
            _ => postcard::from_bytes::<LegacyLayout<LegacyCreate>>(body).map(Self::from),
        }
        .map_err(|e| invalid(&e.to_string()))?;
        if !layout.is_valid() {
            return Err(invalid("layout violates invariants"));
        }
        layout.record_mru(layout.active);
        Ok(layout)
    }

    /// 임시 파일(0600)에 쓰고 fsync한 뒤 rename한다. 중간에 죽어도 이전 파일이나 새 파일 중 하나가 남는다.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let body = postcard::to_stdvec(self).map_err(io::Error::other)?;
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

pub(crate) fn sync_parent(path: &Path) -> io::Result<()> {
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
mod tests;
