use std::path::PathBuf;

use super::super::*;

#[expect(
    clippy::unwrap_used,
    reason = "실패하면 중단하는 테스트 PTY 초기화 헬퍼"
)]
fn fixture() -> (OwnedFd, Tap) {
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let mut mode = rustix::termios::tcgetattr(&pty.user).unwrap();
    mode.make_raw();
    rustix::termios::tcsetattr(&pty.user, rustix::termios::OptionalActions::Now, &mode).unwrap();
    pty::nonblocking(&pty.user).unwrap();
    pty::nonblocking(&pty.controller).unwrap();
    let size = WinSize { cols: 80, rows: 24 };
    let shared = Shared {
        capture: Mutex::new(None),
        history_access: Mutex::new(()),
        id: SessionId(701),
        clients: Mutex::default(),
        input: Mutex::default(),
        live: Mutex::new(Live {
            engine: Engine::new(size),
            revision: 0,
            history_epoch: 0,
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
        gaps: Mutex::default(),
        master: pty.controller,
        look: Arc::new(Appearance::load(PathBuf::new())),
        out: Mutex::new(Out {
            ring: VecDeque::new(),
            end: 0,
            revision: 0,
            closed: false,
            generation: 0,
        }),
        changed: Condvar::new(),
        store: None,
    };
    (pty.user, Tap(Arc::new(shared)))
}

#[test]
fn poisoned_engine_is_replaced_without_resetting_session_bookkeeping() {
    for parked in [false, true] {
        let (slave, tap) = fixture();
        tap.0.publish(b"0123456789012345678", 41, false);
        assert!(
            panic::catch_unwind(AssertUnwindSafe(|| {
                let mut live = tap.0.live.lock().unwrap();
                live.size = WinSize {
                    cols: 100,
                    rows: 40,
                };
                live.seq = 19;
                live.revision = 41;
                live.viewers = 2;
                live.epoch = 7;
                live.parked = parked;
                live.done_ns = 1;
                live.input_since_done = false;
                live.engine
                    .feed(b"partial\x1b[?2004h", Feed::Live, &tap.0.look);
                panic!("engine lock poison injection");
            }))
            .is_err()
        );
        assert!(tap.0.live.is_poisoned());
        {
            let live = tap.0.live();
            assert_eq!(
                (live.seq, live.revision, live.viewers, live.epoch),
                (19, 42, 2, 7)
            );
            assert_eq!(live.parked, parked);
            assert!(live.input_since_done);
            assert!(live.last_input_ns >= live.done_ns);
            assert_eq!(live.resized, Some(live.size));
            assert!(!live.engine.screen_text().contains("partial"));
            assert_eq!(live.engine.screen().lines, 40);
        }
        assert!(!tap.0.live.is_poisoned());
        assert_eq!(tap.0.live().revision, 42, "복구는 한 번만 수행한다");
        assert_eq!(tap.pulse().end, 19);
        assert_eq!(tap.pulse().revision, 42);
        assert_eq!(
            tap.read(0, 19),
            Read::Bytes(b"0123456789012345678".to_vec())
        );
        let (ok, rev, _) = guarded(&tap.0, 8, |engine| {
            engine.feed(b"after\x1b[c", Feed::Live, &tap.0.look)
        });
        assert!(ok);
        tap.0.publish(b"after\x1b[c", rev, false);
        assert!(
            tap.capture()
                .0
                .rows
                .iter()
                .any(|row| row.iter().any(|c| c.ch == 'a'))
        );
        tap.capture_cli(krill_proto::control::CaptureFormat::Plain)
            .unwrap();
        tap.write_input(b"INPUT", None).unwrap();
        let mut bytes = [0; 128];
        let count = read(&slave, &mut bytes).unwrap();
        let received = bytes.get(..count).unwrap();
        assert!(received.ends_with(b"INPUT"));
        assert!(received.starts_with(b"\x1b["), "질의 응답도 계속된다");
    }
}

#[test]
fn resize_panic_keeps_os_size_recording_and_clients_consistent() {
    let (_slave, tap) = fixture();
    let clients = tap.clients().lock().unwrap();
    let size = WinSize {
        cols: 100,
        rows: 40,
    };
    let error = tap
        .resize_with(size, |engine, size| {
            engine.resize(size);
            panic!("resize failure after OS resize");
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    drop(clients);
    assert!(!tap.clients().is_poisoned());
    assert!(!tap.0.live.is_poisoned());
    assert_eq!(pty::size(&tap.0.master).unwrap(), size);
    let live = tap.0.live();
    assert_eq!(live.size, size);
    assert_eq!(live.resized, Some(size));
    assert_eq!(live.engine.screen().lines, size.rows);
    assert_eq!(live.engine.screen().cols, size.cols);
    assert_eq!(live.seq, 0);
    let revision = live.revision;
    drop(live);
    assert_eq!(tap.pulse().revision, revision);
    assert_eq!(tap.pulse().end, 0);
    tap.resize(WinSize { cols: 90, rows: 30 }).unwrap();
    assert_eq!(tap.0.live().size, WinSize { cols: 90, rows: 30 });
}

#[test]
fn failed_engine_mutations_replace_hot_and_parked_engines() {
    let (_slave, tap) = fixture();
    for parked in [false, true] {
        // demote·promote·clear_history의 변경 경계에 부분 변경 뒤 panic을 주입한다.
        for operation in 0..3 {
            let mut live = tap.0.live();
            live.parked = parked;
            let revision = live.revision;
            assert!(
                live.update_engine(|engine| {
                    engine.feed(b"partial\r\nrow\r\n", Feed::Live, &tap.0.look);
                    match operation {
                        0 => engine.demote(&tap.0.look),
                        1 => engine.promote(&tap.0.look),
                        _ => engine.clear_history(),
                    }
                    panic!("partial engine mutation");
                })
                .is_none()
            );
            assert_eq!(live.parked, parked);
            assert!(live.revision > revision);
            assert!(!live.engine.screen_text().contains("partial"));
            assert_eq!(live.engine.history_len(), 0);
            assert!(
                live.update_engine(|engine| engine.feed(b"next", Feed::Live, &tap.0.look))
                    .is_some()
            );
            assert!(live.engine.screen_text().contains("next"));
        }
    }
}

#[test]
fn poisoned_input_refuses_writes_and_cleanup_without_closing_pty() {
    let (slave, tap) = fixture();
    tap.write_input(b"\x1b[200~body", None).unwrap();
    let mut bytes = [0; 64];
    let count = read(&slave, &mut bytes).unwrap();
    assert_eq!(bytes.get(..count).unwrap(), b"\x1b[200~body");
    assert!(
        panic::catch_unwind(AssertUnwindSafe(|| {
            let _held = tap.0.input.lock().unwrap();
            panic!("paste state poison injection");
        }))
        .is_err()
    );
    tap.finish_input();
    assert_eq!(
        tap.write_input(b"UNSAFE", None).unwrap_err().kind(),
        io::ErrorKind::Other
    );
    assert_eq!(read(&slave, &mut bytes), Err(Errno::AGAIN));
    assert!(
        tap.0.input.is_poisoned(),
        "불명확한 입력 상태는 재사용하지 않는다"
    );
    assert!(!tap.pulse().closed);
    write(&slave, b"OUTPUT").unwrap();
    let count = read(&tap.0.master, &mut bytes).unwrap();
    assert_eq!(bytes.get(..count).unwrap(), b"OUTPUT");
}

#[test]
fn poisoned_roles_deny_mutations_and_hibernation_but_keep_output() {
    use crate::attach::roles::Client;
    use krill_proto::roles::{Ack, Request, Role};

    let (slave, tap) = fixture();
    let client = Client::new(tap.clone(), Role::Interactive).unwrap();
    let (peer, _other) = krill_proto::os::net::UnixStream::pair().unwrap();
    assert!(
        panic::catch_unwind(AssertUnwindSafe(|| {
            let _held = tap.clients().lock().unwrap();
            panic!("client state poison injection");
        }))
        .is_err()
    );
    assert_eq!(client.request(Request::Focus), Ack::Closed);
    assert!(Client::new(tap.clone(), Role::Interactive).is_err());
    assert!(tap.write_input(b"DENIED", Some(peer.as_fd())).is_err());
    assert!(tap.hibernation_guard(0, Duration::ZERO).is_none());
    drop(client);
    let mut bytes = [0; 16];
    assert_eq!(read(&slave, &mut bytes), Err(Errno::AGAIN));
    assert!(!tap.pulse().closed);
    write(&slave, b"OUTPUT").unwrap();
    let count = read(&tap.0.master, &mut bytes).unwrap();
    assert_eq!(bytes.get(..count).unwrap(), b"OUTPUT");
}

#[test]
fn viewers_hold_independent_subscriptions_until_each_is_dropped() {
    let (_slave, tap) = fixture();
    let first = tap.watch();
    let second = tap.watch();
    assert_eq!(tap.0.live().viewers, 2);
    drop(first);
    assert_eq!(tap.0.live().viewers, 1);
    drop(second);
    assert_eq!(tap.0.live().viewers, 0);
    assert_eq!(tap.0.live().epoch, 1);
}

#[test]
fn wait_predicate_can_capture_a_poisoned_engine_without_reentering_out() {
    let (_slave, tap) = fixture();
    assert!(
        panic::catch_unwind(AssertUnwindSafe(|| {
            let _held = tap.0.live.lock().unwrap();
            panic!("poison before wait predicate");
        }))
        .is_err()
    );
    let (send, receive) = std::sync::mpsc::channel();
    let waiter = thread::spawn(move || {
        let pulse = tap.wait(|_| {
            tap.capture();
            true
        });
        send.send((pulse, tap.pulse())).unwrap();
    });
    let (_, recovered) = receive.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(recovered.revision, 1);
    assert_eq!(recovered.end, 0);
    waiter.join().unwrap();
}

#[test]
fn wait_does_not_lose_wake_between_predicate_and_condvar() {
    use std::sync::mpsc;

    let (_slave, tap) = fixture();
    let ready = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&ready);
    let waiting = tap.clone();
    let (entered, entering) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let waiter = thread::spawn(move || {
        waiting.wait(|_| {
            let result = observed.load(Ordering::SeqCst);
            if !result {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            result
        });
        done.send(()).unwrap();
    });
    entering.recv_timeout(Duration::from_secs(2)).unwrap();
    ready.store(true, Ordering::SeqCst);
    tap.wake(); // 조건을 본 뒤 Condvar에 들어가기 전에 의도적으로 알림을 보낸다.
    release.send(()).unwrap();
    completed.recv_timeout(Duration::from_secs(2)).unwrap();
    waiter.join().unwrap();
}

#[test]
fn wait_predicate_panic_does_not_poison_output_or_stop_publication() {
    let (_slave, tap) = fixture();
    assert!(
        panic::catch_unwind(AssertUnwindSafe(|| {
            tap.wait(|_| panic!("predicate failure"));
        }))
        .is_err()
    );
    assert!(!tap.0.out.is_poisoned());
    tap.0.publish(b"next", 1, false);
    assert_eq!(tap.wait(|p| p.end == 4).revision, 1);
    assert_eq!(tap.read(0, 4), Read::Bytes(b"next".to_vec()));
}

#[test]
fn park_watch_and_clear_failures_publish_recovery_and_wake_waiters() {
    for operation in 0..3 {
        let (_slave, tap) = fixture();
        if operation == 1 {
            tap.0.live().parked = true;
        }
        let waiting = tap.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let waiter = thread::spawn(move || {
            send.send(waiting.wait(|p| p.revision > 0)).unwrap();
        });
        match operation {
            0 => tap.0.park_with(0, |_| panic!("demote failure")),
            1 => {
                let viewer = tap.watch_with(|_| panic!("promote failure"));
                assert_eq!(tap.0.live().viewers, 1);
                assert!(tap.is_parked());
                drop(viewer);
            }
            _ => {
                assert_eq!(tap.clear_scrollback_with(|_| panic!("clear failure")), 0);
            }
        }
        let pulse = receive.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(pulse.end, 0);
        assert!(!pulse.closed);
        assert!(!tap.0.live.is_poisoned());
        waiter.join().unwrap();
    }
}

#[test]
fn poisoned_session_input_keeps_both_session_readers_and_other_input_running() {
    let mut sessions = Vec::new();
    for id in 0..2 {
        let pty = rustix_openpty::openpty(None, None).unwrap();
        let mut mode = rustix::termios::tcgetattr(&pty.user).unwrap();
        mode.make_raw();
        rustix::termios::tcsetattr(&pty.user, rustix::termios::OptionalActions::Now, &mode)
            .unwrap();
        pty::nonblocking(&pty.user).unwrap();
        let session = Session::start(
            SessionId(710 + id),
            pty.controller,
            WinSize { cols: 80, rows: 24 },
            Arc::new(Appearance::load(PathBuf::new())),
            None,
        )
        .unwrap();
        sessions.push((pty.user, session));
    }
    let broken = sessions[0].1.tap();
    assert!(
        panic::catch_unwind(AssertUnwindSafe(|| {
            let _held = broken.0.input.lock().unwrap();
            panic!("input failure with active readers");
        }))
        .is_err()
    );
    assert!(broken.write_input(b"DENIED", None).is_err());
    for (slave, session) in &sessions {
        write(slave, b"OUTPUT").unwrap();
        let tap = session.tap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while tap.pulse().end < 6 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(tap.read(0, 6), Read::Bytes(b"OUTPUT".to_vec()));
        assert!(session.screen_text().contains("OUTPUT"));
        assert!(!session.is_closed());
    }
    let (other_slave, other) = sessions.get(1).unwrap();
    other.tap().write_input(b"OTHER INPUT", None).unwrap();
    let mut bytes = [0; 32];
    let count = read(other_slave, &mut bytes).unwrap();
    assert_eq!(bytes.get(..count).unwrap(), b"OTHER INPUT");
    for (slave, session) in sessions {
        drop(slave);
        session.wait().unwrap();
    }
}
