//! 모바일 목록도 데스크톱의 projects.toml·sidebar.bin을 읽는다. 세션 유무는 목록의 조건이 아니다.
use super::devices::hex;
use krill_proto::control::RepositoryTrees;
use krill_proto::os::ffi::OsStrExt as _;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

#[derive(Default, Deserialize)]
#[serde(default)]
struct ProjectFile {
    projects: Vec<Project>,
    worktrees: BTreeMap<String, Worktree>,
    lineage: BTreeMap<String, Lineage>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Project {
    id: String,
    path: String,
    display_name: String,
    badge_color: Option<String>,
    repo_icon: Option<RepoIcon>,
    pinned: bool,
    archived: bool,
    host_id: Option<String>,
}
/// `projects.toml`에 저장된 프로젝트 아이콘 명세(데스크톱 `ImportedRepoIcon` 패리티).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RepoIcon {
    kind: String,
    value: String,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct Worktree {
    repo_id: Option<String>,
    display_name: Option<String>,
    pinned: bool,
    archived: bool,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct Lineage {
    repo_id: Option<String>,
    worktree_path: String,
    branch: Option<String>,
}
// sidebar.bin v1 첫 Preferences와 같은 필드 순서. 뒤의 표시 옵션 등은 변경하지 않는다.
#[derive(Default, Deserialize)]
struct Preference {
    pinned: bool,
    name: String,
}
// sidebar.bin v1 꼬리 디코딩용 미러(데스크톱 `sidebar.rs::decode_state`·`options.rs`·`lineage.rs`와
// postcard 필드·열거 순서를 맞춘다. 어긋나면 디코딩이 실패하고 기본값으로 강등된다).
// 유닛 열거형은 postcard에서 변형 인덱스 varint 하나로 인코딩되므로 u32로 읽는다.
#[derive(serde::Serialize, Deserialize)]
struct BinOptions {
    group_by: u32,
    sort_by: u32,
    card_layout: u32,
    card_properties: BinCardProperties,
    filters: BinFilters,
}
#[derive(serde::Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "desktop WorkspaceOptions::CardProperties mirror for sidebar.bin tail decode"
)]
struct BinCardProperties {
    issue: bool,
    pr: bool,
    comment: bool,
    ports: bool,
    branch: bool,
    host: bool,
}
#[derive(serde::Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "desktop WorkspaceOptions::WorkspaceFilters mirror for sidebar.bin tail decode"
)]
struct BinFilters {
    hide_sleeping: bool,
    always_show_default_branch: bool,
    hide_automation: bool,
    hide_cli: bool,
    repos: BTreeSet<PathBuf>,
    hosts: BTreeSet<String>,
}
#[derive(serde::Serialize, Deserialize)]
struct BinLineage {
    parents: BTreeMap<PathBuf, PathBuf>,
    collapsed_lineage: BTreeSet<PathBuf>,
    #[serde(default)]
    sleeping_worktrees: BTreeSet<PathBuf>,
}
#[derive(serde::Serialize, Deserialize)]
enum BinPrGroup {
    Merged,
    Open,
    DraftOrNoPr,
    Closed,
}
#[derive(serde::Serialize, Deserialize)]
enum BinGroupKey {
    WorkspaceStatus(krill_proto::orchestration::WorkspaceStatus),
    PrStatus(BinPrGroup),
}
#[derive(serde::Serialize, Deserialize)]
enum BinHostId {
    Local,
    Remote(String),
}
#[derive(serde::Serialize, Deserialize)]
struct BinUi {
    collapsed: BTreeSet<PathBuf>,
    collapsed_groups: Vec<BinGroupKey>,
    collapsed_hosts: Vec<BinHostId>,
    expanded_cards: BTreeSet<PathBuf>,
    selected_worktree: Option<PathBuf>,
    shown_external: BTreeSet<PathBuf>,
    removed_projects: BTreeSet<PathBuf>,
}
/// 계보 깊이 상한(데스크톱 `lineage::MAX_LINEAGE_DEPTH`와 동일).
pub(super) const MAX_LINEAGE_DEPTH: usize = 4;
#[derive(Default)]
pub(super) struct Catalog {
    pub repositories: Vec<RepositoryTrees>,
    preferences: BTreeMap<PathBuf, Preference>,
    branches: BTreeMap<PathBuf, String>,
    repo_labels: BTreeMap<PathBuf, String>,
    repo_icons: BTreeMap<PathBuf, RepoIcon>,
    repo_badges: BTreeMap<PathBuf, String>,
    parents: BTreeMap<PathBuf, PathBuf>,
    project_order: Vec<PathBuf>,
}

