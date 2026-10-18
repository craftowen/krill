//! OWE-479·493: 실제 `krill-holder` + `krilld` + `krill` 바이너리로 holder 재결합 전체.
//! holder `kill -9` → krilld가 고아로 세션을 계속 운영(출력·목록, 새 세션 거절, 세션 종료는 `NOTE_EXIT`로
//! [`STATUS_UNKNOWN`]) → 감시 연결 상실 통보·실제 `krill doctor`가 새 holder 기동 → krilld#2가 세션을 인계받고
//! holder에 `Adopt` → 처음부터
//! 읽은 원본에 빠진 줄 없음 → krilld#2 `kill -9` 뒤에도 holder가 세션을 돌려줌(맡겨졌다는 증거) → 닫기.
//!
//! `krill-holder`는 `krilld` 옆(같은 target 디렉터리)에 있어야 한다: `cargo nextest run --workspace`.

#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use krill_proto::SessionId;
use krill_proto::control::{Create, Failure, Info, Program, Reply, Request};
use krill_proto::daemon::{self, HolderStatus, client_hello};
use krill_proto::holder::STATUS_UNKNOWN;
use krill_proto::ipc::{self, ClientRole, ServerHello};
use krill_proto::screen::{Attach, Attached, Msg, Seq, Stream, decode_output, kind};
use krilld::store::{Limits, Piece, Store};
use rustix::process::{Pid, Signal, kill_process, test_kill_process};

const KRILLD: &str = env!("CARGO_BIN_EXE_krilld");
const WAIT: Duration = Duration::from_secs(10);

fn pid(p: u32) -> Pid {
    Pid::from_raw(i32::try_from(p).unwrap()).unwrap()
}

fn alive(p: u32) -> bool {
    test_kill_process(pid(p)).is_ok()
}

/// 테스트가 실패해도 holder·krilld·세션을 남기지 않는다.
#[derive(Default)]
struct Kids {
    holders: Vec<Child>,
    detached_holders: Vec<u32>,
    pids: Vec<u32>,
}

