//! 우측 패널 가상화 파일 탐색기(`File Explorer`, `⌘⇧E`, MK-178 / OWE-732).
//!
//! 폴더 지연 로딩(`dir_cache`), `uniform_list` 가상화 트리 렌더링, 실시간 파일명 필터,
//! 숨김 파일(`.dotfiles`) 및 `.gitignore` 파일 표시 토글, 새 파일/폴더 생성·이름 변경·삭제,
//! 우클릭 컨텍스트 메뉴(경로 복사, Finder/외부 에디터 열기)를 제공한다.
//!
//! 트리 투영·필터 구조 참고:
//! `.ref/orca/src/renderer/src/components/right-sidebar/FileExplorer.tsx`,
//! `.ref/orca/src/renderer/src/components/right-sidebar/useFileExplorerVisibleRowProjection.ts`,
//! `.ref/orca/src/renderer/src/components/right-sidebar/file-explorer-name-filter-projection.ts`
//! (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, ClipboardItem, Context, ElementId, Entity, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, UniformListScrollHandle,
    Window, div, px, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_component::{Icon, IconName, Selectable as _, Sizable as _};

use super::{OpenFile, Panel, text};
use crate::open_in::Target;
use crate::ui;

const ROW_HEIGHT: f32 = 26.0;

/// 필터 검색 시 재귀 스캔하는 최대 엔트리 수 상한(메모리 ≤ 120MB 및 응답성 보호).
const MAX_FILTER_ENTRIES: usize = 10_000;

/// 파일 탐색기 이름 필터 입력 디바운스 지연 시간(키 입력마다 동기 스캔·`git check-ignore` 블로킹 방지).
pub const EXPLORER_FILTER_DEBOUNCE: Duration = Duration::from_millis(150);

/// 디렉터리 엔트리 캐시 항목.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub relative_path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// `uniform_list`에 그려지는 평탄화된 파일 트리 행.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub name: String,
    pub path: PathBuf,
    pub relative_path: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
    pub ignored: bool,
}

/// 인라인 입력 작업 종류(새 파일 / 새 폴더 / 이름 변경).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditKind {
    NewFile { parent: PathBuf },
    NewFolder { parent: PathBuf },
    Rename { target: PathBuf },
}

enum Operation {
    CreateFile { parent: PathBuf, name: String },
    CreateFolder { parent: PathBuf, name: String },
    Rename { target: PathBuf, name: String },
    Delete { target: PathBuf },
}

/// UI 핸들을 포함하지 않는 백그라운드 IO용 상태.
struct DiskState {
    repo: PathBuf,
    home: PathBuf,
    expanded: BTreeSet<PathBuf>,
    dir_cache: BTreeMap<PathBuf, Vec<DirEntry>>,
    ignored: BTreeSet<String>,
    selected: Option<PathBuf>,
}

impl DiskState {
    fn capture(explorer: &Explorer) -> Self {
        Self {
            repo: explorer.repo.clone(),
            home: explorer.home.clone(),
            expanded: explorer.expanded.clone(),
            dir_cache: explorer.dir_cache.clone(),
            ignored: explorer.ignored.clone(),
            selected: explorer.selected.clone(),
        }
    }

    fn run(self, operation: Operation) -> (Self, io::Result<PathBuf>, bool) {
        // 디스크 모델은 이 작업 안에서만 생성한다. GPUI 핸들은 스레드 사이로 옮기지 않는다.
        let mut model = Explorer::new(self.repo);
        model.home = self.home;
        model.expanded = self.expanded;
        model.dir_cache = self.dir_cache;
        model.ignored = self.ignored;
        model.selected = self.selected;
        let open = matches!(operation, Operation::CreateFile { .. });
        let result = match operation {
            Operation::CreateFile { parent, name } => model.create_file(&parent, &name),
            Operation::CreateFolder { parent, name } => model.create_folder(&parent, &name),
            Operation::Rename { target, name } => model.rename_path(&target, &name),
            Operation::Delete { target } => model.delete_path(&target),
        };
        (Self::capture(&model), result, open)
    }
}

/// 휴지통 이동 확인 대화상자 상태.
#[derive(Debug, Clone)]
pub struct PendingDelete {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub cancel_focus: gpui::FocusHandle,
}

/// 파일 탐색기 상태.
#[expect(
    clippy::struct_excessive_bools,
    reason = "로드 상태·표시 토글 독립 플래그들(OWE-849 비동기 로드 추가로 4개)"
)]
pub struct Explorer {
    pub(crate) repo: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) loaded_root: bool,
    pub(crate) loading: bool,
    pub(crate) load_generation: u64,
    pub(crate) load_task: Option<Task<()>>,
    pub(crate) filter_generation: u64,
    pub(crate) filter_task: Option<Task<()>>,
    pub(crate) dir_cache: BTreeMap<PathBuf, Vec<DirEntry>>,
    pub(crate) expanded: BTreeSet<PathBuf>,
    pub(crate) ignored: BTreeSet<String>,
    pub(crate) show_dotfiles: bool,
    pub(crate) show_ignored: bool,
    pub(crate) filter_query: String,
    pub(crate) filter_collapsed: BTreeSet<PathBuf>,
    pub(crate) filter_files_cache: Option<Vec<DirEntry>>,
    filter_normalized: Vec<String>,
    pub(crate) rows: Vec<TreeRow>,
    pub(crate) selected: Option<PathBuf>,
    pub(crate) scroll: UniformListScrollHandle,
    pub(crate) tree_focus: Option<gpui::FocusHandle>,
    pub(crate) filter_input: Option<Entity<InputState>>,
    pub(crate) filter_sub: Option<Subscription>,
    pub(crate) editing: Option<(EditKind, Entity<InputState>, Option<Subscription>)>,
    pub(crate) pending_delete: Option<PendingDelete>,
    pub(crate) error: Option<String>,
    operations: VecDeque<Operation>,
    operation_task: Option<Task<()>>,
    operation_generation: u64,
    #[cfg(test)]
    pub(crate) rendered_rows: usize,
}

