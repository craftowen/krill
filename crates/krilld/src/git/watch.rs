//! Visibility-controlled `FSEvents` backend API for MK-053. The owner calls `poll` at least every
//! 100ms while visible, and reads `snapshot` when it returns true. No UI integration lives here.
//! Hidden is the initial state: no watcher, polling, or git children. Hiding cancels and reaps
//! pending work before returning. Filesystem notifications use one bounded wake slot.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::git::Changes;
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use super::{GitQueue, changes};

pub const DEBOUNCE: Duration = Duration::from_millis(200);
pub const MIN_INTERVAL: Duration = Duration::from_secs(3);
pub const SAFETY_INTERVAL: Duration = Duration::from_secs(60);
/// K-16: 숨김 처리 때 진행 중 조회를 회수하려고 기다리는 상한. `Hub` 잠금을 쥔 요청 경로라
/// 같은 레포 작업 대기(최대 30초)를 그대로 기다리면 모든 `GitWatch` 요청이 앱 타임아웃을 넘는다.
/// 취소 플래그가 먼저 서 있어 실행 중 조회는 수십 ms 안에 끝나고, 차례 대기 중이던 조회는
/// 워커가 끝나면 전송 실패로 조용히 버려지므로 유예를 넘기면 그냥 버리고 잠금을 놓는다.
const HIDE_REAP_WAIT: Duration = Duration::from_millis(100);
/// How long `set_visibility` waits for `FSEvents` registration before finishing it in the
/// background. Registration is a synchronous fseventsd round trip: normally under 1ms, but it
/// stalled 5.6-6.5s while other watchers churned, past the app's 5s daemon read timeout.
const SETUP_WAIT: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Hidden,
    Visible,
}

#[derive(Debug, Default)]
struct Pace {
    dirty: Option<Instant>,
    started: Option<Instant>,
}

impl Pace {
    fn event(&mut self, now: Instant) {
        self.dirty.get_or_insert(now);
    }
    fn due(&self, now: Instant) -> bool {
        let elapsed = self.started.map(|t| now.saturating_duration_since(t));
        if elapsed.is_some_and(|d| d < MIN_INTERVAL) {
            return false;
        }
        self.dirty
            .is_some_and(|t| now.saturating_duration_since(t) >= DEBOUNCE)
            || elapsed.is_some_and(|d| d >= SAFETY_INTERVAL)
    }
    fn start(&mut self, now: Instant) {
        self.dirty = None;
        self.started = Some(now);
    }
}

/// A watcher for one worktree root (not a subdirectory). Retain it until the panel closes.
/// notify owns the platform `FSEvents` thread on macOS; all git work uses the shared queue.
#[must_use = "retain the watcher while observing worktree changes"]
pub struct Watch {
    q: Arc<GitQueue>,
    repo: PathBuf,
    visible: bool,
    watcher: Option<ActiveWatcher>,
    setup: Option<Receiver<Result<ActiveWatcher, String>>>,
    events: Receiver<()>,
    pace: Pace,
    cancel: Arc<AtomicBool>,
    pending: Option<Receiver<Result<Changes, String>>>,
    latest: Option<Result<Arc<Changes>, String>>,
}

const STAT_POLL_INTERVAL: Duration = Duration::from_millis(400);
const STAT_ENTRY_BUDGET: usize = 1024;
const STAT_MAX_DEPTH: usize = 8;

struct ActiveWatcher {
    _watcher: RecommendedWatcher,
    stop: Arc<AtomicBool>,
    poller: Option<thread::JoinHandle<()>>,
}

impl Drop for ActiveWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.poller.take() {
            handle.thread().unpark();
        }
    }
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("visible", &self.visible)
            .field("pending", &self.pending.is_some())
            .finish_non_exhaustive()
    }
}

impl Watch {
    /// Use the same daemon queue as worktree creation, removal, and CLI queries.
    pub fn for_repo(repo: PathBuf) -> Result<Self, String> {
        Ok(Self::new(Arc::clone(super::queue()?), repo))
    }

