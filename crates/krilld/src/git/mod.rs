//! git 작업 큐(ARCHITECTURE §6, T4-14). 워크트리 생성·삭제 같은 변경 작업은 앱이 아니라 여기서 돈다.
//!
//! - 동시 실행은 워커 스레드 수([`MAX_CONCURRENT`])로 제한한다. 요청이 몇 개든 git 자식은 그 이하.
//! - 같은 레포 작업은 직렬이다(`packed-refs.lock`·`index.lock` 경합 방지). 워커는 대기열 앞에서부터
//!   다른 워커가 잡고 있지 않은 레포의 작업을 고르므로, 레포 하나 안에서는 제출 순서대로 실행된다.
//! - 옵션·환경은 고정한다: `-c core.quotePath=false`, `GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0`,
//!   stdin은 닫고, krilld를 띄운 쪽에서 물려받았을 수 있는 저장소 지정 변수(`GIT_DIR` 등)는 지운다.

use krill_proto::os::ffi::OsStrExt as _;
use std::collections::{HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::child::{AsFd, PipeReader, kill_and_reap};
use crate::session::lock;

pub mod changes;
pub mod edit;
pub mod watch;
pub mod worktree;
pub mod worktree_refs;

/// git 동시 실행 상한(T4 G8: Orca 일반 작업 2~4).
pub const MAX_CONCURRENT: usize = 4;
const MAX_PENDING: usize = 256;
const MAX_REPO_PENDING: usize = 32;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
const DRAIN_BYTES: usize = 64 * 1024;

/// git 자식 실행 상한. 멈춘 훅·credential helper가 큐 워커를 영구 블록하지 않게 한다.
#[cfg(not(test))]
const TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const TIMEOUT: Duration = Duration::from_secs(2);
/// K-15: 타임아웃 SIGKILL 전 SIGTERM 유예. git의 시그널 핸들러가 `index.lock`을 정리할 시간을 준다.
#[cfg(not(test))]
const TERMINATE_GRACE: Duration = Duration::from_secs(2);
#[cfg(test)]
const TERMINATE_GRACE: Duration = Duration::from_millis(200);

/// 모든 git 호출 앞에 붙는 옵션. 경로를 따옴표·8진수 이스케이프 없이 UTF-8 그대로 받는다(한글 경로).
const FIXED_ARGS: [&str; 2] = ["-c", "core.quotePath=false"];
/// 고정 환경. 조회가 index를 갱신하지 않게, 인증 프롬프트로 멈추지 않게.
const FIXED_ENV: [(&str, &str); 2] = [("GIT_OPTIONAL_LOCKS", "0"), ("GIT_TERMINAL_PROMPT", "0")];
/// 설정돼 있으면 `current_dir` 대신 다른 저장소를 가리키게 만드는 변수들.
const CLEARED_ENV: [&str; 5] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
];

type Execute = Box<dyn FnOnce(&Path, &Path) + Send>;

struct Job {
    repo: PathBuf,
    /// 직렬화 키. `repo`의 정규 경로.
    key: PathBuf,
    execute: Execute,
    retained: usize,
}

#[derive(Default)]
struct State {
    pending: VecDeque<Job>,
    pending_bytes: usize,
    busy: HashSet<PathBuf>,
    active_workers: usize,
    workers: Vec<JoinHandle<()>>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    program: PathBuf,
    max_workers: usize,
}

/// git 작업 큐. 버리면(drop) 실행 중인 작업은 끝까지 돌리고, 시작 전 작업은 버린다(받는 쪽은 `RecvError`).
pub struct GitQueue {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for GitQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitQueue")
            .field("workers", &self.shared.max_workers)
            .finish_non_exhaustive()
    }
}

impl GitQueue {
    /// `PATH`의 `git`, 워커 [`MAX_CONCURRENT`]개.
    pub fn new() -> io::Result<Self> {
        Self::with_program(PathBuf::from("git"), MAX_CONCURRENT)
    }