impl std::fmt::Debug for Explorer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Explorer")
            .field("repo", &self.repo)
            .field("home", &self.home)
            .field("loaded_root", &self.loaded_root)
            .field("loading", &self.loading)
            .field("show_dotfiles", &self.show_dotfiles)
            .field("show_ignored", &self.show_ignored)
            .field("filter_query", &self.filter_query)
            .field("rows_len", &self.rows.len())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl Explorer {
    pub fn new(repo: PathBuf) -> Self {
        Self {
            repo,
            home: crate::accounts::default_home(),
            loaded_root: false,
            loading: false,
            load_generation: 0,
            load_task: None,
            filter_generation: 0,
            filter_task: None,
            dir_cache: BTreeMap::new(),
            expanded: BTreeSet::new(),
            ignored: BTreeSet::new(),
            show_dotfiles: true,
            show_ignored: true,
            filter_query: String::new(),
            filter_collapsed: BTreeSet::new(),
            filter_files_cache: None,
            filter_normalized: Vec::new(),
            rows: Vec::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            tree_focus: None,
            filter_input: None,
            filter_sub: None,
            editing: None,
            pending_delete: None,
            error: None,
            operations: VecDeque::new(),
            operation_task: None,
            operation_generation: 0,
            #[cfg(test)]
            rendered_rows: 0,
        }
    }

    /// 휴지통(`~/.Trash`) 기준 홈 디렉터리를 지정한다.
    pub fn set_home(&mut self, home: PathBuf) {
        self.home = home;
    }

    /// 워크트리 루트가 바뀔 때 캐시와 펼침 상태를 초기화한다.
    pub fn set_repo(&mut self, repo: PathBuf, load_now: bool) {
        self.operations.clear();
        self.operation_task = None;
        self.operation_generation = self.operation_generation.wrapping_add(1);
        self.repo = repo;
        self.loaded_root = false;
        self.loading = false;
        self.load_generation = self.load_generation.wrapping_add(1);
        self.load_task = None;
        self.filter_generation = self.filter_generation.wrapping_add(1);
        self.filter_task = None;
        self.dir_cache.clear();
        self.expanded.clear();
        self.ignored.clear();
        self.filter_collapsed.clear();
        self.filter_files_cache = None;
        self.filter_normalized.clear();
        self.rows.clear();
        self.selected = None;
        self.editing = None;
        self.pending_delete = None;
        self.error = None;
        if load_now {
            self.ensure_loaded();
        }
    }

    /// 인라인 편집 입력 행이 가상 리스트 내 어느 위치에 들어가야 하는지 `(virtual_index, depth, replaces_row)`를 반환한다.
    pub(crate) fn inline_edit_slot(&self) -> Option<(usize, usize, bool)> {
        let (kind, _, _) = self.editing.as_ref()?;
        match kind {
            EditKind::Rename { target } => {
                if let Some(idx) = self.rows.iter().position(|r| &r.path == target) {
                    Some((idx, self.rows[idx].depth, true))
                } else {
                    Some((0, 0, false))
                }
            }
            EditKind::NewFile { parent } | EditKind::NewFolder { parent } => {
                if parent == &self.repo {
                    Some((0, 0, false))
                } else if let Some(idx) = self.rows.iter().position(|r| &r.path == parent) {
                    Some((
                        idx.saturating_add(1),
                        self.rows[idx].depth.saturating_add(1),
                        false,
                    ))
                } else {
                    Some((0, 0, false))
                }
            }
        }
    }

    /// 현재 선택 행의 가상 리스트 인덱스를 반환한다.
    pub fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rows.iter().position(|row| &row.path == selected)
    }

    /// 화살표 키 탐색용: 선택을 `delta`만큼 옮기고 클램프한다.
    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() - 1;
        let next = match self.selected_index() {
            Some(index) => (index.cast_signed().saturating_add(delta))
                .clamp(0, last.cast_signed())
                .cast_unsigned(),
            None => {
                if delta < 0 {
                    last
                } else {
                    0
                }
            }
        };
        self.selected = self.rows.get(next).map(|row| row.path.clone());
    }

    /// 선택 행의 부모 폴더를 선택한다(좌측 화살표 탐색용).
    pub fn select_parent(&mut self) -> bool {
        let Some(index) = self.selected_index() else {
            return false;
        };
        let depth = self.rows[index].depth;
        if depth == 0 {
            return false;
        }
        for row in self.rows[..index].iter().rev() {
            if row.depth == depth - 1 {
                self.selected = Some(row.path.clone());
                return true;
            }
        }
        false
    }
    /// 루트 폴더가 아직 로드되지 않았으면 지연 로드한다.
    pub fn ensure_loaded(&mut self) {
        if !self.loaded_root {
            self.loaded_root = true;
            self.loading = false;
            let root = self.repo.clone();
            self.load_dir(&root);
            self.rebuild_rows();
        }
    }

    /// 지정 폴더의 직속 자식만 읽어 `dir_cache`에 넣는다(지연 로딩).
    pub fn load_dir(&mut self, dir: &Path) {
        let entries = read_dir_entries(&self.repo, dir);
        let rels: Vec<String> = entries.iter().map(|e| e.relative_path.clone()).collect();
        for ignored_rel in query_git_ignored(&self.repo, &rels) {
            self.ignored.insert(ignored_rel);
        }
        self.dir_cache.insert(dir.to_path_buf(), entries);
    }

    /// 루트와 현재 펼쳐진 폴더들을 다시 읽는다.
    pub fn refresh(&mut self) {
        self.loaded_root = true;
        self.ignored.clear();
        self.filter_files_cache = None;
        self.filter_normalized.clear();
        let root = self.repo.clone();
        self.load_dir(&root);
        let dirs: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        for dir in dirs {
            if dir.is_dir() {
                self.load_dir(&dir);
            } else {
                self.expanded.remove(&dir);
                self.dir_cache.remove(&dir);
            }
        }
        self.rebuild_rows();
    }

    /// 폴더 펼침/접기를 토글한다. 처음 펼치는 폴더는 이때 디스크에서 지연 로드한다.
    pub fn toggle_dir(&mut self, dir: &Path) {
        self.ensure_loaded();
        self.selected = Some(dir.to_path_buf());
        if !self.filter_query.trim().is_empty() {
            if self.filter_collapsed.contains(dir) {
                self.filter_collapsed.remove(dir);
            } else {
                self.filter_collapsed.insert(dir.to_path_buf());
            }
            self.rebuild_rows();
            return;
        }
        if self.expanded.contains(dir) {
            self.expanded.remove(dir);
        } else {
            self.expanded.insert(dir.to_path_buf());
            if !self.dir_cache.contains_key(dir) {
                self.load_dir(dir);
            }
        }
        self.rebuild_rows();
    }

    /// 모든 펼쳐진 폴더를 접는다.
    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.filter_collapsed.clear();
        self.rebuild_rows();
    }

    /// 숨김 파일(`.dotfiles`) 표시 여부를 토글한다.
    pub fn toggle_dotfiles(&mut self) {
        self.ensure_loaded();
        self.show_dotfiles = !self.show_dotfiles;
        self.rebuild_rows();
    }

    /// `.gitignore` 파일 표시 여부를 토글한다.
    pub fn toggle_ignored(&mut self) {
        self.ensure_loaded();
        self.show_ignored = !self.show_ignored;
        self.rebuild_rows();
    }

    /// 파일명 필터 검색어를 갱신하고 가시 행을 다시 계산한다.
    pub fn set_filter_query(&mut self, query: impl Into<String>) {
        self.ensure_loaded();
        let next = query.into();
        if next.trim().is_empty() {
            self.filter_collapsed.clear();
        }
        self.filter_query = next;
        // 이 동기 디스크 모델 API는 백그라운드 작업·헤드리스 테스트 전용이다.
        if !self.filter_query.trim().is_empty() && self.filter_files_cache.is_none() {
            let scanned = scan_worktree_entries(&self.repo, MAX_FILTER_ENTRIES);
            let rels: Vec<String> = scanned.iter().map(|e| e.relative_path.clone()).collect();
            self.ignored.extend(query_git_ignored(&self.repo, &rels));
            self.filter_files_cache = Some(scanned);
        }
        self.rebuild_rows();
    }

    /// 새 파일/폴더를 만들 기본 대상 폴더(선택된 폴더, 선택된 파일의 부모 폴더, 없으면 루트).
    pub fn target_parent_dir(&self) -> PathBuf {
        let Some(sel) = &self.selected else {
            return self.repo.clone();
        };
        if self.path_is_dir(sel) {
            sel.clone()
        } else {
            sel.parent()
                .map_or_else(|| self.repo.clone(), Path::to_path_buf)
        }
    }

    fn path_is_dir(&self, path: &Path) -> bool {
        path == self.repo
            || self.rows.iter().any(|row| row.path == path && row.is_dir)
            || self
                .dir_cache
                .values()
                .flatten()
                .any(|entry| entry.path == path && entry.is_dir)
    }

    /// 새 빈 파일을 생성하고 부모 폴더를 펼쳐 트리에 반영한다.
    pub fn create_file(&mut self, parent_dir: &Path, name: &str) -> io::Result<PathBuf> {
        self.ensure_loaded();
        let rel = validate_relative_input(name)?;
        let (canon_repo, full_path) = ensure_worktree_create_path(&self.repo, parent_dir, &rel)?;
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)?;
            let canon_parent = parent.canonicalize().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "target directory is outside worktree",
                )
            })?;
            if !canon_parent.starts_with(&canon_repo) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "target directory is outside worktree",
                ));
            }
        }
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&full_path)?;
        self.expand_ancestors_to(&full_path);
        self.filter_files_cache = None;
        self.filter_normalized.clear();
        self.selected = Some(full_path.clone());
        self.error = None;
        self.rebuild_rows();
        Ok(full_path)
    }

    /// 새 폴더를 생성하고 부모 폴더를 펼쳐 트리에 반영한다.
    pub fn create_folder(&mut self, parent_dir: &Path, name: &str) -> io::Result<PathBuf> {
        self.ensure_loaded();
        let rel = validate_relative_input(name)?;
        let (canon_repo, full_path) = ensure_worktree_create_path(&self.repo, parent_dir, &rel)?;
        if full_path.exists() || fs::symlink_metadata(&full_path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "folder already exists",
            ));
        }
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)?;
            let canon_parent = parent.canonicalize().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "target directory is outside worktree",
                )
            })?;
            if !canon_parent.starts_with(&canon_repo) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "target directory is outside worktree",
                ));
            }
        }
        fs::create_dir_all(&full_path)?;
        self.expand_ancestors_to(&full_path);
        self.expanded.insert(full_path.clone());
        self.load_dir(&full_path);
        self.filter_files_cache = None;
        self.filter_normalized.clear();
        self.selected = Some(full_path.clone());
        self.error = None;
        self.rebuild_rows();
        Ok(full_path)
    }

    /// 파일이나 폴더 이름을 바꾸고 트리에 반영한다.
    pub fn rename_path(&mut self, target: &Path, new_name: &str) -> io::Result<PathBuf> {
        self.ensure_loaded();
        let denied = || {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot rename outside worktree root",
            )
        };
        if !target.starts_with(&self.repo)
            || target == self.repo
            || target
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(denied());
        }
        let clean = new_name.trim();
        if clean.is_empty()
            || clean == "."
            || clean == ".."
            || clean == ".git"
            || clean.contains('/')
            || clean.contains('\\')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid file or folder name",
            ));
        }
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
        let canon_repo = self.repo.canonicalize().map_err(|_| denied())?;
        let parent_rel = parent.strip_prefix(&self.repo).map_err(|_| denied())?;
        let mut cur = self.repo.clone();
        for comp in parent_rel.components() {
            let Component::Normal(part) = comp else {
                return Err(denied());
            };
            if part == OsStr::new(".git") {
                return Err(denied());
            }
            cur.push(part);
            let canon_cur = cur.canonicalize().map_err(|_| denied())?;
            if !canon_cur.starts_with(&canon_repo) {
                return Err(denied());
            }
        }
        let canon_parent = parent.canonicalize().map_err(|_| denied())?;
        if !canon_parent.starts_with(&canon_repo) {
            return Err(denied());
        }
        target.symlink_metadata()?;
        let new_path = parent.join(clean);
        if new_path == target {
            return Ok(new_path);
        }
        if new_path.exists() || fs::symlink_metadata(&new_path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "target name already exists",
            ));
        }
        fs::rename(target, &new_path)?;

        let old_expanded: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|p| p.starts_with(target))
            .cloned()
            .collect();
        for old in old_expanded {
            self.expanded.remove(&old);
            if let Ok(suffix) = old.strip_prefix(target) {
                let mapped = if suffix.as_os_str().is_empty() {
                    new_path.clone()
                } else {
                    new_path.join(suffix)
                };
                self.expanded.insert(mapped);
            }
        }
        self.dir_cache.retain(|k, _| !k.starts_with(target));
        self.load_dir(parent);
        if self.expanded.contains(&new_path) && new_path.is_dir() {
            self.load_dir(&new_path);
        }
        self.filter_files_cache = None;
        self.filter_normalized.clear();
        self.selected = Some(new_path.clone());
        self.error = None;
        self.rebuild_rows();
        Ok(new_path)
    }

    /// 파일이나 폴더를 `~/.Trash`로 옮기고 트리에 반영한다.
    /// 휴지통 이동이 실패하면 영구 삭제로 폴백하지 않고 오류를 기록·반환한다.
    pub fn delete_path(&mut self, target: &Path) -> io::Result<PathBuf> {
        self.ensure_loaded();
        if !target.starts_with(&self.repo) || target == self.repo {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot delete worktree root or outside path",
            ));
        }
        let trashed = match krill_proto::usage::profiles::move_path_to_trash(
            &self.home, &self.repo, target,
        ) {
            Ok(path) => path,
            Err(err) => {
                self.error = Some(err.to_string());
                return Err(err);
            }
        };
        self.expanded.retain(|p| !p.starts_with(target));
        self.dir_cache.retain(|k, _| !k.starts_with(target));
        if let Some(parent) = target.parent()
            && parent.starts_with(&self.repo)
        {
            self.load_dir(parent);
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|sel| sel.starts_with(target))
        {
            self.selected = None;
        }
        self.filter_files_cache = None;
        self.filter_normalized.clear();
        self.error = None;
        self.rebuild_rows();
        Ok(trashed)
    }

    fn expand_ancestors_to(&mut self, leaf: &Path) {
        let mut chain = Vec::new();
        let mut cur = leaf.parent();
        while let Some(dir) = cur {
            if !dir.starts_with(&self.repo) {
                break;
            }
            chain.push(dir.to_path_buf());
            if dir == self.repo {
                break;
            }
            cur = dir.parent();
        }
        for dir in chain.into_iter().rev() {
            if dir != self.repo {
                self.expanded.insert(dir.clone());
            }
            self.load_dir(&dir);
        }
    }

    pub(crate) fn rebuild_rows(&mut self) {
        #[cfg(target_os = "macos")]
        let _work = crate::responsiveness::work("explorer.filter");
        self.rows.clear();
        let tokens: Vec<String> = self
            .filter_query
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect();
        if tokens.is_empty() {
            let root = self.repo.clone();
            self.visit_cached_dir(&root, 0);
            return;
        }
        let Some(all_entries) = self.filter_files_cache.as_ref() else {
            return;
        };
        if self.filter_normalized.len() != all_entries.len() {
            self.filter_normalized = all_entries
                .iter()
                .map(|e| e.relative_path.to_ascii_lowercase())
                .collect();
        }
        let mut matched_dirs = BTreeSet::new();
        let mut matched_entries = std::collections::HashSet::new();
        for (entry, lower_rel) in all_entries.iter().zip(&self.filter_normalized) {
            if !self.show_dotfiles && is_dotfile_rel(&entry.relative_path) {
                continue;
            }
            let ignored = is_rel_ignored(&self.ignored, &entry.relative_path);
            if !self.show_ignored && ignored {
                continue;
            }
            if tokens.iter().all(|t| lower_rel.contains(t)) {
                let mut cur = entry.path.parent();
                while let Some(parent) = cur {
                    if parent == self.repo || !parent.starts_with(&self.repo) {
                        break;
                    }
                    matched_dirs.insert(parent.to_path_buf());
                    cur = parent.parent();
                }
                matched_entries.insert(entry.path.as_path());
            }
        }
        let mut tree: BTreeMap<PathBuf, Vec<DirEntry>> = BTreeMap::new();
        for entry in all_entries {
            if ((entry.is_dir && matched_dirs.contains(&entry.path))
                || matched_entries.contains(entry.path.as_path()))
                && let Some(parent) = entry.path.parent()
            {
                tree.entry(parent.to_path_buf())
                    .or_default()
                    .push(entry.clone());
            }
        }
        for list in tree.values_mut() {
            sort_entries(list);
        }
        tracing::trace!(
            entries = all_entries.len(),
            matched = matched_entries.len(),
            parents = matched_dirs.len(),
            "file filter projection"
        );
        let root = self.repo.clone();
        self.visit_filtered_dir(&tree, &root, 0);
    }

    fn visit_cached_dir(&mut self, dir: &Path, depth: usize) {
        let Some(children) = self.dir_cache.get(dir).cloned() else {
            return;
        };
        for entry in children {
            if !self.show_dotfiles && is_dotfile_rel(&entry.relative_path) {
                continue;
            }
            let ignored = is_rel_ignored(&self.ignored, &entry.relative_path);
            if !self.show_ignored && ignored {
                continue;
            }
            let expanded = entry.is_dir && self.expanded.contains(&entry.path);
            self.rows.push(TreeRow {
                name: entry.name.clone(),
                path: entry.path.clone(),
                relative_path: entry.relative_path.clone(),
                is_dir: entry.is_dir,
                depth,
                expanded,
                ignored,
            });
            if expanded {
                self.visit_cached_dir(&entry.path, depth.saturating_add(1));
            }
        }
    }

    fn visit_filtered_dir(
        &mut self,
        tree: &BTreeMap<PathBuf, Vec<DirEntry>>,
        dir: &Path,
        depth: usize,
    ) {
        let Some(children) = tree.get(dir) else {
            return;
        };
        for entry in children {
            let expanded = entry.is_dir && !self.filter_collapsed.contains(&entry.path);
            let ignored = is_rel_ignored(&self.ignored, &entry.relative_path);
            self.rows.push(TreeRow {
                name: entry.name.clone(),
                path: entry.path.clone(),
                relative_path: entry.relative_path.clone(),
                is_dir: entry.is_dir,
                depth,
                expanded,
                ignored,
            });
            if expanded {
                self.visit_filtered_dir(tree, &entry.path, depth.saturating_add(1));
            }
        }
    }
}

