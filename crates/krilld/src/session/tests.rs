use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use krill_proto::input::{FEAT_INPUT, Input, InputAck, InputId, InputResult};
use krill_proto::ipc;
use krill_proto::roles::Role;
use krill_proto::screen::{Attach, Msg, Stream};
use krill_proto::search::{Direction, FEAT_SEARCH, SearchRequest};

use super::*;

#[test]
fn cross_review_recording_panic_still_publishes_raw_output_and_sequence() {
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let session = Session::start(
        SessionId(1),
        pty.controller,
        WinSize { cols: 80, rows: 24 },
        Arc::new(Appearance::load(PathBuf::new())),
        None,
    )
    .unwrap();
    let tap = session.tap();
    let (_, rev, _) = guarded(&tap.0, 3, |engine| {
        engine.feed(b"abc", Feed::Live, &tap.0.look)
    });
    assert!(!publish_batch(&tap.0, 0, b"abc", rev, || panic!(
        "record failure"
    )));
    let end = lock(&tap.0.out).end;
    let actual = tap.read(0, 3);
    drop(pty.user);
    session.wait().unwrap();
    assert_eq!(
        end, 3,
        "capture/search must not wait forever after a recorder panic"
    );
    assert_eq!(actual, Read::Bytes(b"abc".to_vec()));
}

