//! Windows 원격: 연결마다 원격 `krill __bridge`의 stdin/stdout을 원격 krilld 소켓에 잇는다.
//!
//! Windows용 OpenSSH sshd(9.5p2)는 Unix 소켓 포워딩(`-L`·`-W` 경로)을 열지 못하고 `open failed`로 끝난다
//! (2026-10-24 Windows 11 실측). 원격 셸도 cmd.exe라 [`super::remote_query`]의 `/bin/sh` 질의가 실패한다.
//! 그래서 POSIX 질의가 원격에서 실패하면 `krill __bridge --probe`로 원격 krill이 중계할 수 있는지 묻고,
//! 되면 [`RemoteEndpoint::Bridge`]로 붙는다.
//!
//! - 원격 쪽 [`bridge_main`]은 OS와 무관하다(테스트는 macOS에서 같은 명령을 돌린다).
//! - 로컬 쪽(unix)은 keepalive ssh 하나가 `ControlMaster`를 열고, 로컬 소켓 연결마다 그 위에 중계 ssh를 하나 띄운다.
//!   원격 krilld는 깨우지 않는다 — ssh 세션 안에서 띄운 데몬은 세션이 끝날 때 함께 정리될 수 있다.

use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::io::BufRead as _;
use std::io::{self, Read as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(unix)]
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
#[cfg(unix)]
use std::thread::JoinHandle;
use std::time::Duration;

use krill_proto::daemon;
#[cfg(unix)]
use krill_proto::os::net::UnixListener;
use krill_proto::os::net::UnixStream;

use super::SshOptions;

/// 원격에서 실행하는 명령. cmd.exe·PowerShell·POSIX 셸에서 모두 같은 뜻이다(`PATH`의 `krill`).
const COMMAND: &str = "krill __bridge";
const PROBE_FLAG: &str = "--probe";
const OK_LINE: &str = "krill-bridge=ok";
const ERROR_KEY: &str = "krill-bridge-error=";
/// cmd.exe가 명령을 찾지 못했을 때의 종료 코드.
const CMD_NOT_FOUND: i32 = 9009;
/// ssh 자체가 실패(접속·인증)했을 때의 종료 코드. 원격 명령의 실패와 구분한다.
const SSH_FAILED: i32 = 255;
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(unix)]
const READY_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(50);
#[cfg(unix)]
const READ_POLL: Duration = Duration::from_millis(250);
#[cfg(unix)]
const MAX_CONNECTIONS: usize = 128;
/// ssh가 `ControlPath` 뒤에 붙이는 임시 접미사(`.XXXXXXXXXXXXXXXX`) 길이.
#[cfg(unix)]
const CONTROL_SUFFIX: usize = 17;

/// 원격 krilld에 닿는 방법.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteEndpoint {
    /// 원격 krilld 소켓 경로(macOS·Linux, ssh `-L` 포워딩).
    Socket(PathBuf),
    /// 연결마다 원격 `krill __bridge`가 stdio를 원격 krilld 소켓에 잇는다(Windows 원격).
    Bridge,
}

/// `krill __bridge --probe` 답.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// 원격 `std::env::consts::OS`(`windows` 등).
    pub os: String,
    pub arch: String,
    pub version: String,
}

/// `krill __bridge [--probe]`: 이 기기의 krilld 소켓을 stdin/stdout에 잇는다.
///
/// `--probe`는 OS·아키·버전과 소켓 연결 결과를 한 줄씩 출력한다. 연결되면 stdin이 닫힐 때까지 기다린다
/// (keepalive: 로컬 쪽이 어떻게 끝나든 ssh가 함께 끝난다).
pub fn bridge_main(args: &[OsString]) -> ExitCode {
    let probe = match args.get(1..).unwrap_or_default() {
        [] => false,
        [flag] if flag == PROBE_FLAG => true,
        _ => {
            eprintln!("usage: krill __bridge [--probe]");
            return ExitCode::from(2);
        }
    };
    let stream = daemon::Paths::dev().and_then(|paths| UnixStream::connect(paths.socket));
    if probe {
        let status = match &stream {
            Ok(_) => OK_LINE.to_owned(),
            Err(e) => format!("{ERROR_KEY}{e}"),
        };
        let mut out = io::stdout();
        writeln!(
            out,
            "krill-os={}\nkrill-arch={}\nkrill-version={}\n{status}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            krill_proto::BUILD
        )
        .and_then(|()| out.flush())
        .ok();
        if stream.is_err() {
            return ExitCode::FAILURE;
        }
        io::copy(&mut io::stdin().lock(), &mut io::sink()).ok();
        return ExitCode::SUCCESS;
    }
    match stream.and_then(relay_stdio) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("krill __bridge: {e}");
            ExitCode::FAILURE
        }
    }
}

