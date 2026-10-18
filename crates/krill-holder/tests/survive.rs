//! 실제 `krill-holder` 바이너리로 MK-004·MK-005 기준과 내부 프로토콜 계약을 검사한다.
//!
//! 이 테스트 바이너리가 역할을 겸한다: `MKH_ROLE=krilld`면 `role_krilld`가 가짜 krilld로,
//! `MKH_ROLE=gen`이면 `role_gen`이 결정적 생성기로 돈다(holder가 이 바이너리를 다시 실행한다).
//! 오케스트레이터 ↔ 가짜 krilld는 `MKH_DIR`의 파일로 소통한다.
//!
//! 전체 기준 재현(오래 걸림):
//! `MKH_KILLS=100 cargo nextest run -p krill-holder --locked --no-capture krilld_kill`
//! `MKH_UPGRADES=10 cargo nextest run -p krill-holder --locked --no-capture exec_handoff`
//! holder만 크래시한 뒤 재결합(OWE-478): `cargo nextest run -p krill-holder --locked holder_crash_adopt`

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use krill_holder::link::Link;
use krill_proto::SessionId;
use krill_proto::holder::{FromHolder, STATUS_UNKNOWN, ToHolder, VERSION, WinSize};
use rustix::process::{Pid, Signal};

const HOLDER: &str = env!("CARGO_BIN_EXE_krill-holder");
const LINE: usize = 100;

#[path = "survive/launchd.rs"]
mod launchd;
#[path = "survive/upgrade.rs"]
mod upgrade;

fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

fn mono_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    u64::try_from(t.tv_sec).unwrap() * 1_000_000_000 + u64::try_from(t.tv_nsec).unwrap()
}

fn put(p: &Path, s: &str) {
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, s).unwrap();
    fs::rename(tmp, p).unwrap();
}

fn get(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok()
}

fn wait_for<T>(what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < end, "시간 초과: {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn pid(raw: &str) -> Pid {
    Pid::from_raw(raw.trim().parse().unwrap()).unwrap()
}

// ---------- 역할: 결정적 생성기 ----------

#[test]
fn role_gen() {
    if std::env::var("MKH_ROLE").as_deref() != Ok("gen") {
        return;
    }
    let id = env_u64("MKH_ID", 0);
    let rate = env_u64("MKH_RATE", 1_000_000);
    let stop = PathBuf::from(std::env::var("MKH_STOP").unwrap());
    // 제어 터미널이 잡혔는지(setsid + TIOCSCTTY, MK-004 결정 3).
    let code = if fs::File::open("/dev/tty").is_err() {
        90
    } else {
        let tty = rustix::stdio::stdin();
        let mut t = rustix::termios::tcgetattr(tty).unwrap();
        t.make_raw();
        rustix::termios::tcsetattr(tty, rustix::termios::OptionalActions::Now, &t).unwrap();
        let mut out = std::io::stdout().lock();
        out.write_all(b"BEGIN\n").unwrap();
        let t0 = Instant::now();
        let mut seq = 0u64;
        let mut batch = Vec::with_capacity(10 * LINE);
        while !stop.exists() {
            batch.clear();
            for _ in 0..10 {
                let head = format!("S{id:02} {seq:010} ");
                batch.extend_from_slice(head.as_bytes());
                batch.resize(batch.len() + LINE - 1 - head.len(), b'x');
                batch.push(b'\n');
                seq += 1;
            }
            out.write_all(&batch).unwrap();
            let due = Duration::from_nanos(seq * 100 * 1_000_000_000 / rate);
            if let Some(ahead) = due.checked_sub(t0.elapsed()) {
                std::thread::sleep(ahead);
            }
        }
        writeln!(out, "END {seq}").unwrap();
        out.flush().unwrap();
        100 + i32::try_from(id).unwrap()
    };
    #[expect(
        clippy::exit,
        reason = "종료 코드로 판정하고 libtest 출력을 PTY에 남기지 않는다"
    )]
    std::process::exit(code);
}