fn ensure_worktree_create_path(
    repo: &Path,
    parent_dir: &Path,
    rel: &Path,
) -> io::Result<(PathBuf, PathBuf)> {
    let denied = || {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "target directory is outside worktree",
        )
    };
    if !parent_dir.starts_with(repo)
        || parent_dir
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(denied());
    }
    let parent_rel = parent_dir.strip_prefix(repo).map_err(|_| denied())?;
    let canon_repo = repo.canonicalize().map_err(|_| denied())?;
    let full_path = parent_dir.join(rel);
    let mut cur = repo.to_path_buf();
    for comp in parent_rel.components().chain(rel.components()) {
        let Component::Normal(part) = comp else {
            return Err(denied());
        };
        if part == OsStr::new(".git") {
            return Err(denied());
        }
        cur.push(part);
        match fs::symlink_metadata(&cur) {
            Ok(_) => {
                let canon_cur = cur.canonicalize().map_err(|_| denied())?;
                if !canon_cur.starts_with(&canon_repo) {
                    return Err(denied());
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok((canon_repo, full_path))
}

fn validate_relative_input(raw: &str) -> io::Result<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "name must not be empty",
        ));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "name must be relative",
        ));
    }
    let mut clean = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(part) if part != OsStr::new(".git") => clean.push(part),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid path segment",
                ));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "name must not be empty",
        ));
    }
    Ok(clean)
}

