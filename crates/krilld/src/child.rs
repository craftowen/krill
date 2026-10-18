//! 자식 파이프·프로세스 그룹 제어. Windows Job Object 대응은 이 모듈에서 교체한다.

use std::io;
use std::os::fd::AsFd;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

pub(crate) fn nonblocking(pipe: &impl AsFd) -> io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(pipe)?;
    rustix::fs::fcntl_setfl(pipe, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(())
}

pub(crate) fn kill_and_reap(child: &mut Child) {
    if let Some(pid) = rustix::process::Pid::from_raw(child.id().cast_signed()) {
        rustix::process::kill_process_group(pid, rustix::process::Signal::KILL).ok();
    }
    child.kill().ok();
    child.wait().ok();
}

/// 그룹 SIGTERM으로 lockfile 정리 유예를 준 뒤, 남은 그룹을 SIGKILL하고 회수한다.
pub(crate) fn terminate_gracefully(child: &mut Child, grace: Duration) {
    if let Some(pid) = rustix::process::Pid::from_raw(child.id().cast_signed()) {
        rustix::process::kill_process_group(pid, rustix::process::Signal::TERM).ok();
    } else {
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