/// 생성기 출력을 줄 단위로 검사한다. 결과: `ok <줄 수>` 또는 `err …`.
fn verify_stream(id: u64, mut f: fs::File) -> String {
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut started = false;
    let mut seq = 0u64;
    loop {
        let n = match f.read(&mut chunk) {
            Ok(0) | Err(_) => return format!("err eof after {seq} lines"),
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if !started {
            let Some(at) = buf.windows(6).position(|w| w == b"BEGIN\n") else {
                continue;
            };
            buf.drain(..at + 6);
            started = true;
        }
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            if let Some(rest) = line.strip_prefix(b"END ") {
                let n: u64 = String::from_utf8_lossy(rest)
                    .trim()
                    .parse()
                    .unwrap_or(u64::MAX);
                return if n == seq {
                    format!("ok {seq}")
                } else {
                    format!("err END {n} but got {seq}")
                };
            }
            let mut want = format!("S{id:02} {seq:010} ").into_bytes();
            want.resize(LINE - 1, b'x');
            want.push(b'\n');
            if line != want {
                return format!("err line {seq}: {}", String::from_utf8_lossy(&line));
            }
            seq += 1;
        }
    }
}

// ---------- 역할: 가짜 krilld ----------

fn send(link: &Link, m: &ToHolder, fds: &[std::os::fd::BorrowedFd<'_>]) {
    link.send(&m.encode().unwrap(), fds).unwrap();
}

/// 다음 메시지. `Exited`는 파일로 기록한다(오케스트레이터가 종료 코드를 본다).
fn next(link: &mut Link, dir: &Path) -> Option<(FromHolder, Vec<OwnedFd>)> {
    let p = link.recv().unwrap()?;
    let m = FromHolder::decode(p.frame()).unwrap();
    if let FromHolder::Exited { id, status } = m {
        put(&dir.join(format!("exit-{}", id.0)), &status.to_string());
    }
    Some((m, p.fds))
}

#[test]
fn role_krilld() {
    if std::env::var("MKH_ROLE").as_deref() != Ok("krilld") {
        return;
    }
    let dir = PathBuf::from(std::env::var("MKH_DIR").unwrap());
    let stdin = std::io::stdin().as_fd().try_clone_to_owned().unwrap();
    let mut link = Link::new(stdin);
    let mut masters = Vec::new();
    let mut listener = None;
    let mut hello_pid = 0;
    loop {
        match next(&mut link, &dir).unwrap() {
            (
                FromHolder::Hello {
                    version,
                    holder_pid,
                },
                _,
            ) => {
                assert_eq!(version, VERSION);
                hello_pid = holder_pid;
            }
            (FromHolder::Session { id, status, .. }, mut fds) => {
                if let Some(st) = status {
                    put(&dir.join(format!("exit-{}", id.0)), &st.to_string());
                }
                masters.push((id, fds.pop().unwrap()));
            }
            (FromHolder::Listener, mut fds) => listener = fds.pop(),
            (FromHolder::Ready, _) => break,
            (m, _) => panic!("복원 중 예상 밖: {m:?}"),
        }
    }
    let ready_ns = mono_ns();
    let me = std::process::id();
    if std::env::var("MKH_MODE").as_deref() == Ok("contract") {
        return contract(&mut link, &dir, masters.len(), listener);
    }
    if std::env::var("MKH_MODE").as_deref() == Ok("adopt") {
        return adopt(&mut link, &dir, masters.len());
    }
    if std::env::var("MKH_MODE").as_deref() == Ok("log-io") {
        return upgrade::blocked_log(&mut link, &dir);
    }
    if dir.join("restore-listener").exists() {
        let l = UnixListener::from(listener.expect("exec 뒤 listener 복원"));
        assert_eq!(
            l.local_addr().unwrap().as_pathname(),
            Some(dir.join("l.sock").as_path())
        );
        assert_eq!(masters.len(), 10, "exec 뒤 master fd 전부 복원");
        put(
            &dir.join("krilld"),
            &format!("{me} {ready_ns} {} {hello_pid}", masters.len()),
        );
        put(&dir.join("listener-restored"), "ok");
        while link.recv().unwrap().is_some() {}
        return;
    }
    if masters.is_empty() && !dir.join("pids").exists() {
        masters = spawn_gens(&mut link, &dir);
    }
    put(
        &dir.join("krilld"),
        &format!("{me} {ready_ns} {} {hello_pid}", masters.len()),
    );
    let verify = std::env::var("MKH_VERIFY").is_ok();
    for (id, fd) in masters {
        let dir = dir.clone();
        std::thread::spawn(move || {
            let f = fs::File::from(fd);
            if verify {
                put(
                    &dir.join(format!("verify-{}", id.0)),
                    &verify_stream(id.0, f),
                );
            } else {
                std::io::copy(&mut { f }, &mut std::io::sink()).ok();
            }
        });
    }
    let upgrades = env_u64("MKH_UPGRADES_RUN", 0);
    if upgrades > 0 {
        let l = UnixListener::bind(dir.join("l.sock")).unwrap();
        send(&link, &ToHolder::HoldListener { req: 900 }, &[l.as_fd()]);
        assert_eq!(reply(&mut link, &dir).0, FromHolder::Done { req: 900 });
        // holder만 사본을 보관한다. exec 뒤 krilld 재기동으로 실제 복원을 검사한다.
        let next = dir.join("holder-next");
        if !next.exists() {
            fs::copy(HOLDER, &next).unwrap();
        }
        // 새 실행 경로(새 vnode)의 AppleSystemPolicy 최초 평가 비용은 핸드오프 전에 끝낸다(OWE-497).
        let t0 = mono_ns();
        krill_proto::daemon::preflight(&next).unwrap();
        put(&dir.join("preflight"), &(mono_ns() - t0).to_string());
    }
    let mut log = String::new();
    for k in 0..upgrades {
        std::thread::sleep(Duration::from_millis(env_u64("MKH_EVERY_MS", 300)));
        let req = 1000 + u32::try_from(k).unwrap();
        let t0 = mono_ns();
        // 새 실행 경로(holder-next)와 원래 경로를 번갈아 실행한다.
        let path = if k % 2 == 0 {
            dir.join("holder-next").into_os_string()
        } else {
            HOLDER.into()
        };
        let path = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()).to_vec();
        send(&link, &ToHolder::UpgradeHolder { req, path }, &[]);
        let mut pid_after = 0;
        loop {
            match next(&mut link, &dir).unwrap().0 {
                FromHolder::Hello { holder_pid, .. } => pid_after = holder_pid,
                FromHolder::Done { req: r } if r == req => break,
                FromHolder::Failed { errno, .. } => panic!("UpgradeHolder 실패 errno {errno}"),
                _ => {}
            }
        }
        writeln!(log, "{} {pid_after}", mono_ns() - t0).unwrap();
        if std::env::var_os("MKH_STEP").is_some() {
            put(&dir.join(format!("handoff-{k}")), &pid_after.to_string());
            wait_for("launchd 확인", 30, || {
                dir.join(format!("ack-{k}")).exists().then_some(())
            });
        }
    }
    if upgrades > 0 {
        put(&dir.join("handoffs"), &log);
    }
    while next(&mut link, &dir).is_some() {}
}