/// 어느 방향이든 먼저 끝나면 연결이 끝난 것이다. 남은 스레드는 프로세스 종료와 함께 정리된다.
fn relay_stdio(stream: UnixStream) -> io::Result<()> {
    let mut up = stream.try_clone()?;
    let (done, finished) = mpsc::channel();
    let input_done = done.clone();
    thread::Builder::new()
        .name("bridge-in".into())
        .spawn(move || {
            io::copy(&mut io::stdin().lock(), &mut up).ok();
            input_done.send(()).ok();
        })?;
    thread::Builder::new()
        .name("bridge-out".into())
        .spawn(move || {
            let mut from = stream;
            let mut out = io::stdout().lock();
            let mut buffer = vec![0; 64 * 1024];
            while let Ok(n) = from.read(&mut buffer) {
                if n == 0
                    || out
                        .write_all(&buffer[..n])
                        .and_then(|()| out.flush())
                        .is_err()
                {
                    break;
                }
            }
            done.send(()).ok();
        })?;
    finished.recv().ok();
    Ok(())
}

/// ssh 종료 상태가 "원격까지 닿았고 원격 명령이 실패했다"인지(접속·인증 실패가 아닌지).
pub(crate) fn reached_remote(status: std::process::ExitStatus) -> bool {
    !status.success() && status.code().is_some_and(|code| code != SSH_FAILED)
}

/// 원격이 `krill __bridge`로 krilld에 닿는지 묻는다.
///
/// `Ok(None)`: 중계 대상이 아니다(POSIX 원격 등, 호출자가 원래 오류를 쓴다).
///
/// # Errors
/// 원격이 Windows로 보이지만 krill이 없거나 중계를 모르거나(구버전) 원격 krilld가 꺼져 있을 때 원인을 담는다.
pub(crate) fn probe(opts: &SshOptions, ssh_bin: &OsStr) -> io::Result<Option<Probe>> {
    let mut args = opts.common_args(5);
    args.extend([
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        opts.destination.clone(),
        format!("{COMMAND} {PROBE_FLAG}"),
    ]);
    let out = super::run_ssh_output(ssh_bin, &args, PROBE_TIMEOUT)?;
    classify(
        &String::from_utf8_lossy(&out.stdout),
        &String::from_utf8_lossy(&out.stderr),
        out.status.code(),
    )
}

fn classify(stdout: &str, stderr: &str, code: Option<i32>) -> io::Result<Option<Probe>> {
    let field = |key: &str| {
        stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix(key))
            .map(|value| value.trim().chars().take(64).collect::<String>())
    };
    if stdout.lines().any(|line| line.trim() == OK_LINE) {
        return Ok(Some(Probe {
            os: field("krill-os=").unwrap_or_default(),
            arch: field("krill-arch=").unwrap_or_default(),
            version: field("krill-version=").unwrap_or_default(),
        }));
    }
    if let Some(error) = field(ERROR_KEY) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("Krill is not running on the remote host; open Krill there ({error})"),
        ));
    }
    if stdout.contains("usage: krill") || stderr.contains("usage: krill") {
        return Err(io::Error::other(
            "Krill on the remote host is too old for SSH remote connections; update Krill there",
        ));
    }
    if code == Some(CMD_NOT_FOUND) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "krill is not on PATH on the remote Windows host; install Krill there",
        ));
    }
    Ok(None)
}

/// 원격 중계 터널의 로컬 쪽: keepalive ssh(ControlMaster)와 연결별 중계 ssh를 잇는 accept 스레드.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct Relay {
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

