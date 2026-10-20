//! 부모-자식 워크트리 계보 트리(Worktree Lineage, MK-176 / OWE-730).
//!
//! 계보 정렬·접기/펼치기·사이클 방지·자손 일괄 작업 참고:
//! `.ref/orca/src/renderer/src/components/sidebar/worktree-lineage.ts`
//! (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use krill_proto::os::ffi::OsStrExt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, Window};
use krill_proto::control::{Program, RemoveWorktree, Reply};
use serde::{Deserialize, Serialize};

use super::{NewSession, Selected, Sidebar};
use crate::worktree;

/// 시각적 들여쓰기 최대 깊이(너무 깊은 중첩에서 카드 폭이 무너지지 않게 제한).
pub const MAX_LINEAGE_DEPTH: usize = 4;

/// 사이클 방어용 최대 조상 탐색 단계.
const MAX_CYCLE_STEPS: usize = 64;

/// `sidebar.bin`에 추가로 영속화되는 계보 상태.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct LineageState {
    pub parents: BTreeMap<PathBuf, PathBuf>,
    pub collapsed_lineage: BTreeSet<PathBuf>,
    #[serde(default)]
    pub sleeping_worktrees: BTreeSet<PathBuf>,
}

impl LineageState {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parents.is_empty()
            && self.collapsed_lineage.is_empty()
            && self.sleeping_worktrees.is_empty()
    }
}

/// `worktree`의 부모로 `candidate_parent`를 지정해도 자기 참조나 사이클이 생기지 않는지 검사한다.
#[must_use]
pub fn can_assign_parent(
    parents: &BTreeMap<PathBuf, PathBuf>,
    worktree: &Path,
    candidate_parent: &Path,
) -> bool {
    if worktree == candidate_parent {
        return false;
    }
    let mut visited = BTreeSet::new();
    visited.insert(worktree);
    let mut cur = Some(candidate_parent);
    let mut steps = 0usize;
    while let Some(node) = cur {
        if !visited.insert(node) {
            return false;
        }
        steps += 1;
        if steps > MAX_CYCLE_STEPS {
            return false;
        }
        cur = parents.get(node).map(PathBuf::as_path);
    }
    true
}

/// 현재 그룹(`group_set`) 안에서 유효한 부모 워크트리를 찾는다(사이클이나 그룹 밖 부모는 루트로 취급).
pub(super) fn effective_parent<'a>(
    path: &Path,
    parents: &'a BTreeMap<PathBuf, PathBuf>,
    group_set: &BTreeSet<&Path>,
) -> Option<&'a PathBuf> {
    let direct = parents.get(path)?;
    if direct.as_path() == path || !group_set.contains(direct.as_path()) {
        return None;
    }
    let mut visited = BTreeSet::new();
    visited.insert(path);
    let mut cur: Option<&Path> = Some(direct.as_path());
    let mut steps = 0usize;
    while let Some(node) = cur {
        if !visited.insert(node) {
            return None;
        }
        steps += 1;
        if steps > MAX_CYCLE_STEPS {
            return None;
        }
        cur = parents
            .get(node)
            .map(PathBuf::as_path)
            .filter(|next| group_set.contains(next));
    }
    Some(direct)
}