    pub fn new(q: Arc<GitQueue>, repo: PathBuf) -> Self {
        Self {
            q,
            repo,
            visible: false,
            watcher: None,
            setup: None,
            events: mpsc::channel().1,
            pace: Pace::default(),
            cancel: Arc::new(AtomicBool::new(true)),
            pending: None,
            latest: None,
        }
    }

    /// Start `FSEvents`, or stop it and synchronously cancel/reap outstanding git work.
    /// Revealing always refreshes, including edits made while hidden. The first query waits for
    /// `FSEvents` registration, so no edit falls between the snapshot and the watcher.
    pub fn set_visibility(&mut self, visibility: Visibility) -> Result<(), String> {
        if visibility == Visibility::Hidden {
            self.visible = false;
            self.cancel.store(true, Ordering::Release);
            self.setup = None;
            if let Some(watcher) = self.watcher.take() {
                // Stopping the stream is another fseventsd round trip; drop inline only if spawn fails.
                thread::Builder::new()
                    .name("git-watch-stop".into())
                    .spawn(move || drop(watcher))
                    .ok();
            }
            if let Some(rx) = self.pending.take() {
                rx.recv_timeout(HIDE_REAP_WAIT).ok();
            }
            self.pace.dirty = None;
            self.pace.started = None;
            return Ok(());
        }
        if self.visible {
            return Ok(());
        }
        let canonical = fs::canonicalize(&self.repo).map_err(|e| e.to_string())?;
        let repo = canonical
            .ancestors()
            .find(|dir| dir.join(".git").exists())
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                "fatal: not a git repository (or any of the parent directories): .git".to_owned()
            })?;
        let metadata = metadata_dirs(&repo)?;
        let (tx, rx) = mpsc::sync_channel(1);
        let (setup_tx, setup_rx) = mpsc::channel();
        let watched_repo = repo.clone();
        thread::Builder::new()
            .name("git-watch-setup".into())
            .spawn(move || {
                setup_tx.send(register(tx, &watched_repo, metadata)).ok();
            })
            .map_err(|e| e.to_string())?;
        self.repo = repo;
        self.events = rx;
        self.visible = true;
        self.setup = Some(setup_rx);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.force_refresh();
        self.finish_setup(SETUP_WAIT)
    }

    /// Installs the watcher once registration finishes. A failure hides the watch so the next
    /// reveal retries, matching a failed synchronous setup.
    fn finish_setup(&mut self, wait: Duration) -> Result<(), String> {
        let Some(rx) = &self.setup else {
            return Ok(());
        };
        let result = match rx.recv_timeout(wait) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => return Ok(()),
            Err(RecvTimeoutError::Disconnected) => Err("FSEvents setup thread exited".to_owned()),
        };
        self.setup = None;
        match result {
            Ok(watcher) => {
                self.watcher = Some(watcher);
                Ok(())
            }
            Err(e) => {
                self.set_visibility(Visibility::Hidden).ok();
                Err(e)
            }
        }
    }

    /// Force an immediate refresh on the next `poll()`, bypassing `MIN_INTERVAL` cooldown.
    pub fn force_refresh(&mut self) {
        self.pace.started = None;
        self.pace.dirty = Some(
            Instant::now()
                .checked_sub(DEBOUNCE)
                .unwrap_or_else(Instant::now),
        );
    }

    /// Whether a git query is currently in flight or queued for debounce/cooldown.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending.is_some() || self.pace.dirty.is_some()
    }

    /// Nonblocking scheduler step. Returns true when `snapshot` has changed (including errors).
    /// Event bursts cannot postpone an update indefinitely: debounce starts at the first event.
    pub fn poll(&mut self) -> bool {
        if !self.visible {
            return false;
        }
        if let Err(e) = self.finish_setup(Duration::ZERO) {
            self.latest = Some(Err(e));
            return true;
        }
        if self.setup.is_some() {
            return false;
        }
        let now = Instant::now();
        while self.events.try_recv().is_ok() {
            self.pace.event(now);
        }
        let mut changed = false;
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(result) => {
                    self.latest = Some(result.map(Arc::new));
                    self.pending = None;
                    changed = true;
                }
                Err(TryRecvError::Disconnected) => {
                    self.latest = Some(Err("git queue disconnected".to_owned()));
                    self.pending = None;
                    changed = true;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if self.pending.is_none() && self.pace.due(now) {
            self.pace.start(now);
            self.pending = Some(changes::query(
                &self.q,
                self.repo.clone(),
                Arc::clone(&self.cancel),
            ));
        }
        changed
    }

    /// Arc keeps repeated UI reads from copying the bounded path list.
    pub fn snapshot(&self) -> Option<&Result<Arc<Changes>, String>> {
        self.latest.as_ref()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.set_visibility(Visibility::Hidden).ok();
    }
}