#[cfg(unix)]
impl Relay {
    /// keepalive ssh를 띄워 원격 중계가 준비됐는지 확인한 뒤 `listener`의 연결마다 중계 ssh를 잇는다.
    /// 돌려준 `Child`(keepalive)의 stdin 파이프가 닫히면 원격 `krill __bridge --probe`와 ssh가 끝난다.
    pub(crate) fn start(
        opts: &SshOptions,
        listener: UnixListener,
        control: Option<PathBuf>,
        ssh_bin: &OsStr,
    ) -> io::Result<(Child, Self)> {
        listener.set_nonblocking(true)?;
        let mut child = Command::new(ssh_bin)
            .args(keepalive_args(opts, control.as_deref()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
            child.kill().ok();
            child.wait().ok();
            return Err(io::Error::other("ssh stdio pipes are missing"));
        };
        let (tx, rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("bridge-ready".into())
            .spawn(move || {
                let mut reader = io::BufReader::new(stdout);
                let mut seen = String::new();
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                    seen.push_str(&line);
                    let ended = line.trim() == OK_LINE || line.trim().starts_with(ERROR_KEY);
                    line.clear();
                    if ended {
                        break;
                    }
                }
                tx.send(seen).ok();
                io::copy(&mut reader, &mut io::sink()).ok();
            })?;
        let errors = thread::Builder::new()
            .name("bridge-stderr".into())
            .spawn(move || {
                let mut text = Vec::new();
                (&mut stderr).take(64 * 1024).read_to_end(&mut text).ok();
                io::copy(&mut stderr, &mut io::sink()).ok();
                String::from_utf8_lossy(&text).into_owned()
            })?;
        let host = &opts.destination;
        let ready = match rx.recv_timeout(READY_TIMEOUT) {
            Ok(seen) if seen.lines().any(|line| line.trim() == OK_LINE) => Ok(()),
            Ok(seen) => {
                let status = child.wait()?;
                let stderr = errors.join().unwrap_or_default();
                Err(classify(&seen, &stderr, status.code())
                    .err()
                    .unwrap_or_else(|| {
                        let detail = stderr.trim();
                        io::Error::other(if detail.is_empty() {
                            format!("ssh bridge to {host} failed ({status})")
                        } else {
                            format!("ssh bridge to {host} failed: {detail}")
                        })
                    }))
            }
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("ssh bridge to {host} timed out"),
            )),
        };
        if let Err(e) = ready {
            child.kill().ok();
            child.wait().ok();
            return Err(e);
        }
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let opts = opts.clone();
        let program = ssh_bin.to_owned();
        let worker = thread::Builder::new()
            .name("bridge-accept".into())
            .spawn(move || accept(&listener, &opts, control.as_deref(), &program, &stop));
        match worker {
            Ok(worker) => Ok((
                child,
                Self {
                    stopped,
                    worker: Some(worker),
                },
            )),
            Err(e) => {
                child.kill().ok();
                child.wait().ok();
                Err(e)
            }
        }
    }

    pub(crate) fn is_alive(&self) -> bool {
        !self.stopped.load(Ordering::Acquire)
            && self.worker.as_ref().is_some_and(|w| !w.is_finished())
    }
}

#[cfg(unix)]
impl Drop for Relay {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().ok();
        }
    }
}

/// 인증을 한 번만 하도록 keepalive ssh가 열 `ControlMaster` 소켓 경로(`<run>/c-<pid>-<seq>`).
/// ssh가 임시 접미사를 붙여 bind하므로 그만큼 여유가 없으면 다중화하지 않는다(연결마다 새로 인증).
#[cfg(unix)]
pub(crate) fn control_path(local: &std::path::Path) -> Option<PathBuf> {
    let seq = super::TUNNEL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = local
        .parent()?
        .join(format!("c-{}-{seq}", std::process::id()));
    (path.as_os_str().len() + CONTROL_SUFFIX < daemon::SUN_PATH_MAX).then_some(path)
}

/// `-o ControlPath=…` 값. 상태 디렉터리 경로의 공백은 따옴표로, ssh 토큰 문자 `%`는 `%%`로 지킨다.
#[cfg(unix)]
fn control_option(path: &std::path::Path) -> String {
    format!(
        "ControlPath=\"{}\"",
        path.display().to_string().replace('%', "%%")
    )
}

#[cfg(unix)]
fn keepalive_args(opts: &SshOptions, control: Option<&std::path::Path>) -> Vec<String> {
    let mut args = opts.common_args(10);
    if let Some(path) = control {
        args.extend([
            "-o".to_owned(),
            "ControlMaster=yes".to_owned(),
            "-o".to_owned(),
            "ControlPersist=no".to_owned(),
            "-o".to_owned(),
            control_option(path),
        ]);
    }
    args.extend([
        "-T".to_owned(),
        opts.destination.clone(),
        format!("{COMMAND} {PROBE_FLAG}"),
    ]);
    args
}

