//! OWE-485: `--detach`로 띄운 holder는 띄운 쪽에게서 새어 들어온 fd(3 이상)를 닫는다 — holder도, holder가
//! 띄운 krilld도 쥐지 않는다.

#![cfg(unix)]

use std::io::Read;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use krill_proto::daemon::DETACH_ARG;
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::process::{Pid, Signal, kill_process_group};

const HOLDER: &str = env!("CARGO_BIN_EXE_krill-holder");

#[test]
fn detach_closes_inherited_fds() {
    let (mut r, w) = std::io::pipe().unwrap();
    // 다른 스레드가 CLOEXEC를 걸기 전에 spawn이 끼어든 경합을 그대로 만든다.
    fcntl_setfd(&w, FdFlags::empty()).unwrap();
    let mut holder = Command::new(HOLDER)
        .args([DETACH_ARG, "/bin/sleep", "60"])
        .spawn()
        .unwrap();
    drop(w);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(r.read(&mut [0; 1]).ok()));
    let eof = rx.recv_timeout(Duration::from_secs(5));
    // holder는 setsid로 자기 프로세스 그룹의 리더다. krilld 자리의 sleep도 같은 그룹이다.
    // 그룹 전체를 먼저 죽인다: krilld 자리만 죽이면 holder가 다시 띄운다(OWE-604).
    let pgid = Pid::from_child(&holder);
    let holder_pid = holder.id();
    // EOF는 fd를 닫은 뒤(setsid 전)에 올 수 있다. 그룹 리더가 될 때까지 기다려야 killpg가 ESRCH로 빗나가지 않는다.
    let lead = std::time::Instant::now() + Duration::from_secs(5);
    while rustix::process::getpgid(Some(pgid)).ok() != Some(pgid)
        && std::time::Instant::now() < lead
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    kill_process_group(pgid, Signal::KILL).unwrap();
    holder.wait().unwrap();
    assert_eq!(
        eof,
        Ok(Some(0)),
        "holder·krilld가 띄운 쪽의 파이프 쓰기 끝을 쥐고 있다"
    );
    // 테스트가 띄운 holder·sleep이 남지 않았는지 확인한다(OWE-604).
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while rustix::process::test_kill_process(
        Pid::from_raw(i32::try_from(holder_pid).unwrap()).unwrap(),
    )
    .is_ok()
        && std::time::Instant::now() < end
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &holder_pid.to_string()])
        .output()
        .unwrap();
    assert!(
        out.stdout.is_empty() || String::from_utf8(out.stdout).unwrap().trim().is_empty(),
        "residual holder survived killpg: {holder_pid}"
    );
}
