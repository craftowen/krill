//! `krill computer` 하위 프로세스 수명: GUI 세션 전역 입력 잠금(lease)과 기한·취소·출력 상한이 있는 실행.
//!
//! 라우팅: 관찰([`Access::Observe`])은 잠금 없이 실행하고, 변경([`Access::Mutate`])은 한 CLI 호출의
//! 변경 구간 전체(활성화·restore·AX 쓰기·키/마우스·클립보드 쓰기와 복원) 동안 [`InputLease`]를 쥔다.
//! 모델 판단을 기다리는 동안에는 쥐지 않는다(잠금 수명은 CLI 호출 하나).
//!
//! 잠금은 같은 OS 사용자 공통 고정 경로([`default_lease_path`])의 `flock`이다. `KRILL_STATE_DIR`·`HOME`·`TMPDIR`과
//! 무관해 개발·배포 빌드와 네이티브 실행기가 같은 잠금을 쓴다. 잠금 파일은 지우거나 다시 만들지 않고 TTL로 빼앗지 않는다.
//! 자식에게는 같은 open file description(dup)을 stdin으로 넘긴다. 그래서
//! CLI가 죽어도 실제 실행기와 그 fd를 물려받은 후손이 살아 있는 동안 잠금이 유지된다.
//! 지원되는 협력 실행기 사이의 발행 직렬화이며 다른 도구·사용자를 막는 보안 경계는 아니다.
//!
//! 결과 계약: `Err(NotDispatched)`는 자식이 시작되지 않았다는 뜻이라 재시도·대체 경로가 허용된다.
//! `Ok(Finished)`는 자식이 시작됐다는 뜻이며 종료 방식과 관계없이 변경 동작을 자동 재실행하지 않는다.
//! 자식을 죽여도 대상 앱이 이미 받은 작업은 취소되지 않는다.

use std::fmt;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

const POLL_MIN: Duration = Duration::from_millis(1);
const POLL_MAX: Duration = Duration::from_millis(10);
/// 정리 단계 전체(그룹 SIGKILL·파이프 정리·회수)에 한 번만 쓰는 상한. 정리는 기한·취소 시점, 또는 정상
/// 종료 뒤 파이프를 쥔 후손이 남아 기한에 이른 시점에 시작하므로 [`run`]은 늦어도
/// `deadline + CLEANUP_GRACE`(스케줄링 지연 제외)에 돌아온다. 호출자의 전체 예산은 이만큼 더 잡아야 한다.
pub const CLEANUP_GRACE: Duration = Duration::from_secs(2);
const READ_CHUNK: usize = 16 * 1024;

/// 하위 명령이 데스크톱 입력 상태를 바꿀 수 있는지.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Observe,
    Mutate,
}

/// `krill computer <subcommand> <args>`의 잠금 필요 여부. 모르는 명령은 안전하게 [`Access::Mutate`].
pub fn access_for(subcommand: &str, args: &[String]) -> Access {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    match subcommand {
        "capabilities" | "list-apps" | "list-windows" => Access::Observe,
        // --id는 권한이 없을 때 시스템 설정을 열어 전면 앱을 바꾼다.
        "permissions" if !has("--id") => Access::Observe,
        "get-app-state" | "snapshot" if !has("--restore-window") => Access::Observe,
        _ => Access::Mutate,
    }
}

/// 같은 OS 사용자(= macOS GUI 로그인 세션) 공통 잠금 경로 `/Users/Shared/.krill-computer-<euid>/input.lock`.
/// 환경변수를 읽지 않는다. `/tmp`는 periodic 정리가 오래된 잠금 inode를 지울 수 있어 쓰지 않는다.
/// `/Users/Shared`는 sticky라 다른 사용자가 이 사용자의 디렉터리를 지우거나 바꾸지 못한다.
#[cfg(target_os = "macos")]
pub fn default_lease_path() -> PathBuf {
    let uid = rustix::process::geteuid().as_raw();
    PathBuf::from(format!("/Users/Shared/.krill-computer-{uid}")).join("input.lock")
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn default_lease_path() -> PathBuf {
    let uid = rustix::process::geteuid().as_raw();
    PathBuf::from(format!("/tmp/krill-computer-{uid}")).join("input.lock")
}

#[cfg(not(unix))]
pub fn default_lease_path() -> PathBuf {
    std::env::temp_dir()
        .join("krill-computer")
        .join("input.lock")
}

/// 협력 취소 신호. 복제본 어디서든 [`Cancel::cancel`]하면 대기 중인 획득·실행이 멈춘다.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "M13 cancellation contract; exercised by cancellation/timeout tests and reserved for next status/cancel integration wave"
        )
    )]
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn cancelled(cancel: Option<&Cancel>) -> bool {
    cancel.is_some_and(Cancel::is_cancelled)
}

