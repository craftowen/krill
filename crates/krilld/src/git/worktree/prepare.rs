//! MK-095: one daemon-owned speculative checkout, bounded to 512 MiB and five minutes.
//! Only an open creation dialog requests a lease. No setup, includes, hooks, or network run here.
//! The marker precedes checkout, so daemon restart can remove an interrupted preparation.

use crate::git::worktree::compat::read_config;
use krill_proto::os::fs::DirBuilderExt as _;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::control::{PrepareWorktree, WorktreePreparation as Status};
use serde::{Deserialize, Serialize};

use super::*;
use crate::session::lock;

pub const TTL: Duration = Duration::from_secs(300);
pub const DISK_LIMIT: u64 = 512 * 1024 * 1024;
const DIRECTORY: &str = ".minikrill-preparing";
const MARKER: &str = "owner.json";
const RECENT: usize = 128;
static POOL: OnceLock<Result<Arc<Pool>, String>> = OnceLock::new();

#[derive(Debug, Serialize, Deserialize)]
struct Owner {
    repo: Vec<u8>,
}

#[derive(Debug)]
struct Slot {
    token: u64,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
    ready: Option<(PathBuf, String)>,
}

#[derive(Debug, Default)]
struct State {
    slot: Option<Slot>,
    promoting: bool,
    recent: VecDeque<(u64, Status)>,
}

impl State {
    fn settle(&mut self, token: u64, status: Status) {
        self.recent.retain(|(id, _)| *id != token);
        self.recent.push_back((token, status));
        while self.recent.len() > RECENT {
            self.recent.pop_front();
        }
    }
}

#[derive(Debug)]
pub struct Pool {
    q: Arc<GitQueue>,
    dir: PathBuf,
    state: Mutex<State>,
    changed: Condvar,
    ttl: Duration,
    limit: u64,
}

impl Pool {
    pub fn new(q: Arc<GitQueue>, root: &Path) -> Result<Arc<Self>, String> {
        Self::with_limits(q, root, TTL, DISK_LIMIT)
    }