const MAX_WATCHES: usize = 16;
const STALE_WATCH: Duration = Duration::from_secs(5);

struct HubEntry {
    watch: Watch,
    generation: u64,
    setup_error: Option<String>,
    last_seen: Instant,
    subscribers: std::collections::BTreeMap<String, Instant>,
}

/// OWE-585(MK-053): 데몬 제어 요청(`Request::GitWatch`)을 저장소별 [`Watch`]에 연결한다.
#[derive(Default)]
pub struct Hub {
    q: Option<Arc<GitQueue>>,
    entries: std::collections::BTreeMap<PathBuf, HubEntry>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

fn resolve_key(raw: &Path) -> PathBuf {
    let Ok(canonical) = fs::canonicalize(raw) else {
        return raw.to_path_buf();
    };
    canonical
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
        .unwrap_or(canonical)
}

impl Hub {
    #[must_use]
    pub fn with_queue(q: Arc<GitQueue>) -> Self {
        Self {
            q: Some(q),
            entries: std::collections::BTreeMap::new(),
        }
    }

    pub fn handle(
        &mut self,
        req: &krill_proto::git::GitWatchRequest,
    ) -> krill_proto::git::GitWatchReply {
        self.handle_for("", req)
    }

    pub fn handle_for(
        &mut self,
        subscriber: &str,
        req: &krill_proto::git::GitWatchRequest,
    ) -> krill_proto::git::GitWatchReply {
        use krill_proto::git::GitWatchReply;
        use krill_proto::os::ffi::OsStrExt as _;
        let now = Instant::now();
        let raw_key = PathBuf::from(std::ffi::OsStr::from_bytes(&req.repo));
        if !req.visible {
            if raw_key.as_os_str().is_empty() {
                for entry in self.entries.values_mut() {
                    entry.subscribers.remove(subscriber);
                    entry
                        .subscribers
                        .retain(|_, seen| now.saturating_duration_since(*seen) <= STALE_WATCH);
                    if entry.subscribers.is_empty() {
                        entry.watch.set_visibility(Visibility::Hidden).ok();
                    }
                }
            } else {
                let key = resolve_key(&raw_key);
                for k in [raw_key, key] {
                    if let Some(entry) = self.entries.get_mut(&k) {
                        entry.subscribers.remove(subscriber);
                        entry
                            .subscribers
                            .retain(|_, seen| now.saturating_duration_since(*seen) <= STALE_WATCH);
                        if entry.subscribers.is_empty() {
                            entry.watch.set_visibility(Visibility::Hidden).ok();
                        }
                    }
                }
            }
            return GitWatchReply::Hidden;
        }
        let key = resolve_key(&raw_key);
        for (k, entry) in &mut self.entries {
            entry
                .subscribers
                .retain(|_, seen| now.saturating_duration_since(*seen) <= STALE_WATCH);
            if *k != key
                && entry.subscribers.is_empty()
                && now.saturating_duration_since(entry.last_seen) > STALE_WATCH
            {
                entry.watch.set_visibility(Visibility::Hidden).ok();
            }
        }
        if !self.entries.contains_key(&key) {
            if self.entries.len() >= MAX_WATCHES
                && let Some(oldest) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| (e.subscribers.len(), e.last_seen))
                    .map(|(k, _)| k.clone())
            {
                self.entries.remove(&oldest);
            }
            let queue = match &self.q {
                Some(q) => Ok(Arc::clone(q)),
                None => super::queue().map(Arc::clone),
            };
            let watch = match queue {
                Ok(q) => Watch::new(q, key.clone()),
                Err(e) => {
                    return GitWatchReply::Failed {
                        generation: 1,
                        error: e,
                    };
                }
            };
            self.entries.insert(
                key.clone(),
                HubEntry {
                    watch,
                    generation: 0,
                    setup_error: None,
                    last_seen: now,
                    subscribers: std::collections::BTreeMap::new(),
                },
            );
        }
        let Some(entry) = self.entries.get_mut(&key) else {
            return GitWatchReply::Hidden;
        };
        entry.last_seen = now;
        entry.subscribers.insert(subscriber.to_owned(), now);
        let was_hidden = !entry.watch.visible;
        if let Err(e) = entry.watch.set_visibility(Visibility::Visible) {
            if entry.setup_error.as_deref() != Some(e.as_str()) {
                entry.generation = entry.generation.wrapping_add(1).max(1);
                entry.setup_error = Some(e.clone());
            }
            return if req.since < entry.generation || req.force {
                GitWatchReply::Failed {
                    generation: entry.generation,
                    error: e,
                }
            } else {
                GitWatchReply::Unchanged { pending: false }
            };
        }
        entry.setup_error = None;
        if req.force && !was_hidden {
            entry.watch.force_refresh();
        }
        let changed = entry.watch.poll();
        if changed {
            entry.generation = entry.generation.wrapping_add(1).max(1);
        }
        let fresh = changed || (!req.force && !was_hidden && req.since < entry.generation);
        if fresh {
            match entry.watch.snapshot() {
                Some(Ok(changes)) => GitWatchReply::Changed {
                    generation: entry.generation,
                    changes: (**changes).clone(),
                },
                Some(Err(e)) => GitWatchReply::Failed {
                    generation: entry.generation,
                    error: e.clone(),
                },
                None => GitWatchReply::Unchanged {
                    pending: entry.watch.is_pending(),
                },
            }
        } else {
            GitWatchReply::Unchanged {
                pending: entry.watch.is_pending(),
            }
        }
    }
}