/// 이미 정렬된 그룹 내 워크트리 목록을 부모-자식 계보 DFS 순서로 재배치하고,
/// `collapsed_lineage`에 포함된 부모의 자손 워크트리는 결과에서 숨긴다.
pub(super) fn order_lineage_worktrees<'a>(
    sorted: Vec<&'a PathBuf>,
    parents: Option<&BTreeMap<PathBuf, PathBuf>>,
    collapsed_lineage: Option<&BTreeSet<PathBuf>>,
) -> Vec<&'a PathBuf> {
    let Some(parents) = parents.filter(|m| !m.is_empty()) else {
        return sorted;
    };
    let group_set: BTreeSet<&Path> = sorted.iter().map(|p| p.as_path()).collect();
    let mut roots: Vec<&'a PathBuf> = Vec::with_capacity(sorted.len());
    let mut children_by_parent: BTreeMap<&Path, Vec<&'a PathBuf>> = BTreeMap::new();

    for tree in &sorted {
        if let Some(parent) = effective_parent(tree.as_path(), parents, &group_set) {
            children_by_parent
                .entry(parent.as_path())
                .or_default()
                .push(*tree);
        } else {
            roots.push(*tree);
        }
    }

    let mut out = Vec::with_capacity(sorted.len());
    let mut visited: BTreeSet<&Path> = BTreeSet::new();

    #[expect(
        clippy::items_after_statements,
        reason = "local recursive DFS helper for lineage projection"
    )]
    fn visit<'a>(
        node: &'a PathBuf,
        children_by_parent: &BTreeMap<&Path, Vec<&'a PathBuf>>,
        collapsed_lineage: Option<&BTreeSet<PathBuf>>,
        visited: &mut BTreeSet<&'a Path>,
        out: &mut Vec<&'a PathBuf>,
    ) {
        if !visited.insert(node.as_path()) {
            return;
        }
        out.push(node);
        if collapsed_lineage.is_some_and(|c| c.contains(node.as_path())) {
            return;
        }
        if let Some(children) = children_by_parent.get(node.as_path()) {
            for &child in children {
                visit(child, children_by_parent, collapsed_lineage, visited, out);
            }
        }
    }

    for root in roots {
        visit(
            root,
            &children_by_parent,
            collapsed_lineage,
            &mut visited,
            &mut out,
        );
    }

    out
}

fn splice_lineage_in_map<K: Ord + Clone>(map: &mut BTreeMap<K, K>, target: &K) {
    let direct_children: Vec<K> = map
        .iter()
        .filter(|&(_, p)| p == target)
        .map(|(c, _)| c.clone())
        .collect();
    let parent_of_target = map.remove(target);
    for child in direct_children {
        if let Some(grandparent) = &parent_of_target
            && &child != grandparent
        {
            map.insert(child, grandparent.clone());
        } else {
            map.remove(&child);
        }
    }
}

impl Sidebar {
    pub(super) fn state_dir(&self) -> Option<&Path> {
        self.path
            .as_deref()
            .and_then(Path::parent)
            .filter(|p| !p.as_os_str().is_empty())
    }

    /// 백그라운드에서 갱신한 디스크 계보의 메모리 스냅샷을 검증해 돌려준다.
    pub(super) fn merged_parents(&self) -> BTreeMap<PathBuf, PathBuf> {
        self.parents.clone()
    }

    /// 지정한 워크트리의 부모 경로를 돌려준다.
    #[must_use]
    pub fn worktree_parent(&self, path: &Path) -> Option<PathBuf> {
        self.parents.get(path).cloned()
    }

    /// `path`의 부모를 `parent`로 설정할 수 있는지(자기 참조·사이클·다른 저장소 방지).
    #[must_use]
    pub fn can_set_worktree_parent(&self, path: &Path, parent: &Path) -> bool {
        if path == parent {
            return false;
        }
        if let (Some(r1), Some(r2)) = (self.repo_for_worktree(path), self.repo_for_worktree(parent))
            && r1 != r2
        {
            return false;
        }
        let mut parents = self.merged_parents();
        parents.remove(path);
        can_assign_parent(&parents, path, parent)
    }

    /// 워크트리의 부모를 설정하고 `worktree-comments.toml`에 저장한다.
    pub fn set_worktree_parent(
        &mut self,
        path: PathBuf,
        parent: PathBuf,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.can_set_worktree_parent(&path, &parent) {
            return false;
        }
        if let Some(dir) = self.state_dir().map(Path::to_path_buf) {
            let child_str = path.to_string_lossy().into_owned();
            let parent_str = parent.to_string_lossy().into_owned();
            self.enqueue_disk_write(
                move || {
                    krill_proto::orchestration::WorktreeStore::mutate(&dir, |store| {
                        store.set_parent(&child_str, Some(&parent_str))?;
                        store.parents.insert(child_str, parent_str);
                        Ok(())
                    })
                },
                cx,
            );
        }
        self.parents.insert(path, parent);
        self.persist(cx);
        true
    }

