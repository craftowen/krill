//! MK-093: 워크트리 코멘트(`krill set --comment`) 조회와 외부 `.claude/worktrees` 감지·숨김 분류·표시.
//! 선행 MK-046(`sidebar.rs`)에 대한 의존과 UI 표시 헬퍼는 이 모듈로 격리한다.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use gpui::{Context, Div, Hsla, Stateful, div, prelude::*, px};
use serde::{Deserialize, Serialize};

use super::Sidebar;

pub const COMMENTS_FILE: &str = "worktree-comments.toml";

const LOCK_FILE: &str = "worktree-comments.lock";
const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_COMMENTS: usize = 1_024;
const MAX_COMMENT_CHARS: usize = 256;
const MAX_CLAUDE_WORKTREES: usize = 64;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    comments: BTreeMap<String, String>,
    /// 코멘트별 마지막 기록 시각(UNIX 초). 상한 도달 시 사전순이 아닌 가장 오래된 항목을
    /// 지우기 위한 기록이며, 구 파일에는 없어 0으로 읽혀 가장 먼저 지워진다.
    #[serde(default)]
    updated_at: BTreeMap<String, u64>,
    #[serde(default)]
    statuses: BTreeMap<String, String>,
    #[serde(default)]
    unread: BTreeMap<String, bool>,
    #[serde(default)]
    prs: BTreeMap<String, u64>,
    #[serde(default)]
    issues: BTreeMap<String, String>,
    #[serde(default)]
    references: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    parents: BTreeMap<String, String>,
}

/// 워크트리 경로의 체크포인트 배지(`● REVIEW #128 OWE-626 🔗2`)를 구성한다(MK-116, MK-118).
pub fn checkpoint_badge_for(sidebar_path: Option<&Path>, worktree: &Path) -> Option<String> {
    let dir = sidebar_path
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())?;
    let store = krill_proto::orchestration::WorktreeStore::load_cached(dir);
    let candidates = path_aliases(worktree);
    for key in candidates {
        let unread = store.unread.get(&key).copied().unwrap_or(false);
        let status = store
            .statuses
            .get(&key)
            .and_then(|s| krill_proto::orchestration::WorkspaceStatus::parse(s).ok())
            .map(krill_proto::orchestration::WorkspaceStatus::badge_label);
        let pr = store.prs.get(&key).copied();
        let issue = store.issues.get(&key).map(String::as_str);
        let refs_len = store.references.get(&key).map_or(0, Vec::len);
        if !unread && status.is_none() && pr.is_none() && issue.is_none() && refs_len == 0 {
            continue;
        }
        let mut parts = Vec::new();
        if unread {
            parts.push("●".to_owned());
        }
        if let Some(st) = status {
            parts.push(st.to_owned());
        }
        if let Some(n) = pr {
            parts.push(format!("#{n}"));
        }
        if let Some(iss) = issue {
            parts.push(iss.to_owned());
        }
        if refs_len > 0 {
            parts.push(format!("🔗{refs_len}"));
        }
        return Some(parts.join(" "));
    }
    None
}

/// 세션이 오케스트레이션 워커 또는 지휘자로 등록되어 있으면 배지를 돌려준다(MK-114, MK-118).
pub fn session_orch_badge(sidebar_path: Option<&Path>, session_id: u64) -> Option<String> {
    let dir = sidebar_path
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())?;
    let orch = krill_proto::orchestration::OrchestrationStore::load_cached(dir);
    if let Some(disp) = orch
        .dispatches
        .iter()
        .rev()
        .find(|d| d.session_id == Some(session_id))
    {
        return Some(format!("⚡ {}", disp.worker_name));
    }
    if orch
        .runs
        .iter()
        .any(|r| r.coordinator_session_id == Some(session_id))
    {
        // AS-26: 사용자 노출 배지는 카탈로그 키로(한국어 UI에서 영어 노출 금지).
        return Some(crate::i18n::catalog::sidebar::COORD_BADGE.get().to_owned());
    }
    None
}

fn path_aliases(worktree: &Path) -> Vec<String> {
    let mut out = vec![worktree.to_string_lossy().into_owned()];
    if let Ok(rest) = worktree.strip_prefix("/private") {
        out.push(Path::new("/").join(rest).to_string_lossy().into_owned());
    } else if let Ok(rest) = worktree.strip_prefix("/") {
        out.push(
            Path::new("/private")
                .join(rest)
                .to_string_lossy()
                .into_owned(),
        );
    }
    out
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 상한 도달 시 가장 오래된 기록(시각 없음은 0, 동점은 경로 사전순)의 키를 고른다.
fn oldest_key(store: &Store) -> Option<String> {
    store
        .comments
        .keys()
        .min_by(|a, b| {
            let ta = store.updated_at.get(*a).copied().unwrap_or(0);
            let tb = store.updated_at.get(*b).copied().unwrap_or(0);
            ta.cmp(&tb).then_with(|| a.cmp(b))
        })
        .cloned()
}

/// 사이드바 설정 파일이나 데몬 소켓 경로에서 상태 디렉터리를 찾는다.
pub fn state_dir_from(sidebar_path: Option<&Path>, socket_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = sidebar_path
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Some(dir.to_path_buf());
    }
    if let Some(dir) = socket_path
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Some(dir.to_path_buf());
    }
    krill_proto::daemon::state_dir()
}