fn spawn_gens(link: &mut Link, dir: &Path) -> Vec<(SessionId, OwnedFd)> {
    let n = env_u64("MKH_GENS", 10);
    let exe = std::env::current_exe().unwrap();
    for i in 0..n {
        let env = [
            ("MKH_ROLE", "gen".to_owned()),
            ("MKH_ID", i.to_string()),
            ("MKH_RATE", env_u64("MKH_RATE", 1_000_000).to_string()),
            ("MKH_STOP", dir.join("stop").display().to_string()),
        ];
        let m = ToHolder::Spawn {
            req: u32::try_from(i).unwrap(),
            id: SessionId(i),
            argv: [exe.to_str().unwrap(), "role_gen", "--exact", "--nocapture"]
                .iter()
                .map(|s| s.as_bytes().to_vec())
                .collect(),
            env: env
                .iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
                .collect(),
            cwd: dir.to_str().unwrap().as_bytes().to_vec(),
            size: WinSize { cols: 80, rows: 24 },
        };
        send(link, &m, &[]);
    }
    let mut out = Vec::new();
    let mut pids = String::new();
    while out.len() < usize::try_from(n).unwrap() {
        match next(link, dir).unwrap() {
            (FromHolder::Spawned { id, pid, .. }, mut fds) => {
                writeln!(pids, "{} {pid}", id.0).unwrap();
                out.push((id, fds.pop().unwrap()));
            }
            (FromHolder::Failed { req, errno }, _) => panic!("Spawn {req} 실패 errno {errno}"),
            _ => {}
        }
    }
    put(&dir.join("pids"), &pids);
    out
}

