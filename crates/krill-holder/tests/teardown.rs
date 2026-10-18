//! MK-051(OWE-416): `Terminate` 종료 단계와 탈출한 후손 스윕, 세션 상한 256을 실제 `krill-holder`로 검사한다.
//!
//! 이 테스트 바이너리가 가짜 krilld를 겸한다(`MKT_ROLE=krilld`, holder가 이 바이너리를 다시 실행한다).
//! 시나리오 결과는 `MKT_DIR`의 파일로 오케스트레이터에 넘긴다.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::fs;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use krill_holder::link::Link;
use krill_proto::SessionId;
use krill_proto::holder::{
    FromHolder, MAX_SESSIONS, SESSION_LIMIT_ERRNO, ToHolder, VERSION, WinSize,
};
use rustix::process::Pid;

const HOLDER: &str = env!("CARGO_BIN_EXE_krill-holder");

/// 세션 셸: 그룹 안 `sleep`과, `setsid`로 빠져나가 SIGHUP·SIGTERM을 무시하는 perl(+ 그 자식 `sleep`)을 띄운다.
/// perl은 준비되면 `esc` 파일에 `<perl pid> <자식 pid>`를 쓴다. 무시한 시그널은 exec 뒤에도 무시된다.
const SCRIPT: &str = r#"
/usr/bin/perl -MPOSIX -e 'setsid() or die; $SIG{HUP} = $SIG{TERM} = "IGNORE";
  my $c = fork; if (!$c) { exec "/bin/sleep", "60" }
  open(F, ">esc.tmp"); print F "$$ $c"; close F; rename "esc.tmp", "esc"; sleep 60' &
/bin/sleep 60 &
wait
"#;

fn put(p: &Path, s: &str) {
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, s).unwrap();
    fs::rename(tmp, p).unwrap();
}

fn wait_for<T>(what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < end, "시간 초과: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn pid(raw: &str) -> Pid {
    Pid::from_raw(raw.trim().parse().unwrap()).unwrap()
}

fn alive(p: Pid) -> bool {
    rustix::process::test_kill_process(p).is_ok()
}

// ---------- 역할: 가짜 krilld ----------

/// `Exited`를 건너뛰고 다음 응답을 받는다.
fn ask(link: &mut Link, m: &ToHolder, fds: &[std::os::fd::BorrowedFd<'_>]) -> FromHolder {
    link.send(&m.encode().unwrap(), fds).unwrap();
    loop {
        let p = link.recv().unwrap().unwrap();
        match FromHolder::decode(p.frame()).unwrap() {
            FromHolder::Exited { .. } => {}
            r => return r,
        }
    }
}

fn spawn(req: u32, id: u64, argv: &[&str], cwd: &Path) -> ToHolder {
    ToHolder::Spawn {
        req,
        id: SessionId(id),
        argv: argv.iter().map(|s| s.as_bytes().to_vec()).collect(),
        env: vec![],
        cwd: cwd.to_str().unwrap().as_bytes().to_vec(),
        size: WinSize { cols: 80, rows: 24 },
    }
}

#[test]
fn role_krilld() {
    if std::env::var("MKT_ROLE").as_deref() != Ok("krilld") {
        return;
    }
    let dir = PathBuf::from(std::env::var("MKT_DIR").unwrap());
    let mut link = Link::new(std::io::stdin().as_fd().try_clone_to_owned().unwrap());
    loop {
        let p = link.recv().unwrap().unwrap();
        match FromHolder::decode(p.frame()).unwrap() {
            FromHolder::Hello { version, .. } => assert_eq!(version, VERSION),
            FromHolder::Ready => break,
            m => panic!("복원 중 예상 밖: {m:?}"),
        }
    }
    if dir.join("result").exists() {
        // holder가 재기동한 krilld: 시나리오를 다시 돌리지 않는다.
    } else if std::env::var("MKT_MODE").as_deref() == Ok("terminate") {
        terminate(&mut link, &dir);
    } else {
        limit(&mut link, &dir);
    }
    while link.recv().unwrap().is_some() {} // holder가 죽을 때까지 머문다
}

fn terminate(link: &mut Link, dir: &Path) {
    let r = ask(link, &spawn(1, 1, &["/bin/sh", "-c", SCRIPT], dir), &[]);
    let FromHolder::Spawned { pid, .. } = r else {
        panic!("{r:?}")
    };
    put(&dir.join("leader"), &pid.to_string());
    wait_for("탈출한 후손 준비", 30, || {
        fs::read(dir.join("esc")).ok()
    });
    let m = ToHolder::Terminate {
        req: 2,
        id: SessionId(1),
    };
    assert_eq!(ask(link, &m, &[]), FromHolder::Done { req: 2 });
    put(&dir.join("terminated"), "");
    let m = ToHolder::Terminate {
        req: 3,
        id: SessionId(9),
    };
    let r = ask(link, &m, &[]);
    assert_eq!(
        r,
        FromHolder::Failed { req: 3, errno: 3 },
        "없는 세션 ESRCH"
    );
    put(&dir.join("result"), "ok");
}

/// 세션 256개를 채우고 257번째 `Spawn`이 [`SESSION_LIMIT_ERRNO`]로 거절되는지 본다.
/// PTY 256개를 실제로 열면 이 맥의 다른 터미널이 PTY를 못 얻을 수 있어(`kern.tty.ptmx_max` 511)
/// 255개는 `Adopt`(PTY를 새로 열지 않는다)로, 1개는 실제 `Spawn`으로 채운다.
fn limit(link: &mut Link, dir: &Path) {
    let mut sleeper = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let null = OwnedFd::from(fs::File::open("/dev/null").unwrap());
    let r = ask(link, &spawn(1, 0, &["/bin/sleep", "60"], dir), &[]);
    assert!(matches!(r, FromHolder::Spawned { req: 1, .. }), "{r:?}");
    for id in 1..u64::try_from(MAX_SESSIONS).unwrap() {
        let req = u32::try_from(id + 100).unwrap();
        let m = ToHolder::Adopt {
            req,
            id: SessionId(id),
            pid: sleeper.id(),
        };
        assert_eq!(ask(link, &m, &[null.as_fd()]), FromHolder::Done { req });
    }
    let r = ask(link, &spawn(2, 1000, &["/usr/bin/true"], dir), &[]);
    assert_eq!(
        r,
        FromHolder::Failed {
            req: 2,
            errno: SESSION_LIMIT_ERRNO
        },
        "257번째 세션"
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        rustix::io::Errno::PROCLIM.raw_os_error(),
        SESSION_LIMIT_ERRNO
    );
    // 끝난 세션도 Release 전에는 PTY를 쥐고 있으므로 센다. Release하면 자리가 난다.
    let m = ToHolder::Kill {
        req: 3,
        id: SessionId(0),
        signal: 9,
    };
    assert_eq!(ask(link, &m, &[]), FromHolder::Done { req: 3 });
    let r = ask(link, &spawn(4, 1000, &["/usr/bin/true"], dir), &[]);
    let errno = SESSION_LIMIT_ERRNO;
    assert_eq!(r, FromHolder::Failed { req: 4, errno });
    let m = ToHolder::Release {
        req: 5,
        id: SessionId(0),
    };
    assert_eq!(ask(link, &m, &[]), FromHolder::Done { req: 5 });
    let r = ask(link, &spawn(6, 1000, &["/usr/bin/true"], dir), &[]);
    assert!(matches!(r, FromHolder::Spawned { req: 6, .. }), "{r:?}");
    sleeper.kill().ok();
    sleeper.wait().ok();
    put(&dir.join("result"), "ok");
}

// ---------- 오케스트레이터 ----------

struct Rig {
    dir: PathBuf,
    holder: Child,
}

impl Rig {
    fn start(mode: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mkt-{mode}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let holder = Command::new(HOLDER)
            .arg(std::env::current_exe().unwrap())
            .args(["role_krilld", "--exact", "--nocapture"])
            .env("MKT_ROLE", "krilld")
            .env("MKT_MODE", mode)
            .env("MKT_DIR", &dir)
            .spawn()
            .unwrap();
        Self { dir, holder }
    }

    fn get(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.dir.join(name)).ok()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.holder.kill().ok();
        self.holder.wait().ok();
        fs::remove_dir_all(&self.dir).ok();
    }
}

