//! 앱은 dispatch 작업 스레드에서 holder를 띄우고, std `Command`는 그 스레드의 신호 마스크를 자식에게 물려준다.
//! 그렇게 떠도 holder와 그 자식은 종료 신호를 받아야 한다.

#![cfg(target_os = "macos")]
#![expect(clippy::unwrap_used, reason = "테스트 헬퍼")]

use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};

const HOLDER: &str = env!("CARGO_BIN_EXE_krill-holder");
const DEADLINE: Duration = Duration::from_secs(5);

/// 막힌 마스크는 exec를 지나도 남는다. perl이 신호를 막고 holder로 exec해 앱의 dispatch 스레드를 흉내 낸다.
fn spawn_with_signals_blocked(dir: &std::path::Path) -> Child {
    Command::new("/usr/bin/perl")
        .args([
            "-MPOSIX",
            "-e",
            "sigprocmask(SIG_BLOCK, POSIX::SigSet->new(1..31)) or die; exec @ARGV",
            HOLDER,
            "/bin/sleep",
            "60",
        ])
        .env("KRILL_STATE_DIR", dir)
        .env("HOME", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn child_of(holder: u32) -> Option<i32> {
    let out = Command::new("/usr/bin/pgrep")
        .args(["-P", &holder.to_string()])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().parse().ok()
}

fn wait_until(mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + DEADLINE;
    while !done() {
        if Instant::now() >= end {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
    true
}

#[test]
fn holder_started_with_blocked_signals_still_obeys_term() {
    let dir = std::env::temp_dir().join(format!("krill-stop-signals-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut holder = spawn_with_signals_blocked(&dir);
    let holder_pid = holder.id();

    let mut first = None;
    assert!(
        wait_until(|| {
            first = child_of(holder_pid);
            first.is_some()
        }),
        "holder가 자식을 띄우지 않았다"
    );
    let first = first.unwrap();
    kill_process(Pid::from_raw(first).unwrap(), Signal::TERM).unwrap();
    let child_obeyed = wait_until(|| child_of(holder_pid).is_some_and(|p| p != first));

    let holder_pid_raw = Pid::from_child(&holder);
    kill_process(holder_pid_raw, Signal::TERM).unwrap();
    let holder_obeyed = wait_until(|| holder.try_wait().unwrap().is_some());

    let last = child_of(holder_pid);
    if !holder_obeyed {
        holder.kill().ok();
        holder.wait().ok();
    }
    for p in [Some(first), last].into_iter().flatten() {
        kill_process(Pid::from_raw(p).unwrap(), Signal::KILL).ok();
    }
    std::fs::remove_dir_all(&dir).ok();

    assert!(child_obeyed, "holder의 자식이 TERM을 받지 못했다");
    assert!(holder_obeyed, "holder가 TERM을 받지 못했다");
}