fn read(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (u64::try_from(bytes.len()).ok()? <= limit).then_some(bytes)
}
fn normalize(path: &Path) -> PathBuf {
    // 데스크톱 이관 경로와 동일한 macOS /tmp·/var·/etc 별칭 처리.
    if ["/tmp", "/var", "/etc", "/private"]
        .iter()
        .any(|p| path.starts_with(p))
    {
        if let Ok(path) = fs::canonicalize(path) {
            return path;
        }
        if cfg!(target_os = "macos") {
            for base in ["tmp", "var", "etc"] {
                if let Ok(rest) = path.strip_prefix(Path::new("/").join(base)) {
                    return Path::new("/private").join(base).join(rest);
                }
            }
        }
    }
    path.to_path_buf()
}
fn path(bytes: &[u8]) -> PathBuf {
    normalize(Path::new(std::ffi::OsStr::from_bytes(bytes)))
}
fn label(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
fn git_dir(tree: &Path) -> PathBuf {
    let dot = tree.join(".git");
    read(&dot, 4096)
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.strip_prefix("gitdir:").map(|p| tree.join(p.trim())))
        .unwrap_or(dot)
}
fn main_worktree(tree: &Path) -> Option<PathBuf> {
    let dir = git_dir(tree);
    let common = String::from_utf8(read(&dir.join("commondir"), 4096)?).ok()?;
    let common = fs::canonicalize(dir.join(common.trim())).ok()?;
    (common.file_name()? == ".git")
        .then(|| common.parent().map(normalize))
        .flatten()
}
impl Catalog {
    pub fn load(dir: Option<&Path>, repositories: &[RepositoryTrees]) -> Self {
        let projects: ProjectFile = dir
            .and_then(|d| read(&d.join("projects.toml"), 512 * 1024))
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        let (preferences, bin_parents, project_order) = dir
            .and_then(|d| read(&d.join("sidebar.bin"), 1024 * 1024))
            .and_then(|b| {
                let (&version, body) = b.split_first()?;
                (version == 1).then(|| decode_sidebar_tail(body))
            })
            .unwrap_or_default();
        let preferences: BTreeMap<PathBuf, Preference> = preferences
            .into_iter()
            .map(|(p, v)| (normalize(&p), v))
            .collect();
        let bin_parents: BTreeMap<PathBuf, PathBuf> = bin_parents
            .into_iter()
            .map(|(c, p)| (normalize(&c), normalize(&p)))
            .collect();
        let project_order: Vec<PathBuf> =
            project_order.into_iter().map(|p| normalize(&p)).collect();
        let mut catalog = Self {
            preferences,
            project_order,
            ..Self::default()
        };
        // 데스크톱 계보의 영속 원천(`worktree-comments.toml [parents]`)을 먼저 읽고,
        // sidebar.bin의 최신 편집이 있으면 덮어쓴다.
        if let Some(dir) = dir {
            for (child, parent) in store_parents(dir) {
                catalog.parents.entry(child).or_insert(parent);
            }
        }
        for (child, parent) in bin_parents {
            if can_assign_parent(&catalog.parents, &child, &parent) {
                catalog.parents.insert(child, parent);
            }
        }
        for repo in repositories {
            let root = path(&repo.repo);
            let root = main_worktree(&root).unwrap_or(root);
            if repo.worktrees.is_empty() {
                catalog.repositories.push(RepositoryTrees {
                    repo: root.as_os_str().as_bytes().to_vec(),
                    worktrees: Vec::new(),
                });
            }
            for tree in &repo.worktrees {
                catalog.insert(&root, &path(tree));
            }
        }
        let local: Vec<_> = projects
            .projects
            .iter()
            .filter(|p| !p.archived && p.host_id.is_none() && !p.path.trim().is_empty())
            .collect();
        for project in &local {
            let tree = normalize(Path::new(project.path.trim()));
            let repo = main_worktree(&tree)
                .or_else(|| catalog.repo_for(&tree))
                .unwrap_or_else(|| tree.clone());
            catalog.insert(&repo, &tree);
            if !project.display_name.trim().is_empty() {
                catalog
                    .repo_labels
                    .insert(repo.clone(), project.display_name.trim().to_owned());
            }
            if let Some(icon) = project.repo_icon.as_ref()
                && !icon.value.trim().is_empty()
            {
                catalog
                    .repo_icons
                    .entry(repo.clone())
                    .or_insert_with(|| RepoIcon {
                        kind: icon.kind.trim().to_owned(),
                        value: icon.value.trim().to_owned(),
                    });
            }
            if let Some(color) = project.badge_color.as_deref()
                && !color.trim().is_empty()
            {
                catalog
                    .repo_badges
                    .entry(repo.clone())
                    .or_insert_with(|| color.trim().to_owned());
            }
            if !project.display_name.trim().is_empty() || project.pinned {
                catalog
                    .preferences
                    .entry(tree)
                    .or_insert_with(|| Preference {
                        pinned: project.pinned,
                        name: project.display_name.trim().to_owned(),
                    });
            }
        }
        let resolve = |id: Option<&str>, tree: &Path| {
            local
                .iter()
                .find(|p| id.is_some_and(|id| p.id == id))
                .or_else(|| {
                    local
                        .iter()
                        .find(|p| tree.starts_with(normalize(Path::new(p.path.trim()))))
                })
                .map(|p| normalize(Path::new(p.path.trim())))
        };
        for (key, meta) in projects
            .worktrees
            .iter()
            .filter(|(k, m)| !k.trim().is_empty() && !m.archived)
        {
            let tree = normalize(Path::new(key.trim()));
            let repo = resolve(meta.repo_id.as_deref(), &tree)
                .map(|r| catalog.repo_for(&r).unwrap_or(r))
                .or_else(|| catalog.repo_for(&tree))
                .unwrap_or_else(|| tree.clone());
            catalog.insert(&repo, &tree);
            let name = meta.display_name.as_deref().unwrap_or_default().trim();
            if !name.is_empty() || meta.pinned {
                catalog
                    .preferences
                    .entry(tree)
                    .or_insert_with(|| Preference {
                        pinned: meta.pinned,
                        name: name.to_owned(),
                    });
            }
        }
        for (key, meta) in &projects.lineage {
            let key = if meta.worktree_path.trim().is_empty() {
                key.trim()
            } else {
                meta.worktree_path.trim()
            };
            if key.is_empty() {
                continue;
            }
            let tree = normalize(Path::new(key));
            let repo = resolve(meta.repo_id.as_deref(), &tree)
                .map(|r| catalog.repo_for(&r).unwrap_or(r))
                .or_else(|| catalog.repo_for(&tree))
                .unwrap_or_else(|| tree.clone());
            catalog.insert(&repo, &tree);
            if let Some(branch) = &meta.branch {
                catalog.branches.insert(tree, branch.clone());
            }
        }
        for tree in catalog.repositories.iter().flat_map(|r| &r.worktrees) {
            let tree = path(tree);
            if let Some(branch) = read(&git_dir(&tree).join("HEAD"), 4096)
                .and_then(|b| String::from_utf8(b).ok())
                .and_then(|s| s.trim().strip_prefix("ref: refs/heads/").map(str::to_owned))
            {
                catalog.branches.insert(tree, branch);
            }
        }
        // 데스크톱 `ordered_projects`와 같은 저장소 순서(사용자 드래그 순서, 없는 것은 삽입 순서 유지).
        if !catalog.project_order.is_empty() {
            let order = catalog.project_order.clone();
            catalog.repositories.sort_by_key(|r| {
                order
                    .iter()
                    .position(|o| *o == path(&r.repo))
                    .unwrap_or(usize::MAX)
            });
        }
        catalog
    }
    fn repo_for(&self, tree: &Path) -> Option<PathBuf> {
        self.repositories
            .iter()
            .find(|r| r.worktrees.iter().any(|t| path(t) == tree))
            .map(|r| path(&r.repo))
    }
    fn insert(&mut self, repo: &Path, tree: &Path) {
        let repo = repo.as_os_str().as_bytes().to_vec();
        let tree = tree.as_os_str().as_bytes().to_vec();
        if let Some(entry) = self.repositories.iter_mut().find(|r| r.repo == repo) {
            if !entry.worktrees.contains(&tree) {
                entry.worktrees.push(tree);
            }
        } else {
            self.repositories.push(RepositoryTrees {
                repo,
                worktrees: vec![tree],
            });
        }
    }
    /// 클라이언트의 임의 경로 대신 현재 등록된 워크스페이스 키만 해석한다.
    pub fn cwd(&self, key: &str) -> Option<PathBuf> {
        self.repositories
            .iter()
            .flat_map(|r| &r.worktrees)
            .find(|t| hex(t) == key)
            .map(|t| path(t))
    }
    /// 생성 대상도 데스크톱에 등록된 저장소 키로만 해석한다.
    pub fn repository(&self, key: &str) -> Option<Vec<u8>> {
        self.repositories
            .iter()
            .find(|r| hex(&r.repo) == key)
            .map(|r| r.repo.clone())
    }
    pub fn repositories_json(&self) -> Vec<Value> {
        self.repositories
            .iter()
            .map(|r| {
                let repo = path(&r.repo);
                json!({"key":hex(&r.repo),"path":repo.to_string_lossy(),
                "label":self.repo_labels.get(&repo).cloned().unwrap_or_else(|| label(&repo)),
                "icon":self.repo_icons.get(&repo).map_or(Value::Null, |i| json!({"kind":i.kind,"value":i.value})),
                "badge_color":self.repo_badges.get(&repo).cloned().map_or(Value::Null, Value::String),
                "worktree_keys":r.worktrees.iter().map(|t| hex(t)).collect::<Vec<_>>()})
            })
            .collect()
    }
    /// 데스크톱 사이드바와 같은 워크트리 순서·들여쓰기·아이콘을 낸다.
    /// `ranks`는 워크트리 경로별 스마트 순위(0 승인대기 최우선, 4 유휴)다.
    pub fn workspaces_json(
        &self,
        auto_titles: &BTreeMap<PathBuf, String>,
        ranks: &BTreeMap<PathBuf, u8>,
    ) -> Vec<Value> {
        self.repositories
            .iter()
            .flat_map(|r| {
                let repo = path(&r.repo);
                let mut trees: Vec<PathBuf> = r.worktrees.iter().map(|t| path(t)).collect();
                // 데스크톱 `sort_worktrees`와 같은 키: [기본] → 핀 → 스마트 순위 → 이름.
                trees.sort_by(|a, b| {
                    let primary = |t: &Path| t != repo;
                    let pin = |t: &Path| !self.preferences.get(t).is_some_and(|v| v.pinned);
                    let rank = |t: &Path| ranks.get(t).copied().unwrap_or(4);
                    let name = |t: &Path| worktree_sort_name(t, &self.preferences);
                    (primary(a), pin(a), rank(a), name(a), a).cmp(&(primary(b), pin(b), rank(b), name(b), b))
                });
                order_lineage(&trees, &self.parents)
                    .into_iter()
                    .map(move |(tree, depth)| {
                        let pref = self.preferences.get(&tree);
                        let branch = self.branches.get(&tree);
                        let name = pref.map(|p| p.name.as_str()).filter(|s| !s.is_empty());
                        let custom = name.filter(|n| {
                            !krill_proto::git::is_default_branch_or_worktree_name(n)
                        });
                        let short_branch = || {
                            branch.map(|b| b.rsplit('/').next().unwrap_or(b).replace('-', " "))
                        };
                        let title = if let Some(name) = custom {
                            name.to_owned()
                        } else if tree == repo {
                            branch.cloned().unwrap_or_else(|| label(&tree))
                        } else if branch.is_some_and(|b| {
                            !krill_proto::git::is_default_branch_or_worktree_name(b)
                        }) {
                            short_branch().unwrap_or_default()
                        } else if let Some(auto) = auto_titles.get(&tree) {
                            auto.clone()
                        } else {
                            name.map(str::to_owned)
                                .or_else(short_branch)
                                .unwrap_or_else(|| label(&tree).replace('-', " "))
                        };
                        let raw = tree.as_os_str().as_bytes().to_vec();
                        json!({"key":hex(&raw),"repo_key":hex(&r.repo),"path":tree.to_string_lossy(),
                    "label":title,
                    "pinned":pref.is_some_and(|p| p.pinned),"branch":branch,
                    "depth":depth,"primary":tree == repo,
                    "icon":self.repo_icons.get(&tree).or_else(|| self.repo_icons.get(&repo)).map_or(Value::Null, |i| json!({"kind":i.kind,"value":i.value})),
                    "badge_color":self.repo_badges.get(&tree).or_else(|| self.repo_badges.get(&repo)).cloned().map_or(Value::Null, Value::String)})
                    })
            })
            .collect()
    }
}
/// 데스크톱 정렬용 워크트리 표시 이름(핀 이름 → 브랜치 짧은 이름 → 폴더 이름).
fn worktree_sort_name(path: &Path, prefs: &BTreeMap<PathBuf, Preference>) -> String {
    prefs
        .get(path)
        .filter(|v| !v.name.is_empty())
        .map_or_else(|| label(path), |v| v.name.clone())
}
/// sidebar.bin v1 꼬리에서 계보 부모와 프로젝트 순서를 꺼낸다. 형식 불일치는 기본값으로 강등한다.
fn decode_sidebar_tail(
    body: &[u8],
) -> (
    BTreeMap<PathBuf, Preference>,
    BTreeMap<PathBuf, PathBuf>,
    Vec<PathBuf>,
) {
    let Ok((prefs, mut rest)) =
        postcard::take_from_bytes::<BTreeMap<PathBuf, Preference>>(body).map_err(|_| ())
    else {
        return Default::default();
    };
    if rest.is_empty() {
        return (prefs, BTreeMap::new(), Vec::new());
    }
    let Ok((_, next)) = postcard::take_from_bytes::<BinOptions>(rest).map_err(|_| ()) else {
        return (prefs, BTreeMap::new(), Vec::new());
    };
    rest = next;
    if rest.is_empty() {
        return (prefs, BTreeMap::new(), Vec::new());
    }
    let Ok((lineage, next)) = postcard::take_from_bytes::<BinLineage>(rest).map_err(|_| ()) else {
        return (prefs, BTreeMap::new(), Vec::new());
    };
    rest = next;
    if rest.is_empty() {
        return (prefs, lineage.parents, Vec::new());
    }
    let Ok((_, next)) = postcard::take_from_bytes::<BinUi>(rest).map_err(|_| ()) else {
        return (prefs, lineage.parents, Vec::new());
    };
    rest = next;
    if rest.is_empty() {
        return (prefs, lineage.parents, Vec::new());
    }
    match postcard::take_from_bytes::<Vec<PathBuf>>(rest) {
        Ok((order, _)) => (prefs, lineage.parents, order),
        Err(_) => (prefs, lineage.parents, Vec::new()),
    }
}
/// 계보 저장소(`worktree-comments.toml [parents]`)의 자식→부모를 읽는다.
fn store_parents(dir: &Path) -> BTreeMap<PathBuf, PathBuf> {
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct Store {
        parents: BTreeMap<String, String>,
    }
    dir.join("worktree-comments.toml")
        .to_str()
        .and_then(|_| read(&dir.join("worktree-comments.toml"), 512 * 1024))
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| toml::from_str::<Store>(&s).ok())
        .map(|store| {
            store
                .parents
                .into_iter()
                .filter(|(c, p)| !c.trim().is_empty() && !p.trim().is_empty())
                .map(|(c, p)| {
                    (
                        normalize(Path::new(c.trim())),
                        normalize(Path::new(p.trim())),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}
/// 자기 참조·사이클을 만들지 않을 때만 부모로 인정한다(데스크톱 `can_assign_parent`와 동일).
fn can_assign_parent(
    parents: &BTreeMap<PathBuf, PathBuf>,
    worktree: &Path,
    candidate: &Path,
) -> bool {
    if worktree == candidate {
        return false;
    }
    let mut visited = BTreeSet::from([worktree.to_path_buf()]);
    let mut cur = Some(candidate);
    for _ in 0..64 {
        let Some(node) = cur else { return true };
        if !visited.insert(node.to_path_buf()) {
            return false;
        }
        cur = parents.get(node).map(PathBuf::as_path);
    }
    false
}
/// 같은 저장소 안에서 유효한 부모를 찾는다(그룹 밖·사이클은 루트로 취급).
fn effective_parent<'a>(
    path: &'a Path,
    parents: &'a BTreeMap<PathBuf, PathBuf>,
    group: &BTreeSet<&'a Path>,
) -> Option<&'a PathBuf> {
    let direct = parents.get(path)?;
    if direct.as_path() == path || !group.contains(direct.as_path()) {
        return None;
    }
    let mut visited = BTreeSet::from([path]);
    let mut cur: Option<&Path> = Some(direct.as_path());
    for _ in 0..64 {
        let Some(node) = cur else { break };
        if !visited.insert(node) {
            return None;
        }
        cur = parents
            .get(node)
            .map(PathBuf::as_path)
            .filter(|next| group.contains(next));
    }
    Some(direct)
}
/// 계보 DFS 방문(데스크톱 `order_lineage_worktrees`의 내부 재귀와 동일).
fn visit_lineage<'a>(
    node: &'a PathBuf,
    children: &BTreeMap<&Path, Vec<&'a PathBuf>>,
    visited: &mut BTreeSet<&'a Path>,
    out: &mut Vec<(&'a PathBuf, usize)>,
    depth: usize,
) {
    if !visited.insert(node.as_path()) {
        return;
    }
    out.push((node, depth.min(MAX_LINEAGE_DEPTH)));
    if let Some(kids) = children.get(node.as_path()) {
        for kid in kids {
            visit_lineage(kid, children, visited, out, depth + 1);
        }
    }
}
/// 정렬된 워크트리를 부모-자식 DFS 순서로 펼치고 깊이를 함께 돌려준다(데스크톱 `order_lineage_worktrees`).
fn order_lineage(
    sorted: &[PathBuf],
    parents: &BTreeMap<PathBuf, PathBuf>,
) -> Vec<(PathBuf, usize)> {
    if parents.is_empty() {
        return sorted.iter().map(|t| (t.clone(), 0)).collect();
    }
    let group: BTreeSet<&Path> = sorted.iter().map(PathBuf::as_path).collect();
    let mut children: BTreeMap<&Path, Vec<&PathBuf>> = BTreeMap::new();
    let mut roots: Vec<&PathBuf> = Vec::with_capacity(sorted.len());
    for tree in sorted {
        if let Some(parent) = effective_parent(tree.as_path(), parents, &group) {
            children.entry(parent.as_path()).or_default().push(tree);
        } else {
            roots.push(tree);
        }
    }
    let mut visited = BTreeSet::new();
    let mut out = Vec::with_capacity(sorted.len());
    for root in roots {
        visit_lineage(root, &children, &mut visited, &mut out, 0);
    }
    // 사이클 등으로 빠진 노드는 루트로 보존한다.
    for tree in sorted {
        if !visited.contains(tree.as_path()) {
            out.push((tree, 0));
        }
    }
    out.into_iter().map(|(t, d)| (t.clone(), d)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_worktrees_and_lineage_survive_without_sessions() {
        let dir = std::env::temp_dir().join(format!("krill-workspaces-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("projects.toml"),
            r#"
[[projects]]
id = "repo"
path = "/repo"
display_name = "프로젝트"
[[projects]]
path = "/archived"
archived = true
[[projects]]
path = "/remote"
host_id = "remote"
[worktrees."/repo/feature"]
repo_id = "repo"
display_name = "이관 이름"
pinned = true
[worktrees."/repo/archived"]
repo_id = "repo"
archived = true
[lineage.child]
repo_id = "repo"
worktree_path = "/outside/child"
branch = "task/my-feature"
"#,
        )
        .unwrap();
        // 데스크톱 v1 Preferences의 고정 바이트 형식: map<PathBuf, (bool, String)>.
        let prefs = BTreeMap::from([(PathBuf::from("/repo/feature"), (false, "PC 이름"))]);
        let mut bytes = vec![1];
        bytes.extend(postcard::to_allocvec(&prefs).unwrap());
        fs::write(dir.join("sidebar.bin"), &bytes).unwrap();
        let catalog = Catalog::load(Some(&dir), &[]);
        let rows = catalog.workspaces_json(&BTreeMap::new(), &BTreeMap::new());
        assert_eq!(rows.len(), 3);
        let feature = rows.iter().find(|w| w["path"] == "/repo/feature").unwrap();
        assert_eq!(feature["label"], "PC 이름");
        assert_eq!(feature["pinned"], false);
        assert_eq!(
            rows.iter().find(|w| w["path"] == "/outside/child").unwrap()["label"],
            "my feature"
        );
        assert_eq!(catalog.repositories.len(), 1);
        assert_eq!(catalog.repositories_json()[0]["label"], "프로젝트");
        assert_eq!(
            catalog.cwd(feature["key"].as_str().unwrap()),
            Some(PathBuf::from("/repo/feature"))
        );
        assert!(catalog.cwd("unregistered").is_none());
        // 데스크톱 순서·들여쓰기·아이콘 필드가 기본값으로 채워진다.
        for row in &rows {
            assert_eq!(row["depth"], 0);
            assert!(row["primary"].is_boolean());
            assert!(row.get("icon").is_some() && row.get("badge_color").is_some());
        }
        // 파일을 바꾸면 스냅샷 갱신에서 즉시 반영하며, 손상된 설정은 이관 이름으로 강등한다.
        fs::write(dir.join("sidebar.bin"), [2, 0]).unwrap();
        let rows =
            Catalog::load(Some(&dir), &[]).workspaces_json(&BTreeMap::new(), &BTreeMap::new());
        let feature = rows.iter().find(|w| w["path"] == "/repo/feature").unwrap();
        assert_eq!(feature["label"], "이관 이름");
        assert_eq!(feature["pinned"], true);
        fs::remove_dir_all(dir).unwrap();
    }

    fn write_sidebar_tail(dir: &Path, order: &[PathBuf], parents: &BTreeMap<PathBuf, PathBuf>) {
        // 데스크톱 저장 순서(prefs → options → lineage → ui → order)를 그대로 둔다.
        let prefs = BTreeMap::<PathBuf, (bool, String)>::new();
        let mut bytes = vec![1];
        bytes.extend(postcard::to_allocvec(&prefs).unwrap());
        bytes.extend(
            postcard::to_allocvec(&BinOptions {
                group_by: 0,
                sort_by: 0,
                card_layout: 0,
                card_properties: BinCardProperties {
                    issue: true,
                    pr: true,
                    comment: true,
                    ports: true,
                    branch: true,
                    host: true,
                },
                filters: BinFilters {
                    hide_sleeping: false,
                    always_show_default_branch: true,
                    hide_automation: false,
                    hide_cli: false,
                    repos: BTreeSet::new(),
                    hosts: BTreeSet::new(),
                },
            })
            .unwrap(),
        );
        bytes.extend(
            postcard::to_allocvec(&BinLineage {
                parents: parents.clone(),
                collapsed_lineage: BTreeSet::new(),
                sleeping_worktrees: BTreeSet::new(),
            })
            .unwrap(),
        );
        bytes.extend(
            postcard::to_allocvec(&BinUi {
                collapsed: BTreeSet::new(),
                collapsed_groups: Vec::new(),
                collapsed_hosts: Vec::new(),
                expanded_cards: BTreeSet::new(),
                selected_worktree: None,
                shown_external: BTreeSet::new(),
                removed_projects: BTreeSet::new(),
            })
            .unwrap(),
        );
        bytes.extend(postcard::to_allocvec(&order.to_vec()).unwrap());
        fs::write(dir.join("sidebar.bin"), bytes).unwrap();
    }

    #[test]
    fn sidebar_order_parents_and_icons_match_desktop() {
        let dir = std::env::temp_dir().join(format!("krill-ws-order-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("projects.toml"),
            r##"
[[projects]]
id = "b"
path = "/b"
[[projects]]
id = "a"
path = "/a"
badge_color = "#ff0000"
[projects.repo_icon]
kind = "emoji"
value = "🚀"
"##,
        )
        .unwrap();
        // 드래그 순서(/b 먼저)와 계보(/a/child → /a)를 sidebar.bin 꼬리에 둔다.
        write_sidebar_tail(
            &dir,
            &[PathBuf::from("/b"), PathBuf::from("/a")],
            &BTreeMap::from([(PathBuf::from("/a/child"), PathBuf::from("/a"))]),
        );
        let repositories = vec![
            RepositoryTrees {
                repo: b"/a".to_vec(),
                worktrees: vec![b"/a".to_vec(), b"/a/child".to_vec()],
            },
            RepositoryTrees {
                repo: b"/b".to_vec(),
                worktrees: vec![b"/b".to_vec()],
            },
        ];
        let catalog = Catalog::load(Some(&dir), &repositories);
        // 저장소 순서는 드래그 순서를 따른다.
        let repos = catalog.repositories_json();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0]["path"], "/b");
        assert_eq!(repos[1]["path"], "/a");
        assert_eq!(repos[1]["icon"]["kind"], "emoji");
        assert_eq!(repos[1]["icon"]["value"], "🚀");
        assert_eq!(repos[1]["badge_color"], "#ff0000");
        let rows = catalog.workspaces_json(&BTreeMap::new(), &BTreeMap::new());
        assert_eq!(rows.len(), 3);
        // 같은 저장소 안에서 [기본]이 먼저, 자식은 부모 바로 뒤에 깊이 1로 붙는다.
        let a: Vec<_> = rows
            .iter()
            .filter(|w| w["repo_key"] == repos[1]["key"])
            .collect();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0]["path"], "/a");
        assert_eq!(a[0]["primary"], true);
        assert_eq!(a[0]["depth"], 0);
        assert_eq!(a[1]["path"], "/a/child");
        assert_eq!(a[1]["depth"], 1);
        assert_eq!(a[1]["primary"], false);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn smart_rank_orders_waiting_before_idle() {
        let dir = std::env::temp_dir().join(format!("krill-ws-rank-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let repositories = vec![RepositoryTrees {
            repo: b"/r".to_vec(),
            worktrees: vec![b"/r".to_vec(), b"/r/idle".to_vec(), b"/r/urgent".to_vec()],
        }];
        let catalog = Catalog::load(Some(&dir), &repositories);
        let ranks = BTreeMap::from([(PathBuf::from("/r/urgent"), 0u8)]);
        let rows = catalog.workspaces_json(&BTreeMap::new(), &ranks);
        let paths: Vec<_> = rows
            .iter()
            .map(|w| w["path"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(paths, vec!["/r", "/r/urgent", "/r/idle"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_sidebar_tail_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("krill-ws-tail-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // prefs 뒤에 쓰레기가 오면 꼬리 전체를 버리고 prefs만 쓴다.
        let prefs = BTreeMap::from([(PathBuf::from("/r"), (true, "이름"))]);
        let mut bytes = vec![1];
        bytes.extend(postcard::to_allocvec(&prefs).unwrap());
        bytes.extend([0xff, 0xff, 0xff]);
        fs::write(dir.join("sidebar.bin"), bytes).unwrap();
        let repositories = vec![RepositoryTrees {
            repo: b"/r".to_vec(),
            worktrees: vec![b"/r".to_vec()],
        }];
        let catalog = Catalog::load(Some(&dir), &repositories);
        let rows = catalog.workspaces_json(&BTreeMap::new(), &BTreeMap::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["label"], "이름");
        assert_eq!(rows[0]["depth"], 0);
        fs::remove_dir_all(dir).unwrap();
    }
}