    /// 워크트리의 부모 연결을 해제한다.
    pub fn clear_worktree_parent(&mut self, path: &Path, cx: &mut Context<Self>) -> bool {
        if !self.disk.initialized {
            self.disk.startup.parents.insert(path.to_path_buf());
        }
        if self.parents.remove(path).is_none() && self.disk.initialized {
            return false;
        }
        if let Some(dir) = self.state_dir().map(Path::to_path_buf) {
            let child_str = path.to_string_lossy().into_owned();
            self.enqueue_disk_write(
                move || {
                    krill_proto::orchestration::WorktreeStore::mutate(&dir, |store| {
                        store.parents.remove(&child_str);
                        store.set_parent(&child_str, None)?;
                        Ok(())
                    })
                },
                cx,
            );
        }
        self.persist(cx);
        true
    }

    /// 스냅샷에 저장소·워크트리 목록이 하나라도 있는지.
    #[must_use]
    pub fn has_trees(&self) -> bool {
        !self.snapshot.trees.is_empty()
    }

    /// 지정한 경로가 현재 스냅샷의 저장소 또는 워크트리에 존재하는지.
    #[must_use]
    pub fn contains_worktree(&self, path: &Path) -> bool {
        self.repo_for_worktree(path).is_some()
    }

    /// 지정한 경로가 속한 로컬 스냅샷의 워크트리 루트 경로를 찾는다(하위 디렉터리 포함).
    #[must_use]
    pub fn worktree_for_path(&self, path: &Path) -> Option<PathBuf> {
        if path.as_os_str().is_empty() {
            return None;
        }
        self.snapshot
            .trees
            .keys()
            .chain(self.snapshot.trees.values().flatten())
            .filter(|wt| !wt.as_os_str().is_empty() && path.starts_with(wt.as_path()))
            .max_by_key(|wt| wt.as_os_str().len())
            .cloned()
    }

    /// 워크트리가 속한 저장소 루트 경로를 찾는다.
    pub(super) fn repo_for_worktree(&self, path: &Path) -> Option<PathBuf> {
        self.snapshot
            .trees
            .iter()
            .find(|&(repo, list)| repo == path || list.iter().any(|t| t == path))
            .map(|(repo, _)| repo.clone())
    }

    /// 같은 저장소 안에서 `path`의 부모가 될 수 있는 후보 워크트리 목록.
    #[must_use]
    pub fn eligible_parent_worktrees(&self, path: &Path) -> Vec<PathBuf> {
        let Some(repo) = self.repo_for_worktree(path) else {
            return Vec::new();
        };
        let Some(trees) = self.snapshot.trees.get(&repo) else {
            return Vec::new();
        };
        // Every candidate is already in the same repository. Validate ancestry
        // against one shared map instead of re-scanning every repo per candidate.
        let mut parents = self.parents.clone();
        parents.remove(path);
        trees
            .iter()
            .filter(|cand| cand.as_path() != path && can_assign_parent(&parents, path, cand))
            .cloned()
            .collect()
    }

    /// `path`의 직계 자식 워크트리 목록(현재 스냅샷에 존재하는 워크트리만).
    #[must_use]
    pub fn lineage_children(&self, path: &Path) -> Vec<PathBuf> {
        let parents = &self.parents;
        let known: BTreeSet<&Path> = self
            .snapshot
            .trees
            .values()
            .flatten()
            .map(PathBuf::as_path)
            .collect();
        known
            .iter()
            .filter(|&&cand| {
                effective_parent(cand, parents, &known).is_some_and(|p| p.as_path() == path)
            })
            .map(|&p| p.to_path_buf())
            .collect()
    }

    /// `path`의 모든 자손 워크트리 목록(부모 → 자식 BFS/DFS 순서, 사이클 안전).
    #[must_use]
    pub fn lineage_descendants(&self, path: &Path) -> Vec<PathBuf> {
        let parents = &self.parents;
        let known: BTreeSet<&Path> = self
            .snapshot
            .trees
            .values()
            .flatten()
            .map(PathBuf::as_path)
            .collect();
        let mut children_map: BTreeMap<&Path, Vec<&Path>> = BTreeMap::new();
        for &cand in &known {
            if let Some(p) = effective_parent(cand, parents, &known) {
                children_map.entry(p.as_path()).or_default().push(cand);
            }
        }
        let mut out = Vec::new();
        let mut visited = BTreeSet::new();
        visited.insert(path);
        let mut stack: Vec<&Path> = children_map
            .get(path)
            .map(|c| c.iter().rev().copied().collect())
            .unwrap_or_default();
        while let Some(node) = stack.pop() {
            if !visited.insert(node) {
                continue;
            }
            out.push(node.to_path_buf());
            if let Some(children) = children_map.get(node) {
                for &child in children.iter().rev() {
                    stack.push(child);
                }
            }
        }
        out
    }