/// `Terminate`: 그룹은 SIGHUP으로 바로 끝나고, `setsid`로 빠져나가 HUP·TERM을 무시하는 후손과 그 자식은
/// 리더가 죽은 뒤에도 따라가 SIGKILL 단계(5초)에 정리된다. 세션 밖 프로세스는 건드리지 않는다.
#[test]
fn terminate_sweeps_escaped_descendants() {
    let mut outsider = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let rig = Rig::start("terminate");
    let esc = wait_for("탈출한 후손", 30, || rig.get("esc"));
    let (perl, child) = esc.split_once(' ').unwrap();
    let (perl, child) = (pid(perl), pid(child));
    let leader = pid(&rig.get("leader").unwrap());
    assert_ne!(
        rustix::process::getpgid(Some(perl)).unwrap(),
        leader,
        "perl이 세션 프로세스 그룹을 빠져나가야 시나리오가 성립한다"
    );
    wait_for("Terminate 응답", 30, || rig.get("terminated"));
    let t0 = Instant::now();
    wait_for("리더 종료", 5, || (!alive(leader)).then_some(()));
    std::thread::sleep(Duration::from_millis(1000));
    assert!(
        alive(perl) && alive(child),
        "HUP·TERM을 무시하는 후손은 KILL 단계 전까지 산다"
    );
    wait_for("탈출한 후손 정리", 10, || {
        (!alive(perl)).then_some(())
    });
    let took = t0.elapsed();
    wait_for("탈출한 후손의 자식 정리", 5, || {
        (!alive(child)).then_some(())
    });
    println!("탈출한 후손 정리까지 {:.2}s", took.as_secs_f64());
    assert!(
        took >= Duration::from_millis(4500),
        "{took:?}: SIGKILL은 5초 단계"
    );
    assert_eq!(wait_for("시나리오", 30, || rig.get("result")), "ok");
    let outsider_pid = Pid::from_raw(i32::try_from(outsider.id()).unwrap()).unwrap();
    assert!(alive(outsider_pid), "세션 밖 프로세스가 죽었다");
    outsider.kill().ok();
    outsider.wait().ok();
}

/// 세션 상한 256: 257번째 `Spawn`은 `Failed{SESSION_LIMIT_ERRNO}`. 시나리오는 `limit`.
#[test]
fn session_limit_256() {
    let rig = Rig::start("limit");
    assert_eq!(wait_for("시나리오", 60, || rig.get("result")), "ok");
}
