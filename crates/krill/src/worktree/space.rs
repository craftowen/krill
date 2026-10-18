//! 워크트리 디스크 용량 스캔·클린업 엔진(MK-175, OWE-729 → OWE-744에서 `krill-proto`에서 이동).
//!
//! 파일시스템 순회·git·데몬 소켓 호출은 이 CLI 모듈에만 둔다. 직렬화 타입과 순수 함수는
//! `krill_proto::worktree_space`, 앱은 `krill worktree space|cleanup --json`을 하위 프로세스로 호출한다.

use krill_proto::os::ffi::{OsStrExt as _, OsStringExt as _};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use krill_proto::control::{
    FEAT_CONTROL, FEAT_WORKTREE_REMOVE, RemoveWorktree, RemovedWorktree, Reply, Request,
};
use krill_proto::daemon::{self, Paths};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;
use krill_proto::worktree_space::{
    ArtifactUsage, CleanupItemOutcome, CleanupPrState, CleanupSummary, SECS_PER_DAY, SpaceReport,
    WorktreeCleanupCandidate, WorktreeSpaceEntry, is_artifact_dir_name,
};
use serde::{Deserialize, Serialize};

/// 단일 워크트리 스캔 시 최대 탐색 깊이(심링크 비추적 + 무한 재귀 방지).
const MAX_SCAN_DEPTH: usize = 64;
/// 단일 워크트리 스캔 시 최대 방문 엔트리 수 상한.
const MAX_SCAN_ENTRIES: usize = 250_000;

#[derive(Debug, Default, Serialize, Deserialize)]
struct MetadataStore {
    #[serde(default)]
    updated_at: BTreeMap<String, u64>,
    #[serde(default)]
    statuses: BTreeMap<String, String>,
    #[serde(default)]
    prs: BTreeMap<String, u64>,
    #[serde(default)]
    pr_states: BTreeMap<String, String>,
}

/// 현재 UNIX 초를 돌려준다.
#[must_use]
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 단일 워크트리 디렉터리를 순회하여 전체 용량과 빌드 아티팩트 용량을 계산한다.
#[must_use]
pub fn scan_worktree_space(
    repo: &Path,
    worktree: &Path,
    branch: Option<String>,
    is_main: bool,
) -> WorktreeSpaceEntry {
    let repo_name = repo.file_name().map_or_else(
        || repo.display().to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    let name = worktree.file_name().map_or_else(
        || worktree.display().to_string(),
        |s| s.to_string_lossy().into_owned(),
    );

    let mut total_bytes: u64 = 0;
    let mut artifact_bytes: u64 = 0;
    let mut by_artifact: BTreeMap<String, (u64, String, usize)> = BTreeMap::new();
    let mut visited: usize = 0;

    walk_space(
        worktree,
        worktree,
        0,
        None,
        &mut visited,
        &mut total_bytes,
        &mut artifact_bytes,
        &mut by_artifact,
    );

    let mut artifacts: Vec<ArtifactUsage> = by_artifact
        .into_iter()
        .filter(|(_, (bytes, _, _))| *bytes > 0)
        .map(|(art_name, (bytes, rel_path, count))| ArtifactUsage {
            name: art_name,
            rel_path,
            bytes,
            count,
        })
        .collect();
    artifacts.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));

    WorktreeSpaceEntry {
        repo: repo.to_path_buf(),
        repo_name,
        path: worktree.to_path_buf(),
        name,
        branch,
        is_main,
        total_bytes,
        artifact_bytes,
        artifacts,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "재귀 디렉터리 순회 상태(깊이·예산·총합·아티팩트 맵)를 한 곳에서 전달"
)]
fn walk_space(
    root: &Path,
    current: &Path,
    depth: usize,
    active_artifact: Option<&str>,
    visited: &mut usize,
    total_bytes: &mut u64,
    artifact_bytes: &mut u64,
    by_artifact: &mut BTreeMap<String, (u64, String, usize)>,
) {
    if depth > MAX_SCAN_DEPTH || *visited >= MAX_SCAN_ENTRIES {
        return;
    }
    let Ok(read_dir) = fs::read_dir(current) else {
        return;
    };
    for entry in read_dir.flatten() {
        if *visited >= MAX_SCAN_ENTRIES {
            break;
        }
        *visited = visited.saturating_add(1);
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_file() {
            let len = meta.len();
            *total_bytes = total_bytes.saturating_add(len);
            if let Some(art_name) = active_artifact {
                *artifact_bytes = artifact_bytes.saturating_add(len);
                if let Some(slot) = by_artifact.get_mut(art_name) {
                    slot.0 = slot.0.saturating_add(len);
                }
            }
        } else if file_type.is_dir() {
            let dir_name = entry.file_name();
            let dir_name_str = dir_name.to_string_lossy();
            if dir_name_str == ".git" || dir_name_str == ".minikrill-preparing" {
                continue;
            }
            if let Some(art) = active_artifact {
                walk_space(
                    root,
                    &path,
                    depth.saturating_add(1),
                    Some(art),
                    visited,
                    total_bytes,
                    artifact_bytes,
                    by_artifact,
                );
            } else if is_artifact_dir_name(&dir_name_str) {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                let key = dir_name_str.into_owned();
                let entry_slot = by_artifact
                    .entry(key.clone())
                    .or_insert_with(|| (0, rel, 0));
                entry_slot.2 = entry_slot.2.saturating_add(1);
                walk_space(
                    root,
                    &path,
                    depth.saturating_add(1),
                    Some(&key),
                    visited,
                    total_bytes,
                    artifact_bytes,
                    by_artifact,
                );
            } else {
                walk_space(
                    root,
                    &path,
                    depth.saturating_add(1),
                    None,
                    visited,
                    total_bytes,
                    artifact_bytes,
                    by_artifact,
                );
            }
        }
    }
}