use std::ffi::OsStr;

fn is_dotfile_rel(relative_path: &str) -> bool {
    relative_path
        .split('/')
        .any(|seg| seg.len() > 1 && seg != ".." && seg.starts_with('.'))
}

fn is_rel_ignored(ignored: &BTreeSet<String>, relative_path: &str) -> bool {
    if ignored.contains(relative_path) {
        return true;
    }
    let mut prefix = String::new();
    for seg in relative_path.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(seg);
        if ignored.contains(&prefix) {
            return true;
        }
    }
    false
}

fn sort_entries(entries: &mut [DirEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            })
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn read_dir_entries(repo: &Path, dir: &Path) -> Vec<DirEntry> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let path = entry.path();
        let (is_dir, is_symlink) = entry.file_type().map_or((false, false), |ft| {
            let is_symlink = ft.is_symlink();
            (ft.is_dir() || (is_symlink && path.is_dir()), is_symlink)
        });
        let relative_path = path
            .strip_prefix(repo)
            .ok()
            .map_or_else(|| name.clone(), |p| p.to_string_lossy().replace('\\', "/"));
        out.push(DirEntry {
            name,
            path,
            relative_path,
            is_dir,
            is_symlink,
        });
    }
    sort_entries(&mut out);
    out
}

fn scan_worktree_entries(repo: &Path, limit: usize) -> Vec<DirEntry> {
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::new();
    let mut visited = BTreeSet::new();
    if let Ok(canon_root) = repo.canonicalize() {
        visited.insert(canon_root);
    }
    queue.push_back(repo.to_path_buf());
    while let Some(dir) = queue.pop_front() {
        if out.len() >= limit {
            break;
        }
        for entry in read_dir_entries(repo, &dir) {
            if out.len() >= limit {
                break;
            }
            let recurse = entry.is_dir
                && !entry.is_symlink
                && entry.path.file_name() != Some(OsStr::new("node_modules"))
                && entry
                    .path
                    .canonicalize()
                    .is_ok_and(|canon| visited.insert(canon));
            let path = entry.path.clone();
            out.push(entry);
            if recurse {
                queue.push_back(path);
            }
        }
    }
    out
}

fn query_git_ignored(repo: &Path, relative_paths: &[String]) -> Vec<String> {
    super::search::query_git_ignored_set_with_limits(
        repo,
        relative_paths,
        Duration::from_secs(5),
        4 * 1024 * 1024,
    )
    .into_iter()
    .collect()
}