impl Drop for Kids {
    fn drop(&mut self) {
        // holder부터: krilld가 먼저 죽으면 holder가 다시 띄워 Drop이 모르는 pid가 남는다(OWE-604).
        for h in &mut self.holders {
            h.kill().ok();
            h.wait().ok();
        }
        for &p in &self.detached_holders {
            kill_process(pid(p), Signal::KILL).ok();
        }
        for &p in &self.pids {
            kill_process(pid(p), Signal::KILL).ok();
        }
        // holder가 죽었으므로 재기동은 없다. 고아 krilld가 사라질 때까지.
        let deadline = Instant::now() + Duration::from_secs(10);
        while self
            .detached_holders
            .iter()
            .chain(&self.pids)
            .any(|p| alive(*p))
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

/// 이 소켓 경로를 인자로 가진 holder 프로세스가 0개인지 확인한다(OWE-604).
/// `Kids`를 명시 drop한 뒤 부른다(Drop 안에서는 단언하지 않는다).
fn assert_no_holders(sock: &Path) {
    let out = Command::new("/usr/bin/pgrep")
        .arg("-f")
        .arg(sock)
        .output()
        .unwrap();
    let mut holders = Vec::new();
    for line in String::from_utf8(out.stdout).unwrap().lines() {
        let pid: u32 = line.trim().parse().unwrap();
        let cmd = Command::new("/bin/ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let cmd = String::from_utf8(cmd.stdout).unwrap();
        if cmd.contains("krill-holder") {
            holders.push(format!("{pid}: {}", cmd.trim()));
        }
    }
    assert!(
        holders.is_empty(),
        "residual holder on {}: {holders:?}",
        sock.display()
    );
}

fn holder(dir: &Path, sock: &Path) -> Child {
    let exe = Path::new(KRILLD).parent().unwrap().join("krill-holder");
    assert!(
        exe.is_file(),
        "krill-holder is not built next to krilld (run the whole workspace: cargo nextest run --workspace)"
    );
    Command::new(exe)
        .arg(KRILLD)
        .arg(sock)
        .env("KRILL_STATE_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn connect(sock: &Path) -> Option<(UnixStream, ServerHello)> {
    let mut s = UnixStream::connect(sock).ok()?;
    s.set_read_timeout(Some(WAIT)).unwrap();
    let (k, b) = ipc::read_frame(&mut s).ok()??;
    let hello = ServerHello::decode(k, &b).unwrap();
    std::io::Write::write_all(
        &mut s,
        &client_hello(ClientRole::Cli, "it").encode().unwrap(),
    )
    .unwrap();
    Some((s, hello))
}

/// `instance_id`가 `not`이 아닌 krilld가 게시될 때까지 기다린다.
fn wait_daemon(sock: &Path, not: u64) -> (UnixStream, u32) {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some((c, h)) = connect(sock).filter(|(_, h)| h.instance_id != not) {
            return (c, u32::try_from(h.instance_id).unwrap());
        }
        assert!(Instant::now() < deadline, "krilld did not come up");
        thread::sleep(Duration::from_millis(20));
    }
}

fn request(c: &mut UnixStream, r: &Request) -> Reply {
    std::io::Write::write_all(c, &r.encode().unwrap()).unwrap();
    let (k, b) = ipc::read_frame(c).unwrap().unwrap();
    Reply::decode(k, &b).unwrap()
}

fn list(c: &mut UnixStream) -> Vec<Info> {
    let r = request(c, &Request::List);
    let Reply::Sessions(v) = r else {
        unreachable!("list: {r:?}")
    };
    v
}

fn create(c: &mut UnixStream, dir: &Path, script: &str) -> Reply {
    request(
        c,
        &Request::Create(Create {
            account: None,
            program: Program::Custom {
                program: b"/bin/sh".to_vec(),
                args: vec![b"-c".to_vec(), script.as_bytes().to_vec()],
            },
            cwd: dir.as_os_str().as_encoded_bytes().to_vec(),
            cols: 80,
            rows: 24,
        }),
    )
}

/// 원본 스트림을 `from`부터 받아 `done`이 참이 될 때까지 모은 텍스트와, 붙은 시점의 원본 끝. 프레임의 seq는 앞
/// 프레임 끝에서 끊김·겹침 없이 이어져야 한다(디스크·링 경계, krilld 인계 경계 포함).
fn raw_until(sock: &Path, id: SessionId, from: Seq, done: impl Fn(&str) -> bool) -> (String, u64) {
    let (mut c, _) = connect(sock).unwrap();
    let a = Attach {
        session: id,
        stream: Stream::Raw { from },
    };
    std::io::Write::write_all(&mut c, &a.encode().unwrap()).unwrap();
    let (k, b) = ipc::read_frame(&mut c).unwrap().unwrap();
    let end = Attached::decode(k, &b).unwrap().seq.0;
    let mut text = Vec::new();
    while !done(&String::from_utf8_lossy(&text)) {
        let (k, b) = ipc::read_frame(&mut c).unwrap().unwrap();
        if k == kind::OUTPUT {
            let (seq, bytes) = decode_output(k, &b).unwrap();
            assert_eq!(
                seq.0,
                from.0 + text.len() as u64,
                "raw seq is not contiguous"
            );
            text.extend_from_slice(bytes);
        }
    }
    (String::from_utf8(text).unwrap(), end)
}

fn ticks(text: &str) -> Vec<u64> {
    text.split("tick ")
        .filter_map(|t| t.split_whitespace().next()?.parse().ok())
        .collect()
}

fn last_tick(text: &str) -> u64 {
    ticks(text).into_iter().max().unwrap_or(0)
}

/// 디스크 세그먼트의 원본 `[0, to)`에 있는 Gap(`len == 0`은 봉인 없이 끊긴 krilld 경계).
fn disk_gaps(dir: &Path, id: SessionId, to: u64) -> Vec<(u64, u64)> {
    let store = Store::open(dir, Limits::default()).unwrap();
    let mut gaps = Vec::new();
    store
        .raw(id, 0, to, |p| {
            if let Piece::Gap { seq, len } = p {
                gaps.push((seq, len));
            }
            Ok(())
        })
        .unwrap();
    gaps
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !f() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn tmp(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("mk479-{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn holder_crash_orphan_then_rejoin() {
    let dir = tmp("e2e");
    let sock = dir.join("run/krilld.sock");
    let mut kids = Kids::default();
    kids.holders.push(holder(&dir, &sock));
    let (mut c, k1) = wait_daemon(&sock, 0);
    kids.pids.push(k1);

    let Reply::Created {
        session: ticker,
        pid: tick_pid,
    } = create(
        &mut c,
        &dir,
        // 0.5ms마다 한 줄: 인계 중 두 krilld가 함께 읽는 창(수 ms)이 생기면 줄이 섞이거나 빠진다.
        "exec /usr/bin/perl -e '$|=1; for ($i=1;;$i++) { print \"tick $i\\n\"; select(undef,undef,undef,0.0005) }'",
    )
    else {
        panic!("create ticker failed");
    };
    let Reply::Created {
        session: sleeper,
        pid: sleep_pid,
    } = create(&mut c, &dir, "exec /bin/sleep 60")
    else {
        panic!("create sleeper failed");
    };
    kids.pids.extend([tick_pid, sleep_pid]);
    raw_until(&sock, ticker, Seq(0), |t| last_tick(t) >= 5);

    // 앱과 같은 감시 연결. holder만 잃어도 이 연결로 통보받는다.
    let watch_hello = daemon::watch_hello(ClientRole::App, "mk493");
    let mut watch = daemon::handshake(&sock, &watch_hello, WAIT).unwrap();
    assert_eq!(watch.holder_status, Some(HolderStatus::Linked));
    watch.stream.set_read_timeout(Some(WAIT)).unwrap();

    // holder kill -9: krilld#1은 고아로 계속 운영한다.
    let mut h1 = kids.holders.remove(0);
    h1.kill().unwrap();
    h1.wait().unwrap();
    let (kind, body) = ipc::read_frame(&mut watch.stream).unwrap().unwrap();
    assert_eq!(
        HolderStatus::decode(kind, &body).unwrap(),
        HolderStatus::Orphan
    );
    daemon::wait_closed(&mut watch.stream);
    thread::sleep(Duration::from_millis(200));
    assert!(alive(k1), "krilld exited with its holder");
    assert!(
        alive(tick_pid) && alive(sleep_pid),
        "session died with holder"
    );
    let (mut c, same) = wait_daemon(&sock, 0);
    assert_eq!(same, k1, "socket must still be served by the orphan krilld");
    let now = last_tick(&raw_until(&sock, ticker, Seq(0), |t| last_tick(t) >= 5).0);
    raw_until(&sock, ticker, Seq(0), |t| last_tick(t) > now + 5);
    assert_eq!(
        create(&mut c, &dir, "true"),
        Reply::Failed(Failure::NoHolder)
    );
    // 고아 세션 종료는 NOTE_EXIT로 안다(종료 코드는 없다).
    kill_process(pid(sleep_pid), Signal::KILL).unwrap();
    wait_until("orphan exit noticed", || {
        list(&mut c)
            .iter()
            .any(|i| i.session == sleeper && i.status == Some(STATUS_UNKNOWN))
    });

    // 새 holder: krilld#2가 krilld#1에게서 세션을 넘겨받고 krilld#1은 끝난다.
    // 실제 krill 클라이언트가 새 holder를 띄운다. 자식에도 격리한 상태 디렉터리를 전달한다.
    let krill = Path::new(KRILLD).with_file_name("krill");
    assert!(krill.is_file(), "krill must be built next to krilld");
    let doctor = Command::new(krill)
        .arg("doctor")
        .env("KRILL_STATE_DIR", &dir)
        .output()
        .unwrap();
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stdout)
    );
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("(launched)"));
    let recovered = daemon::handshake(&sock, &watch_hello, WAIT).unwrap();
    assert_eq!(recovered.holder_status, Some(HolderStatus::Linked));
    let k2 = u32::try_from(recovered.server.instance_id).unwrap();
    let parent = Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &k2.to_string()])
        .output()
        .unwrap();
    let h2 = String::from_utf8(parent.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    kids.detached_holders.push(h2);
    kids.pids.push(k2);
    let (mut c, _) = wait_daemon(&sock, u64::from(k1));
    wait_until("orphan krilld gone", || !alive(k1));
    assert!(
        !dir.join("run/krilld.sock.hnd").exists(),
        "handoff socket left behind"
    );
    assert_eq!(
        list(&mut c),
        [
            Info {
                session: ticker,
                pid: tick_pid,
                status: None
            },
            Info {
                session: sleeper,
                pid: sleep_pid,
                status: Some(STATUS_UNKNOWN)
            },
        ]
    );
    // 홀더 크래시·인계 내내 원본이 빠지지 않았다: 처음부터 인계 경계 너머까지 읽은 줄 번호가 1부터 끊김 없이 이어진다.
    let (_, handed) = raw_until(&sock, ticker, Seq(0), |_| true);
    let (all, _) = raw_until(&sock, ticker, Seq(0), |t| t.len() as u64 > handed + 200);
    let seen = ticks(&all);
    assert!(
        seen.iter().copied().eq(1..=seen.len() as u64),
        "lost or duplicated output across handoff: {:?}",
        seen.windows(2).find(|w| w[1] != w[0] + 1)
    );
    // krilld#1은 리더를 멈추고 세그먼트를 봉인한 뒤 넘겼다: 디스크에 크래시 경계(Gap)가 없다.
    assert_eq!(disk_gaps(&dir, ticker, all.len() as u64), []);

    // 재결합 후 새 세션 생성도 다시 가능하다.
    let Reply::Created {
        session: fresh,
        pid: fresh_pid,
    } = create(&mut c, &dir, "exec /bin/sleep 60")
    else {
        panic!("create after recovery failed");
    };
    kids.pids.push(fresh_pid);
    assert_eq!(
        request(&mut c, &Request::Close { session: fresh }),
        Reply::Closed
    );
    wait_until("new session closed", || !alive(fresh_pid));
    wait_until("new session removed", || {
        !list(&mut c).iter().any(|i| i.session == fresh)
    });

    // 새 holder가 Adopt로 세션을 맡았다: krilld#2가 죽어도 holder가 krilld#3에 돌려준다.
    // 끝난 세션(sleeper)은 holder에 맡길 수 없어 krilld#2와 함께 목록에서 사라진다(디스크 기록은 남는다).
    kill_process(pid(k2), Signal::KILL).unwrap();
    let (mut c, k3) = wait_daemon(&sock, u64::from(k2));
    kids.pids.push(k3);
    assert!(alive(tick_pid), "adopted session died with krilld#2");
    assert_eq!(
        list(&mut c),
        [Info {
            session: ticker,
            pid: tick_pid,
            status: None
        }]
    );
    raw_until(&sock, ticker, Seq(0), |t| last_tick(t) > now + 15);

    // 닫기: holder가 Adopt한 세션을 끝내고(NOTE_EXIT) krilld가 지운다.
    assert_eq!(
        request(&mut c, &Request::Close { session: ticker }),
        Reply::Closed
    );
    wait_until("session removed", || list(&mut c).is_empty());
    wait_until("session process gone", || !alive(tick_pid));

    let sock_held = sock.clone();
    drop(kids);
    assert_no_holders(&sock_held);
    fs::remove_dir_all(&dir).ok();
}
