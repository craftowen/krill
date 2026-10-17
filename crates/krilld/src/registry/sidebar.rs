//! Read-only MK-046 metadata. No session lock is held during git or process queries.

use std::collections::BTreeMap;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::control::{Failure, RepositoryTrees, SessionDirectory, SidebarData};

#[derive(Default)]
pub(super) struct Cache {
    directories: BTreeMap<SessionId, PathBuf>,
    trees: BTreeMap<PathBuf, (Instant, Option<Vec<u8>>)>,
    repositories: BTreeMap<Vec<u8>, (Instant, RepositoryTrees)>,
}

impl Cache {
    pub(super) fn snapshot(
        &mut self,
        entries: &[(SessionId, u32, bool)],
    ) -> Result<SidebarData, Failure> {
        // Scope-specific requests must never expose cached directories from another scope.
        let mut sessions = Vec::new();
        let mut repositories = BTreeMap::new();
        let mut bytes = 0;
        for &(session, pid, live) in entries {
            if live && let Ok(path) = crate::sys::current_directory(pid) {
                self.directories.insert(session, path);
            }
            let path = self.directories.get(&session);
            let cwd = path.map_or_else(Vec::new, |p| p.as_os_str().as_bytes().to_vec());
            sessions.push(SessionDirectory { session, cwd });
            let Some(path) = path else {
                continue;
            };
            let expired = self
                .trees
                .get(path)
                .is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(3));
            if expired {
                let known = self
                    .trees
                    .get(path)
                    .and_then(|(_, repo)| repo.as_ref())
                    .filter(|repo| {
                        self.repositories
                            .get(*repo)
                            .is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(3))
                    })
                    .cloned();
                let repo = known.or_else(|| {
                    let trees = discover(path)?;
                    let repo = trees.repo.clone();
                    self.repositories
                        .insert(repo.clone(), (Instant::now(), trees));
                    Some(repo)
                });
                self.trees.insert(path.clone(), (Instant::now(), repo));
            }
            if let Some((_, Some(repo))) = self.trees.get(path)
                && !repositories.contains_key(repo)
                && let Some((_, trees)) = self.repositories.get(repo)
            {
                bytes += trees.repo.len() + trees.worktrees.iter().map(Vec::len).sum::<usize>();
                if bytes > 512 * 1024 {
                    return Err(Failure::BadRequest);
                }
                repositories.insert(repo.clone(), trees.clone());
            }
        }
        // Bound caches to directories reachable from current (scoped) queries; never hold a grid.
        if self.directories.len() > krill_proto::holder::MAX_SESSIONS {
            self.directories
                .retain(|id, _| entries.iter().any(|(s, _, _)| s == id));
        }
        self.trees
            .retain(|_, (at, _)| at.elapsed() < Duration::from_secs(6));
        self.repositories.retain(|repo, _| {
            self.trees
                .values()
                .any(|(_, key)| key.as_ref() == Some(repo))
        });
        Ok(SidebarData {
            sessions,
            repositories: repositories.into_values().collect(),
        })
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
                .any(|part| part == b".minikrill-preparing")
        })
        .take(512)
        .map(<[u8]>::to_vec)
        .collect();
    let repo = worktrees.first()?.clone();
    // Canonical paths make subdirectories and APFS /var -> /private/var agree with cwd.
    for tree in &mut worktrees {
        if let Ok(path) = PathBuf::from(std::ffi::OsString::from_vec(tree.clone())).canonicalize() {
            *tree = path.as_os_str().as_bytes().to_vec();
        }
    }
    let repo = PathBuf::from(std::ffi::OsString::from_vec(repo))
        .canonicalize()
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
        let data = parse(b"worktree /repo\0\0worktree /wt/.minikrill-preparing/tree\0detached\0\0worktree /wt/agent\0\0").unwrap();
        assert_eq!(data.worktrees, [b"/repo".to_vec(), b"/wt/agent".to_vec()]);
    }

    #[test]
    fn scoped_metadata_cannot_leak_cached_sessions() {
        let mut cache = Cache::default();
        cache
            .directories
            .insert(SessionId(1), PathBuf::from("/does-not-exist/one"));
        cache
            .directories
            .insert(SessionId(2), PathBuf::from("/does-not-exist/two"));
        let snapshot = cache.snapshot(&[(SessionId(1), 0, false)]).unwrap();
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].session, SessionId(1));
        assert!(snapshot.repositories.is_empty());
    }

    #[test]
    fn live_process_cwd_query() {
        assert_eq!(
            crate::sys::current_directory(std::process::id()).unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }
}