/// 사이드바 설정 파일(`sidebar.bin`)이 있는 디렉터리에서 워크트리 코멘트를 읽는다.
pub fn load_comments_from_sidebar_path(sidebar_path: Option<&Path>) -> BTreeMap<PathBuf, String> {
    sidebar_path
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
        .map(load_comments)
        .unwrap_or_default()
}

/// 상태 디렉터리의 `worktree-comments.toml`에서 워크트리별 코멘트를 읽는다.
pub fn load_comments(state_dir: &Path) -> BTreeMap<PathBuf, String> {
    let mut bytes = Vec::new();
    let Ok(file) = fs::File::open(state_dir.join(COMMENTS_FILE)) else {
        return BTreeMap::new();
    };
    if file
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || u64::try_from(bytes.len()).map_or(true, |n| n > MAX_FILE_BYTES)
    {
        return BTreeMap::new();
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return BTreeMap::new();
    };
    let store: Store = toml::from_str(text).unwrap_or_default();
    store
        .comments
        .into_iter()
        .filter_map(|(k, v)| {
            let clean = sanitize(&v);
            (!clean.is_empty()).then(|| (PathBuf::from(k), clean))
        })
        .take(MAX_COMMENTS)
        .collect()
}

/// 테스트·내부 호출에서 워크트리 코멘트를 상태 디렉터리에 기록한다.
/// 시각 기록(`updated_at`)을 함께 읽어 상한 도달 시 가장 오래된 항목을 지운다.
pub fn save_comment(state_dir: &Path, worktree: &Path, comment: &str) -> io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state_dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(state_dir.join(LOCK_FILE))?;
    lock.lock()?;
    let file = state_dir.join(COMMENTS_FILE);
    let mut store: Store = match fs::read_to_string(&file) {
        Ok(body) => toml::from_str(&body).map_err(io::Error::other)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Store::default(),
        Err(e) => return Err(e),
    };
    // 손상된 값·시각이 어긋난 항목을 정리한다(표시 경로와 같은 sanitize 기준).
    store.comments.retain(|_, v| !sanitize(v).is_empty());
    store
        .updated_at
        .retain(|k, _| store.comments.contains_key(k));
    let resolved = fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    let key = resolved.to_string_lossy().into_owned();
    let clean = sanitize(comment);
    if clean.is_empty() {
        store.comments.remove(&key);
        store.updated_at.remove(&key);
    } else {
        if store.comments.len() >= MAX_COMMENTS
            && !store.comments.contains_key(&key)
            && let Some(oldest) = oldest_key(&store)
        {
            store.comments.remove(&oldest);
            store.updated_at.remove(&oldest);
        }
        store.comments.insert(key.clone(), clean);
        store.updated_at.insert(key, now_secs());
    }
    let body = krill_proto::config_document::save_toml(&file, &store)?;
    let tmp = file.with_extension("toml.tmp");
    let mut out = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    out.write_all(body.as_bytes())?;
    drop(out);
    fs::rename(tmp, file)
}

/// 워크트리 경로로 등록된 코멘트를 찾는다.
/// 부모 저장소(`/repo`)와 중첩 워크트리(`/repo/.claude/worktrees/...`) 간에 코멘트가 섞이지 않도록
/// 조상·자손 접두사 매칭은 하지 않고, 정확히 같은 경로(macOS `/private` 심링크 별칭 포함)만 매칭한다.
pub fn comment_for<'a>(
    comments: &'a BTreeMap<PathBuf, String>,
    worktree: &Path,
) -> Option<&'a str> {
    if let Some(exact) = comments.get(worktree).filter(|s| !s.is_empty()) {
        return Some(exact.as_str());
    }
    if let Ok(rest) = worktree.strip_prefix("/private") {
        let alias = Path::new("/").join(rest);
        if let Some(found) = comments.get(&alias).filter(|s| !s.is_empty()) {
            return Some(found.as_str());
        }
    } else if let Ok(rest) = worktree.strip_prefix("/") {
        let alias = Path::new("/private").join(rest);
        if let Some(found) = comments.get(&alias).filter(|s| !s.is_empty()) {
            return Some(found.as_str());
        }
    }
    None
}

fn sanitize(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !c.is_control())
        .collect();
    mapped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_COMMENT_CHARS)
        .collect()
}

