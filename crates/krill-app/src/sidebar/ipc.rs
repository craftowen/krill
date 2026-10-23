//! 사이드바 스냅샷을 krilld에서 읽는다(백그라운드 스레드에서만 호출).

use krill_proto::os::ffi::{OsStrExt as _, OsStringExt as _};
use krill_proto::os::net::UnixStream;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use krill_proto::control::{
    AgentState, AgentStatus, FEAT_CONTROL, FEAT_GIT_WATCH, FEAT_SIDEBAR, FEAT_WORKTREE,
    NewWorktree, Reply, Request,
};
use krill_proto::git::{GitWatchReply, GitWatchRequest};
use krill_proto::hook::FEAT_HOOK;
use krill_proto::screen::Msg as _;

use crate::hosts::HostTarget;

use super::{Meta, Session, Snapshot, Source, Tree};

/// krilld `git::watch::Hub`의 최대 감시 슬롯 수(8)를 넘기지 않는다.
const MAX_WATCHES: usize = 8;

#[derive(Default)]
struct WatchState {
    generation: u64,
    dirty: Option<usize>,
}

/// 워크트리별 `GitWatch` 세대·미커밋 파일 수 캐시(N19).
/// `FSEvents` 변화가 없으면 krilld가 `Unchanged`를 즉시 돌려줘 10초 주기 `git status` 폴링 없이 결과를 재사용한다.
#[derive(Default)]
pub(super) struct WatchCache {
    entries: BTreeMap<PathBuf, WatchState>,
}

impl WatchCache {
    pub(super) fn poll(
        &mut self,
        trees: &BTreeMap<PathBuf, Vec<PathBuf>>,
        mut call: impl FnMut(GitWatchRequest) -> io::Result<GitWatchReply>,
    ) -> BTreeMap<PathBuf, usize> {
        let current: Vec<PathBuf> = trees
            .values()
            .flatten()
            .take(MAX_WATCHES)
            .cloned()
            .collect();
        let removed: Vec<PathBuf> = self
            .entries
            .keys()
            .filter(|k| !current.contains(k))
            .cloned()
            .collect();
        for old in removed {
            self.entries.remove(&old);
            call(GitWatchRequest {
                repo: old.as_os_str().as_bytes().to_vec(),
                visible: false,
                since: 0,
                force: false,
            })
            .ok();
        }
        for tree in current {
            let state = self.entries.entry(tree.clone()).or_default();
            let req = GitWatchRequest {
                repo: tree.as_os_str().as_bytes().to_vec(),
                visible: true,
                since: state.generation,
                force: false,
            };
            match call(req) {
                Ok(GitWatchReply::Changed {
                    generation,
                    changes,
                }) => {
                    state.generation = generation;
                    state.dirty = Some(changes.files.len());
                }
                Ok(GitWatchReply::Unchanged { .. }) => {}
                Ok(GitWatchReply::Failed { generation, .. }) => {
                    state.generation = generation;
                    state.dirty = None;
                }
                Ok(GitWatchReply::Hidden) => {
                    state.dirty = None;
                }
                Err(_) => break,
            }
        }
        self.entries
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.dirty?)))
            .collect()
    }

    pub(super) fn clear(
        &mut self,
        mut call: impl FnMut(GitWatchRequest) -> io::Result<GitWatchReply>,
    ) {
        let prev = std::mem::take(&mut self.entries);
        for tree in prev.into_keys() {
            call(GitWatchRequest {
                repo: tree.as_os_str().as_bytes().to_vec(),
                visible: false,
                since: 0,
                force: false,
            })
            .ok();
        }
    }
}