    #[expect(clippy::unnecessary_wraps, reason = "공개 생성자 시그니처 유지")]
    pub fn with_program(program: PathBuf, workers: usize) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            program,
            max_workers: workers,
        });
        Ok(Self { shared })
    }

    /// `repo`에서 `git <고정 옵션> <args>`를 실행하도록 넣는다. 결과(종료 코드·stdout·stderr)는 돌려준 채널로 온다.
    ///
    /// 같은 레포는 정규 경로로 판정한다. 한 저장소의 워크트리들은 refs를 공유하므로 변경 작업은
    /// 주 워크트리(레포 루트)를 `repo`로 넘긴다.
    // ponytail: 경로 키. 링크된 워크트리 경로로 넘기면 다른 레포로 본다 — 그런 호출자가 생기면 common dir로 키를 바꾼다.
    pub fn submit<I, S>(&self, repo: impl Into<PathBuf>, args: I) -> Receiver<io::Result<Output>>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let (reply, rx) = mpsc::channel();
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        let retained = argument_bytes(&args);
        if retained > MAX_ARGUMENT_BYTES {
            reply
                .send(Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "git arguments exceed byte budget",
                )))
                .ok();
            return rx;
        }
        let submitted = Instant::now();
        self.enqueue(
            repo.into(),
            retained,
            Box::new(move |program, repo| {
                // Queue residence and execution have separate budgets so one
                // timed-out predecessor cannot expire the next accepted job.
                let result = if submitted.elapsed() >= TIMEOUT.saturating_mul(2) {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "git queue deadline elapsed before execution",
                    ))
                } else {
                    run(program, repo, &args)
                };
                reply.send(result).ok();
            }),
        );
        rx
    }

    /// Speculative checkout shares normal queue ordering, but can be cancelled and disk limited.
    pub(super) fn speculative<I, S>(
        &self,
        repo: PathBuf,
        args: I,
        cancel: Arc<AtomicBool>,
        tree: PathBuf,
        limit: u64,
    ) -> Receiver<io::Result<Output>>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let (tx, rx) = mpsc::channel();
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        let retained = argument_bytes(&args);
        if retained > MAX_ARGUMENT_BYTES {
            tx.send(Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "git arguments exceed byte budget",
            )))
            .ok();
            return rx;
        }
        self.enqueue(
            repo,
            retained,
            Box::new(move |program, repo| {
                let mut checked = Instant::now().checked_sub(Duration::from_secs(1));
                let result = run_guarded(program, repo, &args, || {
                    if cancel.load(Ordering::Acquire) {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "preparation cancelled",
                        ));
                    }
                    if checked.is_none_or(|at| at.elapsed() >= Duration::from_millis(100)) {
                        worktree::prepare::disk_usage(&tree, limit)?;
                        checked = Some(Instant::now());
                    }
                    Ok(())
                });
                tx.send(result).ok();
            }),
        );
        rx
    }

    fn enqueue(&self, repo: PathBuf, retained: usize, execute: Execute) {
        let key = fs::canonicalize(&repo).unwrap_or_else(|_| repo.clone());
        self.enqueue_key(repo, key, retained, execute);
    }

    /// 링크된 워크트리는 실행 디렉터리와 직렬화할 주 저장소 키가 다르다.
    fn enqueue_key(&self, repo: PathBuf, key: PathBuf, retained: usize, execute: Execute) {
        let retained = retained
            .saturating_add(repo.as_os_str().as_bytes().len())
            .saturating_add(key.as_os_str().as_bytes().len());
        let mut state = lock(&self.shared.state);
        // Reject before starting, with no mutation retry. Retained argument payloads are bounded
        // by MAX_ARGUMENT_BYTES (edit payloads already have stricter protocol limits).
        if state.closed
            || state.pending.len() >= MAX_PENDING
            || retained > MAX_PENDING_BYTES.saturating_sub(state.pending_bytes)
            || state.pending.iter().filter(|job| job.key == key).count() >= MAX_REPO_PENDING
        {
            return; // dropping execute disconnects its result channel
        }
        state.pending_bytes += retained;
        state.pending.push_back(Job {
            repo,
            key,
            execute,
            retained,
        });
        state.workers.retain(|w| !w.is_finished());
        let runnable_repos = state
            .pending
            .iter()
            .filter(|j| !state.busy.contains(&j.key))
            .map(|j| &j.key)
            .collect::<HashSet<_>>()
            .len();
        if state.active_workers < self.shared.max_workers
            && state.active_workers < state.busy.len().saturating_add(runnable_repos)
        {
            let index = state.active_workers;
            let shared = Arc::clone(&self.shared);
            if let Ok(handle) = thread::Builder::new()
                .name(format!("git-{index}"))
                .stack_size(128 * 1024)
                .spawn(move || {
                    crate::alloc::mobile_thread().ok();
                    work(&shared);
                })
            {
                state.active_workers += 1;
                state.workers.push(handle);
            }
        }
        self.shared.changed.notify_one();
    }
}