// Runs off the request path: fseventsd round trips can stall for seconds.
fn register(
    tx: SyncSender<()>,
    repo: &Path,
    metadata: Vec<PathBuf>,
) -> Result<ActiveWatcher, String> {
    let roots = metadata.clone();
    let watched_repo = repo.to_path_buf();
    let notify_tx = tx.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let relevant = event.map_or(true, |e| {
            e.need_rescan()
                || e.paths
                    .iter()
                    .any(|path| relevant(path, &watched_repo, &roots))
        });
        if relevant {
            notify_tx.try_send(()).ok();
        }
    })
    .map_err(|e| e.to_string())?;
    // On macOS every `watch()` restarts the stream (stop, purge, start); add all roots, start once.
    let mut paths = watcher.paths_mut();
    paths
        .add(repo, RecursiveMode::Recursive)
        .map_err(|e| e.to_string())?;
    for root in &metadata {
        paths
            .add(root, RecursiveMode::NonRecursive)
            .map_err(|e| e.to_string())?;
        for (leaf, mode) in [
            ("logs", RecursiveMode::NonRecursive),
            ("worktrees", RecursiveMode::Recursive),
        ] {
            let path = root.join(leaf);
            if path.is_dir() {
                paths.add(&path, mode).map_err(|e| e.to_string())?;
            }
        }
    }
    paths.commit().map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let poller_stop = Arc::clone(&stop);
    let polled_repo = repo.to_path_buf();
    let mut prev = fingerprint(&polled_repo, &metadata);
    let poller = thread::Builder::new()
        .name("git-watch-stat".into())
        .spawn(move || {
            crate::alloc::mobile_thread().ok();
            while !poller_stop.load(Ordering::Acquire) {
                thread::park_timeout(STAT_POLL_INTERVAL);
                if poller_stop.load(Ordering::Acquire) {
                    break;
                }
                let next = fingerprint(&polled_repo, &metadata);
                if next != prev {
                    prev = next;
                    tx.try_send(()).ok();
                }
            }
        })
        .ok();
    Ok(ActiveWatcher {
        _watcher: watcher,
        stop,
        poller,
    })
}