fn watch_exchange(stream: &mut UnixStream, req: GitWatchRequest) -> io::Result<GitWatchReply> {
    stream.write_all(&Request::GitWatch(req).encode()?)?;
    let (kind, bytes) =
        krill_proto::ipc::read_frame(stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    match Reply::decode(kind, &bytes).map_err(io::Error::other)? {
        Reply::GitWatch(reply) => Ok(reply),
        Reply::Failed(f) => Err(io::Error::other(f.to_string())),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

struct Conn {
    stream: UnixStream,
    features: u64,
}

type Job = Box<dyn FnOnce() + Send>;

/// 이보다 오래 걸린 호스트 폴링은 운영 로그에 남긴다.
const SLOW_POLL: Duration = Duration::from_secs(1);

pub(super) struct Socket {
    shared: Arc<Shared>,
    /// 호스트 전용 작업 스레드. 폴링·숨김 정리를 여기서 차례로 돌려, 원격 ssh(`resolve_remote_socket`)나
    /// IPC 대기가 공유 실행기(GCD) 스레드를 붙잡거나 연결 뮤텍스 앞에 줄 세우지 않는다.
    worker: Mutex<Option<mpsc::Sender<Job>>>,
    /// gpui 테스트 스케줄러는 다른 스레드의 깨우기를 비결정으로 보고 실패시킨다. 단위 테스트 빌드는
    /// 기본으로 호출 스레드에서 바로 돌리고(종전 동작), 전용 스레드 경로는 [`Socket::threaded`]로 검증한다.
    threaded: bool,
}

struct Shared {
    target: HostTarget,
    conn: Mutex<Option<Conn>>,
    watches: Mutex<WatchCache>,
    external: Mutex<ExternalCache>,
}

impl Socket {
    pub(super) fn new(target: impl Into<HostTarget>) -> Self {
        Self::build(target.into(), !cfg!(test))
    }

    #[cfg(all(test, unix))]
    pub(super) fn threaded(target: impl Into<HostTarget>) -> Self {
        Self::build(target.into(), true)
    }

    fn build(target: HostTarget, threaded: bool) -> Self {
        Self {
            shared: Arc::new(Shared {
                target,
                conn: Mutex::new(None),
                watches: Mutex::new(WatchCache::default()),
                external: Mutex::new(ExternalCache::default()),
            }),
            worker: Mutex::new(None),
            threaded,
        }
    }

    /// 작업을 전용 스레드에 넣는다. 스레드는 처음 쓸 때 띄우고, 소켓이 사라지면 남은 작업을 마친 뒤 끝난다.
    fn enqueue(&self, job: Job) -> bool {
        if !self.threaded {
            job();
            return true;
        }
        let Ok(mut worker) = self.worker.lock() else {
            return false;
        };
        if worker.is_none() {
            let (send, recv) = mpsc::channel::<Job>();
            let spawned = std::thread::Builder::new()
                .name("sidebar-host".into())
                .spawn(move || recv.into_iter().for_each(|job| job()));
            if let Err(error) = spawned {
                tracing::warn!(%error, "sidebar host worker could not start");
                return false;
            }
            *worker = Some(send);
        }
        worker.as_ref().is_some_and(|send| send.send(job).is_ok())
    }

    /// 테스트: 이미 넣은 작업이 모두 끝날 때까지 기다린다.
    #[cfg(all(test, unix))]
    pub(super) fn settle(&self) {
        let (done, wait) = mpsc::channel();
        if self.enqueue(Box::new(move || {
            done.send(()).ok();
        })) {
            wait.recv_timeout(Duration::from_secs(10)).ok();
        }
    }
}

impl Shared {
    fn with_conn<T>(
        &self,
        required: u64,
        read: Duration,
        f: impl FnOnce(&mut UnixStream) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut guard = self
            .conn
            .lock()
            .map_err(|_| io::Error::other("sidebar socket mutex poisoned"))?;
        if !self.target.is_current() {
            *guard = None;
            return Err(io::ErrorKind::Interrupted.into());
        }
        if guard
            .as_ref()
            .is_none_or(|c| c.features & required != required)
        {
            *guard = Some(connect(&self.target, required, read)?);
        }
        let Some(conn) = guard.as_mut() else {
            return Err(io::Error::other("sidebar socket missing"));
        };
        if let Err(e) = conn.stream.set_read_timeout(Some(read)) {
            *guard = None;
            return Err(e);
        }
        let res = f(&mut conn.stream);
        if !self.target.is_current() {
            *guard = None;
            return Err(io::ErrorKind::Interrupted.into());
        }
        match &res {
            Ok(_) => self.target.record_outcome(true, None),
            Err(e) => {
                *guard = None;
                self.target.record_outcome(false, Some(e));
            }
        }
        res
    }

    /// 워크트리별 미커밋 파일 수(`Request::GitWatch` 재사용). 실패한 워크트리는 빠진다(UI는 수를 숨긴다).
    fn dirty(&self, trees: &BTreeMap<PathBuf, Vec<PathBuf>>) -> BTreeMap<PathBuf, usize> {
        let Ok(mut cache) = self.watches.lock() else {
            return BTreeMap::new();
        };
        if trees.values().flatten().next().is_none() {
            if !cache.entries.is_empty() {
                let mut err = None;
                self.with_conn(
                    FEAT_CONTROL | FEAT_GIT_WATCH,
                    Duration::from_secs(2),
                    |stream| {
                        cache.clear(|req| {
                            let r = watch_exchange(stream, req);
                            if let Err(e) = &r {
                                err = Some(io::Error::new(e.kind(), e.to_string()));
                            }
                            r
                        });
                        err.map_or(Ok(()), Err)
                    },
                )
                .ok();
            }
            return BTreeMap::new();
        }
        let mut err = None;
        self.with_conn(
            FEAT_CONTROL | FEAT_GIT_WATCH,
            Duration::from_secs(5),
            |stream| {
                let out = cache.poll(trees, |req| {
                    let r = watch_exchange(stream, req);
                    if let Err(e) = &r {
                        err = Some(io::Error::new(e.kind(), e.to_string()));
                    }
                    r
                });
                if let Some(e) = err {
                    for state in cache.entries.values_mut() {
                        state.generation = 0;
                    }
                    Err(e)
                } else {
                    Ok(out)
                }
            },
        )
        .unwrap_or_else(|_| {
            for state in cache.entries.values_mut() {
                state.generation = 0;
            }
            cache
                .entries
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), v.dirty?)))
                .collect()
        })
    }

    fn fetch(&self) -> io::Result<Snapshot> {
        let is_local = self.target.allow_local_launch();
        // 외부 에이전트 상태는 로컬 호스트에서만 TTL 캐시에서 읽는다. 실패해도 krilld 스냅샷은 그대로 돌려준다.
        let external = if is_local {
            self.external
                .lock()
                .map(|mut cache| cache.get())
                .unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        self.with_conn(
            FEAT_CONTROL | FEAT_HOOK | FEAT_SIDEBAR,
            Duration::from_secs(5),
            |stream| fetch_from(stream, &external, is_local),
        )
    }

    fn meta(&self, trees: &BTreeMap<PathBuf, Vec<PathBuf>>) -> Meta {
        let is_local = self.target.allow_local_launch();
        let dirty = self.dirty(trees);
        trees
            .values()
            .flatten()
            .map(|tree| {
                let meta = Tree {
                    branch: is_local.then(|| branch(tree)).flatten(),
                    dirty: dirty.get(tree).copied(),
                };
                (tree.clone(), meta)
            })
            .collect()
    }

    fn poll(&self) -> io::Result<(Snapshot, Meta)> {
        self.fetch().map(|snap| {
            let meta = self.meta(&snap.trees);
            (snap, meta)
        })
    }

    fn hide(&self) {
        let Ok(mut cache) = self.watches.lock() else {
            return;
        };
        let Ok(mut connection) = self.conn.lock() else {
            return;
        };
        // 숨김 정리는 이미 열린 연결만 사용한다. 새 터널·연결을 시작하지 않는다.
        if let Some(conn) = connection.as_mut() {
            conn.stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .ok();
            cache.clear(|req| watch_exchange(&mut conn.stream, req));
        }
        cache.entries.clear();
        *connection = None;
    }
}

