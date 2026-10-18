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

use crate::child::{kill_and_reap, nonblocking};
use crate::session::lock;

pub mod changes;
pub mod edit;
pub mod watch;
pub mod worktree;

/// git 동시 실행 상한(T4 G8: Orca 일반 작업 2~4).
pub const MAX_CONCURRENT: usize = 4;

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
}

#[derive(Default)]
struct State {
    pending: VecDeque<Job>,
    busy: HashSet<PathBuf>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    program: PathBuf,
}

/// git 작업 큐. 버리면(drop) 실행 중인 작업은 끝까지 돌리고, 시작 전 작업은 버린다(받는 쪽은 `RecvError`).
pub struct GitQueue {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for GitQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitQueue")
            .field("workers", &self.workers.len())
            .finish_non_exhaustive()
    }
}

impl GitQueue {
    /// `PATH`의 `git`, 워커 [`MAX_CONCURRENT`]개.
    pub fn new() -> io::Result<Self> {
        Self::with_program(PathBuf::from("git"), MAX_CONCURRENT)
    }

    pub fn with_program(program: PathBuf, workers: usize) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            program,
        });
        let mut queue = Self {
            shared,
            workers: Vec::with_capacity(workers),
        };
        for i in 0..workers {
            // 실패하면 `queue`의 Drop이 이미 띄운 워커를 정리한다.
            let handle = thread::Builder::new().name(format!("git-{i}")).spawn({
                let shared = Arc::clone(&queue.shared);
                move || work(&shared)
            })?;
            queue.workers.push(handle);
        }
        Ok(queue)
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
        self.enqueue(
            repo.into(),
            Box::new(move |program, repo| {
                reply.send(run(program, repo, &args)).ok();
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
        self.enqueue(
            repo,
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

    fn enqueue(&self, repo: PathBuf, execute: Execute) {
        let key = fs::canonicalize(&repo).unwrap_or_else(|_| repo.clone());
        self.enqueue_key(repo, key, execute);
    }

    /// 링크된 워크트리는 실행 디렉터리와 직렬화할 주 저장소 키가 다르다.
    fn enqueue_key(&self, repo: PathBuf, key: PathBuf, execute: Execute) {
        lock(&self.shared.state)
            .pending
            .push_back(Job { repo, key, execute });
        self.shared.changed.notify_one();
    }
}

impl Drop for GitQueue {
    fn drop(&mut self) {
        lock(&self.shared.state).closed = true;
        self.shared.changed.notify_all();
        for worker in self.workers.drain(..) {
            // 워커 본문은 panic하지 않는다(Command 실행·채널 전송뿐).
            worker.join().ok();
        }
    }
}

fn work(shared: &Shared) {
    while let Some(job) = next(shared) {
        (job.execute)(&shared.program, &job.repo);
        lock(&shared.state).busy.remove(&job.key);
        // 이 레포를 기다리던 작업이 다른 워커 몫일 수 있다.
        shared.changed.notify_all();
    }
}

/// 다른 워커가 잡지 않은 레포의 가장 오래된 작업을 꺼내고 그 레포를 잡는다. 닫혔으면 `None`.
fn next(shared: &Shared) -> Option<Job> {
    let mut st = lock(&shared.state);
    loop {
        if st.closed {
            return None;
        }
        let State { pending, busy, .. } = &mut *st;
        if let Some(job) = pending
            .iter()
            .position(|j| !busy.contains(&j.key))
            .and_then(|i| pending.remove(i))
        {
            busy.insert(job.key.clone());
            return Some(job);
        }
        st = shared
            .changed
            .wait(st)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

fn command(program: &Path, repo: &Path, args: &[OsString]) -> Command {
    let mut cmd = Command::new(program);
    crate::child::own_group(&mut cmd)
        .args(FIXED_ARGS)
        .args(args)
        .current_dir(repo)
        .env("PATH", OsStr::from_bytes(crate::shell::login_path()))
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
    let mut child = command(program, repo, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let Some(mut stdout) = child.stdout.take() else {
        kill_and_reap(&mut child);
        return Err(io::Error::other("missing git stdout"));
    };
    let Some(mut stderr) = child.stderr.take() else {
        kill_and_reap(&mut child);
        return Err(io::Error::other("missing git stderr"));
    };
    if let Err(e) = nonblocking(&stdout).and_then(|()| nonblocking(&stderr)) {
        kill_and_reap(&mut child);
        return Err(e);
    }
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut buf = [0u8; 8192];
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Err(error) = guard() {
            kill_and_reap(&mut child);
            return Err(error);
        }
        let a = drain(&mut stdout, &mut out, &mut buf);
        let b = drain(&mut stderr, &mut err, &mut buf);
        if let Err(e) = a.and(b) {
            kill_and_reap(&mut child);
            return Err(e);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                drain(&mut stdout, &mut out, &mut buf).ok();
                drain(&mut stderr, &mut err, &mut buf).ok();
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
        thread::sleep(Duration::from_millis(5));
    }
}

fn drain(pipe: &mut impl io::Read, dst: &mut Vec<u8>, buf: &mut [u8]) -> io::Result<()> {
    loop {
        match pipe.read(buf) {
            Ok(0) => return Ok(()),
            Ok(n) => dst.extend(buf.iter().take(n).copied()),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
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