fn reply(link: &mut Link, dir: &Path) -> (FromHolder, Vec<OwnedFd>) {
    loop {
        match next(link, dir).unwrap() {
            (FromHolder::Exited { .. }, _) => {}
            r => return r,
        }
    }
}

/// 계약 시나리오. 1단계는 요청별 응답을 확인하고 끝나고(holder가 재기동), 2단계는 복원 목록을 확인한다.
fn contract(link: &mut Link, dir: &Path, sessions: usize, listener: Option<OwnedFd>) {
    let sock = dir.join("l.sock");
    if dir.join("phase1").exists() {
        let l = UnixListener::from(listener.expect("Listener 복원"));
        let addr = l.local_addr().unwrap();
        assert_eq!(addr.as_pathname(), Some(sock.as_path()));
        assert_eq!(sessions, 1, "끝난 세션 7은 Release 전이라 상태와 함께 복원");
        put(&dir.join("result"), "ok");
        while link.recv().unwrap().is_some() {} // holder가 죽을 때까지 머문다(재기동 반복 방지)
        return;
    }
    // 요청 하나를 보내고 Exited가 아닌 첫 응답을 받는다.
    let ask = |link: &mut Link, m: ToHolder, fds: &[std::os::fd::BorrowedFd<'_>]| {
        send(link, &m, fds);
        reply(link, dir)
    };
    // 모르는 kind → ENOSYS(동결 규칙).
    link.send(&[6, 0, 0, 0, 0x40, 0, 77, 0, 0, 0], &[]).unwrap();
    assert_eq!(
        reply(link, dir).0,
        FromHolder::Failed {
            req: 77,
            errno: rustix::io::Errno::NOSYS.raw_os_error()
        }
    );
    let spawn = |req, id, argv: &[&str]| ToHolder::Spawn {
        req,
        id: SessionId(id),
        argv: argv.iter().map(|s| s.as_bytes().to_vec()).collect(),
        env: vec![],
        cwd: vec![],
        size: WinSize { cols: 80, rows: 24 },
    };
    let (r, _) = ask(link, spawn(1, 5, &["/nonexistent/x"]), &[]);
    assert_eq!(r, FromHolder::Failed { req: 1, errno: 2 });
    let (r, fds) = ask(link, spawn(2, 5, &["/bin/sleep", "30"]), &[]);
    assert!(matches!(
        r,
        FromHolder::Spawned {
            req: 2,
            id: SessionId(5),
            ..
        }
    ));
    assert_eq!(fds.len(), 1);
    let (r, _) = ask(link, spawn(3, 5, &["/bin/sleep", "30"]), &[]);
    assert_eq!(
        r,
        FromHolder::Failed { req: 3, errno: 17 },
        "id 중복 EEXIST"
    );
    let (r, _) = ask(
        link,
        ToHolder::Kill {
            req: 4,
            id: SessionId(5),
            signal: 15,
        },
        &[],
    );
    assert_eq!(r, FromHolder::Done { req: 4 });
    let status = loop {
        if let FromHolder::Exited {
            id: SessionId(5),
            status,
        } = next(link, dir).unwrap().0
        {
            break status;
        }
    };
    assert!(
        libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == 15,
        "{status:#x}"
    );
    let (r, _) = ask(
        link,
        ToHolder::Kill {
            req: 5,
            id: SessionId(5),
            signal: 15,
        },
        &[],
    );
    assert_eq!(
        r,
        FromHolder::Failed { req: 5, errno: 3 },
        "끝난 세션 ESRCH"
    );
    let (r, _) = ask(
        link,
        ToHolder::Kill {
            req: 6,
            id: SessionId(9),
            signal: 15,
        },
        &[],
    );
    assert_eq!(
        r,
        FromHolder::Failed { req: 6, errno: 3 },
        "없는 세션 ESRCH"
    );
    let (r, _) = ask(
        link,
        ToHolder::Release {
            req: 7,
            id: SessionId(5),
        },
        &[],
    );
    assert_eq!(r, FromHolder::Done { req: 7 });
    let (r, _) = ask(
        link,
        ToHolder::Release {
            req: 8,
            id: SessionId(5),
        },
        &[],
    );
    assert_eq!(r, FromHolder::Failed { req: 8, errno: 3 });
    // 끝났지만 Release하지 않은 세션: 재기동 뒤 복원 목록에 상태와 함께 와야 한다.
    let (r, _) = ask(link, spawn(9, 7, &["/usr/bin/true"]), &[]);
    assert!(matches!(r, FromHolder::Spawned { req: 9, .. }), "{r:?}");
    while !matches!(
        next(link, dir).unwrap().0,
        FromHolder::Exited {
            id: SessionId(7),
            ..
        }
    ) {}
    fs::remove_file(&sock).ok();
    let l = UnixListener::bind(&sock).unwrap();
    let (r, _) = ask(link, ToHolder::HoldListener { req: 10 }, &[l.as_fd()]);
    assert_eq!(r, FromHolder::Done { req: 10 });
    let path = b"/nonexistent/holder".to_vec();
    let (r, _) = ask(link, ToHolder::UpgradeHolder { req: 12, path }, &[]);
    assert_eq!(
        r,
        FromHolder::Failed { req: 12, errno: 2 },
        "exec 실패 뒤에도 holder는 계속 돈다"
    );
    // 한 recvmsg에 완성된 후속 요청까지 실리면 exec로 요청을 잃지 않고 EBUSY로 거절한다.
    let parent = rustix::process::getppid().unwrap();
    rustix::process::kill_process(parent, Signal::STOP).unwrap();
    let mut batch = ToHolder::UpgradeHolder {
        req: 13,
        path: HOLDER.as_bytes().to_vec(),
    }
    .encode()
    .unwrap();
    batch.extend(
        ToHolder::Release {
            req: 14,
            id: SessionId(99),
        }
        .encode()
        .unwrap(),
    );
    link.send(&batch, &[]).unwrap();
    rustix::process::kill_process(parent, Signal::CONT).unwrap();
    assert_eq!(
        reply(link, dir).0,
        FromHolder::Failed { req: 13, errno: 16 }
    );
    assert_eq!(reply(link, dir).0, FromHolder::Failed { req: 14, errno: 3 });
    let exe = std::env::current_exe().unwrap();
    let path = exe.to_str().unwrap().as_bytes().to_vec();
    let (r, _) = ask(link, ToHolder::UpgradeKrilld { req: 11, path }, &[]);
    assert_eq!(r, FromHolder::Done { req: 11 });
    put(&dir.join("phase1"), "");
    // 여기서 끝나면 holder가 백오프 없이 같은 바이너리로 2단계를 띄운다.
}

