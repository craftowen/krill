//! MK-026 입력 ID·ack와 끊긴 동안 보류를 실제 PTY·프로세스·UDS로 검사한다.
//! - 끊긴 동안 친 입력은 붙은 뒤 한 번, ack 전에 끊긴 입력은 다시 보내지 않음 → 셸이 받은 줄: 중복 0, 확정분 유실 0.
//! - 끝난 세션에 보낸 입력은 `Closed`로 답한다.
//! - 감시 연결(화면 feature 없음)은 krilld가 닫지 않는다.
//!
//! 재현: `cargo nextest run -p krilld --test input --no-capture`.

#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::collections::HashMap;
use std::io::{ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use std::{env, fs, thread};

use krill_proto::SessionId;
use krill_proto::daemon::{client_hello, watch_hello};
use krill_proto::holder::WinSize;
use krill_proto::input::{Fate, Incarnation, Input, InputAck, Journal, Notice, Now};
use krill_proto::ipc::{self, ClientRole, FEATURES, PROTO_MAX, PROTO_MIN, ServerHello};
use krill_proto::screen::{Attach, Msg, Refused, Stream, kind};
use krilld::attach::{self, Tap};
use krilld::ipc::Listener;
use krilld::{Appearance, Session};
use rustix::termios::Winsize;

const SIZE: WinSize = WinSize { cols: 80, rows: 24 };

fn tmp(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("mk026-{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// `/bin/sh -c script`를 새 PTY에 붙여 띄우고 세션을 시작한다(디스크 기록 없음).
fn spawn(id: u64, script: &str) -> (Child, Session) {
    let ws = Winsize {
        ws_row: SIZE.rows,
        ws_col: SIZE.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = rustix_openpty::openpty(None, Some(&ws)).unwrap();
    let child = Command::new("/bin/sh")
        .args(["-c", script])
        .stdin(Stdio::from(pty.user.try_clone().unwrap()))
        .stdout(Stdio::from(pty.user.try_clone().unwrap()))
        .stderr(Stdio::from(pty.user))
        .spawn()
        .unwrap();
    let look = Arc::new(Appearance::load(PathBuf::from("/nonexistent/mk026/theme")));
    let session = Session::start(SessionId(id), pty.controller, SIZE, look, None).unwrap();
    (child, session)
}

/// 소켓을 게시하고 연결마다 [`attach::serve`]를 돌린다.
fn server(dir: &Path, taps: Vec<Tap>) -> PathBuf {
    let sock = dir.join("run/k.sock");
    let lis = Listener::bind(&sock).unwrap();
    let hello = ServerHello {
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        features: FEATURES,
        build: "it".to_owned(),
        instance_id: 1,
    };
    let taps: Arc<HashMap<SessionId, Tap>> =
        Arc::new(taps.into_iter().map(|t| (t.id(), t)).collect());
    thread::spawn(move || {
        loop {
            let Ok(s) = lis.accept() else { continue };
            let (hello, taps) = (hello.clone(), Arc::clone(&taps));
            thread::spawn(move || attach::serve(s, &hello, |id| taps.get(&id).cloned()).ok());
        }
    });
    sock
}

/// Hello 협상 + 화면 스트림 Attach. `Attached`까지 받는다.
fn attach(sock: &Path, id: u64) -> UnixStream {
    let mut s = UnixStream::connect(sock).unwrap();
    ipc::read_frame(&mut s).unwrap().unwrap();
    s.write_all(&client_hello(ClientRole::App, "it").encode().unwrap())
        .unwrap();
    let a = Attach {
        session: SessionId(id),
        stream: Stream::Screen { known: None },
    };
    s.write_all(&a.encode().unwrap()).unwrap();
    let (k, _) = ipc::read_frame(&mut s).unwrap().unwrap();
    assert_eq!(k, kind::ATTACHED);
    s
}

fn send(s: &mut UnixStream, frames: Vec<Input>) {
    for f in frames {
        s.write_all(&f.encode().unwrap()).unwrap();
    }
}

/// 화면 Update는 건너뛰고 ack를 `n`개 받아 장부에 넣는다. 나온 알림을 돌려준다.
fn acks(conn: &mut UnixStream, journal: &mut Journal, count: usize) -> Vec<Notice> {
    let mut notices = Vec::new();
    let mut got = 0;
    while got < count {
        let (k, b) = ipc::read_frame(conn).unwrap().unwrap();
        if k == krill_proto::input::kind::INPUT_ACK {
            notices.extend(journal.ack(&InputAck::decode(k, &b).unwrap()));
            got += 1;
        }
    }
    notices
}

fn line(i: usize) -> String {
    format!("input-{i:04}\n")
}

fn type_lines(j: &mut Journal, range: std::ops::RangeInclusive<usize>) -> Vec<Input> {
    range
        .flat_map(|i| j.input(line(i).as_bytes(), Now::get()).unwrap())
        .collect()
}

#[test]
fn held_input_is_delivered_once_and_unacked_is_not_resent() {
    let dir = tmp("once");
    let out = dir.join("out.txt");
    let (mut child, session) = spawn(
        1,
        &format!("stty -echo; exec /bin/cat > '{}'", out.display()),
    );
    let sock = server(&dir, vec![session.tap()]);
    let mut j = Journal::new();

    // 1. 붙기 전에 친 입력은 보류된다.
    assert!(type_lines(&mut j, 1..=50).is_empty());
    // 2. 같은 세션에 붙으면 한 번 보낸다.
    let mut a = attach(&sock, 1);
    let (frames, notices) = j.relink(Incarnation::Same, Now::get());
    assert!(notices.is_empty());
    assert_eq!(frames.len(), 50);
    send(&mut a, frames);
    assert!(acks(&mut a, &mut j, 50).is_empty());
    // 3. 보내고 ack를 읽기 전에 끊긴다(krilld 크래시와 같은 처지): 결과 불확실, 다시 보내지 않는다.
    let inflight = type_lines(&mut j, 51..=60);
    assert_eq!(inflight.len(), 10);
    send(&mut a, inflight);
    drop(a);
    let uncertain = j.unlink().unwrap();
    assert_eq!(uncertain.fate, Fate::Uncertain);
    assert_eq!(
        uncertain.bytes,
        (51..=60).map(line).collect::<String>().into_bytes()
    );
    // 4. 끊긴 동안 친 입력은 다시 붙은 뒤 그것만 간다.
    assert!(type_lines(&mut j, 61..=100).is_empty());
    let mut b = attach(&sock, 1);
    let (frames, notices) = j.relink(Incarnation::Same, Now::get());
    assert!(notices.is_empty());
    assert_eq!(
        frames
            .iter()
            .flat_map(|f| f.bytes.clone())
            .collect::<Vec<u8>>(),
        (61..=100).map(line).collect::<String>().into_bytes()
    );
    send(&mut b, frames);
    assert!(acks(&mut b, &mut j, 40).is_empty());
    // 5. Ctrl-D로 cat을 끝내고 받은 줄을 센다.
    let eof = j.input(b"\x04", Now::get()).unwrap();
    send(&mut b, eof);
    acks(&mut b, &mut j, 1);
    assert!(child.wait().unwrap().success());
    let got = fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = got.lines().collect();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for l in &lines {
        *seen.entry(l).or_default() += 1;
    }
    let dup = seen.values().filter(|&&n| n > 1).count();
    let missing = (1..=50)
        .chain(61..=100)
        .filter(|&i| !seen.contains_key(line(i).trim_end()))
        .count();
    let applied_uncertain = (51..=60)
        .filter(|&i| seen.contains_key(line(i).trim_end()))
        .count();
    println!(
        "mk026 input: lines {} duplicates {dup} missing(confirmed) {missing} uncertain-applied {applied_uncertain}/10",
        lines.len()
    );
    assert_eq!(dup, 0, "duplicated input: {got}");
    assert_eq!(missing, 0, "lost input: {got}");
    // 확정된 입력은 친 순서 그대로다(불확실한 51~60은 끊긴 연결의 스레드가 늦게 쓸 수 있다).
    let confirmed: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !(51..=60).any(|i| line(i).trim_end() == *l))
        .collect();
    let mut sorted = confirmed.clone();
    sorted.sort_unstable();
    assert_eq!(confirmed, sorted);

    // 6. 끝난 세션에 보낸 입력은 쓰지 않고 Closed로 답한다.
    while !session.is_closed() {
        thread::sleep(Duration::from_millis(10));
    }
    let late = j.input(b"late\n", Now::get()).unwrap();
    send(&mut b, late);
    assert_eq!(
        acks(&mut b, &mut j, 1),
        [Notice {
            fate: Fate::SessionClosed,
            bytes: b"late\n".to_vec()
        }]
    );
    fs::remove_dir_all(&dir).ok();
}

/// 화면 feature 없이 협상한 감시 연결은 krilld가 닫지 않는다(Attach 대기 시간도 없음). 프레임에는 `BadRequest`.
#[test]
fn watch_connection_stays_open() {
    let dir = tmp("watch");
    let sock = server(&dir, Vec::new());
    let mut s = UnixStream::connect(&sock).unwrap();
    ipc::read_frame(&mut s).unwrap().unwrap();
    s.write_all(&watch_hello(ClientRole::App, "it").encode().unwrap())
        .unwrap();
    s.set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let e = ipc::read_frame(&mut s).unwrap_err();
    assert!(
        matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
        "{e}"
    );
    s.write_all(&ipc::encode_frame(kind::RESYNC, &[]).unwrap())
        .unwrap();
    let (k, b) = ipc::read_frame(&mut s).unwrap().unwrap();
    assert_eq!(Refused::decode(k, &b).unwrap(), Refused::BadRequest);
    fs::remove_dir_all(&dir).ok();
}

/// INPUT·ack는 `FEAT_INPUT`을 협상한 연결에서만 받는다(N-1 클라이언트는 이 kind를 보내지 않는다).
#[test]
fn input_without_feature_is_refused() {
    let dir = tmp("nofeat");
    let (mut child, session) = spawn(2, "exec /bin/cat > /dev/null");
    let sock = server(&dir, vec![session.tap()]);
    let mut s = UnixStream::connect(&sock).unwrap();
    ipc::read_frame(&mut s).unwrap().unwrap();
    let mut hello = client_hello(ClientRole::App, "old");
    hello.features &= !krill_proto::input::FEAT_INPUT;
    s.write_all(&hello.encode().unwrap()).unwrap();
    let a = Attach {
        session: SessionId(2),
        stream: Stream::Screen { known: None },
    };
    s.write_all(&a.encode().unwrap()).unwrap();
    let input = Input {
        id: krill_proto::input::InputId(1),
        bytes: b"x".to_vec(),
    };
    s.write_all(&input.encode().unwrap()).unwrap();
    loop {
        let (k, b) = ipc::read_frame(&mut s).unwrap().unwrap();
        assert_ne!(k, krill_proto::input::kind::INPUT_ACK);
        if k == kind::REFUSED {
            assert_eq!(Refused::decode(k, &b).unwrap(), Refused::BadRequest);
            break;
        }
    }
    child.kill().ok();
    child.wait().ok();
    fs::remove_dir_all(&dir).ok();
}

/// OWE-579: stdin을 읽지 않는 세션에 큰 입력을 보내고 끊어도, 그 세션에 새로 붙는 연결은 바로 `Attached`를 받는다
/// (막힌 입력 쓰기가 연결 장부 잠금을 쥐지 않는다). 끊긴 연결의 남은 입력은 버려 뒤 연결의 입력을 막지 않는다.
#[test]
fn stuck_input_does_not_block_new_attach() {
    let dir = tmp("stuck");
    let out = dir.join("out.txt");
    let (mut child, session) = spawn(
        3,
        &format!(
            "stty raw -echo; echo ready; sleep 2; exec /bin/cat > '{}'",
            out.display()
        ),
    );
    let tap = session.tap();
    // 정규 모드에서는 입력 큐가 차면 커널이 버려 쓰기가 막히지 않는다. raw가 된 뒤에 보낸다.
    tap.wait(|p| p.end > 0);
    let sock = server(&dir, vec![tap]);
    let mut a = attach(&sock, 3);
    a.set_write_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    // 60KB는 PTY 입력 큐보다 커서 krilld 요청 스레드가 쓰기에서 기다린다. 뒤 프레임은 소켓에 남는다.
    let big = 60 * 1_024;
    for (id, len) in [(1, big), (2, 16)] {
        let input = Input {
            id: krill_proto::input::InputId(id),
            bytes: vec![b'x'; len],
        };
        a.write_all(&input.encode().unwrap()).ok();
    }
    thread::sleep(Duration::from_millis(200));
    drop(a);
    let mut s = UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    ipc::read_frame(&mut s).unwrap().unwrap();
    s.write_all(&client_hello(ClientRole::App, "it").encode().unwrap())
        .unwrap();
    let attach = Attach {
        session: SessionId(3),
        stream: Stream::Screen { known: None },
    };
    s.write_all(&attach.encode().unwrap()).unwrap();
    let (k, _) = ipc::read_frame(&mut s)
        .expect("Attached within 1s")
        .unwrap();
    assert_eq!(k, kind::ATTACHED);
    // 자식이 읽기 시작하면 새 연결의 입력이 들어간다.
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut j = Journal::new();
    j.relink(Incarnation::Same, Now::get());
    send(&mut s, j.input(b"END", Now::get()).unwrap());
    assert!(acks(&mut s, &mut j, 1).is_empty());
    let got = (0..100)
        .find_map(|_| {
            let got = fs::read(&out).unwrap_or_default();
            if got.ends_with(b"END") {
                return Some(got);
            }
            thread::sleep(Duration::from_millis(50));
            None
        })
        .expect("END reaches the child");
    // 끊긴 연결의 60KB를 다 썼다면 "END" 앞에 그만큼 있다.
    let before = got.len() - b"END".len();
    assert!(before < big, "abandoned input was written: {before}");
    child.kill().ok();
    child.wait().ok();
    fs::remove_dir_all(&dir).ok();
}