/// 주어진 저장소 목록의 모든 워크트리를 스캔하여 용량 내림차순으로 정렬된 [`SpaceReport`]를 생성한다.
#[must_use]
pub fn scan_workspace_space(repos: &[PathBuf]) -> SpaceReport {
    let mut seen_worktrees = BTreeSet::new();
    let mut worktrees = Vec::new();
    for repo in repos {
        let entries = list_repo_worktrees(repo);
        if entries.is_empty() {
            if repo.is_dir() {
                let canon = fs::canonicalize(repo).unwrap_or_else(|_| repo.clone());
                if seen_worktrees.insert(canon.clone()) {
                    let branch = read_head_branch(&canon);
                    worktrees.push(scan_worktree_space(&canon, &canon, branch, true));
                }
            }
            continue;
        }
        let main_path = entries
            .first()
            .map_or_else(|| repo.clone(), |e| e.path.clone());
        for (idx, item) in entries.into_iter().enumerate() {
            if !item.path.is_dir() || !seen_worktrees.insert(item.path.clone()) {
                continue;
            }
            worktrees.push(scan_worktree_space(
                &main_path,
                &item.path,
                item.branch,
                idx == 0,
            ));
        }
    }

    worktrees.sort_by(|a, b| {
        b.total_bytes
            .cmp(&a.total_bytes)
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut total_bytes: u64 = 0;
    let mut total_artifact_bytes: u64 = 0;
    let mut agg_artifacts: BTreeMap<String, (u64, String, usize)> = BTreeMap::new();

    for wt in &worktrees {
        total_bytes = total_bytes.saturating_add(wt.total_bytes);
        total_artifact_bytes = total_artifact_bytes.saturating_add(wt.artifact_bytes);
        for art in &wt.artifacts {
            let slot = agg_artifacts
                .entry(art.name.clone())
                .or_insert_with(|| (0, art.rel_path.clone(), 0));
            slot.0 = slot.0.saturating_add(art.bytes);
            slot.2 = slot.2.saturating_add(art.count);
        }
    }

    let mut artifacts: Vec<ArtifactUsage> = agg_artifacts
        .into_iter()
        .map(|(name, (bytes, rel_path, count))| ArtifactUsage {
            name,
            rel_path,
            bytes,
            count,
        })
        .collect();
    artifacts.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));

    SpaceReport {
        scanned_at: now_unix_secs(),
        total_bytes,
        total_artifact_bytes,
        worktrees,
        artifacts,
    }
}

/// 상태 디렉터리·환경변수·데몬에서 알려진 저장소 루트 경로들을 수집한다.
#[must_use]
pub fn discover_repositories(state_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(dir) = state_dir {
        if let Ok(raw) = fs::read_to_string(dir.join(krill_proto::orchestration::COMMENTS_FILE))
            && let Ok(store) = toml::from_str::<krill_proto::orchestration::WorktreeStore>(&raw)
        {
            for key in store
                .comments
                .keys()
                .chain(store.updated_at.keys())
                .chain(store.statuses.keys())
                .chain(store.prs.keys())
                .chain(store.issues.keys())
            {
                candidates.push(PathBuf::from(key));
            }
        }
        if let Some(meta) = load_metadata_store(dir) {
            for key in meta
                .updated_at
                .keys()
                .chain(meta.statuses.keys())
                .chain(meta.prs.keys())
                .chain(meta.pr_states.keys())
            {
                candidates.push(PathBuf::from(key));
            }
        }
        let orch = krill_proto::orchestration::OrchestrationStore::load_cached(dir);
        for disp in orch.dispatches {
            if !disp.cwd.is_empty() {
                candidates.push(PathBuf::from(disp.cwd));
            }
        }
    }

    if let Some(wt_root) = std::env::var_os("KRILL_WORKTREES_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            // 테스트 격리 상태 디렉터리가 명시적으로 주어졌을 때는 실제 HOME을 뒤지지 않는다.
            if state_dir.is_some() && state_dir != daemon::state_dir().as_deref() {
                None
            } else {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Minikrill/worktrees"))
            }
        })
        && let Ok(repos_dir) = fs::read_dir(wt_root)
    {
        for repo_entry in repos_dir.flatten() {
            let repo_dir = repo_entry.path();
            if let Ok(wts) = fs::read_dir(&repo_dir) {
                for wt_entry in wts.flatten() {
                    let p = wt_entry.path();
                    if p.join(".git").exists() {
                        candidates.push(p);
                    }
                }
            }
        }
    }

    // 실제 상태 디렉터리일 때 데몬 Sidebar 스냅샷에서도 저장소를 가져온다.
    if (state_dir.is_none() || state_dir == daemon::state_dir().as_deref())
        && let Ok(paths) = Paths::dev()
        && paths.socket.exists()
        && let Ok(mut stream) = krill_proto::os::net::UnixStream::connect(&paths.socket)
    {
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
        let hello = daemon::client_hello(ClientRole::App, "krill-space-discover");
        if let Ok(Some((kind, body))) = ipc::read_frame(&mut stream)
            && let Ok(server) = ipc::ServerHello::decode(kind, &body)
            && let Some(neg) = ipc::negotiate(&server, &hello)
            && neg.features & (FEAT_CONTROL | krill_proto::control::FEAT_SIDEBAR)
                == (FEAT_CONTROL | krill_proto::control::FEAT_SIDEBAR)
            && stream
                .write_all(&hello.encode().unwrap_or_default())
                .is_ok()
            && stream
                .write_all(&Request::Sidebar.encode().unwrap_or_default())
                .is_ok()
            && let Ok(Some((rk, rb))) = ipc::read_frame(&mut stream)
            && let Ok(Reply::Sidebar(data)) = Reply::decode(rk, &rb)
        {
            for repo in data.repositories {
                candidates.push(PathBuf::from(OsString::from_vec(repo.repo)));
            }
        }
    }

    normalize_repo_roots(&candidates)
}

/// 후보 경로들에서 고유한 주 워크트리(저장소 루트) 목록을 추출한다.
#[must_use]
pub fn normalize_repo_roots(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for path in paths {
        if !path.exists() {
            continue;
        }
        let entries = list_repo_worktrees(path);
        if let Some(main) = entries.first() {
            if seen.insert(main.path.clone()) {
                out.push(main.path.clone());
            }
        } else if path.join(".git").exists() {
            let canon = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            if seen.insert(canon.clone()) {
                out.push(canon);
            }
        }
    }
    out
}

/// 저장소 목록에서 링크된 워크트리 클린업 후보들을 탐색한다.
#[must_use]
pub fn discover_cleanup_candidates(
    repos: &[PathBuf],
    state_dir: Option<&Path>,
    now_secs: u64,
) -> Vec<WorktreeCleanupCandidate> {
    let store = state_dir.and_then(load_metadata_store).unwrap_or_default();
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();

    for repo in repos {
        let entries = list_repo_worktrees(repo);
        let Some(main) = entries.first() else {
            continue;
        };
        let main_path = main.path.clone();
        let repo_name = main_path.file_name().map_or_else(
            || main_path.display().to_string(),
            |s| s.to_string_lossy().into_owned(),
        );

        let symlinks = effective_worktree_symlinks(&main_path, state_dir);

        for (idx, entry) in entries.into_iter().enumerate() {
            let is_main = idx == 0 || entry.path == main_path;
            if is_main || !entry.path.is_dir() || !seen.insert(entry.path.clone()) {
                continue;
            }
            let name = entry.path.file_name().map_or_else(
                || entry.path.display().to_string(),
                |s| s.to_string_lossy().into_owned(),
            );
            let (dirty, dirty_count) = check_worktree_dirty(&main_path, &entry.path, &symlinks);
            let merged_into_main = entry
                .branch
                .as_deref()
                .is_some_and(|b| is_branch_merged_into_head(&main_path, b));
            let (explicit_state, pr_number) =
                lookup_pr_metadata(&store, &main_path, &entry.path, entry.branch.as_deref());
            let pr_state = match explicit_state {
                Some(st) if st != CleanupPrState::None => st,
                _ if merged_into_main => CleanupPrState::Merged,
                _ => CleanupPrState::None,
            };
            let last_active_secs = lookup_last_active_secs(&store, &entry.path, now_secs);
            let idle_days = now_secs.saturating_sub(last_active_secs) / SECS_PER_DAY;
            let space = scan_worktree_space(&main_path, &entry.path, entry.branch.clone(), false);

            candidates.push(WorktreeCleanupCandidate {
                repo: main_path.clone(),
                repo_name: repo_name.clone(),
                path: entry.path,
                name,
                branch: entry.branch,
                is_main: false,
                locked: entry.locked,
                dirty,
                dirty_count,
                merged_into_main,
                pr_state,
                pr_number,
                last_active_secs,
                idle_days,
                disk_bytes: space.total_bytes,
                artifact_bytes: space.artifact_bytes,
            });
        }
    }

    candidates.sort_by(|a, b| {
        b.disk_bytes
            .cmp(&a.disk_bytes)
            .then_with(|| b.idle_days.cmp(&a.idle_days))
            .then_with(|| a.path.cmp(&b.path))
    });
    candidates
}