/// holder만 크래시(OWE-478, MK-018 '재결합 절차'). 1단계 krilld는 세션을 띄우고, holder가 죽어 링크가
/// EOF가 되면 master fd를 인계 소켓으로 다음 krilld에 넘긴다(krilld 간 인계의 테스트 대역). 2단계 krilld는
/// 새 holder에 `Adopt`로 맡기고, holder exec 핸드오프를 거친 뒤 종료가 `STATUS_UNKNOWN`으로 오는지 본다.
/// 3단계(재기동된 krilld)는 끝난 Adopt 세션이 같은 상태로 복원되는지 본다.
fn adopt(link: &mut Link, dir: &Path, sessions: usize) {
    let sock = dir.join("handoff.sock");
    let exit = dir.join("exit-3");
    if dir.join("phase2").exists() {
        assert_eq!(sessions, 1, "끝난 Adopt 세션은 Release 전이라 복원");
        assert_eq!(get(&exit), Some(STATUS_UNKNOWN.to_string()));
        put(&dir.join("result"), "ok");
        while link.recv().unwrap().is_some() {}
        return;
    }
    let ask = |link: &mut Link, m: ToHolder, fds: &[std::os::fd::BorrowedFd<'_>]| {
        send(link, &m, fds);
        reply(link, dir).0
    };
    if !dir.join("session").exists() {
        let argv = ["/bin/sleep", "30"].map(|a| a.as_bytes().to_vec()).to_vec();
        let (env, cwd, size) = (vec![], vec![], WinSize { cols: 80, rows: 24 });
        let id = SessionId(3);
        let m = ToHolder::Spawn {
            req: 1,
            id,
            argv,
            env,
            cwd,
            size,
        };
        send(link, &m, &[]);
        let (r, mut fds) = reply(link, dir);
        let FromHolder::Spawned { pid, .. } = r else {
            panic!("{r:?}")
        };
        let master = fds.pop().unwrap();
        fs::remove_file(&sock).ok();
        let l = UnixListener::bind(&sock).unwrap();
        put(&dir.join("session"), &pid.to_string());
        while link.recv().unwrap().is_some() {} // holder kill -9 → EOF. master는 여기 남아 있다.
        let (s, _) = l.accept().unwrap();
        let status = None;
        let m = FromHolder::Session { id, pid, status };
        Link::new(OwnedFd::from(s))
            .send(&m.encode().unwrap(), &[master.as_fd()])
            .unwrap();
        return;
    }
    let mut from = Link::new(OwnedFd::from(UnixStream::connect(&sock).unwrap()));
    let p = from.recv().unwrap().unwrap();
    let FromHolder::Session { id, pid, .. } = FromHolder::decode(p.frame()).unwrap() else {
        panic!("인계 메시지")
    };
    let master = p.fds.into_iter().next().unwrap();
    let fd = [master.as_fd()];
    let mut gone = Command::new("/usr/bin/true").spawn().unwrap();
    let dead = gone.id();
    gone.wait().unwrap();
    let m = ToHolder::Adopt {
        req: 1,
        id: SessionId(4),
        pid: dead,
    };
    assert_eq!(
        ask(link, m, &fd),
        FromHolder::Failed { req: 1, errno: 3 },
        "끝난 pid ESRCH"
    );
    let m = ToHolder::Adopt { req: 2, id, pid };
    assert_eq!(ask(link, m, &fd), FromHolder::Done { req: 2 });
    let m = ToHolder::Adopt { req: 3, id, pid };
    assert_eq!(
        ask(link, m, &fd),
        FromHolder::Failed { req: 3, errno: 17 },
        "id 중복 EEXIST"
    );
    // exec 핸드오프 뒤에도 Adopt 세션의 NOTE_EXIT 감시가 이어져야 한다.
    let path = HOLDER.as_bytes().to_vec();
    send(link, &ToHolder::UpgradeHolder { req: 4, path }, &[]);
    while reply(link, dir).0 != (FromHolder::Done { req: 4 }) {}
    let m = ToHolder::Kill {
        req: 5,
        id,
        signal: 15,
    };
    assert_eq!(ask(link, m, &[]), FromHolder::Done { req: 5 });
    let status = loop {
        if let FromHolder::Exited { id: i, status } = next(link, dir).unwrap().0
            && i == id
        {
            break status;
        }
    };
    assert_eq!(status, STATUS_UNKNOWN, "종료 코드 없음 표시값");
    fs::remove_file(&exit).unwrap();
    put(&dir.join("phase2"), "");
    // 여기서 끝나면 holder가 3단계 krilld를 띄운다.
}