impl Drop for GitQueue {
    fn drop(&mut self) {
        let workers = {
            let mut state = lock(&self.shared.state);
            state.closed = true;
            std::mem::take(&mut state.workers)
        };
        self.shared.changed.notify_all();
        for worker in workers {
            // 워커 본문은 panic하지 않는다(Command 실행·채널 전송뿐).
            worker.join().ok();
        }
    }
}

fn work(shared: &Shared) {
    while let Some(job) = next(shared) {
        (job.execute)(&shared.program, &job.repo);
        crate::alloc::purge();
        lock(&shared.state).busy.remove(&job.key);
        // 이 레포를 기다리던 작업이 다른 워커 몫일 수 있다.
        shared.changed.notify_all();
    }
    crate::alloc::purge();
    crate::session::purge_soon();
}

/// 다른 워커가 잡지 않은 레포의 가장 오래된 작업을 꺼내고 그 레포를 잡는다. 닫혔거나 일이 없으면 `None`.
fn next(shared: &Shared) -> Option<Job> {
    let mut st = lock(&shared.state);
    loop {
        if st.closed || st.pending.is_empty() {
            st.active_workers = st.active_workers.saturating_sub(1);
            return None;
        }
        let State { pending, busy, .. } = &mut *st;
        if let Some(job) = pending
            .iter()
            .position(|j| !busy.contains(&j.key))
            .and_then(|i| pending.remove(i))
        {
            st.pending_bytes = st.pending_bytes.saturating_sub(job.retained);
            st.busy.insert(job.key.clone());
            return Some(job);
        }
        st = shared
            .changed
            .wait(st)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

fn argument_bytes(args: &[OsString]) -> usize {
    args.iter().fold(0usize, |bytes, arg| {
        bytes
            .saturating_add(std::mem::size_of::<OsString>())
            .saturating_add(arg.as_os_str().as_bytes().len())
    })
}

/// 맨이름 프로그램을 `PATH` 디렉터리에서 절대 경로로 풀어 준다. std는 `PATH`를 바꾼 맨이름 실행을 `posix_spawn`으로
/// 못 하고 `fork`+`exec`로 돌리는데, 다중 스레드 데몬의 fork 자식이 exec 전에 죽어(2026-10-27 12:04 `krilld` SIGABRT,
/// "crashed on child side of fork pre-exec") 데몬째 사라진 적이 있다. 못 찾으면 원래 이름을 그대로 쓴다.
fn resolve_program(program: &Path, path: &OsStr) -> PathBuf {
    if program.components().count() != 1 {
        return program.to_path_buf();
    }
    std::env::split_paths(path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| program.to_path_buf())
}

fn command(program: &Path, repo: &Path, args: &[OsString]) -> Command {
    let path = OsStr::from_bytes(crate::shell::login_path());
    let mut cmd = Command::new(resolve_program(program, path));
    crate::child::own_group(&mut cmd)
        .args(FIXED_ARGS)
        .args(args)
        .current_dir(repo)
        .env("PATH", path)
        .envs(FIXED_ENV)
        .stdin(Stdio::null());
    for var in CLEARED_ENV {
        cmd.env_remove(var);
    }
    cmd
}

fn run(program: &Path, repo: &Path, args: &[OsString]) -> io::Result<Output> {
    run_guarded(program, repo, args, || Ok(()))
}

fn run_guarded(
    program: &Path,
    repo: &Path,
    args: &[OsString],
    mut guard: impl FnMut() -> io::Result<()>,
) -> io::Result<Output> {
    guard()?;
    let mut child = crate::child::spawn(
        command(program, repo, args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )?;
    let Some(stdout) = child.stdout.take() else {
        kill_and_reap(&mut child);
        return Err(io::Error::other("missing git stdout"));
    };
    let Some(stderr) = child.stderr.take() else {
        kill_and_reap(&mut child);
        return Err(io::Error::other("missing git stderr"));
    };
    let mut stdout = PipeReader::new(stdout)?;
    let mut stderr = PipeReader::new(stderr)?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut buf = [0u8; 8192];
    let deadline = Instant::now() + TIMEOUT;
    let (mut out_eof, mut err_eof) = (false, false);
    loop {
        if let Err(error) = guard() {
            terminate_gracefully(&mut child);
            return Err(error);
        }
        match (
            drain(&mut stdout, &mut out, &mut buf),
            drain(&mut stderr, &mut err, &mut buf),
        ) {
            (Ok(a), Ok(b)) => {
                out_eof |= a;
                err_eof |= b;
            }
            (Err(e), _) | (_, Err(e)) => {
                // Output budgets abort mutations too: allow Git to release its
                // index/worktree locks before the bounded KILL fallback.
                terminate_gracefully(&mut child);
                return Err(e);
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // Pipes can outlive the leader. Keep draining with the same byte/deadline
                // limits instead of a final unbounded read from an inherited writer.
                if !out_eof || !err_eof {
                    if Instant::now() >= deadline {
                        terminate_gracefully(&mut child);
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "git output timed out",
                        ));
                    }
                    crate::child::wait_readable(
                        &[
                            (!out_eof).then_some(&stdout as &dyn AsFd),
                            (!err_eof).then_some(&stderr as &dyn AsFd),
                        ],
                        Duration::from_millis(5),
                    );
                    continue;
                }
                kill_and_reap(&mut child);
                return Ok(Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            Ok(None) => {}
            Err(e) => {
                kill_and_reap(&mut child);
                return Err(e);
            }
        }
        if Instant::now() >= deadline {
            // K-15: 바로 SIGKILL하면 `index.lock`·반쯤 만든 워크트리가 남아
            // 이후 사용자 터미널 git까지 실패한다. 먼저 SIGTERM 유예를 준다.
            terminate_gracefully(&mut child);
            return Err(io::Error::new(io::ErrorKind::TimedOut, "git timed out"));
        }
        // 5ms 상한은 guard(취소·디스크 상한) 확인 주기다(이전 sleep 폴링과 같다).
        crate::child::wait_readable(
            &[
                (!out_eof).then_some(&stdout as &dyn AsFd),
                (!err_eof).then_some(&stderr as &dyn AsFd),
            ],
            Duration::from_millis(5),
        );
    }
}

/// 지금 읽을 수 있는 만큼 읽는다. EOF면 `true`.
fn drain(pipe: &mut impl io::Read, dst: &mut Vec<u8>, buf: &mut [u8]) -> io::Result<bool> {
    let mut read = 0;
    while read < DRAIN_BYTES {
        match pipe.read(buf) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                if n > MAX_OUTPUT_BYTES.saturating_sub(dst.len()) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "git output exceeds byte budget",
                    ));
                }
                dst.extend_from_slice(buf.get(..n).unwrap_or_default());
                read += n;
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(false);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

/// K-15: 프로세스 그룹에 먼저 SIGTERM을 보내 git이 lockfile(`index.lock`)을 정리하고
/// 스스로 죽을 기회를 준다. 유예 안에 안 죽으면 기존처럼 그룹 SIGKILL + 회수한다.
fn terminate_gracefully(child: &mut Child) {
    crate::child::terminate_gracefully(child, TERMINATE_GRACE);
}

/// 워크트리 변경과 조회가 공유하는 데몬 큐.
pub(crate) fn queue() -> Result<&'static Arc<GitQueue>, String> {
    static QUEUE: OnceLock<Option<Arc<GitQueue>>> = OnceLock::new();
    QUEUE
        .get_or_init(|| GitQueue::new().ok().map(Arc::new))
        .as_ref()
        .ok_or_else(|| "cannot start the git queue".to_owned())
}

#[cfg(test)]
#[cfg(unix)]
mod tests;

#[cfg(test)]
mod platform_tests;

#[cfg(all(test, unix))]
mod resolve_tests {
    use super::*;

    #[test]
    fn bare_names_resolve_against_the_given_path_and_paths_are_kept() {
        let found = resolve_program(Path::new("sh"), OsStr::new("/nonexistent:/bin"));
        assert_eq!(found, Path::new("/bin/sh"));
        assert_eq!(
            resolve_program(Path::new("nope-krill"), OsStr::new("/bin")),
            Path::new("nope-krill")
        );
        assert_eq!(
            resolve_program(Path::new("/usr/bin/git"), OsStr::new("/bin")),
            Path::new("/usr/bin/git")
        );
    }
}