impl Panel {
    /// `Files` 탭으로 전환하고 파일 필터 입력창을 준비한다.
    pub fn show_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_tab(super::PanelTab::Files, cx);
        self.ensure_explorer_loaded_async(cx);
        let input = self.ensure_explorer_filter_input(window, cx);
        input.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    /// 루트 폴더 지연 로드를 백그라운드에서 수행한다(UI 스레드 블로킹 방지, OWE-849).
    ///
    /// 디스크 읽기(`read_dir_entries`)와 `git check-ignore` 자식 프로세스를 백그라운드
    /// 스레드로 옮기고, 완료 세대가 최신일 때만 캐시·행에 반영한다.
    pub fn ensure_explorer_loaded_async(&mut self, cx: &mut Context<Self>) {
        if self.explorer.loaded_root || self.explorer.loading {
            return;
        }
        self.explorer.loaded_root = true;
        self.explorer.loading = true;
        self.explorer.load_generation = self.explorer.load_generation.wrapping_add(1);
        let generation = self.explorer.load_generation;
        let repo = self.explorer.repo.clone();
        self.explorer.load_task = Some(cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let entries = read_dir_entries(&repo, &repo);
                    let rels: Vec<String> =
                        entries.iter().map(|e| e.relative_path.clone()).collect();
                    let ignored = query_git_ignored(&repo, &rels);
                    (repo, entries, ignored)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.explorer.load_generation != generation {
                    return;
                }
                this.explorer.load_task = None;
                this.explorer.loading = false;
                let (root, entries, ignored) = loaded;
                for rel in ignored {
                    this.explorer.ignored.insert(rel);
                }
                this.explorer.dir_cache.insert(root, entries);
                this.explorer.rebuild_rows();
                if !this.explorer.expanded.is_empty() {
                    this.refresh_explorer_async(cx);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// 새로고침 버튼용: 루트와 펼쳐진 폴더를 백그라운드에서 다시 읽는다(OWE-849).
    pub fn refresh_explorer_async(&mut self, cx: &mut Context<Self>) {
        if self.explorer.loading {
            return;
        }
        self.explorer.loaded_root = true;
        self.explorer.loading = true;
        self.explorer.load_generation = self.explorer.load_generation.wrapping_add(1);
        let generation = self.explorer.load_generation;
        let repo = self.explorer.repo.clone();
        let dirs: Vec<PathBuf> = self.explorer.expanded.iter().cloned().collect();
        let requested: BTreeSet<PathBuf> = dirs.iter().cloned().collect();
        self.explorer.load_task = Some(cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let mut out = Vec::new();
                    let mut all_ignored = Vec::new();
                    let mut targets = vec![repo.clone()];
                    targets.extend(dirs.into_iter().filter(|dir| dir.is_dir()));
                    for dir in targets {
                        let entries = read_dir_entries(&repo, &dir);
                        let rels: Vec<String> =
                            entries.iter().map(|e| e.relative_path.clone()).collect();
                        all_ignored.extend(query_git_ignored(&repo, &rels));
                        out.push((dir, entries));
                    }
                    (out, all_ignored)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.explorer.load_generation != generation {
                    return;
                }
                this.explorer.load_task = None;
                this.explorer.loading = false;
                let (dirs_loaded, ignored) = loaded;
                this.explorer.ignored.clear();
                for rel in ignored {
                    this.explorer.ignored.insert(rel);
                }
                let mut live: BTreeSet<PathBuf> = BTreeSet::new();
                for (dir, entries) in dirs_loaded {
                    live.insert(dir.clone());
                    this.explorer.dir_cache.insert(dir, entries);
                }
                let stale: Vec<PathBuf> = this
                    .explorer
                    .expanded
                    .iter()
                    .filter(|dir| requested.contains(*dir) && !live.contains(*dir))
                    .cloned()
                    .collect();
                for dir in stale {
                    this.explorer.expanded.remove(&dir);
                    this.explorer.dir_cache.remove(&dir);
                }
                // 필터 캐시는 무효화하고, 필터가 켜져 있으면 디바운스 경로로 다시 스캔한다.
                this.explorer.filter_files_cache = None;
                this.explorer.filter_normalized.clear();
                let query = this.explorer.filter_query.clone();
                if query.trim().is_empty() {
                    this.explorer.rebuild_rows();
                    cx.notify();
                } else {
                    this.schedule_explorer_filter(query, cx);
                }
                // A user may expand another directory during this scan. Do not
                // collapse it or leave it without data; coalesce into one follow-up.
                if this
                    .explorer
                    .expanded
                    .iter()
                    .any(|dir| !this.explorer.dir_cache.contains_key(dir))
                {
                    this.refresh_explorer_async(cx);
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// 파일명 필터 입력 디바운스 UI 경로(OWE-849): 키 입력마다 동기 스캔하지 않고,
    /// 스캔 캐시가 없으면 150ms 조용해진 뒤 백그라운드에서 전체 스캔한다.
    pub fn schedule_explorer_filter(&mut self, query: String, cx: &mut Context<Self>) {
        self.schedule_explorer_filter_inner(query, true, cx);
    }

    /// 필터 `Enter` 확정용(OWE-849): 디바운스 대기 없이 즉시 백그라운드에서 스캔한다.
    /// UI 스레드에서 동기 전체 스캔·`git check-ignore`를 실행하지 않는다.
    pub fn apply_explorer_filter_now(&mut self, query: String, cx: &mut Context<Self>) {
        self.schedule_explorer_filter_inner(query, false, cx);
    }

    fn schedule_explorer_filter_inner(
        &mut self,
        query: String,
        debounce: bool,
        cx: &mut Context<Self>,
    ) {
        let empty = query.trim().is_empty();
        if empty {
            self.explorer.filter_collapsed.clear();
        }
        self.explorer.filter_query = query;
        self.explorer.filter_generation = self.explorer.filter_generation.wrapping_add(1);
        let generation = self.explorer.filter_generation;
        // 빈 쿼리나 이미 스캔된 캐시가 있으면 동기 재구성으로 충분하다.
        if empty || self.explorer.filter_files_cache.is_some() {
            self.explorer.filter_task = None;
            self.explorer.rebuild_rows();
            cx.notify();
            return;
        }
        let repo = self.explorer.repo.clone();
        self.explorer.filter_task = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor()
                    .timer(EXPLORER_FILTER_DEBOUNCE)
                    .await;
            }
            // 대기 중 새 입력이 들어왔으면 스캔·`git check-ignore`를 스폰하지 않는다.
            let current = this
                .update(cx, |this, _| this.explorer.filter_generation)
                .ok();
            if current != Some(generation) {
                return;
            }
            let scanned = cx
                .background_executor()
                .spawn(async move {
                    let entries = scan_worktree_entries(&repo, MAX_FILTER_ENTRIES);
                    let rels: Vec<String> =
                        entries.iter().map(|e| e.relative_path.clone()).collect();
                    let ignored = query_git_ignored(&repo, &rels);
                    (entries, ignored)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.explorer.filter_generation != generation {
                    return;
                }
                this.explorer.filter_task = None;
                let (entries, ignored) = scanned;
                for rel in ignored {
                    this.explorer.ignored.insert(rel);
                }
                this.explorer.filter_files_cache = Some(entries);
                this.explorer.rebuild_rows();
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// 진행 중인 인라인 이름 입력을 취소한다(`Esc`, OWE-849).
    pub(crate) fn cancel_explorer_edit(&mut self, cx: &mut Context<Self>) {
        if self.explorer.editing.take().is_some() {
            cx.notify();
        }
    }

    /// 파일 탐색기 가시 행 목록.
    pub fn explorer_rows(&self) -> &[TreeRow] {
        &self.explorer.rows
    }

    /// 파일 탐색기에서 폴더 펼침/접기를 토글한다.
    pub fn toggle_explorer_dir(&mut self, dir: &Path, cx: &mut Context<Self>) {
        self.ensure_explorer_loaded_async(cx);
        self.explorer.selected = Some(dir.to_path_buf());
        if !self.explorer.filter_query.trim().is_empty() {
            if !self.explorer.filter_collapsed.remove(dir) {
                self.explorer.filter_collapsed.insert(dir.to_path_buf());
            }
            self.explorer.rebuild_rows();
        } else if self.explorer.expanded.remove(dir) {
            self.explorer.rebuild_rows();
        } else {
            self.explorer.expanded.insert(dir.to_path_buf());
            if self.explorer.dir_cache.contains_key(dir) {
                self.explorer.rebuild_rows();
            } else {
                self.refresh_explorer_async(cx);
            }
        }
        cx.notify();
    }

    /// 파일 탐색기 파일명 필터를 설정한다.
    pub fn set_explorer_filter(&mut self, query: impl Into<String>, cx: &mut Context<Self>) {
        self.ensure_explorer_loaded_async(cx);
        self.apply_explorer_filter_now(query.into(), cx);
    }

    /// 파일 탐색기 숨김 파일(`.dotfiles`) 표시를 토글한다.
    pub fn toggle_explorer_dotfiles(&mut self, cx: &mut Context<Self>) {
        self.ensure_explorer_loaded_async(cx);
        self.explorer.show_dotfiles = !self.explorer.show_dotfiles;
        self.explorer.rebuild_rows();
        cx.notify();
    }

    /// 파일 탐색기 `.gitignore` 파일 표시를 토글한다.
    pub fn toggle_explorer_ignored(&mut self, cx: &mut Context<Self>) {
        self.ensure_explorer_loaded_async(cx);
        self.explorer.show_ignored = !self.explorer.show_ignored;
        self.explorer.rebuild_rows();
        cx.notify();
    }

    fn queue_explorer_operation(&mut self, operation: Operation, cx: &mut Context<Self>) {
        self.explorer.operations.push_back(operation);
        if self.explorer.operation_task.is_some() {
            return;
        }
        let generation = self.explorer.operation_generation;
        self.explorer.operation_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some((state, operation))) = this.update(cx, |s, _| {
                    let operation = s.explorer.operations.pop_front();
                    if operation.is_none() {
                        s.explorer.operation_task = None;
                    }
                    operation.map(|op| (DiskState::capture(&s.explorer), op))
                }) else {
                    return;
                };
                let original_selection = state.selected.clone();
                let original_expanded = state.expanded.clone();
                let (state, result, open) = cx
                    .background_executor()
                    .spawn(async move { state.run(operation) })
                    .await;
                let applied = this.update(cx, |s, cx| {
                    if s.explorer.operation_generation != generation {
                        return false;
                    }
                    match result {
                        Ok(path) => {
                            // 변경 전 읽기 작업이 뒤늦게 캐시를 덮어쓰지 않게 한다.
                            s.explorer.load_generation = s.explorer.load_generation.wrapping_add(1);
                            s.explorer.load_task = None;
                            s.explorer.loading = false;
                            s.explorer.loaded_root = true;
                            s.explorer.dir_cache = state.dir_cache;
                            s.explorer.ignored = state.ignored;
                            // Apply the operation's expansion delta without
                            // replacing unrelated user actions during its IO.
                            for dir in original_expanded.symmetric_difference(&state.expanded) {
                                if s.explorer.expanded.contains(dir)
                                    == original_expanded.contains(dir)
                                {
                                    if state.expanded.contains(dir) {
                                        s.explorer.expanded.insert(dir.clone());
                                    } else {
                                        s.explorer.expanded.remove(dir);
                                    }
                                }
                            }
                            if s.explorer.selected == original_selection {
                                s.explorer.selected = state.selected;
                            }
                            s.explorer.error = None;
                            s.explorer.filter_files_cache = None;
                            s.explorer.filter_normalized.clear();
                            let query = s.explorer.filter_query.clone();
                            s.schedule_explorer_filter(query, cx);
                            if s.explorer
                                .expanded
                                .iter()
                                .any(|dir| !s.explorer.dir_cache.contains_key(dir))
                            {
                                s.refresh_explorer_async(cx);
                            }
                            if open {
                                cx.emit(OpenFile(path));
                            }
                        }
                        Err(error) => s.explorer.error = Some(error.to_string()),
                    }
                    cx.notify();
                    true
                });
                if !matches!(applied, Ok(true)) {
                    return;
                }
            }
        }));
        cx.notify();
    }

    /// 새 파일 생성을 예약하고 완료 시 중앙 에디터로 열기 이벤트를 내보낸다.
    pub fn explorer_create_file(&mut self, parent_dir: &Path, name: &str, cx: &mut Context<Self>) {
        self.queue_explorer_operation(
            Operation::CreateFile {
                parent: parent_dir.to_path_buf(),
                name: name.to_owned(),
            },
            cx,
        );
    }

    /// 파일 탐색기에서 새 폴더를 만든다.
    pub fn explorer_create_folder(
        &mut self,
        parent_dir: &Path,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        self.queue_explorer_operation(
            Operation::CreateFolder {
                parent: parent_dir.to_path_buf(),
                name: name.to_owned(),
            },
            cx,
        );
    }

    /// 파일 탐색기에서 파일이나 폴더 이름을 바꾼다.
    pub fn explorer_rename(&mut self, target: &Path, new_name: &str, cx: &mut Context<Self>) {
        self.queue_explorer_operation(
            Operation::Rename {
                target: target.to_path_buf(),
                name: new_name.to_owned(),
            },
            cx,
        );
    }

    /// 파일 탐색기의 휴지통(`~/.Trash`) 기준 홈 디렉터리를 설정한다.
    pub fn set_explorer_home(&mut self, home: PathBuf) {
        self.explorer.set_home(home);
    }

    /// 파일 탐색기에서 파일이나 폴더를 휴지통(`~/.Trash`)으로 옮긴다.
    pub fn explorer_delete(&mut self, target: &Path, cx: &mut Context<Self>) {
        self.queue_explorer_operation(
            Operation::Delete {
                target: target.to_path_buf(),
            },
            cx,
        );
    }

    /// 파일 탐색기에서 휴지통 이동 확인 대화상자를 열고 취소 버튼에 기본 포커스를 준다.
    pub fn request_explorer_delete(
        &mut self,
        target: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_explorer_loaded_async(cx);
        if !target.starts_with(&self.explorer.repo) || target == self.explorer.repo {
            self.explorer.error = Some("cannot delete worktree root or outside path".to_owned());
            cx.notify();
            return;
        }
        let is_dir = self.explorer.path_is_dir(target);
        let name = target
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let cancel_focus = cx.focus_handle();
        window.focus(&cancel_focus, cx);
        self.explorer.pending_delete = Some(PendingDelete {
            path: target.to_path_buf(),
            name,
            is_dir,
            cancel_focus,
        });
        self.explorer.error = None;
        cx.notify();
    }

    /// 열려 있는 휴지통 이동 확인 대화상자를 취소하고 닫는다.
    pub fn cancel_explorer_delete(&mut self, cx: &mut Context<Self>) {
        self.explorer.pending_delete = None;
        cx.notify();
    }

    /// 열려 있는 휴지통 이동 확인 대화상자를 확정해 `~/.Trash`로 이동한다.
    pub fn confirm_explorer_delete(&mut self, cx: &mut Context<Self>) {
        if let Some(pending) = self.explorer.pending_delete.take() {
            self.explorer_delete(&pending.path, cx);
        }
    }

    /// 현재 열려 있는 휴지통 이동 확인 대화상자 상태.
    pub fn pending_explorer_delete(&self) -> Option<&PendingDelete> {
        self.explorer.pending_delete.as_ref()
    }

    fn ensure_explorer_filter_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(input) = &self.explorer.filter_input {
            return input.clone();
        }
        let initial = self.explorer.filter_query.clone();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(text::EXPLORER_FILTER.get())
                .default_value(initial)
                .clean_on_escape()
        });
        let sub = cx.subscribe(&input, |this, state, event: &InputEvent, cx| {
            match event {
                InputEvent::Change => {
                    let val = state.read(cx).value().to_string();
                    this.schedule_explorer_filter(val, cx);
                }
                InputEvent::PressEnter { .. } => {
                    // Enter는 디바운스 없이 즉시 백그라운드 스캔으로 반영한다.
                    let val = state.read(cx).value().to_string();
                    this.apply_explorer_filter_now(val, cx);
                }
                _ => {}
            }
        });
        self.explorer.filter_input = Some(input.clone());
        self.explorer.filter_sub = Some(sub);
        input
    }

