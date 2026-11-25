//! LL-01 & LL-03 regressions: signal monitoring before first spawn/reap and
//! teardown / adopted-exit cleanup.

use std::fs;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use krill_holder::link::Link;
use krill_proto::SessionId;
use krill_proto::holder::{FromHolder, ToHolder, VERSION};

const HOLDER: &str = env!("CARGO_BIN_EXE_krill-holder");

/// LL-01: if krilld exits immediately on startup (e.g., `KRILLD_DUPLICATE` = 3),
/// holder must not miss SIGCHLD and hang forever in `kevent`/`epoll_wait`.
#[test]
fn immediate_duplicate_krilld_exit_does_not_hang_holder() {
    let dir = std::env::temp_dir().join(format!("krill-ll01-dup-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    for attempt in 0..8 {
        let mut child = Command::new(HOLDER)
            .args(["/bin/sh", "-c", "exit 3"])
            .env("KRILL_LOG_DIR", &dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                child.kill().ok();
                child.wait().ok();
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let status = status.unwrap_or_else(|| {
            panic!("attempt {attempt}: holder hung after immediate KRILLD_DUPLICATE exit")
        });
        assert!(status.success(), "attempt {attempt}: {status}");
    }
    fs::remove_dir_all(&dir).ok();
}

/// LL-01: repeated immediate crash exits (status 1) must hit `GIVE_UP` and exit cleanly.
#[test]
fn immediate_crashing_krilld_hits_give_up_instead_of_hanging() {
    let dir = std::env::temp_dir().join(format!("krill-ll01-crash-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mut child = Command::new(HOLDER)
        .arg("/usr/bin/false")
        .env("KRILL_LOG_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            child.wait().ok();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    fs::remove_dir_all(&dir).ok();
    let status = status.expect("holder hung instead of hitting GIVE_UP");
    assert!(status.success(), "unexpected holder exit status: {status}");
}

#[test]
fn role_ll03_krilld() {
    if std::env::var("MKL_ROLE").as_deref() != Ok("ll03") {
        return;
    }
    let dir = PathBuf::from(std::env::var("MKL_DIR").unwrap());
    let mut link = Link::new(std::io::stdin().as_fd().try_clone_to_owned().unwrap());
    loop {
        let p = link.recv().unwrap().unwrap();
        match FromHolder::decode(p.frame()).unwrap() {
            FromHolder::Hello { version, .. } => assert_eq!(version, VERSION),
            FromHolder::Ready => break,
            other => panic!("unexpected during handshake: {other:?}"),
        }
    }
    // Adopt a short-lived process, wait for Exited, then UpgradeHolder.
    // After restore, holder must NOT watch the already-exited adopted session's PID again.
    let mut adopted_proc = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let null = OwnedFd::from(fs::File::open("/dev/null").unwrap());
    link.send(
        &ToHolder::Adopt {
            req: 1,
            id: SessionId(7),
            pid: adopted_proc.id(),
            size: None,
        }
        .encode()
        .unwrap(),
        &[null.as_fd()],
    )
    .unwrap();
    let p = link.recv().unwrap().unwrap();
    assert_eq!(
        FromHolder::decode(p.frame()).unwrap(),
        FromHolder::Done { req: 1 }
    );

    adopted_proc.kill().ok();
    adopted_proc.wait().ok();
    let mut saw_exited = false;
    while !saw_exited {
        let p = link.recv().unwrap().unwrap();
        if let FromHolder::Exited { id, .. } = FromHolder::decode(p.frame()).unwrap()
            && id == SessionId(7)
        {
            saw_exited = true;
        }
    }

    link.send(
        &ToHolder::UpgradeHolder {
            req: 2,
            path: HOLDER.as_bytes().to_vec(),
        }
        .encode()
        .unwrap(),
        &[],
    )
    .unwrap();
    let p = link.recv().unwrap().unwrap();
    assert!(matches!(
        FromHolder::decode(p.frame()).unwrap(),
        FromHolder::Hello { .. }
    ));
    let p = link.recv().unwrap().unwrap();
    assert_eq!(
        FromHolder::decode(p.frame()).unwrap(),
        FromHolder::Done { req: 2 }
    );

    // Confirm no spurious second Exited arrives for the already-exited adopted session.
    link.send(
        &ToHolder::Release {
            req: 3,
            id: SessionId(7),
        }
        .encode()
        .unwrap(),
        &[],
    )
    .unwrap();
    let p = link.recv().unwrap().unwrap();
    assert_eq!(
        FromHolder::decode(p.frame()).unwrap(),
        FromHolder::Done { req: 3 }
    );

    // Exercise Terminate followed immediately by Release before the leader exits.
    let mut alive_proc = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    link.send(
        &ToHolder::Adopt {
            req: 4,
            id: SessionId(8),
            pid: alive_proc.id(),
            size: None,
        }
        .encode()
        .unwrap(),
        &[null.as_fd()],
    )
    .unwrap();
    let p = link.recv().unwrap().unwrap();
    assert_eq!(
        FromHolder::decode(p.frame()).unwrap(),
        FromHolder::Done { req: 4 }
    );
    link.send(
        &ToHolder::Terminate {
            req: 5,
            id: SessionId(8),
        }
        .encode()
        .unwrap(),
        &[],
    )
    .unwrap();
    let p = link.recv().unwrap().unwrap();
    assert_eq!(
        FromHolder::decode(p.frame()).unwrap(),
        FromHolder::Done { req: 5 }
    );
    link.send(
        &ToHolder::Release {
            req: 6,
            id: SessionId(8),
        }
        .encode()
        .unwrap(),
        &[],
    )
    .unwrap();
    let p = link.recv().unwrap().unwrap();
    assert_eq!(
        FromHolder::decode(p.frame()).unwrap(),
        FromHolder::Done { req: 6 }
    );
    alive_proc.wait().ok();

    fs::write(dir.join("ok"), "done").unwrap();
    while link.recv().unwrap().is_some() {}
}

/// LL-03: `UpgradeHolder` skips re-watching already-exited adopted sessions and
/// Terminate+Release cleanly handles exit notifications.
#[test]
fn upgrade_skips_exited_adopted_watch_and_terminate_release_completes() {
    let dir = std::env::temp_dir().join(format!("krill-ll03-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let mut holder = Command::new(HOLDER)
        .arg(std::env::current_exe().unwrap())
        .args(["early_exit::role_ll03_krilld", "--exact", "--nocapture"])
        .env("MKL_ROLE", "ll03")
        .env("MKL_DIR", &dir)
        .env("KRILL_LOG_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !dir.join("ok").exists() {
        assert!(Instant::now() < deadline, "LL-03 scenario timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
    holder.kill().ok();
    holder.wait().ok();
    fs::remove_dir_all(&dir).ok();
}