/// 경로 구성 요소에 `.claude/worktrees`가 들어 있는지 판정한다.
pub fn is_claude_worktree(path: &Path) -> bool {
    let mut comps = path.components().peekable();
    while let Some(c) = comps.next() {
        if c.as_os_str() == ".claude"
            && comps
                .peek()
                .is_some_and(|next| next.as_os_str() == "worktrees")
        {
            return true;
        }
    }
    false
}

/// 정렬된 워크트리 목록을 일반 워크트리와 `.claude/worktrees` 외부 워크트리로 가른다.
/// 주 저장소 루트(`repo`) 자신은 숨기지 않는다.
pub fn partition_trees<'a>(
    repo: &Path,
    trees: Vec<&'a PathBuf>,
) -> (Vec<&'a PathBuf>, Vec<&'a PathBuf>) {
    trees
        .into_iter()
        .partition(|tree| tree.as_path() == repo || !is_claude_worktree(tree))
}

/// `<repo>/.claude/worktrees` 아래에 있는 디렉터리도 스냅샷에 보탠다(이미 있으면 중복 추가하지 않음).
/// AS-09: 디렉터리 자체의 mtime이 그대로면 재스캔하지 않는다. `apply`가 0.5초마다 UI 스레드에서
/// 부르고 백그라운드 조회가 같은 함수를 이미 실행하므로, 매번 `read_dir`+`canonicalize`(최대 64개)를
/// 반복하면 네트워크·외장 볼륨에서 UI가 멈춘다. 새 워크트리는 부모 mtime을 바꾸므로 발견이 늦어지지 않는다.
pub fn include_claude_worktrees(trees: &mut BTreeMap<PathBuf, Vec<PathBuf>>) {
    for (repo, list) in trees.iter_mut() {
        for path in discover_claude_worktrees(repo) {
            if !list.contains(&path) {
                list.push(path);
            }
        }
    }
}

pub const PROJECTS_FILE: &str = "projects.toml";
const MAX_PROJECTS_BYTES: u64 = 512 * 1024;

#[derive(Debug, Default, Deserialize)]
struct ImportedProjectsFile {
    #[serde(default)]
    projects: Vec<ImportedProject>,
    #[serde(default)]
    worktrees: BTreeMap<String, ImportedWorktreeMeta>,
    #[serde(default)]
    lineage: BTreeMap<String, ImportedWorktreeLineage>,
}

#[derive(Debug, Default, Deserialize)]
struct ImportedProject {
    #[serde(default)]
    id: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    host_id: Option<String>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    archived: bool,
}