#[derive(Debug)]
pub enum LeaseError {
    /// 기한까지 다른 실행기가 잠금을 쥐고 있었다. 아무것도 실행하지 않았다.
    Busy,
    /// 획득을 시작할 때 이미 기한이 지났다. 잠금이 비어 있어도 얻지 않았다.
    DeadlineExpired,
    Cancelled,
    /// 잠금 디렉터리·파일이 이 사용자 소유의 비공개 일반 파일이 아니다.
    UnsafePath(PathBuf),
    Io(io::Error),
}

impl fmt::Display for LeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("another computer-use action holds the input lease"),
            Self::DeadlineExpired => {
                f.write_str("deadline expired before acquiring the input lease")
            }
            Self::Cancelled => f.write_str("cancelled while waiting for the input lease"),
            Self::UnsafePath(path) => write!(
                f,
                "unsafe input lease path {}: expected a private directory and file owned by this user",
                path.display()
            ),
            Self::Io(err) => write!(f, "input lease: {err}"),
        }
    }
}

impl std::error::Error for LeaseError {}

/// 쥐고 있는 전역 입력 잠금. 드롭하면 이 프로세스의 사본이 풀리고, 잠금을 물려받은 자식이 모두 끝나면 해제된다.
#[derive(Debug)]
pub struct InputLease {
    file: File,
}

impl InputLease {
    /// `deadline`까지 잠금을 기다린다. 기한·취소면 아무것도 실행하지 않은 채 오류를 돌려준다.
    /// 매 시도 전에 취소·기한을 먼저 확인하므로 기한이 지난 요청은 잠금이 비어 있어도 얻지 못한다.
    // ponytail: 폴링이라 공정성이 없다. 계속 경합하면 늦게 온 쪽이 기한까지 못 얻을 수 있다(기한이 상한).
    pub fn acquire(
        path: &Path,
        deadline: Instant,
        cancel: Option<&Cancel>,
    ) -> Result<Self, LeaseError> {
        let mut file = None;
        let mut waited = false;
        let mut pause = POLL_MIN;
        loop {
            if cancelled(cancel) {
                return Err(LeaseError::Cancelled);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(if waited {
                    LeaseError::Busy
                } else {
                    LeaseError::DeadlineExpired
                });
            }
            let held = match file.take() {
                Some(held) => held,
                None => open_lock_file(path)?,
            };
            match held.try_lock() {
                Ok(()) if same_inode(&held, path).map_err(LeaseError::Io)? => {
                    return Ok(Self { file: held });
                }
                // 잠근 뒤 경로가 다른 inode를 가리킨다(외부 삭제·교체). 떨어진 inode의 잠금은 다른 실행기와
                // 공유되지 않으니 놓고(드롭) 다음 시도에서 경로의 현재 파일을 연다.
                Ok(()) => drop(held),
                Err(TryLockError::WouldBlock) => file = Some(held),
                Err(TryLockError::Error(err)) => return Err(LeaseError::Io(err)),
            }
            waited = true;
            thread::sleep(pause.min(deadline.saturating_duration_since(Instant::now())));
            pause = (pause * 2).min(POLL_MAX);
        }
    }

    /// 자식 stdio로 넘길 사본. dup이라 같은 open file description(같은 flock)을 공유한다.
    fn carrier(&self) -> io::Result<Stdio> {
        self.file.try_clone().map(Stdio::from)
    }
}

fn open_lock_file(path: &Path) -> Result<File, LeaseError> {
    let dir = path
        .parent()
        .ok_or_else(|| LeaseError::UnsafePath(path.to_owned()))?;
    ensure_private_dir(dir)?;
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let nofollow = i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits())
            .map_err(|e| LeaseError::Io(io::Error::other(e)))?;
        opts.mode(0o600).custom_flags(nofollow);
    }
    let file = opts.open(path).map_err(LeaseError::Io)?;
    ensure_owned_file(&file, path)?;
    Ok(file)
}