#[test]
fn input_is_processed_while_search_waits_for_recording() {
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let session = Session::start(
        SessionId(1),
        pty.controller,
        WinSize { cols: 80, rows: 24 },
        Arc::new(Appearance::load(PathBuf::new())),
        None,
    )
    .unwrap();
    let tap = session.tap();
    // 엔진 반영과 디스크 기록 사이를 고정한다. 검색은 publish까지 기다리지만 입력은 처리돼야 한다.
    lock(&tap.0.live).seq = 1;
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let attached = tap.clone();
    let worker = thread::spawn(move || {
        crate::attach::run_as(
            server,
            Attach {
                session: SessionId(1),
                stream: Stream::Screen { known: None },
            },
            1,
            FEAT_SEARCH | FEAT_INPUT,
            Some(attached),
            Role::Observer,
        )
    });
    assert_eq!(
        ipc::read_frame(&mut client).unwrap().unwrap().0,
        krill_proto::screen::kind::ATTACHED
    );
    client
        .write_all(
            &SearchRequest {
                req: 1,
                query: "needle".into(),
                direction: Direction::Next,
                after: None,
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    client
        .write_all(
            &Input {
                id: InputId(1),
                bytes: b"input".to_vec(),
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    let reply = loop {
        match ipc::read_frame(&mut client) {
            Ok(Some((kind, body))) if kind == krill_proto::input::kind::INPUT_ACK => {
                break InputAck::decode(kind, &body).ok();
            }
            Ok(Some(_)) => {}
            _ => break None,
        }
    };
    // 실패해도 우리가 띄운 스레드·PTY를 정리한 뒤 판정한다.
    tap.0.publish(b"x", 0, false);
    client.shutdown(Shutdown::Both).unwrap();
    drop(client);
    worker.join().unwrap().ok();
    drop(pty.user);
    session.wait().unwrap();
    assert_eq!(
        reply,
        Some(InputAck {
            id: InputId(1),
            result: InputResult::Denied,
        }),
        "search must not block the input request loop"
    );
}

/// OWE-474: `guarded`는 엔진 panic을 잡고 엔진을 새로 만든 뒤 `false`를 돌려준다.
/// 실제 주입원이던 kitty 키보드 스택 overflow는 좁은 포크에서 고쳐져 출력 바이트로는
/// 더 이상 panic을 일으킬 수 없으므로, panic 자체를 클로저로 넣어 격리 기제(P4)를 확인한다.
#[test]
fn guarded_rebuilds_engine_after_panic() {
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let size = WinSize { cols: 80, rows: 24 };
    let look = Arc::new(Appearance::load(PathBuf::new()));
    let shared = Shared {
        id: SessionId(1),
        clients: Mutex::default(),
        input: Mutex::default(),
        live: Mutex::new(Live {
            engine: Engine::new(size),
            revision: 0,
            seq: 0,
            last_output: Instant::now(),
            input_since_done: true,
            last_input_ns: 0,
            done_ns: 0,
            viewers: 0,
            epoch: 0,
            parked: false,
            resized: None,
            size,
        }),
        gaps: Mutex::new(Vec::new()),
        master: pty.controller,
        look: Arc::clone(&look),
        out: Mutex::new(Out {
            ring: VecDeque::new(),
            end: 0,
            revision: 0,
            closed: false,
        }),
        changed: Condvar::new(),
        store: None,
    };
    // panic한 묶음은 `false`를 돌리고, 원본 끝은 먹은 바이트만큼 민다.
    // 복구 엔진은 시작 크기가 아니라 현재 크기(리사이즈 반영)로 만든다(K-19).
    lock(&shared.live).size = WinSize {
        cols: 100,
        rows: 40,
    };
    let (ok, ..) = guarded(&shared, 3, |_| panic!("OWE-474 격리 확인용 주입"));
    assert!(!ok);
    assert_eq!(lock(&shared.live).seq, 3);
    assert_eq!(
        lock(&shared.live).engine.screen_text().lines().count(),
        40,
        "recovered engine must use the current size, not the start size"
    );
    // 교체된 엔진은 다시 정상 동작한다.
    let (ok, ..) = guarded(&shared, 0, |e| e.feed(b"hi", Feed::Live, &look));
    assert!(ok);
    assert!(
        lock(&shared.live).engine.screen_text().contains("hi"),
        "교체된 엔진이 출력을 반영해야 한다"
    );
    drop(pty.user);
}

/// OWE-586: 유휴 세션은 화면을 문자열화하지 않는다. revision이 그대로면 `None`이라
/// 보조 감시(monitor)가 100ms마다 20세션 전체를 문자열화하는 비용이 사라진다.
#[test]
fn idle_sessions_do_not_restringify_unchanged_evidence() {
    let look = Arc::new(Appearance::load(PathBuf::new()));
    let mut sessions = Vec::new();
    for id in 0..20 {
        let pty = rustix_openpty::openpty(None, None).unwrap();
        let session = Session::start(
            SessionId(id),
            pty.controller,
            WinSize { cols: 80, rows: 24 },
            Arc::clone(&look),
            None,
        )
        .unwrap();
        sessions.push((session, pty.user));
    }
    let mut extractions = 0;
    for (session, _) in &sessions {
        let tap = session.tap();
        let (_, _, revision) = tap.evidence_since(None).unwrap();
        for _ in 0..200 {
            extractions += usize::from(tap.evidence_since(Some(revision)).is_some());
        }
        // resize도 revision을 올리므로 출력 없이 캐시를 무효화해야 한다.
        tap.resize(WinSize {
            cols: 100,
            rows: 30,
        })
        .unwrap();
        assert!(tap.evidence_since(Some(revision)).is_some());
    }
    for (session, user) in sessions {
        drop(user);
        session.wait().unwrap();
    }
    assert_eq!(extractions, 0, "unchanged screen was stringified");
}

#[test]
fn disconnected_input_waiter_cancels_before_turn_is_available() {
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let session = Session::start(
        SessionId(609),
        pty.controller,
        WinSize { cols: 80, rows: 24 },
        Arc::new(Appearance::load(PathBuf::new())),
        None,
    )
    .unwrap();
    let tap = session.tap();
    let held_tap = tap.clone();
    let held = lock(&held_tap.0.input);
    let (client, server) = UnixStream::pair().unwrap();
    let (done, recv) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        done.send(tap.write_input(b"must not write", Some(server.as_fd())))
            .unwrap();
    });
    drop(client);
    let result = recv
        .recv_timeout(Duration::from_secs(1))
        .expect("HUP must cancel a lock waiter");
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
    drop(held);
    worker.join().unwrap();
    drop(pty.user);
}

/// 실제 raw PTY 입력 큐를 채운 뒤 연결을 닫는다. 프레임을 가로지른 시작 표지와 잘린 본문을 복구한다.
#[test]
fn disconnected_paste_appends_end_marker_after_partial_write() {
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let mut termios = rustix::termios::tcgetattr(&pty.user).unwrap();
    termios.make_raw();
    rustix::termios::tcsetattr(&pty.user, rustix::termios::OptionalActions::Now, &termios).unwrap();
    let session = Session::start(
        SessionId(610),
        pty.controller,
        WinSize { cols: 80, rows: 24 },
        Arc::new(Appearance::load(PathBuf::new())),
        None,
    )
    .unwrap();
    let tap = session.tap();
    let (client, server) = UnixStream::pair().unwrap();
    let (done, recv) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        tap.write_input(b"\x1b[20", Some(server.as_fd())).unwrap();
        let mut body = b"0~".to_vec();
        body.extend(vec![b'x'; 1024 * 1024]);
        body.extend_from_slice(b"\x1b[201~");
        done.send(tap.write_input(&body, Some(server.as_fd())))
            .unwrap();
    });
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    // 64바이트 이상은 시작 표지뿐 아니라 본문도 PTY에 들어갔음을 보장한다.
    while got.len() < 64 {
        assert!(Instant::now() < deadline, "paste must reach the PTY");
        let mut fds = [PollFd::new(&pty.user, PollFlags::IN)];
        if poll(
            &mut fds,
            Some(&Timespec {
                tv_sec: 0,
                tv_nsec: 100_000_000,
            }),
        )
        .unwrap()
            > 0
        {
            let mut bytes = [0; 64];
            let n = read(&pty.user, &mut bytes).unwrap();
            got.extend_from_slice(bytes.get(..n).unwrap());
        }
    }
    assert!(matches!(
        recv.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    drop(client);
    while !got.ends_with(b"\x1b[201~") {
        assert!(Instant::now() < deadline, "missing paste end marker");
        let mut fds = [PollFd::new(&pty.user, PollFlags::IN)];
        if poll(
            &mut fds,
            Some(&Timespec {
                tv_sec: 0,
                tv_nsec: 100_000_000,
            }),
        )
        .unwrap()
            > 0
        {
            let mut bytes = [0; 4096];
            let n = read(&pty.user, &mut bytes).unwrap();
            got.extend_from_slice(bytes.get(..n).unwrap());
        }
    }
    assert!(got.starts_with(b"\x1b[200~"));
    assert!(
        got.len() < 1024 * 1024,
        "abandoned paste body must be discarded"
    );
    assert_eq!(
        recv.recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err()
            .kind(),
        io::ErrorKind::ConnectionAborted
    );
    worker.join().unwrap();
    drop(pty.user);
    session.wait().unwrap();
}

#[test]
fn disconnect_between_paste_frames_also_closes_the_paste() {
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let mut termios = rustix::termios::tcgetattr(&pty.user).unwrap();
    termios.make_raw();
    rustix::termios::tcsetattr(&pty.user, rustix::termios::OptionalActions::Now, &termios).unwrap();
    let session = Session::start(
        SessionId(611),
        pty.controller,
        WinSize { cols: 80, rows: 24 },
        Arc::new(Appearance::load(PathBuf::new())),
        None,
    )
    .unwrap();
    let tap = session.tap();
    let (mut client, server) = UnixStream::pair().unwrap();
    let worker = thread::spawn(move || {
        crate::attach::run(
            server,
            Attach {
                session: SessionId(611),
                stream: Stream::Screen { known: None },
            },
            1,
            FEAT_INPUT,
            Some(tap),
        )
    });
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    ipc::read_frame(&mut client).unwrap().unwrap();
    client
        .write_all(
            &Input {
                id: InputId(1),
                bytes: b"\x1b[200~body\x1b[20".to_vec(),
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    // ack까지 기다려 첫 프레임의 입력이 적용됐음을 보장한다.
    loop {
        if ipc::read_frame(&mut client).unwrap().unwrap().0 == krill_proto::input::kind::INPUT_ACK {
            break;
        }
    }
    drop(client);
    let result = worker.join().unwrap();
    assert!(
        result.is_ok()
            || result.is_err_and(|error| matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
            ))
    );
    let mut bytes = [0; 64];
    let n = read(&pty.user, &mut bytes).unwrap();
    assert_eq!(bytes.get(..n).unwrap(), b"\x1b[200~body\x1b[201~");
    drop(pty.user);
    session.wait().unwrap();
}

#[test]
fn paste_cleanup_race_is_repaired_before_the_next_writer() {
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let mut termios = rustix::termios::tcgetattr(&pty.user).unwrap();
    termios.make_raw();
    rustix::termios::tcsetattr(&pty.user, rustix::termios::OptionalActions::Now, &termios).unwrap();
    let session = Session::start(
        SessionId(612),
        pty.controller,
        WinSize { cols: 80, rows: 24 },
        Arc::new(Appearance::load(PathBuf::new())),
        None,
    )
    .unwrap();
    let tap = session.tap();
    let (client, server) = UnixStream::pair().unwrap();
    tap.write_input(b"\x1b[200~body", Some(server.as_fd()))
        .unwrap();
    drop(client);
    let held = lock(&tap.0.input);
    tap.finish_input(); // 다른 입력이 잠금을 쥐고 있어 연결 정리가 즉시 끝난 경우.
    drop(held);
    thread::spawn(move || tap.write_input(b"NEXT", None))
        .join()
        .unwrap()
        .unwrap();
    let mut bytes = [0; 64];
    let n = read(&pty.user, &mut bytes).unwrap();
    assert_eq!(bytes.get(..n).unwrap(), b"\x1b[200~body\x1b[201~NEXT");
    drop(pty.user);
    session.wait().unwrap();
}

/// K-05: PTY 읽기 오류 재시도 간격은 25ms부터 두 배로 최대 800ms다.
/// 상한이 없으면 영구 오류 때 CPU를 쓰고, 재시도 자체가 없으면 리더가 끝나
/// PTY를 읽는 쪽이 없어 세션 출력이 영구 정지한다.
#[test]
fn read_error_backoff_is_capped() {
    assert_eq!(super::backoff(0), Duration::from_millis(25));
    assert_eq!(super::backoff(1), Duration::from_millis(50));
    assert_eq!(super::backoff(5), Duration::from_millis(800));
    assert_eq!(super::backoff(u32::MAX), Duration::from_millis(800));
}

/// K-19: 엔진 panic 복구에 쓰는 크기는 마지막 리사이즈 크기다.
/// 시작 크기로 되돌리면 화면 격자가 PTY 크기와 다음 resize까지 어긋난다.
#[test]
fn live_size_tracks_resize_for_panic_recovery() {
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let session = Session::start(
        SessionId(613),
        pty.controller,
        WinSize { cols: 80, rows: 24 },
        Arc::new(Appearance::load(PathBuf::new())),
        None,
    )
    .unwrap();
    let tap = session.tap();
    assert_eq!(lock(&tap.0.live).size, WinSize { cols: 80, rows: 24 });
    tap.resize(WinSize {
        cols: 100,
        rows: 40,
    })
    .unwrap();
    assert_eq!(
        lock(&tap.0.live).size,
        WinSize {
            cols: 100,
            rows: 40
        }
    );
    // 거대 크기는 잘린 값으로 기록된다(K-01). 복구 엔진도 잘린 격자다.
    tap.resize(WinSize {
        cols: u16::MAX,
        rows: u16::MAX,
    })
    .unwrap();
    assert_eq!(
        lock(&tap.0.live).size,
        WinSize {
            cols: krill_proto::screen::MAX_COLS,
            rows: krill_proto::screen::MAX_LINES,
        }
    );
    drop(pty.user);
    session.wait().unwrap();
}
