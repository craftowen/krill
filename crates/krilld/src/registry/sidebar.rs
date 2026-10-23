//! Read-only MK-046 metadata. No session lock is held during git or process queries.

use crate::session::lock;
use krill_proto::os::ffi::{OsStrExt as _, OsStringExt as _};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::control::{Failure, RepositoryTrees, SessionDirectory, SidebarData};

#[derive(Default)]
pub(super) struct Cache {
    directories: BTreeMap<SessionId, PathBuf>,
    trees: BTreeMap<PathBuf, (Instant, Option<Vec<u8>>)>,
    repositories: BTreeMap<Vec<u8>, (Instant, RepositoryTrees)>,
    inflight: BTreeMap<PathBuf, Arc<Discovery>>,
}

#[derive(Default)]
enum DiscoveryResult {
    #[default]
    Pending,
    Ready(Option<RepositoryTrees>),
}

#[derive(Default)]
struct Discovery {
    paths: Mutex<BTreeSet<PathBuf>>,
    result: Mutex<DiscoveryResult>,
    ready: Condvar,
}

pub(super) fn snapshot(
    cache: &Mutex<Cache>,
    entries: &[(SessionId, u32, Option<crate::session::Tap>)],
) -> Result<SidebarData, Failure> {
    let mut sessions = Vec::new();
    let mut repositories = BTreeMap::new();
    let mut bytes = 0usize;
    for (session, pid, live) in entries {
        // OS cwd and Git IO never run under the shared metadata cache lock.
        let observed = live.as_ref().and_then(|tap| tap.cwd(*pid));
        // Windows cwd is the shell's report (`C:/x`, `c:\x\`); match canonical worktree paths.
        #[cfg(windows)]
        let observed = observed.map(|path| krill_proto::os::canonicalize(&path).unwrap_or(path));
        let path = {
            let mut cache = lock(cache);
            if let Some(path) = observed {
                cache.directories.insert(*session, path);
            }
            cache.directories.get(session).cloned()
        };
        sessions.push(SessionDirectory {
            session: *session,
            cwd: path
                .as_ref()
                .map_or_else(Vec::new, |p| p.as_os_str().as_bytes().to_vec()),
        });
        if let Some(path) = path
            && let Some(trees) = discover_cached(cache, &path)
            && !repositories.contains_key(&trees.repo)
        {
            bytes += trees.repo.len() + trees.worktrees.iter().map(Vec::len).sum::<usize>();
            if bytes > 512 * 1024 {
                return Err(Failure::BadRequest);
            }
            repositories.insert(trees.repo.clone(), trees);
        }
    }
    let mut cache = lock(cache);
    if cache.directories.len() > krill_proto::holder::MAX_SESSIONS {
        cache
            .directories
            .retain(|id, _| entries.iter().any(|(s, _, _)| s == id));
    }
    cache
        .trees
        .retain(|_, (at, _)| at.elapsed() < Duration::from_secs(6));
    let reachable: Vec<_> = cache
        .trees
        .values()
        .filter_map(|(_, key)| key.as_ref())
        .cloned()
        .collect();
    cache
        .repositories
        .retain(|repo, _| reachable.contains(repo));
    Ok(SidebarData {
        sessions,
        repositories: repositories.into_values().collect(),
    })
}

fn discover_cached(cache: &Mutex<Cache>, path: &Path) -> Option<RepositoryTrees> {
    let key = repository_key(path);
    let (flight, owner) = {
        let mut cache = lock(cache);
        if let Some((at, repo)) = cache.trees.get(path)
            && at.elapsed() < Duration::from_secs(3)
        {
            if repo.is_none() {
                return None;
            }
            if let Some((at, trees)) = repo.as_ref().and_then(|repo| cache.repositories.get(repo))
                && at.elapsed() < Duration::from_secs(3)
            {
                return Some(trees.clone());
            }
        }
        if let Some(flight) = cache.inflight.get(&key) {
            lock(&flight.paths).insert(path.to_owned());
            (Arc::clone(flight), false)
        } else {
            let flight = Arc::new(Discovery::default());
            lock(&flight.paths).insert(path.to_owned());
            cache.inflight.insert(key.clone(), Arc::clone(&flight));
            (flight, true)
        }
    };
    if owner {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| discover(path)))
            .ok()
            .flatten();
        publish_discovery(cache, &key, &flight, result.clone());
        result
    } else {
        let mut result = lock(&flight.result);
        while matches!(*result, DiscoveryResult::Pending) {
            result = flight
                .ready
                .wait(result)
                .unwrap_or_else(PoisonError::into_inner);
        }
        match &*result {
            DiscoveryResult::Ready(trees) => trees.clone(),
            DiscoveryResult::Pending => None,
        }
    }
}