    fn with_limits(
        q: Arc<GitQueue>,
        root: &Path,
        ttl: Duration,
        limit: u64,
    ) -> Result<Arc<Self>, String> {
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let dir = root.join(DIRECTORY);
        match fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir() && !m.is_symlink()) {
                    return Err("preparation directory is not a real directory".to_owned());
                }
            }
            Err(e) => return Err(e.to_string()),
        }
        let pool = Arc::new(Self {
            q,
            dir: fs::canonicalize(dir).map_err(|e| e.to_string())?,
            state: Mutex::default(),
            changed: Condvar::new(),
            ttl,
            limit,
        });
        // A previous daemon may have died between marker creation and worktree add/move.
        if pool.dir.join(MARKER).exists() {
            let owner: Owner = serde_json::from_slice(
                &fs::read(pool.dir.join(MARKER)).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let repo = PathBuf::from(OsString::from_vec(owner.repo));
            let _transaction = transaction(&pool.q, &repo)?;
            pool.cleanup(&repo)?;
        } else if pool.tree().exists() {
            return Err("unowned preparation directory; refusing cleanup".to_owned());
        }
        Ok(pool)
    }

    fn tree(&self) -> PathBuf {
        self.dir.join("tree")
    }

    /// Starts once, queries the same token, or cancels. No git IO runs under the state mutex.
    pub fn request(self: &Arc<Self>, req: &PrepareWorktree) -> Status {
        let mut state = lock(&self.state);
        if req.cancel {
            if let Some(slot) = state.slot.as_ref().filter(|s| s.token == req.token) {
                slot.cancel.store(true, Ordering::Release);
            }
            state.settle(req.token, Status::Cancelled);
            self.changed.notify_all();
            return Status::Cancelled;
        }
        if let Some((_, status)) = state.recent.iter().find(|(id, _)| *id == req.token) {
            return status.clone();
        }
        if state.promoting {
            return Status::Busy;
        }
        if let Some(slot) = &state.slot {
            return if slot.token != req.token {
                Status::Busy
            } else if slot.ready.is_some() {
                Status::Ready
            } else {
                Status::Preparing
            };
        }
        let cancel = Arc::new(AtomicBool::new(false));
        state.slot = Some(Slot {
            token: req.token,
            cancel: cancel.clone(),
            deadline: Instant::now() + self.ttl,
            ready: None,
        });
        let (pool, req) = (self.clone(), req.clone());
        let token = req.token;
        if let Err(e) = thread::Builder::new()
            .name("worktree-prepare".to_owned())
            .spawn(move || pool.run(&req, &cancel))
        {
            state.slot = None;
            state.settle(token, Status::Failed(e.to_string()));
            return Status::Failed(e.to_string());
        }
        Status::Preparing
    }

    fn run(&self, req: &PrepareWorktree, cancel: &Arc<AtomicBool>) {
        let result = self.prepare(req, cancel);
        if let Ok(Some((repo, commit))) = &result {
            let mut state = lock(&self.state);
            if let Some(slot) = state.slot.as_mut().filter(|s| s.token == req.token) {
                slot.ready = Some((repo.clone(), commit.clone()));
            }
            self.changed.notify_all();
            loop {
                let Some(slot) = state.slot.as_ref().filter(|s| s.token == req.token) else {
                    return;
                };
                if cancel.load(Ordering::Acquire) || Instant::now() >= slot.deadline {
                    break;
                }
                let remaining = slot.deadline.saturating_duration_since(Instant::now());
                state = self
                    .changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
        // The marker is the sole authority for cleanup, including failed/partial checkouts.
        let cleanup = (|| {
            if self.dir.join(MARKER).exists() {
                let owner: Owner = serde_json::from_slice(
                    &fs::read(self.dir.join(MARKER)).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let repo = PathBuf::from(OsString::from_vec(owner.repo));
                let _transaction = transaction(&self.q, &repo)?;
                self.cleanup(&repo)?;
            }
            Ok::<_, String>(())
        })();
        let mut state = lock(&self.state);
        // Failed cleanup occupies the only slot: never accumulate abandoned disk usage.
        if cleanup.is_ok() {
            state.slot = None;
        }
        let status = match (result, cleanup) {
            (_, Err(e)) | (Err(e), _) => Status::Failed(e),
            (Ok(None), _) => Status::Skipped,
            (Ok(Some(_)), _) => Status::Cancelled,
        };
        if !state.recent.iter().any(|(id, _)| *id == req.token) {
            state.settle(req.token, status);
        }
        self.changed.notify_all();
    }

    fn prepare(
        &self,
        req: &PrepareWorktree,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Option<(PathBuf, String)>, String> {
        let given = Path::new(OsStr::from_bytes(&req.repo));
        let repo = output_path(
            git(&self.q, given, ["rev-parse", "--show-toplevel"])?,
            "not a git repository",
        )?;
        let _transaction = transaction(&self.q, &repo)?;
        let config = read_config(&repo)?;
        let base = base(
            &self.q,
            &repo,
            req.base.as_deref().or(config.worktree.base.as_deref()),
        )?;
        let commit = text(
            git(
                &self.q,
                &repo,
                [
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &format!("{base}^{{commit}}"),
                ],
            )?,
            "cannot resolve base",
        )?;
        // Smudge/process filters may download data or expand blobs without a bound. Use ordinary
        // creation for these repositories rather than invoke arbitrary programs speculatively.
        let filters = git(
            &self.q,
            &repo,
            ["config", "--get-regexp", "^filter\\..*\\.(smudge|process)$"],
        )?;
        if !filters.stdout.is_empty() && uses_filters(&self.q, &repo, &commit)? {
            return Ok(None);
        }
        let tree = git(&self.q, &repo, ["ls-tree", "-r", "-l", "-z", &commit])?;
        if !tree.status.success() {
            return Err("cannot estimate preparation checkout".to_owned());
        }
        if !fits(&tree.stdout, self.limit) {
            return Ok(None);
        }
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        let owner = Owner {
            repo: repo.as_os_str().as_bytes().to_vec(),
        };
        let bytes = serde_json::to_vec(&owner).map_err(|e| e.to_string())?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.dir.join(MARKER))
            .and_then(|mut f| f.write_all(&bytes))
            .map_err(|e| e.to_string())?;
        let path = self.tree();
        let add = [
            OsString::from("-c"),
            "core.hooksPath=/dev/null".into(),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "worktree".into(),
            "add".into(),
            "--detach".into(),
            "--no-checkout".into(),
            "--lock".into(),
            "--reason=minikrill speculative preparation".into(),
            path.clone().into_os_string(),
            commit.clone().into(),
        ];
        self.checkout(&repo, add, cancel)?;
        self.checkout(
            &repo,
            [
                OsString::from("-c"),
                "core.hooksPath=/dev/null".into(),
                "-c".into(),
                "core.fsmonitor=false".into(),
                "-C".into(),
                path.into_os_string(),
                "reset".into(),
                "--hard".into(),
                commit.clone().into(),
            ],
            cancel,
        )?;
        disk_usage(&self.tree(), self.limit).map_err(|e| e.to_string())?;
        Ok(Some((repo, commit)))
    }

    fn checkout(
        &self,
        repo: &Path,
        args: impl IntoIterator<Item = OsString>,
        cancel: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        let out = self
            .q
            .speculative(
                repo.to_path_buf(),
                args,
                cancel.clone(),
                self.tree(),
                self.limit,
            )
            .recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        text(out, "speculative checkout failed").map(|_| ())
    }

    fn cleanup(&self, repo: &Path) -> Result<(), String> {
        let path = self.tree();
        // Interrupted `worktree add` may register the tree before writing its .git file.
        // Query registration rather than trusting that file, and never prune other worktrees.
        let listed = git(&self.q, repo, ["worktree", "list", "--porcelain", "-z"])?;
        if !listed.status.success() {
            return Err("cannot list preparation registration".to_owned());
        }
        let registered = listed
            .stdout
            .split(|b| *b == 0)
            .filter_map(|record| record.strip_prefix(b"worktree "))
            .any(|registered| registered == path.as_os_str().as_bytes());
        if registered {
            text(
                git(
                    &self.q,
                    repo,
                    [
                        OsString::from("worktree"),
                        "remove".into(),
                        "--force".into(),
                        "--force".into(),
                        path.clone().into_os_string(),
                    ],
                )?,
                "cannot remove preparation",
            )?;
        } else {
            match fs::remove_dir_all(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        fs::remove_file(self.dir.join(MARKER)).map_err(|e| e.to_string())
    }

    /// Caller holds the repository transaction lock. A moved base never consumes a stale checkout.
    pub(super) fn promote(
        &self,
        q: &GitQueue,
        repo: &Path,
        base: &str,
        path: &Path,
        branch: &str,
    ) -> Result<bool, String> {
        let commit = text(
            git(
                q,
                repo,
                [
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &format!("{base}^{{commit}}"),
                ],
            )?,
            "cannot resolve base",
        )?;
        let token = {
            let mut state = lock(&self.state);
            let matches = state.slot.as_ref().is_some_and(|s| {
                !s.cancel.load(Ordering::Acquire)
                    && Instant::now() < s.deadline
                    && s.ready
                        .as_ref()
                        .is_some_and(|(r, c)| r == repo && *c == commit)
            });
            if !matches {
                return Ok(false);
            }
            let Some(slot) = state.slot.take() else {
                return Ok(false);
            };
            state.promoting = true;
            self.changed.notify_all();
            slot.token
        };
        let moved = (|| {
            // move requires a missing destination, while free_name reserves an empty directory.
            fs::remove_dir(path).map_err(|e| e.to_string())?;
            text(
                git(
                    q,
                    repo,
                    [
                        OsString::from("worktree"),
                        "unlock".into(),
                        self.tree().into_os_string(),
                    ],
                )?,
                "cannot unlock preparation",
            )?;
            text(
                git(
                    q,
                    repo,
                    [
                        OsString::from("worktree"),
                        "move".into(),
                        self.tree().into_os_string(),
                        path.as_os_str().to_owned(),
                    ],
                )?,
                "cannot move preparation",
            )
        })();
        if let Err(err) = moved {
            let cleaned = self.cleanup(repo).is_ok();
            let mut state = lock(&self.state);
            state.promoting = !cleaned;
            state.settle(token, Status::Failed(err.clone()));
            self.changed.notify_all();
            return Err(err);
        }
        // Ownership transfers at move. A failed hook/branch operation must preserve the new tree.
        let marker = fs::remove_file(self.dir.join(MARKER)).map_err(|e| e.to_string());
        {
            let mut state = lock(&self.state);
            state.promoting = false;
            state.settle(token, Status::Skipped);
        }
        self.changed.notify_all();
        marker?;
        text(
            git(
                q,
                repo,
                [
                    OsString::from("-C"),
                    path.as_os_str().to_owned(),
                    "checkout".into(),
                    "--no-track".into(),
                    "-b".into(),
                    branch.into(),
                    commit.into(),
                ],
            )?,
            "cannot create prepared branch",
        )?;
        Ok(true)
    }
}

/// A configured filter runs only for paths whose attributes name it. `git lfs install` configures
/// `filter.lfs.*` globally, so skip only when an attributes source of `commit` mentions `filter=`
/// (macros included). Git without `git var GIT_ATTR_*` (before 2.42) is treated as filtered.
fn uses_filters(q: &GitQueue, repo: &Path, commit: &str) -> Result<bool, String> {
    let tree = git(
        q,
        repo,
        [
            "grep",
            "-q",
            "-I",
            "-F",
            "-e",
            "filter=",
            commit,
            "--",
            ".gitattributes",
            "*/.gitattributes",
        ],
    )?;
    match tree.status.code() {
        Some(1) => {}
        Some(0) => return Ok(true),
        _ => return Err("cannot inspect attributes".to_owned()),
    }
    let mut files = vec![repo.join(output_path(
        git(q, repo, ["rev-parse", "--git-path", "info/attributes"])?,
        "cannot locate attributes",
    )?)];
    for var in ["GIT_ATTR_GLOBAL", "GIT_ATTR_SYSTEM"] {
        let out = git(q, repo, ["var", var])?;
        if !out.status.success() {
            return Ok(true);
        }
        files.push(output_path(out, "cannot locate attributes")?);
    }
    Ok(files
        .iter()
        .any(|file| fs::read(file).is_ok_and(|bytes| bytes.windows(7).any(|w| w == b"filter="))))
}

/// Conservative preflight: rounded blobs, unique directories, index entries, and metadata reserve.
fn fits(bytes: &[u8], limit: u64) -> bool {
    let mut total = 1024_u64 * 1024;
    let mut dirs = HashSet::new();
    for record in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let mut fields = record.splitn(2, |b| *b == b'\t');
        let (Some(meta), Some(path)) = (fields.next(), fields.next()) else {
            return false;
        };
        let Some(size) = meta
            .rsplit(|b| *b == b' ')
            .next()
            .and_then(|n| std::str::from_utf8(n).ok())
            .and_then(|n| n.parse::<u64>().ok())
        else {
            return false;
        };
        total = total
            .saturating_add(size.div_ceil(4096).max(1).saturating_mul(4096))
            .saturating_add(256);
        for (index, byte) in path.iter().enumerate() {
            if *byte == b'/'
                && let Some(dir) = path.get(..index)
                && dirs.insert(dir)
            {
                total = total.saturating_add(4096);
            }
        }
        if total > limit {
            return false;
        }
    }
    total <= limit
}

/// Count logical or allocated bytes (whichever is larger), without following symlinks.
pub(crate) fn disk_usage(path: &Path, limit: u64) -> io::Result<u64> {
    fn visit(path: &Path, used: &mut u64, limit: u64) -> io::Result<()> {
        let m = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        *used = used.saturating_add(m.len().max(crate::os::allocated(&m)));
        if *used > limit {
            return Err(io::Error::other("preparation exceeds disk budget"));
        }
        if m.is_dir() {
            for entry in fs::read_dir(path)? {
                visit(&entry?.path(), used, limit)?;
            }
        }
        Ok(())
    }
    let mut used = 0;
    visit(path, &mut used, limit)?;
    Ok(used)
}

pub(super) fn global() -> Result<&'static Arc<Pool>, String> {
    POOL.get_or_init(|| Pool::new(Arc::clone(queue()?), &root()?))
        .as_ref()
        .map_err(Clone::clone)
}

pub(super) fn existing() -> Option<&'static Pool> {
    POOL.get()
        .and_then(|pool| pool.as_ref().ok())
        .map(AsRef::as_ref)
}

pub(super) fn recover() -> Result<(), String> {
    if root()?.join(DIRECTORY).join(MARKER).exists() {
        global()?;
    }
    Ok(())
}

pub fn serve(req: &PrepareWorktree) -> Reply {
    Reply::WorktreePreparation(global().map_or_else(Status::Failed, |pool| pool.request(req)))
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