/// 상태 디렉터리의 `worktree-comments.toml`에 워크트리별 PR 상태 및 마지막 활동 시각을 기록한다(테스트 시드용).
#[cfg(test)]
pub fn record_worktree_metadata(
    state_dir: &Path,
    worktree: &Path,
    pr_state: Option<CleanupPrState>,
    pr_number: Option<u64>,
    updated_at_secs: Option<u64>,
) -> std::io::Result<()> {
    let key = krill_proto::orchestration::worktree_key(worktree);
    krill_proto::orchestration::WorktreeStore::mutate(state_dir, |store| {
        if let Some(st) = pr_state {
            if st == CleanupPrState::None {
                store.pr_states.remove(&key);
            } else {
                store.pr_states.insert(key.clone(), st.as_str().to_owned());
            }
        }
        if let Some(num) = pr_number {
            store.prs.insert(key.clone(), num);
        }
        if let Some(ts) = updated_at_secs {
            store.updated_at.insert(key, ts);
        }
        Ok(())
    })
}

/// 선택된 클린업 후보 워크트리들을 일괄 삭제하고 병합된 로컬 브랜치를 안전하게 정리한다.
#[must_use]
pub fn execute_cleanup(
    selected: &[WorktreeCleanupCandidate],
    force_dirty: bool,
    paths: Option<&Paths>,
    state_dir: Option<&Path>,
) -> CleanupSummary {
    let mut summary = CleanupSummary::default();
    let inferred_state = state_dir
        .map(Path::to_path_buf)
        .or_else(|| {
            paths
                .and_then(|p| p.socket.parent())
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        })
        .or_else(|| daemon::state_dir_override().map(PathBuf::from));

    for item in selected {
        if item.is_main {
            summary.failed_count = summary.failed_count.saturating_add(1);
            summary.items.push(CleanupItemOutcome {
                path: item.path.clone(),
                branch: item.branch.clone(),
                removed: false,
                branch_deleted: false,
                freed_bytes: 0,
                warnings: Vec::new(),
                error: Some("cannot remove the main worktree".to_owned()),
            });
            continue;
        }
        if item.locked {
            summary.failed_count = summary.failed_count.saturating_add(1);
            summary.items.push(CleanupItemOutcome {
                path: item.path.clone(),
                branch: item.branch.clone(),
                removed: false,
                branch_deleted: false,
                freed_bytes: 0,
                warnings: Vec::new(),
                error: Some("worktree is locked; unlock it explicitly before removal".to_owned()),
            });
            continue;
        }
        let symlinks = effective_worktree_symlinks(&item.repo, inferred_state.as_deref());
        let live_dirty = match check_worktree_dirty_status(&item.repo, &item.path, &symlinks) {
            Ok((dirty, _)) => dirty,
            Err(err) => {
                summary.failed_count = summary.failed_count.saturating_add(1);
                summary.items.push(CleanupItemOutcome {
                    path: item.path.clone(),
                    branch: item.branch.clone(),
                    removed: false,
                    branch_deleted: false,
                    freed_bytes: 0,
                    warnings: Vec::new(),
                    error: Some(err),
                });
                continue;
            }
        };
        if (item.dirty || live_dirty) && !force_dirty {
            summary.failed_count = summary.failed_count.saturating_add(1);
            summary.items.push(CleanupItemOutcome {
                path: item.path.clone(),
                branch: item.branch.clone(),
                removed: false,
                branch_deleted: false,
                freed_bytes: 0,
                warnings: Vec::new(),
                error: Some("worktree is dirty; excluded by safety filter".to_owned()),
            });
            continue;
        }

        // 안전 필터(force_dirty == false)가 켜져 있을 때는 force: false를 전달해
        // krilld 및 git worktree remove의 dirty·비심링크 보호 검사를 반드시 거치게 한다.
        let req = RemoveWorktree {
            repo: item.repo.as_os_str().as_bytes().to_vec(),
            path: item.path.as_os_str().as_bytes().to_vec(),
            force: force_dirty,
        };
        // 삭제 뒤에는 canonicalize가 실패하므로 계보 키는 삭제 전에 정규화해 둔다.
        let lineage_key = krill_proto::orchestration::worktree_key(&item.path);
        match remove_worktree_live_or_git(paths, &req, inferred_state.as_deref()) {
            Ok(removed) => {
                // `krill worktree rm`과 같이 삭제된 워크트리의 계보를 지워 고아 항목을 남기지 않는다.
                if let Some(dir) = inferred_state.as_deref() {
                    krill_proto::orchestration::WorktreeStore::mutate(dir, |store| {
                        store.remove_worktree_lineage(&lineage_key);
                        Ok(())
                    })
                    .ok();
                }
                summary.removed_count = summary.removed_count.saturating_add(1);
                summary.freed_bytes = summary.freed_bytes.saturating_add(item.disk_bytes);
                if removed.branch.is_some() {
                    if removed.branch_deleted {
                        summary.branches_deleted = summary.branches_deleted.saturating_add(1);
                    } else {
                        summary.branches_preserved = summary.branches_preserved.saturating_add(1);
                    }
                }
                summary.items.push(CleanupItemOutcome {
                    path: item.path.clone(),
                    branch: removed.branch.or_else(|| item.branch.clone()),
                    removed: true,
                    branch_deleted: removed.branch_deleted,
                    freed_bytes: item.disk_bytes,
                    warnings: removed.warnings,
                    error: None,
                });
            }
            Err(err) => {
                summary.failed_count = summary.failed_count.saturating_add(1);
                summary.items.push(CleanupItemOutcome {
                    path: item.path.clone(),
                    branch: item.branch.clone(),
                    removed: false,
                    branch_deleted: false,
                    freed_bytes: 0,
                    warnings: Vec::new(),
                    error: Some(err),
                });
            }
        }
    }

    summary
}