// ---------- 오케스트레이터 ----------

struct Rig {
    dir: PathBuf,
    holder: Child,
}

impl Rig {
    fn start(name: &str, env: &[(&str, String)]) -> Self {
        let dir = std::env::temp_dir().join(format!("mkh-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let holder = holder(&dir, env);
        Self { dir, holder }
    }

    fn krilld(&self) -> (Pid, u64) {
        let s = wait_for("krilld 준비", 30, || get(&self.dir.join("krilld")));
        let v: Vec<&str> = s.split(' ').collect();
        (pid(v[0]), v[1].parse().unwrap())
    }

    fn gens(&self) -> Vec<(u64, Pid)> {
        get(&self.dir.join("pids"))
            .unwrap()
            .lines()
            .map(|l| {
                let (id, p) = l.split_once(' ').unwrap();
                (id.parse().unwrap(), pid(p))
            })
            .collect()
    }

    /// 생성기를 멈추고 모든 종료 상태를 받는다. 모두 `100+id`로 끝나야 한다(SIGHUP 0).
    fn stop_and_check_exits(&self) {
        put(&self.dir.join("stop"), "");
        for (id, _) in self.gens() {
            let st: i32 = wait_for("생성기 종료 상태", 30, || {
                get(&self.dir.join(format!("exit-{id}")))
            })
            .parse()
            .unwrap();
            assert!(
                libc::WIFEXITED(st) && libc::WEXITSTATUS(st) == 100 + i32::try_from(id).unwrap(),
                "세션 {id} 상태 {st:#x} (signal {})",
                libc::WTERMSIG(st)
            );
        }
    }
}

fn holder(dir: &Path, env: &[(&str, String)]) -> Child {
    Command::new(HOLDER)
        .arg(std::env::current_exe().unwrap())
        .args(["role_krilld", "--exact", "--nocapture"])
        .env("MKH_ROLE", "krilld")
        .env("MKH_DIR", dir)
        .env("KRILL_LOG_DIR", dir.join("logs"))
        .envs(env.iter().map(|(k, v)| (k, v)))
        .spawn()
        .unwrap()
}

impl Drop for Rig {
    fn drop(&mut self) {
        put(&self.dir.join("stop"), "");
        // holder부터: krilld가 먼저 죽으면 holder가 다시 띄워 Drop이 모르는 pid가 남는다(OWE-604).
        self.holder.kill().ok();
        self.holder.wait().ok();
        if let Some(s) = get(&self.dir.join("krilld")) {
            let p = s.split(' ').next().unwrap_or("0");
            if let Some(p) = p.parse().ok().and_then(Pid::from_raw) {
                rustix::process::kill_process(p, Signal::KILL).ok();
                let end = Instant::now() + Duration::from_secs(10);
                while rustix::process::test_kill_process(p).is_ok() && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        fs::remove_dir_all(&self.dir).ok();
    }
}

/// 백분위(최근접 순위)를 ms로.
fn pct(v: &mut [u64], p: usize) -> f64 {
    v.sort_unstable();
    let i = (v.len() * p).div_ceil(100).saturating_sub(1);
    Duration::from_nanos(v[i]).as_secs_f64() * 1e3
}

/// MK-005: krilld를 kill -9 해도 세션은 SIGHUP 없이 살고, 재게시 p95 ≤ 300ms,
/// 첫 크래시는 즉시·반복 크래시만 백오프(0 → 250 → 500 → 1000ms).
#[test]
fn krilld_kill() {
    let rig = Rig::start(
        "kill",
        &[("MKH_GENS", "10".into()), ("MKH_RATE", "1000000".into())],
    );
    let (mut cur, _) = rig.krilld();
    let gens = rig.gens();
    let kills = env_u64("MKH_KILLS", 5);
    let mut lat = Vec::new();
    let kill = |cur: &mut Pid| {
        let t0 = mono_ns();
        rustix::process::kill_process(*cur, Signal::KILL).unwrap();
        let (next, ready) = wait_for("새 krilld", 60, || {
            let (p, r) = rig.krilld();
            (p != *cur).then_some((p, r))
        });
        *cur = next;
        for (id, g) in &gens {
            assert!(
                rustix::process::test_kill_process(*g).is_ok(),
                "세션 {id} 소실"
            );
        }
        ready.saturating_sub(t0)
    };
    for _ in 0..kills {
        std::thread::sleep(Duration::from_millis(1100)); // STABLE(1s) 이상 살린 뒤 kill = 첫 크래시
        lat.push(kill(&mut cur));
    }
    std::thread::sleep(Duration::from_millis(1100));
    let burst: Vec<u64> = (0..4).map(|_| kill(&mut cur) / 1_000_000).collect();
    println!(
        "krilld kill {kills}회: 재게시 p50 {:.2}ms p95 {:.2}ms 최대 {:.2}ms, 연속 kill 지연 {burst:?}ms",
        pct(&mut lat, 50),
        pct(&mut lat, 95),
        pct(&mut lat, 100)
    );
    assert!(pct(&mut lat, 95) <= 300.0);
    for (got, want) in burst.iter().zip([0, 250, 500, 1000]) {
        assert!(
            *got >= want && *got < want + 250,
            "연속 kill 지연 {burst:?}"
        );
    }
    rig.stop_and_check_exits();
}

/// 내부 프로토콜 계약: 모르는 kind·Spawn 실패·id 중복·Kill·Release 응답, 끝난 세션과 listen 소켓의
/// 복원, `UpgradeKrilld` 뒤 즉시 재기동.
#[test]
fn protocol_contract() {
    let rig = Rig::start("contract", &[("MKH_MODE", "contract".into())]);
    let r = wait_for("계약 2단계", 30, || get(&rig.dir.join("result")));
    assert_eq!(r, "ok");
}

/// holder만 크래시(ARCHITECTURE §4.1): holder kill -9 뒤에도 세션이 살고(krilld가 master 보유), 새 holder가
/// `Adopt`로 맡은 세션의 종료를 `NOTE_EXIT`로 감지해 `Exited{STATUS_UNKNOWN}`을 보낸다. 시나리오는 `adopt`.
#[test]
fn holder_crash_adopt() {
    let env = [("MKH_MODE", "adopt".to_owned())];
    let mut rig = Rig::start("adopt", &env);
    let session = pid(&wait_for("세션 spawn", 30, || {
        get(&rig.dir.join("session"))
    }));
    rig.holder.kill().unwrap(); // SIGKILL
    rig.holder.wait().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        rustix::process::test_kill_process(session).is_ok(),
        "holder kill -9 뒤 세션 소실"
    );
    rig.holder = holder(&rig.dir, &env);
    let r = wait_for("재결합 3단계", 30, || get(&rig.dir.join("result")));
    assert_eq!(r, "ok");
}