    pub(crate) fn start_explorer_edit(
        &mut self,
        kind: EditKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let default_val = match &kind {
            EditKind::NewFile { .. } | EditKind::NewFolder { .. } => String::new(),
            EditKind::Rename { target } => target
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        };
        let placeholder = match &kind {
            EditKind::NewFile { .. } => text::EXPLORER_NEW_FILE.get(),
            EditKind::NewFolder { .. } => text::EXPLORER_NEW_FOLDER.get(),
            EditKind::Rename { .. } => text::EXPLORER_RENAME.get(),
        };
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(default_val)
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit_explorer_edit(cx);
            }
        });
        self.explorer.editing = Some((kind, input, Some(sub)));
        self.explorer.error = None;
        cx.notify();
    }

    pub(crate) fn submit_explorer_edit(&mut self, cx: &mut Context<Self>) {
        let Some((kind, input, _)) = self.explorer.editing.take() else {
            return;
        };
        let value = input.read(cx).value().to_string();
        if value.trim().is_empty() {
            cx.notify();
            return;
        }
        match kind {
            EditKind::NewFile { parent } => {
                self.explorer_create_file(&parent, &value, cx);
            }
            EditKind::NewFolder { parent } => {
                self.explorer_create_folder(&parent, &value, cx);
            }
            EditKind::Rename { target } => self.explorer_rename(&target, &value, cx),
        }
        cx.notify();
    }

    pub(crate) fn render_explorer_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.ensure_explorer_loaded_async(cx);
        let filter_input = self.ensure_explorer_filter_input(window, cx);
        let p = ui::palette(cx);
        let show_dotfiles = self.explorer.show_dotfiles;
        let show_ignored = self.explorer.show_ignored;
        let count = self.explorer.rows.len();
        let has_filter = !self.explorer.filter_query.trim().is_empty();
        let root_repo = self.explorer.repo.clone();
        let weak_bg = cx.weak_entity();

        let toolbar = div()
            .id("explorer-toolbar")
            .debug_selector(|| "explorer-toolbar".into())
            .flex_none()
            .px_2()
            .py_1()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(p.sidebar_border)
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&filter_input)
                        .xsmall()
                        .prefix(Icon::new(IconName::Search).xsmall())
                        .cleanable(true),
                ),
            )
            .child(
                Button::new("explorer-new-file")
                    .debug_selector(|| "explorer-new-file".into())
                    .icon(IconName::File)
                    .ghost()
                    .xsmall()
                    .tooltip(text::EXPLORER_NEW_FILE.get())
                    .accessibility_label(text::EXPLORER_NEW_FILE.get())
                    .on_click(cx.listener(|this, _, window, cx| {
                        let parent = this.explorer.target_parent_dir();
                        this.start_explorer_edit(EditKind::NewFile { parent }, window, cx);
                    })),
            )
            .child(
                Button::new("explorer-new-folder")
                    .debug_selector(|| "explorer-new-folder".into())
                    .icon(Icon::default().data(ui::icons::FOLDER_PLUS_SVG))
                    .ghost()
                    .xsmall()
                    .tooltip(text::EXPLORER_NEW_FOLDER.get())
                    .accessibility_label(text::EXPLORER_NEW_FOLDER.get())
                    .on_click(cx.listener(|this, _, window, cx| {
                        let parent = this.explorer.target_parent_dir();
                        this.start_explorer_edit(EditKind::NewFolder { parent }, window, cx);
                    })),
            )
            .child(
                Button::new("explorer-toggle-dotfiles")
                    .debug_selector(|| "explorer-toggle-dotfiles".into())
                    .label(".*")
                    .ghost()
                    .xsmall()
                    .selected(show_dotfiles)
                    .tooltip(text::EXPLORER_DOTFILES.get())
                    .accessibility_label(text::EXPLORER_DOTFILES.get())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_explorer_dotfiles(cx);
                    })),
            )
            .child(
                Button::new("explorer-toggle-ignored")
                    .debug_selector(|| "explorer-toggle-ignored".into())
                    .label("git")
                    .ghost()
                    .xsmall()
                    .selected(show_ignored)
                    .tooltip(text::EXPLORER_IGNORED.get())
                    .accessibility_label(text::EXPLORER_IGNORED.get())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_explorer_ignored(cx);
                    })),
            )
            .child(
                Button::new("explorer-collapse-all")
                    .debug_selector(|| "explorer-collapse-all".into())
                    .icon(IconName::ChevronRight)
                    .ghost()
                    .xsmall()
                    .tooltip(text::EXPLORER_COLLAPSE_ALL.get())
                    .accessibility_label(text::EXPLORER_COLLAPSE_ALL.get())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.explorer.collapse_all();
                        cx.notify();
                    })),
            );

        let error_bar = self.explorer.error.as_ref().map(|msg| {
            div()
                .flex_none()
                .p_2()
                .child(ui::error_banner(p, msg.clone()))
        });

        // 인라인 편집 행: 이름 변경이면 해당 행을 대체하고, 새 파일/폴더면 부모 다음에 삽입한다.
        let edit_slot = self.explorer.inline_edit_slot();
        let edit_inserts = edit_slot.is_some_and(|(_, _, replaces)| !replaces);
        let total_rows = count.saturating_add(usize::from(edit_inserts));
        let loading = self.explorer.loading;
        let filter_pending = self.explorer.filter_task.is_some();
        let tree_focus = self
            .explorer
            .tree_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();

        let body = if total_rows == 0 {
            let empty_msg = if has_filter {
                text::EXPLORER_FILTER_EMPTY.get()
            } else {
                text::EXPLORER_EMPTY.get()
            };
            if loading || (has_filter && filter_pending) {
                Self::render_empty_state(
                    "explorer-loading",
                    IconName::Folder,
                    text::EXPLORER_LOADING.get(),
                    "",
                    cx,
                )
                .into_any_element()
            } else {
                Self::render_empty_state("explorer-empty", IconName::Folder, empty_msg, "", cx)
                    .into_any_element()
            }
        } else {
            let scroll = self.explorer.scroll.clone();
            uniform_list(
                "explorer-files",
                total_rows,
                cx.processor(Self::render_explorer_rows),
            )
            .track_scroll(&scroll)
            .size_full()
            .into_any_element()
        };

        let confirm_dialog = self.render_delete_confirm_dialog(window, cx);

        div()
            .id("explorer-pane")
            .debug_selector(|| "explorer-pane".into())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(toolbar)
            .children(error_bar)
            .child(
                div()
                    .id("explorer-tree-container")
                    .track_focus(&tree_focus)
                    .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                        this.on_explorer_tree_key(ev.keystroke.key.as_str(), cx);
                    }))
                    .flex_1()
                    .min_h_0()
                    .context_menu(move |menu, _, _| {
                        background_context_menu(menu, &weak_bg, &root_repo)
                    })
                    .child(body),
            )
            .children(confirm_dialog)
    }

    /// 트리 화살표 키 탐색(OWE-849): `↑/↓` 이동, `→` 펼치기, `←` 접기/부모로, `Enter` 고정 열기.
    fn on_explorer_tree_key(&mut self, key: &str, cx: &mut Context<Self>) {
        match key {
            "up" => {
                cx.stop_propagation();
                self.explorer.move_selection(-1);
                cx.notify();
            }
            "down" => {
                cx.stop_propagation();
                self.explorer.move_selection(1);
                cx.notify();
            }
            "right" => {
                cx.stop_propagation();
                if let Some(selected) = self.explorer.selected.clone()
                    && self.explorer.path_is_dir(&selected)
                    && !self.explorer.expanded.contains(&selected)
                {
                    self.toggle_explorer_dir(&selected, cx);
                }
            }
            "left" => {
                cx.stop_propagation();
                if let Some(selected) = self.explorer.selected.clone() {
                    if self.explorer.path_is_dir(&selected)
                        && self.explorer.expanded.contains(&selected)
                    {
                        self.toggle_explorer_dir(&selected, cx);
                    } else {
                        self.explorer.select_parent();
                        cx.notify();
                    }
                }
            }
            "enter" => {
                cx.stop_propagation();
                if let Some(selected) = self.explorer.selected.clone() {
                    if self.explorer.path_is_dir(&selected) {
                        self.toggle_explorer_dir(&selected, cx);
                    } else {
                        self.open_file_preview = false;
                        cx.emit(OpenFile(selected));
                    }
                }
            }
            "escape" if self.explorer.editing.is_some() => {
                cx.stop_propagation();
                self.cancel_explorer_edit(cx);
            }
            _ => {}
        }
    }

    fn render_delete_confirm_dialog(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        let pending = self.explorer.pending_delete.as_ref()?;
        let p = ui::palette(cx);
        let title = if pending.is_dir {
            text::EXPLORER_DELETE_FOLDER_TITLE.format(&[("name", &pending.name)])
        } else {
            text::EXPLORER_DELETE_FILE_TITLE.format(&[("name", &pending.name)])
        };
        let desc = if pending.is_dir {
            text::EXPLORER_DELETE_FOLDER_DESC.get()
        } else {
            text::EXPLORER_DELETE_FILE_DESC.get()
        };
        let cancel_focus = pending.cancel_focus.clone();
        let card = div()
            .id("explorer-delete-dialog")
            .debug_selector(|| "explorer-delete-dialog".into())
            .occlude()
            .role(gpui::Role::AlertDialog)
            .w(ui::DIALOG_WIDTH)
            .max_w(window.viewport_size().width - ui::SPACE_4 * 2.)
            .p_6()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_MD)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.cancel_explorer_delete(cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .id("explorer-delete-dialog-title")
                            .debug_selector(|| "explorer-delete-dialog-title".into())
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .id("explorer-delete-dialog-desc")
                            .debug_selector(|| "explorer-delete-dialog-desc".into())
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(desc),
                    ),
            )
            .child(
                ui::controls()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .id("explorer-delete-cancel-focus")
                            .track_focus(&cancel_focus)
                            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                                let k = ev.keystroke.key.as_str();
                                if k == "escape" || k == "enter" || k == " " || k == "space" {
                                    cx.stop_propagation();
                                    this.cancel_explorer_delete(cx);
                                }
                            }))
                            .child(
                                Button::new("explorer-delete-cancel")
                                    .debug_selector(|| "explorer-delete-cancel".into())
                                    .ghost()
                                    .label(text::EXPLORER_CANCEL.get())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cancel_explorer_delete(cx);
                                    })),
                            ),
                    )
                    .child(
                        Button::new("explorer-delete-confirm")
                            .debug_selector(|| "explorer-delete-confirm".into())
                            .danger()
                            .label(text::EXPLORER_DELETE.get())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_explorer_delete(cx);
                            })),
                    ),
            );
        Some(ui::modal_backdrop(window.viewport_size(), card))
    }

    fn render_explorer_rows(
        &mut self,
        range: std::ops::Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        #[cfg(test)]
        {
            self.explorer.rendered_rows += range.len();
        }
        let p = ui::palette(cx);
        let hover_bg = p.sidebar_accent;
        let fg = p.sidebar_foreground;
        let muted = p.muted_foreground;
        let tree_focus = self
            .explorer
            .tree_focus
            .clone()
            .unwrap_or_else(|| cx.focus_handle());
        // 인라인 편집 행의 가상 위치(대체/삽입)와 입력 엔티티를 미리 꺼낸다.
        let edit_slot = self.explorer.inline_edit_slot();
        let edit_input = self
            .explorer
            .editing
            .as_ref()
            .map(|(_, input, _)| input.clone());
        let edit_is_dir = self
            .explorer
            .editing
            .as_ref()
            .is_some_and(|(kind, _, _)| match kind {
                EditKind::Rename { target } => self.explorer.path_is_dir(target),
                EditKind::NewFile { .. } => false,
                EditKind::NewFolder { .. } => true,
            });

        range
            .filter_map(|ix| {
                // 인라인 편집 위치면 이름 입력창을 행으로 그린다(OWE-849: 상단 바 대신 트리 인라인).
                if let Some((slot, depth, _)) = edit_slot
                    && ix == slot
                    && let Some(input) = edit_input.clone()
                {
                    return Some(Self::render_inline_edit_row(depth, edit_is_dir, &input, cx));
                }
                let data_ix = match edit_slot {
                    Some((slot, _, false)) if ix > slot => ix.saturating_sub(1),
                    _ => ix,
                };
                let row = self.explorer.rows.get(data_ix)?.clone();
                let is_selected = self.explorer.selected.as_ref() == Some(&row.path);
                let selected_bg = p.sidebar_accent;
                let indent_px =
                    f32::from(u16::try_from(row.depth).unwrap_or(16).min(16)) * 14.0 + 8.0;
                let row_path = row.path.clone();
                let is_dir = row.is_dir;
                let weak = cx.weak_entity();
                let menu_row = row.clone();
                let repo = self.explorer.repo.clone();

                let chevron = if row.is_dir {
                    Icon::new(if row.expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .xsmall()
                    .text_color(muted)
                    .into_any_element()
                } else {
                    div().w(px(12.)).flex_none().into_any_element()
                };

                let icon = Icon::new(if row.is_dir {
                    if row.expanded {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    }
                } else {
                    IconName::File
                })
                .xsmall()
                .text_color(if row.is_dir { fg } else { muted });

                let selector = format!("explorer-row-{ix}");
                let path_selector = format!("explorer-item-{}", row.relative_path);
                let row_fg = if row.ignored {
                    muted
                } else if is_selected {
                    p.sidebar_accent_foreground
                } else {
                    fg
                };
                let focus = tree_focus.clone();
                let row_path_click = row_path.clone();

                Some(
                    div()
                        .id(ElementId::Name(SharedString::from(selector.clone())))
                        .debug_selector(move || selector.clone())
                        .h(px(ROW_HEIGHT))
                        .w_full()
                        .pl(px(indent_px))
                        .pr_2()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .cursor_pointer()
                        .text_color(row_fg)
                        .when(is_selected, |d| d.bg(selected_bg))
                        .when(!is_selected, |d| {
                            d.hover(move |s| s.bg(hover_bg).text_color(p.sidebar_accent_foreground))
                        })
                        .child(chevron)
                        .child(icon)
                        .child(
                            div()
                                .id(ElementId::Name(SharedString::from(path_selector.clone())))
                                .debug_selector(move || path_selector.clone())
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(ui::FONT_MONO)
                                .text_size(ui::TEXT_SM)
                                .text_color(row_fg)
                                .when(is_selected, |d| d.font_weight(FontWeight::MEDIUM))
                                .child(row.name.clone()),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                window.focus(&focus, cx);
                                if is_dir {
                                    this.toggle_explorer_dir(&row_path, cx);
                                } else {
                                    this.explorer.selected = Some(row_path.clone());
                                    // 단일 클릭은 미리보기 탭으로 연다(OWE-849).
                                    this.open_file_preview = true;
                                    cx.emit(OpenFile(row_path.clone()));
                                    cx.notify();
                                }
                            }),
                        )
                        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                            // 더블클릭은 고정 탭으로 연다(OWE-849).
                            if e.click_count() >= 2 && !is_dir {
                                this.open_file_preview = false;
                                cx.emit(OpenFile(row_path_click.clone()));
                            }
                        }))
                        .context_menu(move |menu, _, _| {
                            row_context_menu(menu, &weak, &menu_row, &repo)
                        })
                        .into_any_element(),
                )
            })
            .collect()
    }

    /// 인라인 이름 입력 행(OWE-849): `Esc` 취소, `Enter` 확정(구독에서 처리).
    fn render_inline_edit_row(
        depth: usize,
        is_dir: bool,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let indent_px = f32::from(u16::try_from(depth).unwrap_or(16).min(16)) * 14.0 + 8.0;
        div()
            .id("explorer-edit-inline")
            .debug_selector(|| "explorer-edit-inline".into())
            .h(px(ROW_HEIGHT))
            .w_full()
            .pl(px(indent_px))
            .pr_2()
            .flex()
            .items_center()
            .gap_1p5()
            .child(div().w(px(12.)).flex_none())
            .child(
                Icon::new(if is_dir {
                    IconName::Folder
                } else {
                    IconName::File
                })
                .xsmall()
                .text_color(ui::palette(cx).foreground),
            )
            .child(div().flex_1().min_w_0().child(Input::new(input).xsmall()))
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key.as_str() == "escape" {
                    cx.stop_propagation();
                    this.cancel_explorer_edit(cx);
                }
            }))
            .into_any_element()
    }
}