fn remove_worktree_live_or_git(
    paths: Option<&Paths>,
    req: &RemoveWorktree,
    state_dir: Option<&Path>,
) -> Result<RemovedWorktree, String> {
    if let Some(p) = paths
        && p.socket.exists()
        && let Ok(res) = remove_via_socket(&p.socket, req)
    {
        return res;
    }
    remove_worktree_git(req, state_dir)
}

fn remove_via_socket(
    socket: &Path,
    req: &RemoveWorktree,
) -> Result<Result<RemovedWorktree, String>, ()> {
    let mut stream = krill_proto::os::net::UnixStream::connect(socket).map_err(|_| ())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| ())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| ())?;
    let hello = daemon::client_hello(ClientRole::Cli, "krill worktree cleanup");
    let (kind, body) = ipc::read_frame(&mut stream).map_err(|_| ())?.ok_or(())?;
    let server = ipc::ServerHello::decode(kind, &body).map_err(|_| ())?;
    let negotiated = ipc::negotiate(&server, &hello).ok_or(())?;
    let required = FEAT_CONTROL | FEAT_WORKTREE_REMOVE;
    if negotiated.features & required != required {
        return Err(());
    }
    stream
        .write_all(&hello.encode().map_err(|_| ())?)
        .map_err(|_| ())?;
    stream
        .write_all(
            &Request::RemoveWorktree(req.clone())
                .encode()
                .map_err(|_| ())?,
        )
        .map_err(|_| ())?;
    let (rk, rb) = ipc::read_frame(&mut stream).map_err(|_| ())?.ok_or(())?;
    match Reply::decode(rk, &rb).map_err(|_| ())? {
        Reply::WorktreeRemoved(w) => Ok(Ok(w)),
        Reply::WorktreeFailed(e) => Ok(Err(e)),
        Reply::Failed(e) => Ok(Err(e.to_string())),
        _ => Err(()),
    }
}

/// 데몬이 떠 있지 않은 환경에서도 `krilld::git::worktree::remove`와 동일한 안전 규칙으로
/// 링크된 워크트리를 삭제하고 병합된 로컬 브랜치를 정리한다.
pub fn remove_worktree_git(
    req: &RemoveWorktree,
    state_dir: Option<&Path>,
) -> Result<RemovedWorktree, String> {
    let given = Path::new(OsStr::from_bytes(&req.repo));
    let entries = list_repo_worktrees(given);
    let main = entries.first().ok_or("repository has no worktrees")?;
    let path = fs::canonicalize(Path::new(OsStr::from_bytes(&req.path)))
        .map_err(|e| format!("cannot resolve worktree: {e}"))?;
    let target = entries
        .iter()
        .find(|e| e.path == path)
        .ok_or("path is not a registered worktree of this repository")?;
    if target.path == main.path {
        return Err("cannot remove the main worktree".to_owned());
    }
    if target.locked {
        return Err("worktree is locked; unlock it explicitly before removal".to_owned());
    }
    let repo = &main.path;
    let symlinks = effective_worktree_symlinks(repo, state_dir);
    let (dirty, _) = check_worktree_dirty_status(repo, &path, &symlinks)?;
    if !req.force && dirty {
        return Err("worktree is dirty; use --force to discard its files".to_owned());
    }

    // 삭제 전 minikrill.toml / orca.yaml / 상태 디렉터리의 공유 심볼릭 링크를 먼저 해제한다.
    unlink_worktree_symlinks(&path, &symlinks);

    let mut args: Vec<OsString> = ["worktree", "remove"].map(OsString::from).into();
    if req.force {
        args.push("--force".into());
    }
    args.push("--".into());
    args.push(path.into_os_string());
    let rm_out = run_git(repo, args)?;
    if !rm_out.status.success() {
        return Err(git_err(&rm_out, "git worktree remove failed"));
    }

    let mut result = RemovedWorktree {
        branch: target.branch.clone(),
        branch_deleted: false,
        warnings: Vec::new(),
    };
    if let Some(branch) = &result.branch {
        match delete_merged_branch_git(repo, branch) {
            Ok(()) => result.branch_deleted = true,
            Err(e) => result
                .warnings
                .push(format!("branch {branch:?} preserved: {e}")),
        }
    }
    if let Ok(prune_out) = run_git(repo, ["worktree", "prune"])
        && !prune_out.status.success()
    {
        result.warnings.push(format!(
            "worktree prune failed: {}",
            git_err(&prune_out, "git worktree prune failed")
        ));
    }
    Ok(result)
}