impl Source for Socket {
    fn target(&self) -> Option<&HostTarget> {
        Some(&self.shared.target)
    }

    fn fetch(&self) -> io::Result<Snapshot> {
        self.shared.fetch()
    }

    fn meta(&self, trees: &BTreeMap<PathBuf, Vec<PathBuf>>) -> Meta {
        self.shared.meta(trees)
    }

    fn poll_snapshot(&self) -> super::PollSnapshot {
        let (send, recv) = tokio::sync::oneshot::channel();
        let shared = Arc::clone(&self.shared);
        self.enqueue(Box::new(move || {
            // 폴링 루프가 바뀌어 기다리는 쪽이 없으면 ssh·IPC를 새로 시작하지 않는다.
            if send.is_closed() {
                return;
            }
            let started = Instant::now();
            let polled = shared.poll();
            // 꺼진 원격 호스트 재시도(ssh) 같은 긴 폴링을 운영 로그에 남겨 UI 멈춤 시각과 대조할 수 있게 한다.
            let elapsed = started.elapsed();
            if elapsed >= SLOW_POLL {
                tracing::info!(
                    remote = !shared.target.allow_local_launch(),
                    ok = polled.is_ok(),
                    elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                    "sidebar host poll slow"
                );
            }
            send.send(polled).ok();
        }));
        Box::pin(async move {
            recv.await
                .unwrap_or_else(|_| Err(io::Error::other("sidebar host worker stopped")))
        })
    }

