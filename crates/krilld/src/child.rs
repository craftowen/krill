//! 자식 파이프·프로세스 그룹 제어. Windows Job Object 대응은 이 모듈에서 교체한다.

use std::io;
#[cfg(unix)]
pub(crate) use std::os::fd::AsFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(windows)]
pub(crate) use std::os::windows::io::AsHandle as AsFd;
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

/// 자식을 새 프로세스 그룹의 리더로 띄운다([`kill_and_reap`]이 그룹째 죽인다). Windows는 Job Object(Phase 3)
/// 전까지 아무것도 하지 않는다.
pub(crate) fn own_group(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
}

#[cfg(unix)]
pub(crate) fn nonblocking(pipe: &impl AsFd) -> io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(pipe)?;
    rustix::fs::fcntl_setfl(pipe, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(())
}

/// 익명 파이프 비차단 읽기는 Windows에 없다(Phase 3에서 리더 스레드로 바꾼다).
#[cfg(windows)]
pub(crate) fn nonblocking(_pipe: &impl AsFd) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// 열린 파이프 중 하나가 읽을 거리·EOF를 가질 때까지 최대 `timeout` 기다린다(sleep 폴링 대신 poll(2)).
/// 열린 파이프가 없으면(둘 다 EOF, 자식 종료 대기) 1ms만 쉬어 바쁜 루프를 막는다. poll 오류·EINTR은 바로
/// 돌아가 호출자가 다시 읽고 판단한다.
#[cfg(unix)]
pub(crate) fn wait_readable(pipes: &[Option<&dyn AsFd>], timeout: Duration) {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let mut fds: Vec<PollFd<'_>> = pipes
        .iter()
        .flatten()
        .map(|pipe| PollFd::from_borrowed_fd(pipe.as_fd(), PollFlags::IN))
        .collect();
    if fds.is_empty() {
        thread::sleep(Duration::from_millis(1));
        return;
    }
    // 변환 실패를 무한 대기(None)로 넘기면 취소 확인이 멈춘다.
    let Ok(limit) = Timespec::try_from(timeout) else {
        thread::sleep(timeout);
        return;
    };
    poll(&mut fds, Some(&limit)).ok();
}

/// Windows 익명 파이프는 poll할 수 없다(Phase 3 리더 스레드 전까지 기존 sleep 폴링).
#[cfg(windows)]
pub(crate) fn wait_readable(_pipes: &[Option<&dyn AsFd>], timeout: Duration) {
    thread::sleep(timeout);
}

pub(crate) fn kill_and_reap(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = rustix::process::Pid::from_raw(child.id().cast_signed()) {
        rustix::process::kill_process_group(pid, rustix::process::Signal::KILL).ok();
    }
    child.kill().ok();
    child.wait().ok();
}

/// 그룹 SIGTERM으로 lockfile 정리 유예를 준 뒤, 남은 그룹을 SIGKILL하고 회수한다. Windows는 바로 죽인다.
pub(crate) fn terminate_gracefully(child: &mut Child, grace: Duration) {
    #[cfg(unix)]
    let grouped = rustix::process::Pid::from_raw(child.id().cast_signed())
        .map(|pid| rustix::process::kill_process_group(pid, rustix::process::Signal::TERM))
        .is_some();
    #[cfg(windows)]
    let grouped = false;
    if !grouped {
        child.kill().ok();
    }
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                // 부모가 TERM 정리를 마쳐도 hook 자식은 TERM을 무시할 수 있다.
                kill_and_reap(child);
                return;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    kill_and_reap(child);
}