fn publish_discovery(
    cache: &Mutex<Cache>,
    key: &Path,
    flight: &Arc<Discovery>,
    result: Option<RepositoryTrees>,
) {
    {
        let mut cache = lock(cache);
        if cache
            .inflight
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, flight))
        {
            let completed = Instant::now();
            for path in lock(&flight.paths).iter() {
                cache.trees.insert(
                    path.clone(),
                    (completed, result.as_ref().map(|trees| trees.repo.clone())),
                );
            }
            if let Some(trees) = &result {
                cache
                    .repositories
                    .insert(trees.repo.clone(), (completed, trees.clone()));
            }
            cache.inflight.remove(key);
        }
    }
    // Waiters return this flight's result without changing the authoritative
    // cache or refreshing its TTL after a newer discovery has completed.
    *lock(&flight.result) = DiscoveryResult::Ready(result);
    flight.ready.notify_all();
}

fn repository_key(cwd: &Path) -> PathBuf {
    for root in cwd.ancestors() {
        let dot = root.join(".git");
        let dir = if dot.is_dir() {
            Some(dot)
        } else {
            std::fs::read_to_string(&dot).ok().and_then(|text| {
                text.strip_prefix("gitdir:")
                    .map(|dir| root.join(dir.trim()))
            })
        };
        if let Some(dir) = dir {
            let common = std::fs::read_to_string(dir.join("commondir"))
                .ok()
                .map_or(dir.clone(), |text| dir.join(text.trim()));
            return krill_proto::os::canonicalize(&common).unwrap_or(common);
        }
    }
    cwd.to_owned()
}

/// 앱에 등록된 프로젝트(`<state>/projects.toml`)를 루트 경로만 덧붙인다. 원격 앱이 세션 없는
/// 프로젝트까지 볼 수 있게 하려는 것이다(로컬 앱은 같은 파일을 이미 합치므로 결과가 같다). git 호출 없음.
pub(super) fn include_projects(data: &mut SidebarData) {
    if let Some(dir) = krill_proto::daemon::state_dir() {
        merge_projects(data, &dir);
    }
}

fn merge_projects(data: &mut SidebarData, state_dir: &Path) {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct File {
        projects: Vec<Project>,
    }
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct Project {
        path: String,
        archived: bool,
        host_id: Option<String>,
    }
    let Ok(raw) = std::fs::read_to_string(state_dir.join("projects.toml")) else {
        return;
    };
    let Ok(file) = toml::from_str::<File>(&raw) else {
        return;
    };
    for p in file.projects {
        let path = p.path.trim().as_bytes();
        if p.archived || p.host_id.is_some() || path.is_empty() || path.len() > 4096 {
            continue;
        }
        let path = path.to_vec();
        // 예전 앱이 연결 워크트리를 프로젝트로 등록했으면 이미 본 저장소 아래에 있다 — 따로 세우지 않는다.
        if data
            .repositories
            .iter()
            .any(|r| r.worktrees.contains(&path))
        {
            continue;
        }
        if let Some(repo) = data.repositories.iter_mut().find(|r| r.repo == path) {
            if !repo.worktrees.contains(&path) && repo.worktrees.len() < 512 {
                repo.worktrees.push(path);
            }
        } else if data.repositories.len() < krill_proto::holder::MAX_SESSIONS {
            data.repositories.push(RepositoryTrees {
                repo: path.clone(),
                worktrees: vec![path],
            });
        }
    }
}

fn discover(cwd: &Path) -> Option<RepositoryTrees> {
    // All reads share the existing repository queue and fixed git environment.
    let queue = crate::git::queue().ok()?;
    let output = queue
        .submit(cwd, ["worktree", "list", "--porcelain", "-z"])
        .recv()
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse(&output.stdout)
}