fn background_context_menu(
    menu: PopupMenu,
    weak: &gpui::WeakEntity<Panel>,
    repo: &Path,
) -> PopupMenu {
    let (w1, r1) = (weak.clone(), repo.to_path_buf());
    let (w2, r2) = (weak.clone(), repo.to_path_buf());
    menu.item(
        PopupMenuItem::new(text::EXPLORER_NEW_FILE.get())
            .icon(IconName::File)
            .on_click(move |_, window, cx| {
                w1.update(cx, |this, cx| {
                    this.start_explorer_edit(EditKind::NewFile { parent: r1.clone() }, window, cx);
                })
                .ok();
            }),
    )
    .item(
        PopupMenuItem::new(text::EXPLORER_NEW_FOLDER.get())
            .icon(IconName::Folder)
            .on_click(move |_, window, cx| {
                w2.update(cx, |this, cx| {
                    this.start_explorer_edit(
                        EditKind::NewFolder { parent: r2.clone() },
                        window,
                        cx,
                    );
                })
                .ok();
            }),
    )
}

fn row_context_menu(
    mut menu: PopupMenu,
    weak: &gpui::WeakEntity<Panel>,
    row: &TreeRow,
    repo: &Path,
) -> PopupMenu {
    let target_dir = if row.is_dir {
        row.path.clone()
    } else {
        row.path
            .parent()
            .map_or_else(|| repo.to_path_buf(), Path::to_path_buf)
    };
    let (w_new_file, dir_file) = (weak.clone(), target_dir.clone());
    let (w_new_folder, dir_folder) = (weak.clone(), target_dir);
    menu = menu
        .item(
            PopupMenuItem::new(text::EXPLORER_NEW_FILE.get())
                .icon(IconName::File)
                .on_click(move |_, window, cx| {
                    w_new_file
                        .update(cx, |this, cx| {
                            this.start_explorer_edit(
                                EditKind::NewFile {
                                    parent: dir_file.clone(),
                                },
                                window,
                                cx,
                            );
                        })
                        .ok();
                }),
        )
        .item(
            PopupMenuItem::new(text::EXPLORER_NEW_FOLDER.get())
                .icon(IconName::Folder)
                .on_click(move |_, window, cx| {
                    w_new_folder
                        .update(cx, |this, cx| {
                            this.start_explorer_edit(
                                EditKind::NewFolder {
                                    parent: dir_folder.clone(),
                                },
                                window,
                                cx,
                            );
                        })
                        .ok();
                }),
        )
        .separator();

    if !row.is_dir {
        let (w_open, open_path) = (weak.clone(), row.path.clone());
        menu = menu.item(
            PopupMenuItem::new(text::OPEN_IN_EDITOR.get())
                .icon(IconName::FileText)
                .on_click(move |_, _, cx| {
                    w_open
                        .update(cx, |this, cx| {
                            // 컨텍스트 메뉴 열기는 고정 탭이다(OWE-849).
                            this.open_file_preview = false;
                            cx.emit(OpenFile(open_path.clone()));
                        })
                        .ok();
                }),
        );
    }

    let abs_str = row.path.to_string_lossy().into_owned();
    let rel_str = row.relative_path.clone();
    menu = menu
        .item(
            PopupMenuItem::new(text::EXPLORER_COPY_PATH.get())
                .icon(IconName::Copy)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(abs_str.clone()));
                }),
        )
        .item(
            PopupMenuItem::new(text::EXPLORER_COPY_REL_PATH.get())
                .icon(IconName::Copy)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(rel_str.clone()));
                }),
        )
        .separator();

    let reveal_path = row.path.clone();
    menu = menu.item(
        PopupMenuItem::new(
            text::EXPLORER_REVEAL_FINDER
                .mac_or(crate::i18n::catalog::other_os::REVEAL)
                .get(),
        )
        .icon(IconName::FolderOpen)
        .on_click(move |_, _, cx| {
            cx.reveal_path(&reveal_path);
        }),
    );

    for target in [Target::VsCode, Target::Cursor, Target::Zed] {
        let p = row.path.clone();
        menu = menu.item(
            PopupMenuItem::new(format!(
                "{}{}",
                target.label(),
                crate::i18n::catalog::sidebar::OPEN_IN.get()
            ))
            .icon(IconName::ExternalLink)
            .on_click(move |_, _, _| {
                target.open_path(&p).ok();
            }),
        );
    }

    let (w_rename, rename_target) = (weak.clone(), row.path.clone());
    let (w_delete, delete_target) = (weak.clone(), row.path.clone());
    menu.separator()
        .item(
            PopupMenuItem::new(text::EXPLORER_RENAME.get()).on_click(move |_, window, cx| {
                w_rename
                    .update(cx, |this, cx| {
                        this.start_explorer_edit(
                            EditKind::Rename {
                                target: rename_target.clone(),
                            },
                            window,
                            cx,
                        );
                    })
                    .ok();
            }),
        )
        .item(
            PopupMenuItem::new(text::EXPLORER_DELETE.get())
                .icon(Icon::default().data(ui::icons::TRASH_SVG))
                .on_click(move |_, window, cx| {
                    w_delete
                        .update(cx, |this, cx| {
                            this.request_explorer_delete(&delete_target, window, cx);
                        })
                        .ok();
                }),
        )
}