#[derive(Debug, Default, Deserialize)]
struct ImportedWorktreeMeta {
    #[serde(default)]
    repo_id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    workspace_status: Option<String>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    issue: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ImportedWorktreeLineage {
    #[serde(default)]
    repo_id: Option<String>,
    #[serde(default)]
    worktree_path: String,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    parent_worktree_path: Option<String>,
}

fn load_imported_projects(state_dir: &Path) -> Option<ImportedProjectsFile> {
    let mut bytes = Vec::new();
    let file = fs::File::open(state_dir.join(PROJECTS_FILE)).ok()?;
    if file
        .take(MAX_PROJECTS_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || u64::try_from(bytes.len()).map_or(true, |n| n > MAX_PROJECTS_BYTES)
    {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    toml::from_str(text).ok()
}

fn resolve_repo_for_worktree(
    projects: &[ImportedProject],
    repo_id: Option<&str>,
    wt_path: &Path,
) -> PathBuf {
    if let Some(id) = repo_id
        && let Some(proj) = projects
            .iter()
            .find(|p| !p.archived && p.host_id.is_none() && p.id == id)
    {
        return PathBuf::from(proj.path.trim());
    }
    if let Some(proj) = projects.iter().find(|p| {
        !p.archived
            && p.host_id.is_none()
            && !p.path.trim().is_empty()
            && (wt_path == Path::new(p.path.trim()) || wt_path.starts_with(p.path.trim()))
    }) {
        return PathBuf::from(proj.path.trim());
    }
    wt_path.to_path_buf()
}

/// `<state_dir>/projects.toml`(Orca 이관 프로젝트·워크트리·계보)을 사이드바 스냅샷 트리와 브랜치 메타에 병합한다.
pub fn include_imported_projects(
    state_dir: &Path,
    trees: &mut BTreeMap<PathBuf, Vec<PathBuf>>,
    meta: &mut super::Meta,
) {
    let Some(file) = load_imported_projects(state_dir) else {
        return;
    };
    for proj in &file.projects {
        if proj.archived || proj.host_id.is_some() || proj.path.trim().is_empty() {
            continue;
        }
        let repo = PathBuf::from(proj.path.trim());
        let list = trees.entry(repo.clone()).or_default();
        if !list.contains(&repo) {
            list.push(repo);
        }
    }
    for (wt_key, wt) in &file.worktrees {
        if wt.archived || wt_key.trim().is_empty() {
            continue;
        }
        let wt_path = PathBuf::from(wt_key.trim());
        let repo = resolve_repo_for_worktree(&file.projects, wt.repo_id.as_deref(), &wt_path);
        let list = trees.entry(repo).or_default();
        if !list.contains(&wt_path) {
            list.push(wt_path);
        }
    }
    for (lin_key, lin) in &file.lineage {
        let wt_str = if lin.worktree_path.trim().is_empty() {
            lin_key.trim()
        } else {
            lin.worktree_path.trim()
        };
        if wt_str.is_empty() {
            continue;
        }
        let wt_path = PathBuf::from(wt_str);
        let repo = resolve_repo_for_worktree(&file.projects, lin.repo_id.as_deref(), &wt_path);
        let list = trees.entry(repo).or_default();
        if !list.contains(&wt_path) {
            list.push(wt_path.clone());
        }
        if let Some(ref branch) = lin.branch
            && !branch.trim().is_empty()
        {
            let entry = meta.entry(wt_path).or_default();
            if entry.branch.is_none() {
                entry.branch = Some(branch.trim().to_owned());
            }
        }
    }
}

/// `<state_dir>/projects.toml`의 표시 이름·고정 여부·코멘트·이슈·상태·계보(부모 워크트리)를
/// 사이드바 인메모리 상태에 반영한다.
///
/// `pinned`와 `display_name`은 `prefs`에 해당 경로 엔트리가 아직 없을 때(`Entry::Vacant`)만
/// 초기값으로 주입해, 이미 사용자가 수정한 `sidebar.bin` 설정(핀 해제·이름 초기화)을 덮어쓰지 않는다(`OWE-767`).
pub(super) fn apply_imported_project_prefs(
    state_dir: &Path,
    prefs: &mut super::Preferences,
    comments: &mut BTreeMap<PathBuf, String>,
    issues: &mut BTreeMap<PathBuf, String>,
    statuses: &mut BTreeMap<PathBuf, krill_proto::orchestration::WorkspaceStatus>,
    parents: &mut BTreeMap<PathBuf, PathBuf>,
    seeded_parents: &mut std::collections::BTreeSet<PathBuf>,
) {
    let Some(file) = load_imported_projects(state_dir) else {
        return;
    };
    for proj in &file.projects {
        if proj.archived || proj.host_id.is_some() || proj.path.trim().is_empty() {
            continue;
        }
        let repo = PathBuf::from(proj.path.trim());
        let dn = proj.display_name.trim();
        if (!dn.is_empty() || proj.pinned)
            && let std::collections::btree_map::Entry::Vacant(vacant) = prefs.entry(repo)
        {
            vacant.insert(super::Preference {
                pinned: proj.pinned,
                name: dn.to_owned(),
            });
        }
    }
    for (wt_key, wt) in &file.worktrees {
        if wt.archived || wt_key.trim().is_empty() {
            continue;
        }
        let wt_path = PathBuf::from(wt_key.trim());
        let dn = wt
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if (dn.is_some() || wt.pinned)
            && let std::collections::btree_map::Entry::Vacant(vacant) = prefs.entry(wt_path.clone())
        {
            vacant.insert(super::Preference {
                pinned: wt.pinned,
                name: dn.unwrap_or_default().to_owned(),
            });
        }
        if let Some(ref c) = wt.comment {
            let clean = sanitize(c);
            if !clean.is_empty() {
                comments.entry(wt_path.clone()).or_insert(clean);
            }
        }
        if let Some(ref iss) = wt.issue
            && !iss.trim().is_empty()
        {
            issues
                .entry(wt_path.clone())
                .or_insert_with(|| iss.trim().to_owned());
        }
        if let Some(ref st_str) = wt.workspace_status
            && let Ok(st) = krill_proto::orchestration::WorkspaceStatus::parse(st_str)
        {
            statuses.entry(wt_path).or_insert(st);
        }
    }
    for (lin_key, lin) in &file.lineage {
        let wt_str = if lin.worktree_path.trim().is_empty() {
            lin_key.trim()
        } else {
            lin.worktree_path.trim()
        };
        let Some(parent_str) = lin
            .parent_worktree_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if wt_str.is_empty() {
            continue;
        }
        let child = PathBuf::from(wt_str);
        let parent = PathBuf::from(parent_str);
        if seeded_parents.insert(child.clone())
            && !parents.contains_key(&child)
            && super::lineage::can_assign_parent(parents, &child, &parent)
        {
            parents.insert(child, parent);
        }
    }
}

/// 디렉터리 mtime별 발견 목록 캐시(스냅샷 폴링마다 디렉터리 IO를 반복하지 않기 위해).
type WorktreeDiscovery = (Option<SystemTime>, Vec<PathBuf>);
static WORKTREE_CACHE: LazyLock<Mutex<BTreeMap<PathBuf, WorktreeDiscovery>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn discover_claude_worktrees(repo: &Path) -> Vec<PathBuf> {
    let dir = repo.join(".claude/worktrees");
    let mtime = fs::metadata(&dir).ok().and_then(|m| m.modified().ok());
    if let Ok(cache) = WORKTREE_CACHE.lock()
        && let Some((cached_mtime, cached)) = cache.get(repo)
        && *cached_mtime == mtime
    {
        return cached.clone();
    }
    let mut discovered: Vec<PathBuf> = fs::read_dir(&dir).map_or(Vec::new(), |entries| {
        entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .take(MAX_CLAUDE_WORKTREES)
            .map(|e| {
                let p = e.path();
                fs::canonicalize(&p).unwrap_or(p)
            })
            .collect()
    });
    discovered.sort();
    if let Ok(mut cache) = WORKTREE_CACHE.lock() {
        cache.insert(repo.to_path_buf(), (mtime, discovered.clone()));
    }
    discovered
}

/// 외부 에이전트 한 줄: 사이드바 표시 상태 + 프로세스 id(AS-08 계보 판정용).
/// pid가 없으면 `stop` 뒤 잔류형이라 계보를 확인할 수 없다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalHit {
    pub state: krill_proto::control::AgentState,
    pub pid: Option<u32>,
}

/// AS-07: Finder·Dock 실행(launchd 기본 PATH)에서도 외부 `claude`를 찾는다.
/// `KRILL_CLAUDE_BIN` 지정 > 사용자 PATH(`krill_proto::agent::which`) > 설치 위치
/// (`~/.local/bin` 실측, Homebrew) 순이다. 없으면 `"claude"`를 그대로 돌려줘
/// 기존처럼 조용히 fail-open한다.
pub fn resolve_claude_bin(explicit: Option<&str>, path_env: &OsStr, home: &Path) -> PathBuf {
    if let Some(bin) = explicit.filter(|s| !s.is_empty()) {
        return PathBuf::from(bin);
    }
    if let Some(found) = krill_proto::agent::which("claude", path_env) {
        return found;
    }
    for dir in [
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        let candidate = dir.join("claude");
        if is_executable(&candidate) {
            return candidate;
        }
    }
    PathBuf::from("claude")
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `ps -eo pid,ppid` 출력 파서(pid → ppid). 헤더·깨진 행은 건너뛴다.
pub fn parse_ps_parents(text: &str) -> BTreeMap<u32, u32> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid: u32 = parts.next()?.parse().ok()?;
            let ppid: u32 = parts.next()?.parse().ok()?;
            (pid != 0).then_some((pid, ppid))
        })
        .collect()
}

