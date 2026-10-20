//! 모바일 목록도 데스크톱의 projects.toml·sidebar.bin을 읽는다. 세션 유무는 목록의 조건이 아니다.
use super::devices::hex;
use krill_proto::control::RepositoryTrees;
use krill_proto::os::ffi::OsStrExt as _;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
    pinned: bool,
    archived: bool,
    host_id: Option<String>,
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
#[derive(Default)]
pub(super) struct Catalog {
    pub repositories: Vec<RepositoryTrees>,
    preferences: BTreeMap<PathBuf, Preference>,
    branches: BTreeMap<PathBuf, String>,
    repo_labels: BTreeMap<PathBuf, String>,
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
        let preferences = dir
            .and_then(|d| read(&d.join("sidebar.bin"), 1024 * 1024))
            .and_then(|b| {
                let (&version, body) = b.split_first()?;
                (version == 1)
                    .then(|| {
                        postcard::take_from_bytes::<BTreeMap<PathBuf, Preference>>(body)
                            .ok()
                            .map(|(p, _)| p)
                    })
                    .flatten()
            })
            .unwrap_or_default()
            .into_iter()
            .map(|(p, v)| (normalize(&p), v))
            .collect();
        let mut catalog = Self {
            preferences,
            ..Self::default()
        };
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
                    .insert(repo, project.display_name.trim().to_owned());
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
                "worktree_keys":r.worktrees.iter().map(|t| hex(t)).collect::<Vec<_>>()})
            })
            .collect()
    }
    pub fn workspaces_json(&self, auto_titles: &BTreeMap<PathBuf, String>) -> Vec<Value> {
        self.repositories
            .iter()
            .flat_map(|r| {
                r.worktrees.iter().map(move |t| {
                    let tree = path(t);
                    let pref = self.preferences.get(&tree);
                    let branch = self.branches.get(&tree);
                    let name = pref.map(|p| p.name.as_str()).filter(|s| !s.is_empty());
                    let custom =
                        name.filter(|n| !krill_proto::git::is_default_branch_or_worktree_name(n));
                    let short_branch =
                        || branch.map(|b| b.rsplit('/').next().unwrap_or(b).replace('-', " "));
                    let title = if let Some(name) = custom {
                        name.to_owned()
                    } else if tree == path(&r.repo) {
                        branch.cloned().unwrap_or_else(|| label(&tree))
                    } else if branch
                        .is_some_and(|b| !krill_proto::git::is_default_branch_or_worktree_name(b))
                    {
                        short_branch().unwrap_or_default()
                    } else if let Some(auto) = auto_titles.get(&tree) {
                        auto.clone()
                    } else {
                        name.map(str::to_owned)
                            .or_else(short_branch)
                            .unwrap_or_else(|| label(&tree).replace('-', " "))
                    };
                    json!({"key":hex(t),"repo_key":hex(&r.repo),"path":tree.to_string_lossy(),
                "label":title,
                "pinned":pref.is_some_and(|p| p.pinned),"branch":branch})
                })
            })
            .collect()
    }
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
        let rows = catalog.workspaces_json(&BTreeMap::new());
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
        // 파일을 바꾸면 스냅샷 갱신에서 즉시 반영하며, 손상된 설정은 이관 이름으로 강등한다.
        fs::write(dir.join("sidebar.bin"), [2, 0]).unwrap();
        let rows = Catalog::load(Some(&dir), &[]).workspaces_json(&BTreeMap::new());
        let feature = rows.iter().find(|w| w["path"] == "/repo/feature").unwrap();
        assert_eq!(feature["label"], "이관 이름");
        assert_eq!(feature["pinned"], true);
        fs::remove_dir_all(dir).unwrap();
    }
}