    fn set_visible(&self, visible: bool) {
        if visible {
            return;
        }
        // 진행 중인 폴링(원격 ssh 포함) 뒤에 차례로 정리한다. 호출자는 연결 뮤텍스를 기다리지 않는다.
        let shared = Arc::clone(&self.shared);
        if !self.enqueue(Box::new(move || shared.hide())) {
            self.shared.hide();
        }
    }

    fn new_worktree(&self, req: NewWorktree) -> io::Result<Reply> {
        // ponytail: 큰 레포의 생성·include 복사를 기다리는 고정 상한. 넘으면 오류로 보이고 결과는 다음 폴링에 나타난다.
        let mut stream = control(
            &self.shared.target,
            FEAT_CONTROL | FEAT_WORKTREE,
            Duration::from_secs(300),
        )?;
        stream.write_all(&Request::NewWorktree(req).encode()?)?;
        let (kind, bytes) =
            krill_proto::ipc::read_frame(&mut stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        Reply::decode(kind, &bytes).map_err(io::Error::other)
    }

    fn remove_worktree(&self, req: krill_proto::control::RemoveWorktree) -> io::Result<Reply> {
        let mut stream = control(
            &self.shared.target,
            FEAT_CONTROL | krill_proto::control::FEAT_WORKTREE_REMOVE,
            Duration::from_secs(60),
        )?;
        stream.write_all(&Request::RemoveWorktree(req).encode()?)?;
        let (kind, bytes) =
            krill_proto::ipc::read_frame(&mut stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        Reply::decode(kind, &bytes).map_err(io::Error::other)
    }

    fn prepare_worktree(&self, req: krill_proto::control::PrepareWorktree) -> io::Result<Reply> {
        let mut stream = control(
            &self.shared.target,
            FEAT_CONTROL | krill_proto::control::FEAT_WORKTREE_PREPARE,
            Duration::from_secs(2),
        )?;
        stream.write_all(&Request::PrepareWorktree(req).encode()?)?;
        let (kind, bytes) =
            krill_proto::ipc::read_frame(&mut stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        Reply::decode(kind, &bytes).map_err(io::Error::other)
    }

    fn comments(&self) -> BTreeMap<PathBuf, String> {
        if !self.shared.target.allow_local_launch() {
            return BTreeMap::new();
        }
        super::external::state_dir_from(None, Some(self.shared.target.socket()))
            .map(|d| super::external::load_comments(&d))
            .unwrap_or_default()
    }
}

/// 작은 git 메타 파일 하나(HEAD, `.git` 링크)를 읽는다.
fn small(path: &Path) -> Option<String> {
    let mut text = String::new();
    fs::File::open(path)
        .ok()?
        .take(4096)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// 워크트리의 현재 브랜치. `.git` 디렉터리(주 워크트리)와 `gitdir:` 링크 파일(추가 워크트리)을 둘 다 따른다.
/// detached HEAD·읽기 실패는 `None`.
pub(super) fn branch(tree: &Path) -> Option<String> {
    let git = tree.join(".git");
    let dir = if git.is_dir() {
        git
    } else {
        let link = small(&git)?;
        let dir = PathBuf::from(link.strip_prefix("gitdir:")?.trim());
        tree.join(dir)
    };
    let head = small(&dir.join("HEAD"))?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .filter(|b| !b.is_empty())
        .map(str::to_owned)
}

/// Hello 협상을 마친 제어 연결. `required` 비트를 모두 협상하지 못하면 `Unsupported`.
fn connect(target: &HostTarget, required: u64, read: Duration) -> io::Result<Conn> {
    use krill_proto::ipc::{self, ClientRole};
    let hello = krill_proto::daemon::client_hello(ClientRole::App, "krill-app-sidebar");
    // Never launch a second local daemon from the sidebar; Shell owns local daemon reconnection.
    // For remote hosts (`!allow_local_launch`), `target.connect` ensures the shared tunnel socket
    // and handshakes without ever spawning a local daemon.
    if target.allow_local_launch() {
        let mut stream = UnixStream::connect(target.socket())?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let (kind, body) = ipc::read_frame(&mut stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        let server = ipc::ServerHello::decode(kind, &body)?;
        let negotiated = ipc::negotiate(&server, &hello).ok_or(io::ErrorKind::Unsupported)?;
        if negotiated.features & required != required {
            return Err(io::ErrorKind::Unsupported.into());
        }
        stream.write_all(&hello.encode()?)?;
        stream.set_read_timeout(Some(read))?;
        Ok(Conn {
            stream,
            features: negotiated.features,
        })
    } else {
        let c = target.connect(&hello).map_err(|e| match e {
            krill_proto::daemon::Error::Io(io_err) => io_err,
            other => io::Error::other(other.to_string()),
        })?;
        if c.negotiated.features & required != required {
            return Err(io::ErrorKind::Unsupported.into());
        }
        c.stream.set_read_timeout(Some(read))?;
        c.stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        Ok(Conn {
            stream: c.stream,
            features: c.negotiated.features,
        })
    }
}

fn control(target: &HostTarget, required: u64, read: Duration) -> io::Result<UnixStream> {
    connect(target, required, read).map(|c| c.stream)
}

/// 외부 에이전트(`claude agents --json`) 상태의 TTL 캐시(MK-083).
/// `fetch`는 백그라운드 스레드에서 500ms마다 도는데 CLI 실측 p50이 279ms라
/// (docs/spikes/MK-083.md) 매번 실행할 수 없어 결과를 묶어 재사용한다.
#[derive(Default)]
pub(super) struct ExternalCache {
    fetched_at: Option<Instant>,
    states: BTreeMap<PathBuf, super::external::ExternalHit>,
}

/// `claude agents --json` 재조회 최소 간격.
const EXTERNAL_TTL: Duration = Duration::from_secs(30);
/// `claude agents --json` 실행 상한. 넘기면 빈 목록으로 fail-open한다.
const EXTERNAL_TIMEOUT: Duration = Duration::from_secs(10);
/// `claude agents --json` stdout 상한(`krill_proto::agent::external::MAX_INPUT` 준수).
const EXTERNAL_MAX_BYTES: u64 = 8 * 1024 * 1024;

impl ExternalCache {
    fn get(&mut self) -> BTreeMap<PathBuf, super::external::ExternalHit> {
        // AS-07: launchd 기본 PATH에서도 찾도록 실행 파일을 미리 결정한다.
        let claude_bin = std::env::var_os("KRILL_CLAUDE_BIN");
        let bin = super::external::resolve_claude_bin(
            claude_bin.as_deref().and_then(|s| s.to_str()),
            &std::env::var_os("PATH").unwrap_or_default(),
            &crate::accounts::default_home(),
        );
        let agents_json = std::env::var_os("KRILL_EXTERNAL_AGENTS_JSON");
        self.get_with(agents_json.as_deref().and_then(|s| s.to_str()), &bin)
    }

    /// 외부 상태 조회의 본체. 주입 JSON·CLI 경로를 인자로 받아 환경변수 없이 검증한다.
    /// 주입 JSON이 최우선이고 TTL을 무시한다.
    fn get_with(
        &mut self,
        json_override: Option<&str>,
        cli_bin: &Path,
    ) -> BTreeMap<PathBuf, super::external::ExternalHit> {
        if let Some(raw) = json_override {
            self.states = states_from_json(raw);
            self.fetched_at = Some(Instant::now());
            return self.states.clone();
        }
        let fresh = self
            .fetched_at
            .is_some_and(|at| at.elapsed() < EXTERNAL_TTL);
        if !fresh {
            self.states = read_cli_states_with(cli_bin);
            self.fetched_at = Some(Instant::now());
        }
        self.states.clone()
    }
}

/// `claude agents --json` stdout(JSON 배열)에서 cwd → 표시 상태·pid를 만든다.
/// 배열이 아니면 빈 목록, 깨진 원소·빈 cwd는 건너뛴다.
fn states_from_json(raw: &str) -> BTreeMap<PathBuf, super::external::ExternalHit> {
    krill_proto::agent::external::parse(raw)
        .into_iter()
        .filter(|ext| !ext.cwd.is_empty())
        .map(|ext| {
            let hit = super::external::ExternalHit {
                state: ext.agent_state(),
                pid: ext.pid,
            };
            (PathBuf::from(ext.cwd), hit)
        })
        .collect()
}

/// `claude agents --json`을 bounded 실행한다. CLI 경로는 인자로 받는다.
/// `KRILL_CLAUDE_BIN` 환경변수로 실행 파일을 바꿀 수 있다(테스트 주입·수동 지정용).
/// 바이너리가 없거나·시간을 넘기거나·실패하면 빈 목록으로 fail-open해 사이드바 폴링을 막지 않는다.
fn read_cli_states_with(cli_bin: &Path) -> BTreeMap<PathBuf, super::external::ExternalHit> {
    use std::process::Stdio;
    let Ok(mut child) = crate::platform::background_command(cli_bin)
        .args(["agents", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return BTreeMap::new();
    };
    let Some(stdout) = child.stdout.take() else {
        child.kill().ok();
        child.wait().ok();
        return BTreeMap::new();
    };
    let deadline = Instant::now() + EXTERNAL_TIMEOUT;
    let Ok(reader) = crate::process_io::read(stdout, EXTERNAL_MAX_BYTES + 1, deadline) else {
        child.kill().ok();
        child.wait().ok();
        return BTreeMap::new();
    };
    let exited_ok = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Err(_) => break false,
            Ok(None) => {
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    // 자손이 파이프를 상속해도 직접 자식의 종료 후 무기한 join하지 않는다.
    child.kill().ok();
    child.wait().ok();
    let Ok(captured) = reader.finish(deadline + Duration::from_millis(50)) else {
        return BTreeMap::new();
    };
    if captured.timed_out {
        return BTreeMap::new();
    }
    let bytes = captured.bytes;
    if !exited_ok || u64::try_from(bytes.len()).is_ok_and(|n| n > EXTERNAL_MAX_BYTES) {
        return BTreeMap::new();
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return BTreeMap::new();
    };
    states_from_json(text)
}

/// 예전 Windows 데몬은 `canonicalize`한 워크트리를 `\\?\C:\x`로 보냈다. 셸 cwd(`C:\x`)와 맞추고 이름도
/// 읽히게 접두사를 뗀다. 다른 장치 이름공간(`\\?\Volume{..}`)은 뜻이 바뀌므로 그대로 둔다.
fn strip_verbatim(bytes: Vec<u8>) -> Vec<u8> {
    if let Some(share) = bytes.strip_prefix(b"\\\\?\\UNC\\") {
        return [b"\\\\", share].concat();
    }
    match bytes.strip_prefix(b"\\\\?\\") {
        Some(rest @ [drive, b':', ..]) if drive.is_ascii_alphabetic() => rest.to_vec(),
        _ => bytes,
    }
}

fn fetch_from(
    stream: &mut UnixStream,
    external: &BTreeMap<PathBuf, super::external::ExternalHit>,
    is_local: bool,
) -> io::Result<Snapshot> {
    use krill_proto::ipc;
    let mut call = |request: Request| -> io::Result<Reply> {
        stream.write_all(&request.encode()?)?;
        let (kind, bytes) = ipc::read_frame(stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        Reply::decode(kind, &bytes).map_err(io::Error::other)
    };
    let Reply::Sessions(sessions) = call(Request::List)? else {
        return Err(io::ErrorKind::InvalidData.into());
    };
    let Reply::States(states) = call(Request::States)? else {
        return Err(io::ErrorKind::InvalidData.into());
    };
    let Reply::Sidebar(data) = call(Request::Sidebar)? else {
        return Err(io::ErrorKind::InvalidData.into());
    };
    // OWE-707: 실행 당시 계정 표시용. 구 데몬(`BadRequest`·`Failed`)이면 비워 배지를 숨긴다.
    // 같은 연결에서 이어서 읽어 세션 목록과 어긋나지 않게 한다. PTY에는 손대지 않는다.
    let accounts: BTreeMap<_, _> = match call(Request::SessionAccounts) {
        Ok(Reply::SessionAccounts(list)) => list.into_iter().map(|a| (a.session, a)).collect(),
        Ok(_) | Err(_) => BTreeMap::new(),
    };
    let path = |bytes: Vec<u8>| PathBuf::from(std::ffi::OsString::from_vec(strip_verbatim(bytes)));
    let mut trees: BTreeMap<_, _> = data
        .repositories
        .into_iter()
        .map(|r| {
            (
                path(r.repo),
                r.worktrees.into_iter().map(path).collect::<Vec<_>>(),
            )
        })
        .collect();
    if is_local {
        super::external::include_claude_worktrees(&mut trees);
    }
    let dirs: BTreeMap<_, _> = data
        .sessions
        .into_iter()
        .map(|s| (s.session, path(s.cwd)))
        .collect();
    let external_states = external;
    let mut states: BTreeMap<_, _> = states.into_iter().map(|s| (s.session, s.status)).collect();
    // AS-08: 외부 pid가 있는 후보가 있을 때만 프로세스 표를 한 번 읽는다.
    let want_parents = sessions.iter().any(|s| {
        s.status.is_none()
            && states
                .get(&s.session)
                .is_none_or(|st| st.state == AgentState::Unknown)
            && dirs.get(&s.session).is_some_and(|cwd| {
                external_states
                    .get(cwd)
                    .is_some_and(|hit| hit.pid.is_some())
            })
    });
    let parents = want_parents.then(super::external::parent_map).flatten();
    let mut sessions: Vec<_> = sessions
        .into_iter()
        .map(|s| {
            let mut status = states.remove(&s.session).unwrap_or(AgentStatus::UNKNOWN);
            if s.status.is_some() {
                status.state = AgentState::Exited;
                status.foreground = None;
            } else if status.state == AgentState::Unknown
                && let Some(cwd) = dirs.get(&s.session)
                && let Some(&hit) = external_states.get(cwd)
                // AS-08: cwd만 같다고 무관한 셸에 덮어씌우지 않는다. 외부 에이전트 pid가
                // 세션 루트 자신이거나 후손일 때만 외부 상태를 쓴다.
                && super::external::linked_to_session(hit.pid, s.pid, parents.as_ref())
            {
                status.state = hit.state;
            }
            let worktree = dirs.get(&s.session).and_then(|cwd| {
                trees
                    .values()
                    .flatten()
                    .filter(|p| super::path_within(cwd, p))
                    // Windows 호스트 경로는 이 OS에서 한 덩어리라 구성 요소 수 대신 길이로 가장 깊은 것을 고른다.
                    .max_by_key(|p| p.as_os_str().len())
                    .cloned()
            });
            Session {
                id: s.session,
                status,
                worktree,
                cwd: dirs
                    .get(&s.session)
                    .filter(|p| !p.as_os_str().is_empty())
                    .cloned(),
                account: accounts.get(&s.session).cloned(),
            }
        })
        .collect();
    sessions.sort_by_key(|s| s.id);
    Ok(Snapshot { sessions, trees })
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"[
        {"kind":"background","id":"a1","sessionId":"s-a1","name":"build","cwd":"/repo/a","pid":11,"startedAt":1000,"state":"working","status":"busy","waitingFor":""},
        {"kind":"background","id":"b2","sessionId":"s-b2","name":"review","cwd":"/repo/b","pid":12,"startedAt":1001,"state":"","status":"idle","waitingFor":""},
        {"kind":"background","id":"c3","sessionId":"s-c3","name":"blocked","cwd":"/repo/c","pid":13,"startedAt":1002,"state":"blocked","status":"busy","waitingFor":"permission prompt"},
        {"kind":"background","id":"d4","sessionId":"s-d4","name":"weird","cwd":"/repo/d","pid":14,"startedAt":1003,"state":"mystery","status":"odd","waitingFor":""},
        {"kind":"background","id":"e5","sessionId":"s-e5","name":"nocwd","cwd":"","pid":15,"startedAt":1004,"state":"working","status":"busy","waitingFor":""},
        {"kind":"interactive","id":"f6","sessionId":"s-f6","name":"fg","cwd":"/repo/f","pid":16,"startedAt":1005,"state":"working","status":"busy","waitingFor":""},
        {"kind":"background","name":"noid","cwd":"/repo/g","state":"working","status":"busy"}
    ]"#;

    #[test]
    fn windows_verbatim_prefix_is_dropped_only_for_drives_and_shares() {
        assert_eq!(strip_verbatim(br"\\?\C:\Users\x".to_vec()), br"C:\Users\x");
        assert_eq!(
            strip_verbatim(br"\\?\UNC\srv\share\x".to_vec()),
            br"\\srv\share\x"
        );
        assert_eq!(
            strip_verbatim(br"\\?\Volume{1}\x".to_vec()),
            br"\\?\Volume{1}\x"
        );
        assert_eq!(strip_verbatim(b"/repo".to_vec()), b"/repo");
    }

    #[test]
    fn states_from_json_maps_cli_stdout_and_skips_unusable_rows() {
        let states = states_from_json(FIXTURE);
        let state = |cwd: &str| states.get(Path::new(cwd)).map(|hit| hit.state);
        assert_eq!(state("/repo/a"), Some(AgentState::Working));
        // 백그라운드 idle은 턴 종료 후이므로 Done이다.
        assert_eq!(state("/repo/b"), Some(AgentState::Done));
        assert_eq!(state("/repo/c"), Some(AgentState::Blocked));
        assert_eq!(state("/repo/d"), Some(AgentState::Unknown));
        // AS-08: pid도 함께 들고 온다.
        assert_eq!(
            states.get(Path::new("/repo/a")).and_then(|hit| hit.pid),
            Some(11)
        );
        assert_eq!(states.len(), 4, "{states:?}");
        assert!(states_from_json("not json").is_empty());
        assert!(states_from_json("{}").is_empty());
    }

    /// 원격 폴링은 호스트 전용 스레드에서 돈다. 응답이 늦어도 호출자(공유 실행기 스레드)는 바로 돌아오고,
    /// 숨김 정리도 진행 중인 폴링이 쥔 연결 뮤텍스를 기다리지 않는다.
    #[test]
    fn slow_host_does_not_hold_caller_thread() -> io::Result<()> {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("mk-sidebar-host-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        let path = dir.join("d.sock");
        fs::remove_file(&path).ok();
        let listener = UnixListener::bind(&path)?;
        let (release, released) = mpsc::channel::<()>();
        // 연결은 받되 Hello를 보내지 않고 붙잡아 둔다(느린 ssh·응답 없는 원격 호스트 흉내).
        let server = std::thread::spawn(move || {
            let held = listener.accept();
            released.recv().ok();
            drop(held);
        });
        let socket = Socket::threaded(krill_proto::daemon::Paths {
            socket: path,
            holder: PathBuf::new(),
            krilld: PathBuf::new(),
        });
        let started = Instant::now();
        let polled = socket.poll_snapshot();
        socket.set_visible(false);
        // 고치기 전에는 두 호출 모두 호출 스레드에서 읽기 상한(5초)까지 막혔다.
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        release.send(()).ok();
        let result = tokio::runtime::Builder::new_current_thread()
            .build()?
            .block_on(polled);
        assert!(result.is_err());
        socket.settle();
        assert!(socket.shared.conn.lock().is_ok_and(|conn| conn.is_none()));
        server.join().ok();
        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    /// 외부 상태 읽기 전체 경로(주입 JSON·가짜 CLI 실행·TTL 캐시·fail-open)를 검증한다.
    /// 환경변수를 만지지 않아 병렬 실행에 안전하다.
    #[test]
    fn external_cli_wiring_prefers_injected_json_and_caches_cli_results() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let missing = Path::new("/nonexistent/mk-test-claude");
        // 1) 주입 JSON이 최우선이다(CLI를 실행하지 않는다).
        let mut cache = ExternalCache::default();
        assert_eq!(cache.get_with(Some(FIXTURE), missing).len(), 4);

        // 2) TTL 안에는 CLI를 다시 실행하지 않고 캐시를 돌려준다.
        assert_eq!(cache.get_with(None, missing).len(), 4);

        // 3) 바이너리가 없으면 빈 목록으로 fail-open한다.
        assert!(read_cli_states_with(missing).is_empty());

        // 4) 가짜 `claude` 스크립트로 실행·bounded 읽기·파싱 전체 경로를 검증한다.
        let dir = std::env::temp_dir().join(format!("mk-external-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        let script = dir.join("claude");
        let mut body = "#!/bin/sh\nprintf '%s' '".to_owned();
        body.push_str(FIXTURE);
        body.push_str("'\n");
        fs::write(&script, body)?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
        let states = read_cli_states_with(&script);
        assert_eq!(states.len(), 4, "{states:?}");
        assert_eq!(
            states
                .get(Path::new("/repo/a"))
                .map(|hit| (hit.state, hit.pid)),
            Some((AgentState::Working, Some(11)))
        );
        fs::remove_dir_all(&dir).ok();
        Ok(())
    }
}