fn delete_merged_branch_git(repo: &Path, branch: &str) -> Result<(), String> {
    if !is_branch_merged_into_head(repo, branch) {
        return Err("not merged into the main worktree HEAD".to_owned());
    }
    let out = run_git(repo, ["branch", "-d", "--", branch])?;
    if !out.status.success() {
        return Err(git_err(&out, "git branch -d failed"));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct GitWorktreeEntry {
    path: PathBuf,
    branch: Option<String>,
    locked: bool,
}

fn list_repo_worktrees(repo: &Path) -> Vec<GitWorktreeEntry> {
    let Ok(out) = run_git(repo, ["worktree", "list", "--porcelain", "-z"]) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_porcelain_worktrees(&out.stdout)
}

fn parse_porcelain_worktrees(bytes: &[u8]) -> Vec<GitWorktreeEntry> {
    let mut entries = Vec::new();
    for field in bytes.split(|b| *b == 0) {
        if let Some(raw_path) = field.strip_prefix(b"worktree ") {
            if raw_path
                .split(|b| *b == b'/')
                .any(|part| part == b".minikrill-preparing")
            {
                continue;
            }
            let raw = PathBuf::from(OsString::from_vec(raw_path.to_vec()));
            let path = fs::canonicalize(&raw).unwrap_or(raw);
            entries.push(GitWorktreeEntry {
                path,
                branch: None,
                locked: false,
            });
        } else if let Some(last) = entries.last_mut() {
            if let Some(branch) = field.strip_prefix(b"branch refs/heads/") {
                if let Ok(b) = String::from_utf8(branch.to_vec()) {
                    last.branch = Some(b);
                }
            } else if field == b"locked" || field.starts_with(b"locked ") {
                last.locked = true;
            }
        }
    }
    entries
}

fn effective_worktree_symlinks(repo: &Path, state_dir: Option<&Path>) -> Vec<String> {
    let resolved_state = state_dir
        .map(Path::to_path_buf)
        .or_else(|| daemon::state_dir_override().map(PathBuf::from));
    let mut symlinks =
        krill_proto::orchestration::inspect_repo_hooks(repo, resolved_state.as_deref())
            .effective_symlinks;
    if let Some(dir) = resolved_state.as_deref()
        && let Ok(store) = krill_proto::orchestration::WorktreeStore::load(dir)
    {
        for alias in path_aliases(repo) {
            for item in store.symlinks_for(&alias) {
                let trimmed = item.trim().trim_end_matches('/').to_owned();
                if !trimmed.is_empty() && !symlinks.contains(&trimmed) {
                    symlinks.push(trimmed);
                }
            }
        }
    }
    symlinks
}

fn safe_parent(root: &Path, rel: &Path) -> std::io::Result<()> {
    let mut path = root.to_path_buf();
    for part in rel.components() {
        path.push(part);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_symlink() {
            return Err(std::io::Error::other("refused, path has a symlink"));
        }
    }
    Ok(())
}

fn is_valid_worktree_symlink(wt_real: &Path, raw: &str) -> bool {
    let trimmed = raw.trim().trim_end_matches('/');
    let rel = Path::new(trimmed);
    if trimmed.is_empty()
        || trimmed.contains('\\')
        || !rel.components().all(|c| {
            matches!(
                c,
                std::path::Component::Normal(n) if !n.as_bytes().eq_ignore_ascii_case(b".git")
            )
        })
    {
        return false;
    }
    let parent = rel.parent().unwrap_or(Path::new(""));
    if safe_parent(wt_real, parent).is_err() {
        return false;
    }
    fs::symlink_metadata(wt_real.join(rel)).is_ok_and(|m| m.file_type().is_symlink())
}

fn dir_contains_only_active_symlinks(
    wt_real: &Path,
    rel_dir: &Path,
    active_symlinks: &[&str],
) -> bool {
    let dir_path = wt_real.join(rel_dir);
    let Ok(meta) = fs::symlink_metadata(&dir_path) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return rel_dir
            .to_str()
            .is_some_and(|s| active_symlinks.contains(&s.trim_end_matches('/')));
    }
    if !meta.is_dir() {
        return false;
    }
    let Ok(read_dir) = fs::read_dir(&dir_path) else {
        return false;
    };
    let mut has_entry = false;
    for entry in read_dir {
        let Ok(entry) = entry else {
            return false;
        };
        has_entry = true;
        let child_rel = rel_dir.join(entry.file_name());
        let Some(child_str) = child_rel.to_str() else {
            return false;
        };
        if active_symlinks.contains(&child_str) {
            continue;
        }
        if !dir_contains_only_active_symlinks(wt_real, &child_rel, active_symlinks) {
            return false;
        }
    }
    has_entry
}

fn unlink_worktree_symlinks(wt_real: &Path, symlinks: &[String]) {
    for raw in symlinks {
        if is_valid_worktree_symlink(wt_real, raw) {
            let trimmed = raw.trim().trim_end_matches('/');
            let rel = Path::new(trimmed);
            fs::remove_file(wt_real.join(rel)).ok();
            let mut parent = rel.parent();
            while let Some(p) = parent {
                if p.as_os_str().is_empty() {
                    break;
                }
                if fs::remove_dir(wt_real.join(p)).is_err() {
                    break;
                }
                parent = p.parent();
            }
        }
    }
}

fn check_worktree_dirty(repo: &Path, worktree: &Path, symlinks: &[String]) -> (bool, usize) {
    check_worktree_dirty_status(repo, worktree, symlinks).unwrap_or((true, 1))
}

fn check_worktree_dirty_status(
    repo: &Path,
    worktree: &Path,
    symlinks: &[String],
) -> Result<(bool, usize), String> {
    if !worktree.is_dir() {
        return Err(format!(
            "worktree directory does not exist: {}",
            worktree.display()
        ));
    }
    let args = [
        OsString::from("-C"),
        worktree.as_os_str().to_owned(),
        OsString::from("status"),
        OsString::from("--porcelain"),
        OsString::from("-z"),
        OsString::from("--untracked-files=all"),
        OsString::from("--ignored=matching"),
    ];
    let out = run_git(repo, args)?;
    if !out.status.success() {
        return Err(git_err(&out, "cannot check worktree status"));
    }
    if out.stdout.is_empty() {
        return Ok((false, 0));
    }
    let wt_real = fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    let active_symlinks: Vec<&str> = symlinks
        .iter()
        .map(|s| s.trim().trim_end_matches('/'))
        .filter(|s| !s.is_empty() && is_valid_worktree_symlink(&wt_real, s))
        .collect();

    let mut count = 0usize;
    let mut fields = out.stdout.split(|b| *b == 0);
    while let Some(entry) = fields.next() {
        if entry.is_empty() {
            continue;
        }
        let [x, y, b' ', rel_bytes @ ..] = entry else {
            count = count.saturating_add(1);
            continue;
        };
        if rel_bytes.is_empty() {
            count = count.saturating_add(1);
            continue;
        }
        if *x == b'R' || *x == b'C' {
            // rename/copy의 원본 경로 토큰 소비
            let _ = fields.next();
            count = count.saturating_add(1);
            continue;
        }
        if ([*x, *y] == *b"??" || [*x, *y] == *b"!!")
            && !active_symlinks.is_empty()
            && let Ok(rel_str) = std::str::from_utf8(rel_bytes)
        {
            let trimmed = rel_str.trim_end_matches('/');
            if active_symlinks.contains(&trimmed) {
                continue;
            }
            if rel_str.ends_with('/')
                && dir_contains_only_active_symlinks(&wt_real, Path::new(trimmed), &active_symlinks)
            {
                continue;
            }
        }
        count = count.saturating_add(1);
    }
    Ok((count > 0, count))
}

fn is_branch_merged_into_head(main_repo: &Path, branch: &str) -> bool {
    let reference = format!("refs/heads/{branch}");
    let Ok(out) = run_git(
        main_repo,
        ["merge-base", "--is-ancestor", "--", &reference, "HEAD"],
    ) else {
        return false;
    };
    out.status.success()
}

fn lookup_pr_metadata(
    store: &MetadataStore,
    main_repo: &Path,
    worktree: &Path,
    branch: Option<&str>,
) -> (Option<CleanupPrState>, Option<u64>) {
    for key in path_aliases(worktree) {
        let pr_num = store.prs.get(&key).copied();
        if let Some(raw_state) = store.pr_states.get(&key)
            && let Some(parsed) = CleanupPrState::parse(raw_state)
        {
            return (Some(parsed), pr_num);
        }
        if let Some(raw_status) = store.statuses.get(&key)
            && let Some(parsed) = CleanupPrState::parse(raw_status)
            && parsed != CleanupPrState::None
        {
            return (Some(parsed), pr_num);
        }
        if pr_num.is_some() {
            if let Some(branch) = branch
                && let Some(git_cfg) = read_git_branch_pr_state(main_repo, branch)
            {
                return (Some(git_cfg), pr_num);
            }
            return (None, pr_num);
        }
    }
    if let Some(branch) = branch
        && let Some(git_cfg) = read_git_branch_pr_state(main_repo, branch)
    {
        return (Some(git_cfg), None);
    }
    (None, None)
}

fn read_git_branch_pr_state(main_repo: &Path, branch: &str) -> Option<CleanupPrState> {
    let key = format!("branch.{branch}.krillPrState");
    let out = run_git(main_repo, ["config", "--get", &key]).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    CleanupPrState::parse(&text)
}

fn lookup_last_active_secs(store: &MetadataStore, worktree: &Path, now_secs: u64) -> u64 {
    for key in path_aliases(worktree) {
        if let Some(&ts) = store.updated_at.get(&key)
            && ts > 0
        {
            return ts;
        }
    }
    fs::metadata(worktree)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(now_secs, |d| d.as_secs())
}

fn load_metadata_store(state_dir: &Path) -> Option<MetadataStore> {
    let file = fs::File::open(state_dir.join(krill_proto::orchestration::COMMENTS_FILE)).ok()?;
    let mut raw = String::new();
    file.take(krill_proto::orchestration::MAX_COMMENTS_BYTES.saturating_add(1))
        .read_to_string(&mut raw)
        .ok()?;
    toml::from_str(&raw).ok()
}

fn path_aliases(worktree: &Path) -> Vec<String> {
    let mut out = vec![worktree.to_string_lossy().into_owned()];
    if let Ok(canon) = fs::canonicalize(worktree) {
        let s = canon.to_string_lossy().into_owned();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    if let Ok(rest) = worktree.strip_prefix("/private") {
        let s = Path::new("/").join(rest).to_string_lossy().into_owned();
        if !out.contains(&s) {
            out.push(s);
        }
    } else if let Ok(rest) = worktree.strip_prefix("/") {
        let s = Path::new("/private")
            .join(rest)
            .to_string_lossy()
            .into_owned();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn read_head_branch(worktree: &Path) -> Option<String> {
    let git = worktree.join(".git");
    let dir = if git.is_dir() {
        git
    } else {
        let link = fs::read_to_string(&git).ok()?;
        let rel = link.strip_prefix("gitdir:")?.trim();
        worktree.join(rel)
    };
    let head = fs::read_to_string(dir.join("HEAD")).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .filter(|b| !b.is_empty())
        .map(str::to_owned)
}

fn run_git<I, S>(cwd: &Path, args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("git")
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git: {e}"))
}

fn git_err(out: &Output, fallback: &str) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    if stderr.is_empty() {
        fallback.to_owned()
    } else {
        stderr
    }
}

#[cfg(test)]
mod tests {
    use krill_proto::worktree_space::{CleanupFilter, CleanupPrFilter, filter_cleanup_candidates};

    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("krill-wt-space-{}-{label}", std::process::id()));
        fs::remove_dir_all(&d).ok();
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn git(cwd: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn space_scan_aggregates_artifacts_and_sorts_descending() {
        let root = temp_dir("space-scan");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("README.md"), vec![b'x'; 500]).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "init"]);

        let wt_small = root.join("wt-small");
        let wt_large = root.join("wt-large");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feat-small",
                wt_small.to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feat-large",
                wt_large.to_str().unwrap(),
            ],
        );

        // wt-small: node_modules 2,000B
        fs::create_dir_all(wt_small.join("node_modules/pkg")).unwrap();
        fs::write(
            wt_small.join("node_modules/pkg/index.js"),
            vec![b'a'; 2_000],
        )
        .unwrap();

        // wt-large: target 8,000B + .next 3,000B + 일반 파일 1,000B
        fs::create_dir_all(wt_large.join("target/debug")).unwrap();
        fs::write(wt_large.join("target/debug/bin"), vec![b'b'; 8_000]).unwrap();
        fs::create_dir_all(wt_large.join(".next/cache")).unwrap();
        fs::write(wt_large.join(".next/cache/data"), vec![b'c'; 3_000]).unwrap();
        fs::write(wt_large.join("README.md"), vec![b'd'; 1_000]).unwrap();

        let report = scan_workspace_space(std::slice::from_ref(&repo));
        assert_eq!(report.worktrees.len(), 3);
        // 가장 큰 wt-large가 1순위로 정렬되어야 한다.
        assert_eq!(report.worktrees[0].name, "wt-large");
        assert_eq!(report.worktrees[0].artifact_bytes, 11_000);
        assert!(report.worktrees[0].total_bytes >= 12_000);
        assert_eq!(report.worktrees[0].artifacts[0].name, "target");
        assert_eq!(report.worktrees[0].artifacts[0].bytes, 8_000);
        assert_eq!(report.worktrees[0].artifacts[1].name, ".next");
        assert_eq!(report.worktrees[0].artifacts[1].bytes, 3_000);

        assert_eq!(report.worktrees[1].name, "wt-small");
        assert_eq!(report.worktrees[1].artifact_bytes, 2_000);
        assert_eq!(report.worktrees[1].artifacts[0].name, "node_modules");
        assert_eq!(report.worktrees[1].artifacts[0].bytes, 2_000);

        assert_eq!(report.total_artifact_bytes, 13_000);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cleanup_filters_by_pr_state_idle_days_and_dirty_safety_and_removes_worktrees() {
        let root = temp_dir("cleanup-flow");
        let state_dir = root.join("state");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("file.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base"]);

        // 1) wt-merged-old: 머지됨 + 15일 경과 + clean -> 삭제 대상 & 브랜치 삭제
        let wt_merged_old = root.join("wt-merged-old");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "branch-merged-old",
                wt_merged_old.to_str().unwrap(),
            ],
        );
        // 2) wt-closed-old: PR Closed + 미병합 커밋 보유 + 20일 경과 + clean -> 삭제 대상 & 미병합 브랜치 보존 경고
        let wt_closed_old = root.join("wt-closed-old");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "branch-closed-old",
                wt_closed_old.to_str().unwrap(),
            ],
        );
        fs::write(wt_closed_old.join("unmerged.txt"), "work\n").unwrap();
        git(&wt_closed_old, &["add", "."]);
        git(&wt_closed_old, &["commit", "-m", "unmerged work"]);

        // 3) wt-dirty-old: 머지됨 + 30일 경과 + dirty -> 안전 필터로 제외
        let wt_dirty_old = root.join("wt-dirty-old");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "branch-dirty-old",
                wt_dirty_old.to_str().unwrap(),
            ],
        );
        fs::write(wt_dirty_old.join("dirty.txt"), "uncommitted\n").unwrap();

        // 4) wt-merged-recent: 머지됨 + 2일 경과 + clean -> 14일 필터에서 제외
        let wt_merged_recent = root.join("wt-merged-recent");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "branch-merged-recent",
                wt_merged_recent.to_str().unwrap(),
            ],
        );

        let now = 1_700_000_000_u64;
        record_worktree_metadata(
            &state_dir,
            &wt_merged_old,
            Some(CleanupPrState::Merged),
            Some(101),
            Some(now.saturating_sub(15_u64.saturating_mul(SECS_PER_DAY))),
        )
        .unwrap();
        record_worktree_metadata(
            &state_dir,
            &wt_closed_old,
            Some(CleanupPrState::Closed),
            Some(102),
            Some(now.saturating_sub(20_u64.saturating_mul(SECS_PER_DAY))),
        )
        .unwrap();
        record_worktree_metadata(
            &state_dir,
            &wt_dirty_old,
            Some(CleanupPrState::Merged),
            Some(103),
            Some(now.saturating_sub(35_u64.saturating_mul(SECS_PER_DAY))),
        )
        .unwrap();
        record_worktree_metadata(
            &state_dir,
            &wt_merged_recent,
            Some(CleanupPrState::Merged),
            Some(104),
            Some(now.saturating_sub(2_u64.saturating_mul(SECS_PER_DAY))),
        )
        .unwrap();

        let all = discover_cleanup_candidates(std::slice::from_ref(&repo), Some(&state_dir), now);
        assert_eq!(all.len(), 4);

        // Merged + 14일 이상 + dirty 제외 -> wt-merged-old 하나만 매칭
        let merged_14d = filter_cleanup_candidates(
            &all,
            &CleanupFilter {
                pr_filter: CleanupPrFilter::Merged,
                min_idle_days: 14,
                exclude_dirty: true,
            },
        );
        assert_eq!(merged_14d.len(), 1);
        assert_eq!(merged_14d[0].name, "wt-merged-old");

        // Closed + 14일 이상 + dirty 제외 -> wt-closed-old 하나만 매칭
        let closed_14d = filter_cleanup_candidates(
            &all,
            &CleanupFilter {
                pr_filter: CleanupPrFilter::Closed,
                min_idle_days: 14,
                exclude_dirty: true,
            },
        );
        assert_eq!(closed_14d.len(), 1);
        assert_eq!(closed_14d[0].name, "wt-closed-old");

        // MergedOrClosed + 14일 이상 + dirty 제외 -> wt-merged-old, wt-closed-old (2개)
        let both_14d = filter_cleanup_candidates(
            &all,
            &CleanupFilter {
                pr_filter: CleanupPrFilter::MergedOrClosed,
                min_idle_days: 14,
                exclude_dirty: true,
            },
        );
        assert_eq!(both_14d.len(), 2);

        let key = |p: &Path| krill_proto::orchestration::worktree_key(p);
        let (merged_key, closed_key, dirty_key) =
            (key(&wt_merged_old), key(&wt_closed_old), key(&wt_dirty_old));
        krill_proto::orchestration::WorktreeStore::mutate(&state_dir, |store| {
            store.set_parent(&merged_key, Some(&key(&repo)))?;
            store.set_parent(&closed_key, Some(&key(&repo)))?;
            store.set_parent(&dirty_key, Some(&closed_key))
        })
        .unwrap();

        // 일괄 삭제 실행: 머지된 브랜치는 삭제되고, 미병합 브랜치(branch-closed-old)는 경고 후 보존되어야 한다.
        let summary = execute_cleanup(&both_14d, false, None, Some(&state_dir));
        // 삭제된 워크트리를 자식·부모로 가진 계보는 남지 않는다(OWE-748).
        let parents = krill_proto::orchestration::WorktreeStore::load(&state_dir)
            .unwrap()
            .parents;
        assert!(parents.is_empty(), "orphan lineage left: {parents:?}");
        assert_eq!(summary.removed_count, 2);
        assert_eq!(summary.failed_count, 0);
        assert_eq!(summary.branches_deleted, 1);
        assert_eq!(summary.branches_preserved, 1);
        assert!(!wt_merged_old.exists());
        assert!(!wt_closed_old.exists());
        assert!(wt_dirty_old.exists(), "dirty worktree must be preserved");
        assert!(
            wt_merged_recent.exists(),
            "recent worktree must be preserved"
        );

        let branches_out = Command::new("git")
            .current_dir(&repo)
            .args(["branch", "--list"])
            .output()
            .unwrap();
        let branches = String::from_utf8_lossy(&branches_out.stdout);
        assert!(
            !branches.contains("branch-merged-old"),
            "merged branch should be deleted: {branches}"
        );
        assert!(
            branches.contains("branch-closed-old"),
            "unmerged closed-PR branch must be preserved: {branches}"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn git_status_failure_marks_worktree_dirty_and_blocks_cleanup() {
        let root = temp_dir("owe758-status-fail");
        let state_dir = root.join("state");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("file.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base"]);

        let wt_corrupt = root.join("wt-corrupt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "branch-corrupt",
                wt_corrupt.to_str().unwrap(),
            ],
        );
        fs::write(wt_corrupt.join("file.txt"), "modified uncommitted work\n").unwrap();

        // 워크트리의 git index를 손상시켜 `git status`가 비정상 종료(!status.success())하게 만든다.
        let index_path = repo.join(".git/worktrees/wt-corrupt/index");
        fs::write(&index_path, b"corrupt-index-bytes").unwrap();

        assert!(check_worktree_dirty_status(&repo, &wt_corrupt, &[]).is_err());
        assert_eq!(check_worktree_dirty(&repo, &wt_corrupt, &[]), (true, 1));

        let now = 1_700_000_000_u64;
        record_worktree_metadata(
            &state_dir,
            &wt_corrupt,
            Some(CleanupPrState::Merged),
            Some(201),
            Some(now.saturating_sub(20 * SECS_PER_DAY)),
        )
        .unwrap();

        let all = discover_cleanup_candidates(std::slice::from_ref(&repo), Some(&state_dir), now);
        assert_eq!(all.len(), 1);
        assert!(
            all[0].dirty,
            "git status failure must mark candidate as dirty"
        );
        assert_eq!(all[0].dirty_count, 1);

        let safe_filtered = filter_cleanup_candidates(&all, &CleanupFilter::default());
        assert!(
            safe_filtered.is_empty(),
            "safety filter must exclude worktree when git status failed"
        );

        // 후보를 직접 넘겨 실행하더라도 강제 삭제되지 않고 실패로 기록되어야 한다.
        let summary = execute_cleanup(&all, false, None, Some(&state_dir));
        assert_eq!(summary.removed_count, 0);
        assert_eq!(summary.failed_count, 1);
        assert!(wt_corrupt.exists(), "worktree must not be force-removed");
        assert_eq!(
            fs::read_to_string(wt_corrupt.join("file.txt")).unwrap(),
            "modified uncommitted work\n"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn safety_filter_protects_untracked_artifact_named_dirs_and_gitignored_files() {
        let root = temp_dir("owe758-artifact-and-ignored");
        let state_dir = root.join("state");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        fs::write(repo.join(".gitignore"), ".env\n").unwrap();
        fs::write(repo.join("README.md"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base"]);

        let wt_vendor = root.join("wt-vendor");
        let wt_build = root.join("wt-build");
        let wt_env = root.join("wt-env");
        let wt_race = root.join("wt-race");
        for (wt, branch) in [
            (&wt_vendor, "feat-vendor"),
            (&wt_build, "feat-build"),
            (&wt_env, "feat-env"),
            (&wt_race, "feat-race"),
        ] {
            git(
                &repo,
                &["worktree", "add", "-b", branch, wt.to_str().unwrap()],
            );
        }

        // KNOWN_ARTIFACT_DIRS 이름(`vendor`, `build`) 아래의 미추적(`??`) 새 소스 파일과
        // 심링크되지 않은 로컬 gitignore(`!!`) `.env` 파일 생성
        fs::create_dir_all(wt_vendor.join("vendor/pkg")).unwrap();
        fs::write(
            wt_vendor.join("vendor/pkg/new_source.rs"),
            "pub fn a() {}\n",
        )
        .unwrap();
        fs::create_dir_all(wt_build.join("build")).unwrap();
        fs::write(wt_build.join("build/script.sh"), "#!/bin/sh\n").unwrap();
        fs::write(wt_env.join(".env"), "LOCAL_SECRET=keep\n").unwrap();

        let now = 1_700_000_000_u64;
        for (idx, wt) in [&wt_vendor, &wt_build, &wt_env, &wt_race]
            .into_iter()
            .enumerate()
        {
            record_worktree_metadata(
                &state_dir,
                wt,
                Some(CleanupPrState::Merged),
                Some(300 + idx as u64),
                Some(now.saturating_sub(30 * SECS_PER_DAY)),
            )
            .unwrap();
        }

        let all = discover_cleanup_candidates(std::slice::from_ref(&repo), Some(&state_dir), now);
        assert_eq!(all.len(), 4);
        for name in ["wt-vendor", "wt-build", "wt-env"] {
            let cand = all.iter().find(|c| c.name == name).unwrap();
            assert!(cand.dirty, "{name} must be detected as dirty");
            assert!(cand.dirty_count >= 1);
        }

        // 안전 필터(exclude_dirty: true)는 깨끗한 wt-race 하나만 통과시켜야 한다.
        let matched = filter_cleanup_candidates(&all, &CleanupFilter::default());
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].name, "wt-race");

        // 스캔 직후 삭제 직전에 wt-race에 로컬 `.env`나 `out/result.txt`가 생기면
        // execute_cleanup(force_dirty: false)이 이를 감지해 삭제를 거부해야 한다.
        fs::write(wt_race.join(".env"), "RACE_SECRET=1\n").unwrap();
        let summary = execute_cleanup(&matched, false, None, Some(&state_dir));
        assert_eq!(summary.removed_count, 0);
        assert_eq!(summary.failed_count, 1);
        assert!(wt_race.join(".env").exists());
        assert!(wt_vendor.join("vendor/pkg/new_source.rs").exists());
        assert!(wt_build.join("build/script.sh").exists());
        assert!(wt_env.join(".env").exists());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn remove_worktree_git_unlinks_configured_symlinks_and_preserves_repo_targets() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("owe758-symlinks");
        let state_dir = root.join("state");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        fs::write(
            repo.join(".gitignore"),
            ".env\n.env.local\nsecret/\nshared-dir/\n",
        )
        .unwrap();
        fs::write(
            repo.join("minikrill.toml"),
            "[worktree]\nsymlinks = [\".env\", \"secret/nested.txt\"]\n",
        )
        .unwrap();
        fs::write(
            repo.join("orca.yaml"),
            "worktree:\n  sharedDirectories:\n    - shared-dir/\n",
        )
        .unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "init"]);

        let repo_canon = fs::canonicalize(&repo).unwrap();
        let repo_key = repo_canon.to_string_lossy().into_owned();
        krill_proto::orchestration::WorktreeStore::mutate(&state_dir, |store| {
            store.add_symlink(&repo_key, ".env.local")?;
            Ok(())
        })
        .unwrap();

        fs::write(repo.join(".env"), "ROOT_ENV=1").unwrap();
        fs::write(repo.join(".env.local"), "LOCAL_ENV=2").unwrap();
        fs::create_dir_all(repo.join("secret")).unwrap();
        fs::write(repo.join("secret/nested.txt"), "nested-secret").unwrap();
        fs::create_dir_all(repo.join("shared-dir")).unwrap();
        fs::write(repo.join("shared-dir/cache.txt"), "shared-cache").unwrap();

        let wt_linked = root.join("wt-linked");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feat-linked",
                wt_linked.to_str().unwrap(),
            ],
        );
        let wt_canon = fs::canonicalize(&wt_linked).unwrap();
        symlink(repo_canon.join(".env"), wt_canon.join(".env")).unwrap();
        symlink(repo_canon.join(".env.local"), wt_canon.join(".env.local")).unwrap();
        fs::create_dir_all(wt_canon.join("secret")).unwrap();
        symlink(
            repo_canon.join("secret/nested.txt"),
            wt_canon.join("secret/nested.txt"),
        )
        .unwrap();
        symlink(repo_canon.join("shared-dir"), wt_canon.join("shared-dir")).unwrap();

        let now = 1_700_000_000_u64;
        record_worktree_metadata(
            &state_dir,
            &wt_linked,
            Some(CleanupPrState::Merged),
            Some(401),
            Some(now.saturating_sub(20 * SECS_PER_DAY)),
        )
        .unwrap();

        // 설정된 공유 심볼릭 링크만 있는 워크트리는 clean으로 판정되어야 한다.
        let all = discover_cleanup_candidates(std::slice::from_ref(&repo), Some(&state_dir), now);
        assert_eq!(all.len(), 1);
        assert!(
            !all[0].dirty,
            "configured symlinks must not mark worktree dirty"
        );
        assert_eq!(all[0].dirty_count, 0);

        // 심볼릭 링크 외 일반 미추적 파일이 있으면 force: false에서 거부하고 심볼릭 링크도 유지해야 한다.
        fs::write(wt_canon.join("untracked.txt"), "keep").unwrap();
        let blocked = execute_cleanup(&all, false, None, Some(&state_dir));
        assert_eq!(blocked.removed_count, 0);
        assert_eq!(blocked.failed_count, 1);
        assert!(
            fs::symlink_metadata(wt_canon.join(".env"))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        // 일반 미추적 파일을 지우면 force: false에서도 공유 심링크를 먼저 해제한 뒤 워크트리를 깔끔히 삭제한다.
        fs::remove_file(wt_canon.join("untracked.txt")).unwrap();
        let summary = execute_cleanup(&all, false, None, Some(&state_dir));
        assert_eq!(summary.removed_count, 1);
        assert_eq!(summary.failed_count, 0);
        assert!(!wt_linked.exists());
        assert_eq!(fs::read_to_string(repo.join(".env")).unwrap(), "ROOT_ENV=1");
        assert_eq!(
            fs::read_to_string(repo.join(".env.local")).unwrap(),
            "LOCAL_ENV=2"
        );
        assert_eq!(
            fs::read_to_string(repo.join("secret/nested.txt")).unwrap(),
            "nested-secret"
        );
        assert_eq!(
            fs::read_to_string(repo.join("shared-dir/cache.txt")).unwrap(),
            "shared-cache"
        );

        fs::remove_dir_all(&root).ok();
    }
}
