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
use krill_proto::input::{Input, InputAck, InputId, InputResult};
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
    fixture_command(exe, dir)
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
    rejoin_round(false);
}

#[test]
fn failed_master_receiver_then_rejoin_restores_live_input_and_output() {
    rejoin_round(true);
}

#[expect(
    clippy::panic,
    reason = "integration fixture requires successful spawn replies"
)]
fn rejoin_round(failed_receiver: bool) {
    let dir = tmp(if failed_receiver {
        "failed-receiver"
    } else {
        "e2e"
    });
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
        "stty raw -echo; exec /usr/bin/perl -MIO::Select -e '$|=1; $r=IO::Select->new(\\*STDIN); for ($i=1;;$i++) { print \"tick $i\\r\\n\"; if ($r->can_read(0)) { $s=<STDIN>; chomp $s; print \"REPLY:$s\\r\\n\"; } select(undef,undef,undef,0.0005) }'",
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

    if failed_receiver {
        // Consume a real master fd, then disappear without acknowledging Ready.
        // The next successor must recover while the old reader stays parked.
        let stream = UnixStream::connect(sock.with_extension("sock.hnd")).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let mut link = krilld::holder::link::Link::new(stream.into());
        let hello = link.recv().unwrap().unwrap();
        assert!(matches!(
            krill_proto::holder::FromHolder::decode(hello.frame()).unwrap(),
            krill_proto::holder::FromHolder::Hello { .. }
        ));
        let transferred = link.recv().unwrap().unwrap();
        assert!(matches!(
            krill_proto::holder::FromHolder::decode(transferred.frame()).unwrap(),
            krill_proto::holder::FromHolder::Session { .. }
        ));
        assert_eq!(transferred.fds.len(), 1);
        drop((transferred, link));
        assert!(alive(k1) && alive(tick_pid));
    }

    // 새 holder: krilld#2가 krilld#1에게서 세션을 넘겨받고 krilld#1은 끝난다.
    // 실제 krill 클라이언트가 새 holder를 띄운다. 자식에도 격리한 상태 디렉터리를 전달한다.
    let krill = Path::new(KRILLD).with_file_name("krill");
    assert!(krill.is_file(), "krill must be built next to krilld");
    let doctor = fixture_command(krill, &dir)
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
    assert_round_trip(&sock, ticker, b"AFTER-REJOIN\n");
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
    assert_round_trip(&sock, ticker, b"AFTER-ADOPTED-DAEMON-CRASH\n");

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

fn assert_round_trip(sock: &Path, session: SessionId, bytes: &[u8]) {
    use std::io::Write as _;
    let (mut stream, _) = connect(sock).unwrap();
    stream
        .write_all(
            &Attach {
                session,
                stream: Stream::Raw { from: Seq(0) },
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
    Attached::decode(kind, &body).unwrap();
    stream
        .write_all(
            &Input {
                id: InputId(1),
                bytes: bytes.to_vec(),
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    let expected = format!("REPLY:{}\r\n", String::from_utf8_lossy(bytes).trim_end());
    let mut output = Vec::new();
    let mut acknowledged = false;
    let deadline = Instant::now() + WAIT;
    while !acknowledged || !String::from_utf8_lossy(&output).contains(&expected) {
        assert!(
            Instant::now() < deadline,
            "no fresh response after handover"
        );
        stream
            .set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))
            .unwrap();
        let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
        if kind == krill_proto::input::kind::INPUT_ACK {
            let ack = InputAck::decode(kind, &body).unwrap();
            assert_eq!(ack.id, InputId(1));
            assert_eq!(ack.result, InputResult::Applied);
            acknowledged = true;
        } else {
            let (seq, bytes) = decode_output(kind, &body).unwrap();
            assert_eq!(seq.0, output.len() as u64, "round-trip output gap");
            output.extend_from_slice(bytes);
        }
    }
}

fn fixture_command(program: impl AsRef<std::ffi::OsStr>, dir: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env("KRILL_STATE_DIR", dir)
        .env("KRILL_LOG_DIR", dir.join("logs"))
        .env("HOME", dir)
        .env("ZDOTDIR", dir)
        .env("BASH_ENV", "/dev/null")
        .env("DO_NOT_TRACK", "1")
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    for name in [
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "KRILL_ACCOUNT_PROVIDER",
        "KRILL_ACCOUNT_DIR",
    ] {
        command.env_remove(name);
    }
    command
}

/// `<sock><suffix>` 경로.
fn beside(sock: &Path, suffix: &str) -> PathBuf {
    let mut s = sock.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// A-01: holder 링크가 holder의 죽음(EOF)이 아닌 이유로 깨지면 krilld는 바로 고아가 되지 않는다. 링크를 닫아 살아 있는
/// holder가 krilld를 다시 띄우게 한다(고아가 되면 클라이언트가 holder를 하나 더 띄워 두 holder가 생긴다). 아무도
/// 거두지 않으면(holder도 없다) 유예 뒤 고아 운영으로 세션을 지킨다.
#[test]
fn broken_holder_link_is_closed_before_orphaning() {
    use krill_proto::holder::{FromHolder, VERSION};
    use krilld::holder::link::Link;
    use std::sync::mpsc;

    let dir = tmp("broken-link");
    let sock = dir.join("run/k.sock");
    let (ours, theirs) = rustix::net::socketpair(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::STREAM,
        rustix::net::SocketFlags::empty(),
        None,
    )
    .unwrap();
    rustix::io::fcntl_setfd(&ours, rustix::io::FdFlags::CLOEXEC).unwrap();
    rustix::io::fcntl_setfd(&theirs, rustix::io::FdFlags::CLOEXEC).unwrap();
    let mut kids = Kids::default();
    // `holders`는 kill 뒤 wait까지 한다(거두지 않으면 좀비가 살아 있는 것으로 보인다).
    kids.holders.push(
        fixture_command(KRILLD, &dir)
            .arg(&sock)
            .stdin(Stdio::from(theirs))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut link = Link::new(ours);
    let hello = FromHolder::Hello {
        version: VERSION,
        holder_pid: std::process::id(),
        inbox: false,
    };
    for m in [hello, FromHolder::Ready] {
        link.send(&m.encode().unwrap(), &[]).unwrap();
    }
    wait_daemon(&sock, 0);
    link.send(&[0, 0, 0, 0], &[]).unwrap(); // 길이 0 프레임: 프로토콜 위반
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(Some(_)) = link.recv() {} // HoldListener 뒤 EOF
        tx.send(()).ok();
    });
    rx.recv_timeout(Duration::from_secs(1))
        .expect("krilld kept the broken link open; a live holder would never restart it");
    let hnd = beside(&sock, ".hnd");
    assert!(!hnd.exists(), "orphaned while the holder may be alive");
    wait_until("orphan after the grace period", || hnd.exists());
    drop(kids);
    fs::remove_dir_all(dir).ok();
}

/// C-2: 고아 krilld는 있는데 인계 전송이 끊기면 중복 기동으로 끝내지 않는다. 같은 holder가 krilld를 다시 띄워
/// 재시도하고, 두 번째 인계가 끝나면 소켓을 게시한다.
#[test]
fn failed_handover_transfer_is_retried_under_the_same_holder() {
    use krill_proto::holder::{FromHolder, VERSION};
    use krilld::holder::link::Link;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::UnixListener;

    let dir = tmp("transfer-retry");
    let sock = dir.join("run/k.sock");
    fs::create_dir_all(sock.parent().unwrap()).unwrap();
    fs::set_permissions(sock.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
    // 고아 krilld 흉내: 소켓 락을 쥐고 인계 소켓에서 기다린다.
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(beside(&sock, ".lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let hnd = UnixListener::bind(beside(&sock, ".hnd")).unwrap();
    hnd.set_nonblocking(true).unwrap();
    let accept = || {
        let deadline = Instant::now() + WAIT;
        loop {
            match hnd.accept() {
                Ok((s, _)) => {
                    s.set_nonblocking(false).unwrap();
                    return s;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no handover attempt");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("{e}"),
            }
        }
    };
    let mut kids = Kids::default();
    kids.holders.push(holder(&dir, &sock));
    drop(accept()); // 첫 시도: Ready 전에 끊는다
    let mut link = Link::new(OwnedFd::from(accept()));
    let hello = FromHolder::Hello {
        version: VERSION,
        holder_pid: std::process::id(),
        inbox: false,
    };
    for m in [hello, FromHolder::Ready] {
        link.send(&m.encode().unwrap(), &[]).unwrap();
    }
    let p = link.recv().unwrap().unwrap();
    assert_eq!(FromHolder::decode(p.frame()).unwrap(), FromHolder::Ready);
    drop(lock);
    drop(link);
    let (_c, krilld) = wait_daemon(&sock, 0);
    kids.pids.push(krilld);
    assert!(
        kids.holders[0].try_wait().unwrap().is_none(),
        "holder gave up after a failed transfer"
    );
    drop(kids);
    fs::remove_dir_all(dir).ok();
}

/// 경로 B: krilld가 재기동을 기다리는 동안(소켓 락이 비었다) 클라이언트가 holder를 하나 더 띄워도 그 krilld는 살아
/// 있는 holder의 소켓을 덮어쓰지 않는다. 새 holder는 중복으로 끝나고, 원래 holder가 krilld를 다시 띄워 세션을
/// 그대로 돌려준다. 덮어쓰면 원래 holder의 세션에는 아무도 붙지 못한다.
#[test]
fn second_holder_cannot_take_the_socket_while_its_owner_restarts_krilld() {
    let dir = tmp("owner-lock");
    let sock = dir.join("run/k.sock");
    let mut kids = Kids::default();
    kids.holders.push(holder(&dir, &sock));
    let (mut c, first) = wait_daemon(&sock, 0);
    kids.pids.push(first);
    let Reply::Created {
        session,
        pid: shell,
    } = create(&mut c, &dir, "sleep 60")
    else {
        unreachable!("create failed")
    };
    kids.pids.push(shell);
    drop(c);
    // 원래 holder를 멈춰 krilld 재기동 대기를 붙잡아 둔다.
    let owner = Pid::from_child(&kids.holders[0]);
    kill_process(owner, Signal::STOP).unwrap();
    kill_process(pid(first), Signal::KILL).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(beside(&sock, ".lock"))
        .unwrap();
    wait_until("krilld released the socket lock", || {
        lock.try_lock().is_ok()
    });
    drop(lock);
    kids.holders.push(holder(&dir, &sock));
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(s) = kids.holders[1].try_wait().unwrap() {
            break s;
        }
        assert!(
            Instant::now() < deadline,
            "second holder took over a socket whose holder is alive"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "second holder: {status}");
    kill_process(owner, Signal::CONT).unwrap();
    let (mut c, next) = wait_daemon(&sock, u64::from(first));
    kids.pids.push(next);
    assert_eq!(
        list(&mut c),
        vec![Info {
            session,
            pid: shell,
            status: None
        }]
    );
    drop(c);
    drop(kids);
    fs::remove_dir_all(dir).ok();
}

/// 소켓을 맡은 holder가 krilld를 다시 띄우는 순간 다른 krilld의 bind 탐침이 `.lock`을 잠깐 쥐면 새 krilld는 중복으로
/// 끝난다. 그 holder가 owner 락과 listener를 쥔 채 멈추면 아무도 소켓을 받지 못해 세션에 영영 붙지 못한다.
/// 탐침이 끝나면 krilld를 다시 띄워 세션을 돌려줘야 한다.
#[test]
fn socket_owner_restarts_krilld_after_a_lock_probe_overlaps() {
    let dir = tmp("owner-probe");
    let sock = dir.join("run/k.sock");
    let mut kids = Kids::default();
    kids.holders.push(holder(&dir, &sock));
    let (mut c, first) = wait_daemon(&sock, 0);
    kids.pids.push(first);
    let Reply::Created {
        session,
        pid: shell,
    } = create(&mut c, &dir, "sleep 60")
    else {
        unreachable!("create failed")
    };
    kids.pids.push(shell);
    drop(c);
    let owner = Pid::from_child(&kids.holders[0]);
    kill_process(owner, Signal::STOP).unwrap();
    kill_process(pid(first), Signal::KILL).unwrap();
    let probe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(beside(&sock, ".lock"))
        .unwrap();
    wait_until("krilld released the socket lock", || {
        probe.try_lock().is_ok()
    });
    // 실제 탐침은 수십 µs다. owner가 다시 띄우는 동안 쥐고 있어 겹침을 고정한다.
    kill_process(owner, Signal::CONT).unwrap();
    thread::sleep(Duration::from_secs(1));
    drop(probe);
    let (mut c, next) = wait_daemon(&sock, u64::from(first));
    kids.pids.push(next);
    assert_eq!(
        list(&mut c),
        vec![Info {
            session,
            pid: shell,
            status: None
        }]
    );
    drop(c);
    drop(kids);
    fs::remove_dir_all(dir).ok();
}