#[cfg(unix)]
fn ensure_private_dir(dir: &Path) -> Result<(), LeaseError> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(LeaseError::Io(err)),
    }
    let meta = std::fs::symlink_metadata(dir).map_err(LeaseError::Io)?;
    let euid = rustix::process::geteuid().as_raw();
    if !meta.file_type().is_dir() || meta.uid() != euid || meta.mode() & 0o077 != 0 {
        return Err(LeaseError::UnsafePath(dir.to_owned()));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_dir(dir: &Path) -> Result<(), LeaseError> {
    std::fs::create_dir_all(dir).map_err(LeaseError::Io)
}

#[cfg(unix)]
fn ensure_owned_file(file: &File, path: &Path) -> Result<(), LeaseError> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = file.metadata().map_err(LeaseError::Io)?;
    // 하드 링크(nlink>1)는 다른 경로와 inode를 공유하므로 거절한다.
    if !meta.is_file() || meta.uid() != rustix::process::geteuid().as_raw() || meta.nlink() != 1 {
        return Err(LeaseError::UnsafePath(path.to_owned()));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owned_file(_file: &File, _path: &Path) -> Result<(), LeaseError> {
    Ok(())
}

#[cfg(unix)]
fn same_inode(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;
    let held = file.metadata()?;
    match std::fs::symlink_metadata(path) {
        Ok(now) => Ok(held.dev() == now.dev() && held.ino() == now.ino()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(not(unix))]
fn same_inode(_file: &File, _path: &Path) -> io::Result<bool> {
    Ok(true)
}

/// 자식에게 잠금 사본을 넘길 stdio 자리.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseSlot {
    /// 자식 stdin이 잠금 파일이 된다(읽으면 즉시 EOF). [`RunOpts::stdin`]과 함께 쓸 수 없다.
    Stdin,
}

/// [`run`] 옵션. `deadline`은 큐·실행 예산이며, 정리는 그 뒤 [`CLEANUP_GRACE`] 한 번만 더 쓴다.
#[derive(Debug)]
pub struct RunOpts<'a> {
    /// 변경 실행이면 쥐고 있는 잠금과 넘길 자리. 자식이 같은 open file description의 사본을 물려받는다.
    pub lease: Option<(&'a InputLease, LeaseSlot)>,
    /// 자식 stdin으로 보낼 데이터. 잠금과 함께 쓸 수 없다(잠금이 stdin 자리를 쓴다).
    pub stdin: Option<&'a [u8]>,
    pub deadline: Instant,
    pub cancel: Option<&'a Cancel>,
    /// stdout·stderr 각각의 수집 상한(바이트). 넘친 출력은 계속 읽어 버린다(파이프 막힘 방지).
    pub max_output: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Captured {
    /// 수집한 출력. `truncated`면 상한을 넘었거나 파이프를 쥔 후손이 남아 끝까지 읽지 못했다.
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

#[derive(Debug)]
pub enum End {
    Exited(ExitStatus),
    /// 기한이 지나 프로세스 그룹을 죽였다. 전달 여부는 알 수 없다.
    TimedOut,
    /// 취소로 프로세스 그룹을 죽였다. 전달 여부는 알 수 없다.
    Cancelled,
    /// 준비·상태 확인에 실패했다(회수 전이면 그룹을 죽였다). 전달 여부는 알 수 없다.
    WaitFailed,
}

/// 시작된 자식의 결과. `End::Exited`가 아니면 변경 동작의 결과는 Unknown이다.
#[derive(Debug)]
pub struct Finished {
    pub end: End,
    pub stdout: Captured,
    pub stderr: Captured,
    /// 직계 자식을 회수했는지. false면 커널이 SIGKILL 뒤 [`CLEANUP_GRACE`] 안에 끝내지 못했다.
    pub reaped: bool,
}

/// 자식이 시작되지 않은 실패. 이 경우에만 같은 요청을 다시 보내거나 다른 경로로 대체해도 된다.
#[derive(Debug)]
pub enum NotDispatched {
    Lease(LeaseError),
    Cancelled,
    DeadlineExpired,
    /// 옵션 조합이 잘못됐다(예: stdin 데이터와 잠금).
    InvalidOptions(&'static str),
    Spawn(io::Error),
}

impl fmt::Display for NotDispatched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lease(err) => err.fmt(f),
            Self::Cancelled => f.write_str("cancelled before the action started"),
            Self::DeadlineExpired => f.write_str("deadline expired before the action started"),
            Self::InvalidOptions(why) => write!(f, "invalid executor options: {why}"),
            Self::Spawn(err) => write!(f, "start computer-use executor: {err}"),
        }
    }
}

impl std::error::Error for NotDispatched {}

impl From<LeaseError> for NotDispatched {
    fn from(err: LeaseError) -> Self {
        Self::Lease(err)
    }
}

#[derive(Debug, Default)]
struct Sink {
    captured: Captured,
    done: bool,
}

type SharedSink = Arc<Mutex<Sink>>;

fn lock(sink: &SharedSink) -> MutexGuard<'_, Sink> {
    sink.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `cmd`를 자기 프로세스 그룹에서 실행하고, 끝나거나 기한·취소가 올 때까지 출력을 상한 안에서 모은다.
/// 기한·취소면 그룹 전체를 SIGKILL하고 회수한다. stdio와 프로세스 그룹은 이 함수가 설정한다.
pub fn run(mut cmd: Command, opts: &RunOpts<'_>) -> Result<Finished, NotDispatched> {
    if cancelled(opts.cancel) {
        return Err(NotDispatched::Cancelled);
    }
    if Instant::now() >= opts.deadline {
        return Err(NotDispatched::DeadlineExpired);
    }
    if opts.stdin.is_some() && opts.lease.is_some() {
        return Err(NotDispatched::InvalidOptions(
            "stdin payload cannot share stdin with the lease",
        ));
    }
    let stdin = match opts.lease {
        Some((lease, LeaseSlot::Stdin)) => lease.carrier().map_err(NotDispatched::Spawn)?,
        None if opts.stdin.is_some() => Stdio::piped(),
        None => Stdio::null(),
    };
    cmd.stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(NotDispatched::Spawn)?;
    // 여기부터 자식이 시작됐다. 어떤 결과든 Ok(Finished)로 돌려 호출자가 재실행하지 못하게 한다.

    let (tx, rx) = mpsc::channel();
    let mut setup_failed = false;
    let mut sinks = Vec::new();
    for pipe in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ] {
        let sink = SharedSink::default();
        match pipe.map(|p| drain(p, opts.max_output, Arc::clone(&sink), tx.clone())) {
            Some(Err(_)) => setup_failed = true,
            Some(Ok(())) => {}
            None => lock(&sink).done = true,
        }
        sinks.push(sink);
    }
    drop(tx);
    if let (Some(bytes), Some(mut pipe)) = (opts.stdin, child.stdin.take()) {
        let bytes = bytes.to_vec();
        // 자식이 읽지 않으면 쓰기가 막힌다. 별도 스레드에 두고, 자식이 죽으면 EPIPE로 끝난다.
        let writer = thread::Builder::new()
            .name("krill-computer-stdin".to_owned())
            .spawn(move || pipe.write_all(&bytes).ok());
        setup_failed |= writer.is_err();
    }

    // 리더는 아래 reap_within 전까지 회수하지 않는다(wait_child는 waitid WNOWAIT로 보기만 한다). 좀비 리더가
    // pid·pgid를 붙잡고 있어 그룹 신호가 재사용된 남의 그룹에 가지 않는다. 회수 뒤에는 어떤 신호도 보내지 않는다.
    let wait = if setup_failed {
        Wait::Failed
    } else {
        wait_child(&mut child, &rx, opts)
    };
    let mut cleanup_until = Instant::now();
    // 정상 종료라도 파이프를 쥔 후손이 남으면 기한까지만 기다린다.
    let clean_exit = matches!(wait, Wait::Exited) && wait_drained(&sinks, &rx, opts.deadline);
    if !clean_exit {
        cleanup_until = Instant::now() + CLEANUP_GRACE;
        if !matches!(wait, Wait::Lost) {
            kill_tree(&mut child);
        }
        // 그룹을 벗어난(setsid) 후손이 파이프를 쥐면 상한에서 포기한다. 그 후손에게는 신호를 보내지 않는다.
        // ponytail: 포기한 읽기 스레드는 그 후손이 끝날 때까지 남는다. CLI 한 번의 수명이라 두고, 상주
        // 프로세스에서 쓰게 되면 비차단 파이프로 바꾼다.
        wait_drained(&sinks, &rx, cleanup_until);
    }
    let status = reap_within(&mut child, cleanup_until);
    let end = match (wait, status) {
        (Wait::Exited, Some(status)) => End::Exited(status),
        (Wait::Exited, None) | (Wait::Failed | Wait::Lost, _) => End::WaitFailed,
        (Wait::TimedOut, _) => End::TimedOut,
        (Wait::Cancelled, _) => End::Cancelled,
    };
    let take = |idx: usize| {
        sinks
            .get(idx)
            .map(|s| {
                let mut s = lock(s);
                let mut captured = std::mem::take(&mut s.captured);
                captured.truncated |= !s.done;
                captured
            })
            .unwrap_or_default()
    };
    Ok(Finished {
        end,
        stdout: take(0),
        stderr: take(1),
        reaped: status.is_some(),
    })
}

fn drain(
    mut pipe: Box<dyn Read + Send>,
    cap: usize,
    sink: SharedSink,
    tx: Sender<()>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("krill-computer-drain".to_owned())
        .spawn(move || {
            let mut chunk = [0_u8; READ_CHUNK];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut s = lock(&sink);
                        let room = cap.saturating_sub(s.captured.bytes.len());
                        let keep = n.min(room);
                        s.captured
                            .bytes
                            .extend_from_slice(chunk.get(..keep).unwrap_or_default());
                        if keep < n {
                            s.captured.truncated = true;
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            lock(&sink).done = true;
            tx.send(()).ok();
        })
        .map(drop)
}

/// 회수 전 관찰 결과.
enum Wait {
    Exited,
    TimedOut,
    Cancelled,
    /// 출력 수집 준비 실패. 리더는 회수 전이라 그룹 신호가 안전하다.
    Failed,
    /// 리더 상태를 읽지 못했다(ECHILD 등: 다른 곳에서 이미 회수했을 수 있다). pid 재사용 위험이 있어 신호하지 않는다.
    Lost,
}

fn wait_child(child: &mut Child, rx: &Receiver<()>, opts: &RunOpts<'_>) -> Wait {
    let mut pause = POLL_MAX;
    loop {
        match leader_exited(child) {
            Ok(true) => return Wait::Exited,
            Ok(false) => {}
            Err(_) => return Wait::Lost,
        }
        if cancelled(opts.cancel) {
            return Wait::Cancelled;
        }
        let now = Instant::now();
        if now >= opts.deadline {
            return Wait::TimedOut;
        }
        let slice = pause.min(opts.deadline - now);
        match rx.recv_timeout(slice) {
            // 출력 파이프가 닫혔다: 곧 종료할 가능성이 커서 짧게 다시 확인한다.
            Ok(()) => pause = POLL_MIN,
            Err(RecvTimeoutError::Timeout) => pause = (pause * 2).min(POLL_MAX),
            Err(RecvTimeoutError::Disconnected) => {
                thread::sleep(slice);
                pause = (pause * 2).min(POLL_MAX);
            }
        }
    }
}

/// 리더가 끝났는지 회수하지 않고 본다(`waitid(P_PID, WEXITED|WNOHANG|WNOWAIT)`). 리더는 좀비로 남는다.
#[cfg(unix)]
fn leader_exited(child: &Child) -> io::Result<bool> {
    use rustix::process::{WaitId, WaitIdOptions, waitid};
    let pid = child_pid(child)?;
    let flags = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
    loop {
        match waitid(WaitId::Pid(pid), flags) {
            Ok(status) => return Ok(status.is_some()),
            Err(rustix::io::Errno::INTR) => {}
            Err(err) => return Err(err.into()),
        }
    }
}

/// Windows는 `Child` 핸들이 열려 있는 동안 pid가 재사용되지 않고, 상태는 `Child`에 남는다.
#[cfg(not(unix))]
fn leader_exited(child: &mut Child) -> io::Result<bool> {
    child.try_wait().map(|status| status.is_some())
}

fn wait_drained(sinks: &[SharedSink], rx: &Receiver<()>, until: Instant) -> bool {
    loop {
        if sinks.iter().all(|s| lock(s).done) {
            return true;
        }
        let now = Instant::now();
        if now >= until {
            return false;
        }
        let slice = (until - now).min(POLL_MAX);
        if rx.recv_timeout(slice) == Err(RecvTimeoutError::Disconnected) {
            thread::sleep(slice);
        }
    }
}

/// 리더를 회수한다. 기한이 지났어도 한 번은 확인한다(이미 좀비면 즉시 성공).
fn reap_within(child: &mut Child, until: Instant) -> Option<ExitStatus> {
    let mut pause = POLL_MIN;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => return None,
        }
        let now = Instant::now();
        if now >= until {
            return None;
        }
        thread::sleep(pause.min(until - now));
        pause = (pause * 2).min(POLL_MAX);
    }
}

/// 아직 회수하지 않은 리더(살아 있거나 좀비)의 프로세스 그룹 전체를 죽인다. 리더 pid가 남아 있어
/// pgid가 재사용되지 않는다. 회수 뒤에는 부르지 않는다.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    if child_pid(child)
        .and_then(|pid| {
            rustix::process::kill_process_group(pid, rustix::process::Signal::KILL)
                .map_err(io::Error::from)
        })
        .is_ok()
    {
        return;
    }
    // 그룹 신호가 실패하면(좀비만 남은 그룹, Windows 포함) 직계 자식만 끝낸다. 회수 전이라 재사용된 pid를
    // 건드리지 않는다.
    child.kill().ok();
}

#[cfg(unix)]
fn child_pid(child: &Child) -> io::Result<rustix::process::Pid> {
    i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| io::Error::other("child pid out of range"))
}