fn fingerprint(repo: &Path, metadata: &[PathBuf]) -> u64 {
    let mut budget = STAT_ENTRY_BUDGET;
    let mut acc = 0_u64;
    visit_dir(repo, 0, true, &mut budget, &mut acc);
    for root in metadata {
        for leaf in ["HEAD", "index", "packed-refs", "config"] {
            if budget == 0 {
                break;
            }
            let path = root.join(leaf);
            if let Ok(meta) = fs::symlink_metadata(&path) {
                budget -= 1;
                acc = acc.wrapping_add(hash_entry(&path, &meta));
            }
        }
        for (dir_leaf, max_depth) in [("logs", 1_usize), ("worktrees", 4), ("refs", 4)] {
            if budget == 0 {
                break;
            }
            let path = root.join(dir_leaf);
            if path.is_dir() {
                visit_dir(
                    &path,
                    STAT_MAX_DEPTH - max_depth,
                    false,
                    &mut budget,
                    &mut acc,
                );
            }
        }
    }
    acc
}

fn visit_dir(dir: &Path, depth: usize, skip_ignored: bool, budget: &mut usize, acc: &mut u64) {
    if *budget == 0 || depth > STAT_MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if *budget == 0 {
            return;
        }
        let name = entry.file_name();
        if skip_ignored
            && [
                ".git",
                "node_modules",
                "target",
                "dist",
                "build",
                ".next",
                ".venv",
            ]
            .iter()
            .any(|s| name == *s)
        {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        *budget -= 1;
        *acc = acc.wrapping_add(hash_entry(&path, &meta));
        if meta.is_dir() && depth < STAT_MAX_DEPTH {
            visit_dir(&path, depth + 1, skip_ignored, budget, acc);
        }
    }
}

fn hash_entry(path: &Path, meta: &fs::Metadata) -> u64 {
    use krill_proto::os::ffi::OsStrExt as _;
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for &b in path.as_os_str().as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^= meta.len();
    h = h.wrapping_mul(0x0100_0000_01b3);
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .unwrap_or_default();
    h ^= mtime.as_secs();
    h = h.wrapping_mul(0x0100_0000_01b3);
    h ^= u64::from(mtime.subsec_nanos());
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

// Resolve .git and commondir using files rather than starting git during watcher setup.
fn metadata_dirs(repo: &Path) -> Result<Vec<PathBuf>, String> {
    let dot = repo.join(".git");
    let git = if dot.is_dir() {
        dot
    } else {
        let text = fs::read(&dot).map_err(|e| e.to_string())?;
        let path = text.strip_prefix(b"gitdir: ").ok_or("invalid .git file")?;
        repo.join(path_from_line(path))
    };
    let git = fs::canonicalize(git).map_err(|e| e.to_string())?;
    let mut dirs = vec![git.clone()];
    match fs::read(git.join("commondir")) {
        Ok(bytes) => {
            let common =
                fs::canonicalize(git.join(path_from_line(&bytes))).map_err(|e| e.to_string())?;
            if common != git {
                dirs.push(common);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    Ok(dirs)
}

fn path_from_line(bytes: &[u8]) -> &Path {
    use krill_proto::os::ffi::OsStrExt as _;
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    Path::new(std::ffi::OsStr::from_bytes(bytes))
}

fn relevant(path: &Path, repo: &Path, metadata: &[PathBuf]) -> bool {
    if metadata.iter().any(|root| {
        path.strip_prefix(root).is_ok_and(|p| {
            p.as_os_str().is_empty()
                || p.components().next().is_some_and(|c| {
                    [
                        "HEAD",
                        "index",
                        "packed-refs",
                        "config",
                        "logs",
                        "worktrees",
                        "refs",
                    ]
                    .iter()
                    .any(|s| c.as_os_str() == *s)
                })
        })
    }) {
        return true;
    }
    path.strip_prefix(repo).is_ok_and(|p| {
        !p.components().any(|c| {
            [
                ".git",
                "node_modules",
                "target",
                "dist",
                "build",
                ".next",
                ".venv",
            ]
            .iter()
            .any(|s| c.as_os_str() == *s)
        })
    })
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