#[cfg(unix)]
fn relay_args(opts: &SshOptions, control: Option<&std::path::Path>) -> Vec<String> {
    let mut args = opts.common_args(10);
    if let Some(path) = control {
        args.extend([
            "-o".to_owned(),
            "ControlMaster=no".to_owned(),
            "-o".to_owned(),
            control_option(path),
        ]);
    }
    args.extend([
        "-T".to_owned(),
        opts.destination.clone(),
        COMMAND.to_owned(),
    ]);
    args
}

#[cfg(unix)]
fn accept(
    listener: &UnixListener,
    opts: &SshOptions,
    control: Option<&std::path::Path>,
    program: &OsStr,
    stopped: &Arc<AtomicBool>,
) {
    let mut workers = Vec::<JoinHandle<()>>::new();
    while !stopped.load(Ordering::Acquire) {
        workers.retain(|worker| !worker.is_finished());
        match listener.accept() {
            Ok((socket, _)) if workers.len() < MAX_CONNECTIONS => {
                let opts = opts.clone();
                let control = control.map(std::path::Path::to_path_buf);
                let program = program.to_owned();
                let stopped = Arc::clone(stopped);
                if let Ok(worker) =
                    thread::Builder::new()
                        .name("bridge-forward".into())
                        .spawn(move || {
                            forward(&socket, &opts, control.as_deref(), &program, &stopped).ok();
                        })
                {
                    workers.push(worker);
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    stopped.store(true, Ordering::Release);
    for worker in workers {
        worker.join().ok();
    }
}

/// 로컬 연결 하나를 원격 `krill __bridge` 하나에 잇는다.
#[cfg(unix)]
fn forward(
    socket: &UnixStream,
    opts: &SshOptions,
    control: Option<&std::path::Path>,
    program: &OsStr,
    stopped: &AtomicBool,
) -> io::Result<()> {
    // macOS는 accept한 소켓이 리스너의 non-blocking을 물려받는다.
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(READ_POLL))?;
    socket.set_write_timeout(Some(READ_POLL))?;
    let mut back = socket.try_clone()?;
    let cancel = socket.try_clone()?;
    let mut child = Command::new(program)
        .args(relay_args(opts, control))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
        child.kill().ok();
        child.wait().ok();
        return Err(io::Error::other("ssh stdio pipes are missing"));
    };
    let finished = AtomicBool::new(false);
    thread::scope(|scope| {
        let finished_ref = &finished;
        let from_local = scope.spawn(move || {
            let mut socket = socket;
            let mut buffer = vec![0; 64 * 1024];
            while !finished_ref.load(Ordering::Acquire) && !stopped.load(Ordering::Acquire) {
                match socket.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if stdin.write_all(&buffer[..n]).is_err() {
                            break;
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::TimedOut
                                | io::ErrorKind::WouldBlock
                                | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => break,
                }
            }
            // stdin을 닫으면 원격 `krill __bridge`가 EOF를 보고 끝난다.
            drop(stdin);
        });
        let from_remote = scope.spawn(|| {
            let mut buffer = vec![0; 64 * 1024];
            'pump: while !stopped.load(Ordering::Acquire) {
                let n = match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                let mut offset = 0;
                while offset < n {
                    if stopped.load(Ordering::Acquire) {
                        break 'pump;
                    }
                    match back.write(buffer.get(offset..n).unwrap_or_default()) {
                        Ok(0) => break 'pump,
                        Ok(written) => offset += written,
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::TimedOut
                                    | io::ErrorKind::WouldBlock
                                    | io::ErrorKind::Interrupted
                            ) => {}
                        Err(_) => break 'pump,
                    }
                }
            }
            back.shutdown(std::net::Shutdown::Write).ok();
            finished.store(true, Ordering::Release);
        });
        while !stopped.load(Ordering::Acquire) && !finished.load(Ordering::Acquire) {
            if !matches!(child.try_wait(), Ok(None)) {
                break;
            }
            thread::sleep(POLL);
        }
        child.kill().ok();
        child.wait().ok();
        if stopped.load(Ordering::Acquire) {
            cancel.shutdown(std::net::Shutdown::Both).ok();
        }
        while !from_remote.is_finished() {
            if stopped.load(Ordering::Acquire) {
                cancel.shutdown(std::net::Shutdown::Both).ok();
                break;
            }
            thread::sleep(POLL);
        }
        finished.store(true, Ordering::Release);
        from_local.join().ok();
        from_remote.join().ok();
    });
    Ok(())
}

#[cfg(test)]
mod tests;