#[cfg(test)]
mod performance_tests {
    use super::*;

    #[test]
    fn broad_cached_filter_preserves_parents_hidden_ignored_and_order() {
        let root = PathBuf::from("/repo");
        let entry = |relative: &str, is_dir| DirEntry {
            name: relative.rsplit('/').next().unwrap().to_owned(),
            path: root.join(relative),
            relative_path: relative.to_owned(),
            is_dir,
            is_symlink: false,
        };
        let mut explorer = Explorer::new(root.clone());
        let mut entries = vec![
            entry("src", true),
            entry("src/.hidden-match", false),
            entry("src/ignored-match", false),
        ];
        entries.extend((0..2_000).map(|i| entry(&format!("src/MATCH-{i:04}"), false)));
        explorer.filter_files_cache = Some(entries);
        explorer.filter_query = "match".into();
        explorer.show_dotfiles = false;
        explorer.show_ignored = false;
        explorer.ignored.insert("src/ignored-match".into());
        explorer.rebuild_rows();
        assert_eq!(explorer.rows.len(), 2_001);
        assert_eq!(explorer.rows[0].relative_path, "src");
        assert_eq!(explorer.rows[1].relative_path, "src/MATCH-0000");
        assert_eq!(
            explorer.rows.last().unwrap().relative_path,
            "src/MATCH-1999"
        );
        explorer.filter_query = "MATCH 0010".into();
        explorer.rebuild_rows();
        assert_eq!(explorer.rows.len(), 2);
        assert_eq!(explorer.rows[1].relative_path, "src/MATCH-0010");
    }
}