    /// 현재 표시 기준에서 `path`의 계보 들여쓰기 깊이(`0..=MAX_LINEAGE_DEPTH`).
    #[must_use]
    pub fn lineage_depth(&self, path: &Path) -> usize {
        let parents = &self.parents;
        if parents.is_empty() {
            return 0;
        }
        let scope: BTreeSet<&Path> = match self.options.group_by {
            super::GroupBy::Repo => self
                .repo_for_worktree(path)
                .and_then(|r| self.snapshot.trees.get(&r))
                .map(|list| list.iter().map(PathBuf::as_path).collect())
                .unwrap_or_default(),
            _ => self
                .snapshot
                .trees
                .values()
                .flatten()
                .map(PathBuf::as_path)
                .collect(),
        };
        let mut depth = 0usize;
        let mut visited = BTreeSet::new();
        visited.insert(path);
        let mut cur = path;
        while let Some(parent) = effective_parent(cur, parents, &scope) {
            if !visited.insert(parent.as_path()) {
                break;
            }
            depth += 1;
            if depth >= MAX_LINEAGE_DEPTH {
                return MAX_LINEAGE_DEPTH;
            }
            cur = parent.as_path();
        }
        depth
    }

    /// `path`의 자식 워크트리 트리가 접혀 있는지.
    #[must_use]
    pub fn is_lineage_collapsed(&self, path: &Path) -> bool {
        self.collapsed_lineage.contains(path)
    }

    /// `path`의 자식 워크트리 트리를 접거나 펼친다.
    pub fn toggle_lineage(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.collapsed_lineage.remove(path) {
            self.collapsed_lineage.insert(path.to_path_buf());
        }
        self.persist(cx);
    }

    /// `path`의 조상 중 접혀 있는 계보가 있으면 모두 펼쳐 `path`가 사이드바에 드러나게 한다.
    pub(super) fn expand_lineage_ancestors(&mut self, path: &Path) -> bool {
        let parents = &self.parents;
        let mut changed = false;
        let mut visited = BTreeSet::new();
        let mut cur = Some(path);
        while let Some(node) = cur {
            if !visited.insert(node) {
                break;
            }
            if let Some(parent) = parents.get(node) {
                if self.collapsed_lineage.remove(parent.as_path()) {
                    changed = true;
                }
                cur = Some(parent.as_path());
            } else {
                break;
            }
        }
        changed
    }

    /// 부모 워크트리 아래에 자식 워크트리를 만드는 대화상자를 연다.
    pub fn new_child_worktree(
        &mut self,
        parent: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let repo = self
            .repo_for_worktree(&parent)
            .unwrap_or_else(|| parent.clone());
        self.open_worktree_dialog(repo, Some(parent), window, cx);
    }