fn parse(bytes: &[u8]) -> Option<RepositoryTrees> {
    let mut worktrees: Vec<Vec<u8>> = bytes
        .split(|&b| b == 0)
        .filter_map(|record| record.strip_prefix(b"worktree "))
        .filter(|p| !p.is_empty() && p.len() <= 4096)
        .filter(|p| {
            !p.split(|b| *b == b'/')
                .any(|part| part == b".krill-preparing")
        })
        .take(512)
        .map(<[u8]>::to_vec)
        .collect();
    let repo = worktrees.first()?.clone();
    // Canonical paths make subdirectories and APFS /var -> /private/var agree with cwd.
    for tree in &mut worktrees {
        if let Ok(path) = krill_proto::os::canonicalize(&PathBuf::from(
            std::ffi::OsString::from_vec(tree.clone()),
        )) {
            *tree = path.as_os_str().as_bytes().to_vec();
        }
    }
    let repo = krill_proto::os::canonicalize(&PathBuf::from(std::ffi::OsString::from_vec(repo)))
        .ok()
        .map_or_else(
            || worktrees.first().cloned().unwrap_or_default(),
            |p| p.as_os_str().as_bytes().to_vec(),
        );
    Some(RepositoryTrees { repo, worktrees })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_projects_join_without_duplicates() {
        let dir =
            std::env::temp_dir().join(format!("krilld-sidebar-projects-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("projects.toml"),
            "[[projects]]\npath = \"/repo\"\n\n[[projects]]\npath = \"/folder\"\n\n\
             [[projects]]\npath = \"/repo-wt\"\n\n\
             [[projects]]\npath = \"/old\"\narchived = true\n\n\
             [[projects]]\npath = \"/remote\"\nhost_id = \"ssh-x\"\n",
        )
        .unwrap();
        let mut data = SidebarData {
            sessions: Vec::new(),
            repositories: vec![RepositoryTrees {
                repo: b"/repo".to_vec(),
                worktrees: vec![b"/repo".to_vec(), b"/repo-wt".to_vec()],
            }],
        };
        merge_projects(&mut data, &dir);
        std::fs::remove_dir_all(&dir).ok();
        let repos: Vec<_> = data
            .repositories
            .iter()
            .map(|r| r.repo.as_slice())
            .collect();
        assert_eq!(repos, [&b"/repo"[..], &b"/folder"[..]]);
        assert_eq!(data.repositories[0].worktrees.len(), 2);
    }

    #[test]
    fn nul_paths_preserve_newlines_and_non_utf8() {
        let data = parse(b"worktree /repo\0HEAD abc\0branch refs/heads/main\0\0worktree /tmp/a\n\xff\0HEAD abc\0detached\0\0").unwrap();
        assert_eq!(data.repo, b"/repo");
        assert_eq!(
            data.worktrees,
            [b"/repo".to_vec(), b"/tmp/a\n\xff".to_vec()]
        );
        assert!(parse(b"HEAD abc\0").is_none());
    }

    #[test]
    fn internal_preparations_are_hidden() {
        let data = parse(b"worktree /repo\0\0worktree /wt/.krill-preparing/tree\0detached\0\0worktree /wt/agent\0\0").unwrap();
        assert_eq!(data.worktrees, [b"/repo".to_vec(), b"/wt/agent".to_vec()]);
    }

    #[test]
    fn scoped_metadata_cannot_leak_cached_sessions() {
        let cache = Mutex::new(Cache::default());
        lock(&cache)
            .directories
            .insert(SessionId(1), PathBuf::from("/does-not-exist/one"));
        lock(&cache)
            .directories
            .insert(SessionId(2), PathBuf::from("/does-not-exist/two"));
        let snapshot = snapshot(&cache, &[(SessionId(1), 0, None)]).unwrap();
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].session, SessionId(1));
        assert!(snapshot.repositories.is_empty());
    }

    #[test]
    fn live_process_cwd_query() {
        assert_eq!(
            crate::sys::current_directory(std::process::id())
                .unwrap()
                .canonicalize()
                .unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    #[test]
    fn an_old_discovery_waiter_cannot_replace_a_newer_cache_or_refresh_its_ttl() {
        let path = PathBuf::from("/krill-sidebar-old-waiter-not-a-repository");
        let cache = Arc::new(Mutex::new(Cache::default()));
        let flight = Arc::new(Discovery::default());
        lock(&cache)
            .inflight
            .insert(repository_key(&path), Arc::clone(&flight));
        let pending = Arc::clone(&cache);
        let requested = path.clone();
        let waiter = std::thread::spawn(move || discover_cached(&pending, &requested));
        let deadline = Instant::now() + Duration::from_secs(2);
        while Arc::strong_count(&flight) < 3 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let old = RepositoryTrees {
            repo: b"/repo".to_vec(),
            worktrees: vec![b"/old".to_vec()],
        };
        let new = RepositoryTrees {
            repo: b"/repo".to_vec(),
            worktrees: vec![b"/new".to_vec()],
        };
        let completed = Instant::now();
        {
            let mut state = lock(&cache);
            state.inflight.clear();
            state
                .trees
                .insert(path.clone(), (completed, Some(new.repo.clone())));
            state
                .repositories
                .insert(new.repo.clone(), (completed, new.clone()));
        }
        *lock(&flight.result) = DiscoveryResult::Ready(Some(old.clone()));
        flight.ready.notify_all();
        assert_eq!(waiter.join().unwrap(), Some(old));
        let state = lock(&cache);
        assert_eq!(state.repositories.get(&new.repo), Some(&(completed, new)));
        assert_eq!(state.trees.get(&path).unwrap().0, completed);
    }
    #[test]
    fn repository_discovery_wait_releases_global_cache_lock() {
        let path = PathBuf::from("/krill-sidebar-inflight-fixture-not-a-repository");
        let cache = Arc::new(Mutex::new(Cache::default()));
        let flight = Arc::new(Discovery::default());
        lock(&cache)
            .inflight
            .insert(repository_key(&path), Arc::clone(&flight));
        let pending = Arc::clone(&cache);
        let thread = std::thread::spawn(move || discover_cached(&pending, &path));
        let deadline = Instant::now() + Duration::from_secs(2);
        while Arc::strong_count(&flight) < 3 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let unlocked = cache.try_lock().is_ok();
        *lock(&flight.result) = DiscoveryResult::Ready(None);
        flight.ready.notify_all();
        assert!(thread.join().unwrap().is_none());
        assert!(
            unlocked,
            "an inflight discovery must not retain the global cache lock"
        );
    }
}