/// 현재 프로세스 표(pid → ppid). 실패하면 `None`(호출자는 기존 cwd 동작으로 fail-open).
pub fn parent_map() -> Option<BTreeMap<u32, u32>> {
    let out = std::process::Command::new("ps")
        .args(["-eo", "pid,ppid"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    std::str::from_utf8(&out.stdout).ok().map(parse_ps_parents)
}

/// AS-08: 외부 에이전트 pid가 세션 루트 pid 자신이거나 후손이면 참.
/// 외부에서(다른 터미널·웹) 띄운 에이전트를 cwd만 같다고 무관한 셸에 덮어씌우지 않는다.
/// pid가 없으면(잔류형) 계보를 확인할 수 없어 참으로 둔다(기존 cwd 동작).
/// 프로세스 표가 없으면(`ps` 실패) 조용히 fail-open한다.
pub fn linked_to_session(
    ext_pid: Option<u32>,
    session_pid: u32,
    parents: Option<&BTreeMap<u32, u32>>,
) -> bool {
    let Some(pid) = ext_pid else {
        return true;
    };
    if pid == session_pid {
        return true;
    }
    let Some(parents) = parents else {
        return true;
    };
    let mut current = pid;
    for _ in 0..64 {
        let Some(&parent) = parents.get(&current) else {
            return false;
        };
        if parent == session_pid {
            return true;
        }
        if parent == current || parent == 0 {
            return false;
        }
        current = parent;
    }
    false
}

/// 스크린 리더용 워크트리 행 레이블을 조합한다.
pub fn worktree_a11y_label(title: &str, name: &str, comment: Option<&str>) -> String {
    match comment {
        Some(c) => format!("{title} {name}, {c}"),
        None => format!("{title} {name}"),
    }
}

/// 워크트리 행의 코멘트 배지 요소를 만든다(UI-2 사이드바 재설계 대비 단일 호출 지점).
pub fn render_comment(comment: Option<&str>, color: Hsla) -> Option<Div> {
    comment.map(|c| {
        div()
            .w_full()
            .min_w_0()
            .truncate()
            .text_size(crate::ui::TEXT_XS)
            .text_color(color)
            .child(c.to_owned())
    })
}

/// `.claude/worktrees 숨김 (N)` 접기/펼치기 카드 행을 렌더링한다.
pub fn render_hidden_row(
    base: Stateful<Div>,
    repo: &Path,
    count: usize,
    expanded: bool,
    cx: &mut Context<Sidebar>,
) -> Stateful<Div> {
    use gpui_component::{Icon, IconName, Sizable as _};
    let p = crate::ui::palette(cx);
    let repo = repo.to_path_buf();
    let a11y_label = format!(
        "{} ({count})",
        crate::i18n::catalog::general::HIDDEN_WORKTREES.get()
    );
    let display_text =
        match (expanded, count) {
            (true, 1) => crate::i18n::catalog::sidebar::SHOWING_DISCOVERED_ONE
                .get()
                .to_owned(),
            (true, _) => crate::i18n::catalog::sidebar::SHOWING_DISCOVERED
                .format(&[("n", &count.to_string())]),
            (false, 1) => crate::i18n::catalog::sidebar::HIDING_DISCOVERED_ONE
                .get()
                .to_owned(),
            (false, _) => crate::i18n::catalog::sidebar::HIDING_DISCOVERED
                .format(&[("n", &count.to_string())]),
        };
    base.aria_label(a11y_label)
        .aria_expanded(expanded)
        .h(px(26.))
        .px_2p5()
        .mx_1p5()
        .cursor_pointer()
        .rounded(crate::ui::RADIUS_SM)
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(crate::ui::TEXT_XS)
        .text_color(p.muted_foreground.opacity(0.75))
        .hover(|d| {
            d.bg(p.sidebar_accent.opacity(0.35))
                .text_color(p.sidebar_foreground)
        })
        .child(
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .with_size(px(12.))
            .text_color(p.muted_foreground.opacity(0.65)),
        )
        .child(div().flex_1().min_w_0().truncate().child(display_text))
        .on_click(cx.listener(move |s, _, _, cx| s.toggle_external(&repo, cx)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::orchestration::{OrchestrationStore, WorktreeStore};

    /// AS-07: Finder·Dock 실행처럼 PATH에 `~/.local/bin`이 없어도 외부 `claude`를 찾는다.
    #[test]
    fn resolve_claude_bin_prefers_override_then_path_then_known_spots() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = std::env::temp_dir().join(format!("mk-claude-bin-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let local_bin = home.join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        let local_claude = local_bin.join("claude");
        std::fs::write(&local_claude, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&local_claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let empty_path = std::ffi::OsStr::new("/nonexistent");

        // 명시 지정이 최우선.
        assert_eq!(
            resolve_claude_bin(Some("/custom/claude"), empty_path, &home),
            PathBuf::from("/custom/claude")
        );
        // PATH에 없어도 `~/.local/bin`을 찾는다(launchd 기본 PATH 대응).
        assert_eq!(resolve_claude_bin(None, empty_path, &home), local_claude);
        // PATH에 있으면 그걸 쓴다.
        let path_with = std::env::join_paths([local_bin.as_os_str(), empty_path]).unwrap();
        assert_eq!(
            resolve_claude_bin(None, &path_with, Path::new("/nonexistent-home")),
            local_claude
        );
        // 아무 데도 없으면 기존처럼 `"claude"`로 조용히 fail-open.
        assert_eq!(
            resolve_claude_bin(None, empty_path, Path::new("/nonexistent-home")),
            PathBuf::from("claude")
        );
        std::fs::remove_dir_all(&home).ok();
    }

    /// AS-08: pid 계보 판정. 외부에서 띄운 에이전트는 무관한 세션에 묶지 않는다.
    #[test]
    fn linked_to_session_checks_process_kinship() {
        let parents: BTreeMap<u32, u32> = BTreeMap::from([(10, 5), (5, 1), (20, 1), (1, 0)]);
        // pid가 없으면(잔류형) 기존 cwd 동작 유지.
        assert!(linked_to_session(None, 999, Some(&parents)));
        // 세션 루트 자신이거나 후손이면 연결.
        assert!(linked_to_session(Some(5), 5, Some(&parents)));
        assert!(linked_to_session(Some(10), 5, Some(&parents)));
        // 같은 조상(1)을 공유해도 세션(5)의 자손이 아니면 끊는다.
        assert!(!linked_to_session(Some(20), 5, Some(&parents)));
        assert!(!linked_to_session(Some(999), 5, Some(&parents)));
        // 프로세스 표가 없으면 조용히 fail-open.
        assert!(linked_to_session(Some(20), 5, None));
    }

    /// AS-08: `ps` 출력 파서(헤더·깨진 행 무시).
    #[test]
    fn parse_ps_parents_skips_header_and_broken_rows() {
        let parsed = parse_ps_parents("  PID  PPID\n    1     0\n  424     1\nbroken\n  777\n");
        assert_eq!(parsed, BTreeMap::from([(1, 0), (424, 1)]));
    }

    /// AS-09: mtime이 그대로면 재스캔하지 않고, 새 디렉터리는 mtime이 바뀌어 발견된다.
    #[test]
    fn claude_worktrees_cached_by_mtime() -> io::Result<()> {
        let repo = std::env::temp_dir().join(format!("mk-claude-wt-{}", std::process::id()));
        std::fs::remove_dir_all(&repo).ok();
        let dir = repo.join(".claude/worktrees");
        std::fs::create_dir_all(&dir)?;
        let mut trees: BTreeMap<PathBuf, Vec<PathBuf>> =
            BTreeMap::from([(repo.clone(), Vec::new())]);
        include_claude_worktrees(&mut trees);
        assert!(trees[&repo].is_empty());
        // 새 워크트리를 만들면 다음 폴링에 보인다.
        std::fs::create_dir_all(dir.join("wt-1"))?;
        include_claude_worktrees(&mut trees);
        assert_eq!(trees[&repo].len(), 1, "{trees:?}");
        // mtime이 그대로면 캐시로 같은 결과가 나온다.
        include_claude_worktrees(&mut trees);
        assert_eq!(trees[&repo].len(), 1, "{trees:?}");
        std::fs::remove_dir_all(&repo).ok();
        Ok(())
    }

    #[test]
    fn checkpoint_badge_reads_persisted_fields_and_private_path_aliases() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "mk-sidebar-checkpoint-badge-{}",
            std::process::id()
        ));
        let sidebar = dir.join("sidebar.bin");
        let full = "/tmp/minikrill-review";
        let store = WorktreeStore {
            comments: BTreeMap::from([("/repo/comment-only".to_owned(), "Review me".to_owned())]),
            statuses: BTreeMap::from([
                (full.to_owned(), "in-review".to_owned()),
                ("/repo/todo".to_owned(), "todo".to_owned()),
                ("/repo/wip".to_owned(), "in-progress".to_owned()),
                ("/repo/done".to_owned(), "completed".to_owned()),
                ("/repo/invalid".to_owned(), "unknown".to_owned()),
                ("/repo/linked".to_owned(), "unknown".to_owned()),
            ]),
            unread: BTreeMap::from([
                (full.to_owned(), true),
                ("/repo/unread".to_owned(), true),
                ("/repo/read".to_owned(), false),
            ]),
            prs: BTreeMap::from([(full.to_owned(), 128)]),
            issues: BTreeMap::from([
                (full.to_owned(), "OWE-626".to_owned()),
                ("/repo/linked".to_owned(), "OWE-627".to_owned()),
            ]),
            references: BTreeMap::from([(
                full.to_owned(),
                vec![
                    "https://example.com/design".to_owned(),
                    "https://example.com/review".to_owned(),
                ],
            )]),
            ..WorktreeStore::default()
        };
        store.save(&dir)?;
        assert!(dir.join(COMMENTS_FILE).is_file());

        assert_eq!(
            checkpoint_badge_for(Some(&sidebar), Path::new(full)).as_deref(),
            Some("● REVIEW #128 OWE-626 🔗2")
        );
        assert_eq!(
            checkpoint_badge_for(Some(&sidebar), Path::new("/private/tmp/minikrill-review"))
                .as_deref(),
            Some("● REVIEW #128 OWE-626 🔗2")
        );
        for (path, badge) in [
            ("/repo/todo", "TODO"),
            ("/repo/wip", "WIP"),
            ("/repo/done", "DONE"),
            ("/repo/unread", "●"),
            ("/repo/linked", "OWE-627"),
        ] {
            assert_eq!(
                checkpoint_badge_for(Some(&sidebar), Path::new(path)).as_deref(),
                Some(badge)
            );
        }
        for path in [
            "/repo/missing",
            "/repo/read",
            "/repo/comment-only",
            "/repo/invalid",
            "/tmp/minikrill-review/child",
        ] {
            assert_eq!(checkpoint_badge_for(Some(&sidebar), Path::new(path)), None);
        }
        assert_eq!(checkpoint_badge_for(None, Path::new(full)), None);
        assert_eq!(
            checkpoint_badge_for(Some(Path::new("sidebar.bin")), Path::new(full)),
            None
        );
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn session_orch_badge_reads_latest_worker_and_coordinator_from_disk() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "mk-sidebar-orchestration-badge-{}",
            std::process::id()
        ));
        let sidebar = dir.join("sidebar.bin");
        let mut store = OrchestrationStore::default();
        let run = store.create_run("Badge run", Some(1))?;
        store.create_run("Coordinator only", Some(2))?;
        store.start_worker(
            &run.id,
            None,
            "older-worker",
            "claude",
            Some(10),
            "/tmp",
            "First dispatch",
        )?;
        store.start_worker(
            &run.id,
            None,
            "latest-worker",
            "codex",
            Some(10),
            "/tmp",
            "Latest dispatch",
        )?;
        store.start_worker(
            &run.id,
            None,
            "coordinator-worker",
            "claude",
            Some(1),
            "/tmp",
            "Worker badge takes precedence",
        )?;
        store.save(&dir)?;
        assert!(dir.join(krill_proto::orchestration::ORCH_FILE).is_file());

        assert_eq!(
            session_orch_badge(Some(&sidebar), 10).as_deref(),
            Some("⚡ latest-worker")
        );
        assert_eq!(
            session_orch_badge(Some(&sidebar), 1).as_deref(),
            Some("⚡ coordinator-worker")
        );
        assert_eq!(
            session_orch_badge(Some(&sidebar), 2).as_deref(),
            Some(crate::i18n::catalog::sidebar::COORD_BADGE.get()),
            "지휘자 배지도 카탈로그 키로 보인다(AS-26)"
        );
        assert_eq!(session_orch_badge(Some(&sidebar), 999), None);
        assert_eq!(session_orch_badge(None, 10), None);
        assert_eq!(session_orch_badge(Some(Path::new("sidebar.bin")), 10), None);
        assert_eq!(
            session_orch_badge(Some(&dir.join("missing/sidebar.bin")), 10),
            None
        );
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}

#[cfg(test)]
#[test]
fn as12_sidebar_comment_save_preserves_future_fields() {
    let dir = std::env::temp_dir().join(format!("as12-sidebar-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(COMMENTS_FILE);
    fs::write(
        &path,
        "# keep\nfuture = { mode = 'v2' }\n[comments]\n'/work' = 'old'\n",
    )
    .unwrap();
    save_comment(&dir, Path::new("/work"), "edited").unwrap();
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.contains("# keep"));
    let value: toml::Value = toml::from_str(&body).unwrap();
    assert_eq!(value["future"]["mode"].as_str(), Some("v2"));
    assert_eq!(value["comments"]["/work"].as_str(), Some("edited"));
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(test)]
#[test]
fn owe767_imported_prefs_do_not_clobber_unpin_or_cleared_name_and_seed_lineage_parent() {
    let dir = std::env::temp_dir().join(format!("owe767-sidebar-prefs-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(PROJECTS_FILE),
        r#"
[[projects]]
id = "repo-1"
path = "/Users/test/repo"
display_name = "Imported Repo"
kind = "git"
pinned = true

[worktrees."/Users/test/repo-wt-1"]
repo_id = "repo-1"
display_name = "Imported Worktree"
pinned = true

[lineage."/Users/test/repo-wt-1"]
repo_id = "repo-1"
worktree_path = "/Users/test/repo-wt-1"
branch = "feat/owe-767"
parent_worktree_path = "/Users/test/repo"
"#,
    )
    .unwrap();

    let repo = PathBuf::from("/Users/test/repo");
    let wt = PathBuf::from("/Users/test/repo-wt-1");
    let mut prefs = super::Preferences::default();
    let mut comments = BTreeMap::new();
    let mut issues = BTreeMap::new();
    let mut statuses = BTreeMap::new();
    let mut parents = BTreeMap::new();
    let mut seeded_parents = std::collections::BTreeSet::new();

    // First sync seeds vacant prefs and lineage parent
    apply_imported_project_prefs(
        &dir,
        &mut prefs,
        &mut comments,
        &mut issues,
        &mut statuses,
        &mut parents,
        &mut seeded_parents,
    );
    assert_eq!(
        prefs.get(&repo),
        Some(&super::Preference {
            pinned: true,
            name: "Imported Repo".to_owned(),
        })
    );
    assert_eq!(
        prefs.get(&wt),
        Some(&super::Preference {
            pinned: true,
            name: "Imported Worktree".to_owned(),
        })
    );
    assert_eq!(parents.get(&wt), Some(&repo));

    // User unpins and clears custom names (as Sidebar::toggle_pin and Sidebar::finish_rename do),
    // and unlinks the worktree parent
    prefs.get_mut(&repo).unwrap().pinned = false;
    prefs.get_mut(&repo).unwrap().name.clear();
    prefs.get_mut(&wt).unwrap().pinned = false;
    prefs.get_mut(&wt).unwrap().name.clear();
    parents.remove(&wt);

    // Subsequent polling sync must NOT overwrite the occupied prefs or re-insert cleared parent
    apply_imported_project_prefs(
        &dir,
        &mut prefs,
        &mut comments,
        &mut issues,
        &mut statuses,
        &mut parents,
        &mut seeded_parents,
    );
    assert_eq!(
        prefs.get(&repo),
        Some(&super::Preference {
            pinned: false,
            name: String::new(),
        })
    );
    assert_eq!(
        prefs.get(&wt),
        Some(&super::Preference {
            pinned: false,
            name: String::new(),
        })
    );
    assert_eq!(parents.get(&wt), None);

    fs::remove_dir_all(dir).unwrap();
}