    pub(super) fn open_worktree_dialog(
        &mut self,
        repo: PathBuf,
        parent: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let parent_for_event = parent.clone();
        // OWE-853: 호출한 워크트리(부모 우선, 없으면 레포)의 현재 브랜치를 기본값으로 쓴다.
        let parent_branch = parent
            .as_ref()
            .and_then(|p| self.meta.get(p).and_then(|t| t.branch.clone()));
        let repo_branch = self.meta.get(&repo).and_then(|t| t.branch.clone());
        // OWE-853: 로컬 git refs를 읽어 후보를 풍부하게 한다(새 IPC 없이, 실패 시 폴백만).
        let git_refs = Vec::new();
        let refs_repo = repo.clone();
        let (default_base, initial_candidates) = worktree::branch_suggestions(
            parent_branch.as_deref(),
            repo_branch.as_deref(),
            &git_refs,
        );
        let dialog = cx.new(|cx| {
            let mut dialog = worktree::Dialog::new_with_parent(repo, parent, source, window, cx);
            dialog.set_branch_candidates(initial_candidates, cx);
            if let Some(ref base) = default_base {
                dialog.set_base_default(Some(base.clone()), window, cx);
            }
            dialog
        });
        let weak_dialog = dialog.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let refs = cx
                .background_executor()
                .spawn(async move { worktree::list_git_refs(&refs_repo) })
                .await;
            let (base, candidates) = worktree::branch_suggestions(
                parent_branch.as_deref(),
                repo_branch.as_deref(),
                &refs,
            );
            weak_dialog
                .update_in(cx, |dialog, window, cx| {
                    dialog.set_branch_candidates(candidates, cx);
                    dialog.set_base_default(base, window, cx);
                })
                .ok();
        })
        .detach();
        let events = cx.subscribe(
            &dialog,
            move |s, dialog_entity, event: &worktree::Event, cx| match event {
                worktree::Event::Created(w) => {
                    // OWE-853 / OWE-924: 생성 완료 즉시 새 워크트리로 전환한다.
                    // 멀티 에이전트 Fan-Out 중이면 모든 워크트리가 만들어질 때까지 모달을 유지한다.
                    let created_path = PathBuf::from(std::ffi::OsStr::from_bytes(&w.path));
                    let (agent, prompt, has_more) = {
                        let d = dialog_entity.read(cx);
                        (
                            d.last_created_agent().map(str::to_owned),
                            d.last_created_prompt().map(str::to_owned),
                            d.has_pending_fan_out(),
                        )
                    };
                    if let Some(parent_path) = &parent_for_event {
                        s.set_worktree_parent(created_path.clone(), parent_path.clone(), cx);
                    }
                    if let Some(prompt_text) = &prompt {
                        s.queue_initial_prompt(created_path.clone(), prompt_text);
                    }
                    if !has_more {
                        s.creating = None;
                    }
                    s.refresh(cx);
                    if let Some(agent_id) = agent {
                        cx.emit(NewSession {
                            cwd: created_path,
                            program: Program::Agent { id: agent_id },
                        });
                    } else if let Some(id) = w.setup {
                        cx.emit(Selected(id));
                    } else {
                        cx.emit(NewSession {
                            cwd: created_path,
                            program: Program::Login,
                        });
                    }
                    cx.notify();
                }
                worktree::Event::Close => {
                    s.creating = None;
                    cx.notify();
                }
            },
        );
        self.creating = Some((dialog, events));
        cx.notify();
    }

    pub fn queue_initial_prompt(&mut self, cwd: PathBuf, prompt: &str) {
        let trimmed = prompt.trim();
        if !trimmed.is_empty() {
            self.pending_prompts.insert(cwd, trimmed.to_owned());
        }
    }

    pub fn take_initial_prompt(&mut self, cwd: &Path) -> Option<String> {
        self.pending_prompts.remove(cwd)
    }

    /// 워크트리가 수동 수면 상태인지.
    #[must_use]
    pub fn is_worktree_sleeping(&self, path: &Path) -> bool {
        self.sleeping_worktrees.contains(path)
    }

    /// 워크트리를 수면 상태로 전환한다.
    pub fn sleep_worktree(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.ui.expanded_cards.insert(path.clone());
        }
        self.expanded_cards.remove(&path);
        self.collapsed.insert(path.clone());
        self.sleeping_worktrees.insert(path);
        self.persist(cx);
    }

    /// 수면 상태인 워크트리를 깨운다.
    pub fn wake_worktree(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.sleeping.insert(path.to_path_buf());
        }
        if self.sleeping_worktrees.remove(path) || !self.disk.initialized {
            self.persist(cx);
        }
    }

    /// 워크트리와 모든 자손 워크트리를 함께 수면 상태로 전환한다.
    pub fn sleep_with_descendants(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let descendants = self.lineage_descendants(&path);
        for target in std::iter::once(path).chain(descendants) {
            if !self.disk.initialized {
                self.disk.startup.ui.expanded_cards.insert(target.clone());
            }
            self.expanded_cards.remove(&target);
            self.collapsed.insert(target.clone());
            self.sleeping_worktrees.insert(target);
        }
        self.persist(cx);
    }

    fn remove_worktree_lineage_local(&mut self, target: &Path, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.parents.insert(target.to_path_buf());
            self.disk
                .startup
                .collapsed_lineage
                .insert(target.to_path_buf());
            self.disk.startup.sleeping.insert(target.to_path_buf());
        }
        splice_lineage_in_map(&mut self.parents, &target.to_path_buf());
        self.collapsed_lineage.remove(target);
        self.sleeping_worktrees.remove(target);
        if let Some(dir) = self.state_dir().map(Path::to_path_buf) {
            let target_str = target.to_string_lossy().into_owned();
            let target_path = target.to_path_buf();
            self.enqueue_disk_write(
                move || {
                    let norm_key = krill_proto::orchestration::worktree_key(&target_path);
                    krill_proto::orchestration::WorktreeStore::mutate(&dir, |store| {
                        splice_lineage_in_map(&mut store.parents, &target_str);
                        if norm_key != target_str {
                            splice_lineage_in_map(&mut store.parents, &norm_key);
                        }
                        store.remove_worktree_lineage(&target_str);
                        Ok(())
                    })
                },
                cx,
            );
        }
    }

    /// 단일 워크트리를 삭제하고 계보 연결을 정리한다.
    pub fn delete_worktree(&mut self, path: PathBuf, force: bool, cx: &mut Context<Self>) {
        self.delete_worktrees_batch(vec![path], force, cx);
    }

    /// 워크트리와 모든 자손 워크트리를 리프(하위 자손)부터 순서대로 삭제한다.
    pub fn delete_with_descendants(&mut self, path: PathBuf, force: bool, cx: &mut Context<Self>) {
        let mut targets = self.lineage_descendants(&path);
        targets.reverse();
        targets.push(path);
        self.delete_worktrees_batch(targets, force, cx);
    }

    fn delete_worktrees_batch(
        &mut self,
        targets: Vec<PathBuf>,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let parents = self.parents.clone();
        let mut ipc_targets = Vec::new();
        for target in targets {
            if self.snapshot.trees.contains_key(&target) {
                continue;
            }
            let repo = self
                .repo_for_worktree(&target)
                .unwrap_or_else(|| target.clone());
            ipc_targets.push((repo, target));
        }
        if ipc_targets.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            let mut removed_paths = Vec::new();
            let mut blocked_ancestors = BTreeSet::new();
            let mut last_err = None;
            for (repo, target) in ipc_targets {
                if blocked_ancestors.contains(&target) {
                    continue;
                }
                let src = source.clone();
                let req = RemoveWorktree {
                    repo: repo.as_os_str().as_bytes().to_vec(),
                    path: target.as_os_str().as_bytes().to_vec(),
                    force,
                };
                let reply = bg.spawn(async move { src.remove_worktree(req) }).await;
                match reply {
                    Ok(Reply::WorktreeRemoved(_)) => {
                        removed_paths.push(target);
                    }
                    other => {
                        match other {
                            Ok(Reply::WorktreeFailed(e)) => last_err = Some(e),
                            Ok(Reply::Failed(f)) => last_err = Some(f.to_string()),
                            Err(e) if e.kind() != std::io::ErrorKind::Unsupported => {
                                last_err = Some(e.to_string());
                            }
                            _ => {}
                        }
                        let mut cur = parents.get(&target);
                        let mut visited = BTreeSet::new();
                        visited.insert(target);
                        while let Some(anc) = cur {
                            if !visited.insert(anc.clone()) {
                                break;
                            }
                            blocked_ancestors.insert(anc.clone());
                            cur = parents.get(anc);
                        }
                    }
                }
            }
            this.update(cx, |s, cx| {
                if let Some(err) = last_err {
                    s.notice = Some(err);
                }
                if removed_paths.is_empty() {
                    cx.notify();
                } else {
                    for removed in &removed_paths {
                        s.remove_worktree_lineage_local(removed, cx);
                        for list in s.snapshot.trees.values_mut() {
                            list.retain(|t| t != removed);
                        }
                    }
                    s.persist(cx);
                    if s.task.is_some() {
                        s.refresh(cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}
