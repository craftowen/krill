//! MK-050: 앱 + CLI interactive + observer 둘의 실제 PTY·UDS 계약.

#![cfg(unix)]
#![expect(clippy::unwrap_used, reason = "통합 테스트 헬퍼")]

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::net::Shutdown;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::control::{Create, Failure, Program, Reply, Request};
use krill_proto::daemon::client_hello;
use krill_proto::holder::WinSize;
use krill_proto::input::{self, Input, InputAck, InputId, InputResult, MAX_INPUT};
use krill_proto::ipc::{self, ClientRole, FEATURES, PROTO_MAX, PROTO_MIN, ServerHello};
use krill_proto::roles::{self, Ack, FEAT_ROLES, Role};
use krill_proto::screen::{self, Attach, Msg, Refused, Seq, Stream, Update, View};
use krilld::attach::{self, Tap};
use krilld::registry::Registry;
use krilld::{Appearance, Session};

const WAIT: Duration = Duration::from_secs(10);

fn hello() -> ServerHello {
    ServerHello {
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        features: FEATURES,
        build: "mk050-test".to_owned(),
        instance_id: 1,
    }
}

/// 연결을 닫고 서버가 장부에서 제거할 때까지 기다린다. 테스트 실패 때도 스레드를 남기지 않는다.
/// 필드 순서가 계약이다: `socket`을 먼저 닫아야(drop) `_worker` join이 끝난다. macOS는 `shutdown`만으로는 상대의
/// 막힌 read를 드물게 깨우지 못한다(OWE-530). shutdown은 그대로 먼저 해 서버가 보통 때처럼 EOF를 보게 한다.
struct Wire {
    socket: UnixStream,
    pending: VecDeque<(u8, Vec<u8>)>,
    _worker: Joined,
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.socket.shutdown(Shutdown::Both).ok();
    }
}

/// drop 때 서버 스레드를 기다린다.
struct Joined(Option<JoinHandle<()>>);

impl Drop for Joined {
    fn drop(&mut self) {
        if let Some(worker) = self.0.take() {
            worker.join().unwrap();
        }
    }
}

impl Wire {
    fn new(app: ClientRole, serve: impl FnOnce(UnixStream) + Send + 'static) -> Self {
        let (mut socket, server) = UnixStream::pair().unwrap();
        socket.set_read_timeout(Some(WAIT)).unwrap();
        socket.set_write_timeout(Some(WAIT)).unwrap();
        let worker = thread::spawn(move || serve(server));
        let (kind, body) = ipc::read_frame(&mut socket).unwrap().unwrap();
        ServerHello::decode(kind, &body).unwrap();
        socket
            .write_all(&client_hello(app, "roles-test").encode().unwrap())
            .unwrap();
        Self {
            socket,
            pending: VecDeque::new(),
            _worker: Joined(Some(worker)),
        }
    }

    fn frame(&mut self, wanted: u8) -> Vec<u8> {
        if let Some(at) = self.pending.iter().position(|(kind, _)| *kind == wanted) {
            return self.pending.remove(at).unwrap().1;
        }
        loop {
            let (kind, body) = ipc::read_frame(&mut self.socket).unwrap().unwrap();
            if kind == wanted {
                return body;
            }
            // 역할/입력 응답 사이에 오는 초기 키프레임·델타도 순서대로 보관한다.
            self.pending.push_back((kind, body));
        }
    }

    fn role(&mut self, request: roles::Request) -> Ack {
        self.socket.write_all(&request.encode().unwrap()).unwrap();
        Ack::decode(roles::kind::ROLE_ACK, &self.frame(roles::kind::ROLE_ACK)).unwrap()
    }

    fn input(&mut self, id: u64, bytes: Vec<u8>) {
        self.socket
            .write_all(
                &Input {
                    id: InputId(id),
                    bytes,
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
    }

    fn applied(&mut self, id: u64, bytes: &[u8]) {
        self.input(id, bytes.to_vec());
        let ack =
            InputAck::decode(input::kind::INPUT_ACK, &self.frame(input::kind::INPUT_ACK)).unwrap();
        assert_eq!(
            ack,
            InputAck {
                id: InputId(id),
                result: InputResult::Applied
            }
        );
    }

    fn control(&mut self, request: &Request) -> Reply {
        self.socket.write_all(&request.encode().unwrap()).unwrap();
        Reply::decode(
            krill_proto::control::kind::REPLY,
            &self.frame(krill_proto::control::kind::REPLY),
        )
        .unwrap()
    }
}

struct Pty {
    child: Child,
    session: Session,
    master: OwnedFd,
}

impl Pty {
    fn new(script: &str) -> Self {
        let size = WinSize { cols: 80, rows: 24 };
        let pty = rustix_openpty::openpty(
            None,
            Some(&rustix::termios::Winsize {
                ws_row: size.rows,
                ws_col: size.cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
        )
        .unwrap();
        let child = Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::from(pty.user.try_clone().unwrap()))
            .stdout(Stdio::from(pty.user.try_clone().unwrap()))
            .stderr(Stdio::from(pty.user))
            .spawn()
            .unwrap();
        let master = pty.controller.try_clone().unwrap();
        let appearance = Arc::new(Appearance::load(PathBuf::from("/nonexistent/mk050/theme")));
        let session =
            Session::start(SessionId(50), pty.controller, size, appearance, None).unwrap();
        let pty = Self {
            child,
            session,
            master,
        };
        until(|| pty.session.screen_text().contains("READY"));
        pty
    }

    fn connect(&self, app: ClientRole, role: Role, stream: Stream) -> Wire {
        let tap = self.session.tap();
        let mut wire = Wire::new(app, move |s| {
            attach::serve(s, &hello(), |_| Some(tap)).ok();
        });
        assert_eq!(wire.role(roles::Request::Set(role)), Ack::Applied);
        wire.socket
            .write_all(
                &Attach {
                    session: SessionId(50),
                    stream,
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        wire.frame(screen::kind::ATTACHED);
        wire
    }

    fn size(&self, cols: u16, rows: u16) {
        let ws = rustix::termios::tcgetwinsize(&self.master).unwrap();
        assert_eq!((ws.ws_col, ws.ws_row), (cols, rows), "kernel PTY");
        let (screen, _, _) = self.session.tap().capture();
        assert_eq!((screen.cols, screen.lines), (cols, rows), "emulator");
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out");
        thread::sleep(Duration::from_millis(2));
    }
}

fn viewport(wire: &mut Wire, cols: u16, rows: u16) {
    assert_eq!(
        wire.role(roles::Request::Resize { cols, rows }),
        Ack::Applied
    );
}

#[test]
fn app_cli_and_two_observers_follow_latest_activity() {
    let pty = Pty::new(
        "stty -echo; printf 'READY\\n'; while IFS= read -r line; do printf '%s:' \"$line\"; stty size; done",
    );
    let mut app = pty.connect(
        ClientRole::App,
        Role::Interactive,
        Stream::Screen { known: None },
    );
    let mut cli = pty.connect(
        ClientRole::Cli,
        Role::Interactive,
        Stream::Raw { from: Seq(0) },
    );
    let mut raw_observer = pty.connect(
        ClientRole::Cli,
        Role::Observer,
        Stream::Raw { from: Seq(0) },
    );
    let mut screen_observer = pty.connect(
        ClientRole::App,
        Role::Observer,
        Stream::Screen { known: None },
    );

    viewport(&mut app, 100, 30);
    viewport(&mut cli, 120, 40);
    pty.size(80, 24); // 새 뷰포트만으로 포커스를 빼앗지 않는다.
    assert_eq!(app.role(roles::Request::Focus), Ack::Applied);
    pty.size(100, 30);
    viewport(&mut cli, 110, 35);
    pty.size(100, 30);
    viewport(&mut app, 105, 32); // 현재 소유자의 리사이즈는 바로 반영한다.
    pty.size(105, 32);
    assert_eq!(
        app.role(roles::Request::Resize { cols: 0, rows: 10 }),
        Ack::Invalid
    );
    assert_eq!(
        app.role(roles::Request::Resize {
            cols: u16::MAX,
            rows: 10
        }),
        Ack::Invalid
    );
    pty.size(105, 32);

    for observer in [&mut raw_observer, &mut screen_observer] {
        assert_eq!(observer.role(roles::Request::Focus), Ack::Denied);
        assert_eq!(
            observer.role(roles::Request::Resize {
                cols: 200,
                rows: 50
            }),
            Ack::Denied
        );
        observer.input(1, b"FORBIDDEN\n".to_vec());
        assert_eq!(
            InputAck::decode(
                input::kind::INPUT_ACK,
                &observer.frame(input::kind::INPUT_ACK)
            )
            .unwrap(),
            InputAck {
                id: input::InputId(1),
                result: InputResult::Denied
            }
        );
    }
    pty.size(105, 32);
    cli.applied(1, b"cli-first\n");
    pty.size(110, 35);
    until(|| pty.session.screen_text().contains("cli-first:35 110"));
    app.applied(1, b"app-second\n");
    pty.size(105, 32);
    until(|| pty.session.screen_text().contains("app-second:32 105"));

    // 둘 다 같은 실제 출력·최신 크기를 받으며 observer 입력은 PTY에 도달하지 않았다.
    let mut raw = Vec::new();
    while !String::from_utf8_lossy(&raw).contains("app-second:32 105") {
        let body = raw_observer.frame(screen::kind::OUTPUT);
        raw.extend_from_slice(
            screen::decode_output(screen::kind::OUTPUT, &body)
                .unwrap()
                .1,
        );
    }
    let text = String::from_utf8(raw).unwrap();
    assert!(!text.contains("FORBIDDEN"));
    assert!(text.find("cli-first").unwrap() < text.find("app-second").unwrap());
    let mut view = View::default();
    loop {
        let body = screen_observer.frame(screen::kind::UPDATE);
        let update = Update::decode(screen::kind::UPDATE, &body).unwrap();
        view.apply(update).unwrap();
        let text: String = view
            .screen
            .rows
            .iter()
            .flatten()
            .map(|cell| cell.ch)
            .collect();
        if text.contains("app-second:32 105") {
            assert_eq!((view.screen.cols, view.screen.lines), (105, 32));
            assert!(!text.contains("FORBIDDEN"));
            break;
        }
    }

    assert_eq!(app.role(roles::Request::Set(Role::Observer)), Ack::Applied);
    pty.size(110, 35); // 강등하면 이전 활동자로 돌아간다.
    assert_eq!(
        app.role(roles::Request::Set(Role::Interactive)),
        Ack::Applied
    );
    pty.size(110, 35); // 승격 자체는 포커스가 아니다.
    assert_eq!(app.role(roles::Request::Focus), Ack::Applied);
    pty.size(105, 32);
    drop(app);
    pty.size(110, 35); // 연결 종료도 이전 활동자로 돌아간다.
    drop(cli);
    pty.size(110, 35); // observer만 남으면 마지막 크기를 유지한다.
    assert!(pty.child.id() > 0);
    assert!(!pty.session.is_closed());
}

/// OWE-530: macOS에서 socketpair 한쪽의 `shutdown`이 상대 스레드의 막힌 read를 드물게 깨우지 못한다(이 맥 C 재현:
/// 72,000회 중 14회, 6초 더 기다려도 안 깨어남. `close`는 48,000회 중 0회). [`Wire`] 정리가 shutdown에만 기대면
/// 서버 스레드 join에서 영영 멈춘다(check.sh 180초 타임아웃의 원인). 고치기 전에는 5회 중 4회 실패했다.
#[test]
fn wire_drop_ends_server_even_if_shutdown_wakeup_is_lost() {
    let (done, finished) = std::sync::mpsc::channel();
    for _ in 0..8 {
        let done = done.clone();
        thread::spawn(move || {
            for _ in 0..2_500 {
                drop(Wire::new(ClientRole::Cli, |mut s| {
                    krilld::ipc::handshake(&mut s, &hello()).ok();
                    while let Ok(Some(_)) = ipc::read_frame(&mut s) {}
                }));
            }
            done.send(()).ok();
        });
    }
    for _ in 0..8 {
        finished
            .recv_timeout(Duration::from_secs(60)) // 부하 없이 1초 안쪽
            .expect("Wire drop hung on server join");
    }
}

#[test]
fn simultaneous_clients_cannot_interleave_partial_input_frames() {
    let path = PathBuf::from(format!("/tmp/mk050-input-{}", std::process::id()));
    let pty = Pty::new(&format!(
        "stty raw -echo; printf READY; exec /bin/cat > '{}'",
        path.display()
    ));
    let app = pty.connect(
        ClientRole::App,
        Role::Interactive,
        Stream::Screen { known: None },
    );
    let cli = pty.connect(
        ClientRole::Cli,
        Role::Interactive,
        Stream::Raw { from: Seq(0) },
    );
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = [(b'A', app), (b'B', cli)]
        .into_iter()
        .map(|(label, mut wire)| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for id in 1..=4u8 {
                    let mut bytes = vec![label; MAX_INPUT];
                    bytes[1] = b'0' + id;
                    *bytes.last_mut().unwrap() = b'\n';
                    wire.applied(u64::from(id), &bytes);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    until(|| fs::metadata(&path).is_ok_and(|m| m.len() == (MAX_INPUT * 8) as u64));
    let bytes = fs::read(&path).unwrap();
    let mut next = *b"11";
    let (frames, tail) = bytes.as_chunks::<MAX_INPUT>();
    assert!(tail.is_empty());
    for frame in frames {
        let label = frame[0];
        let slot = match label {
            b'A' => 0,
            b'B' => 1,
            _ => panic!("interleaved frame"),
        };
        assert_eq!(frame[1], next[slot], "per-client order");
        next[slot] += 1;
        assert!(
            frame[2..MAX_INPUT - 1].iter().all(|&byte| byte == label),
            "mixed input frames"
        );
        assert_eq!(frame[MAX_INPUT - 1], b'\n');
    }
    assert_eq!(next, *b"55");
    fs::remove_file(path).unwrap();
}

#[test]
fn client_without_viewport_keeps_its_initial_dimensions() {
    let pty = Pty::new("printf 'READY\\n'; exec /bin/cat");
    let mut app = pty.connect(
        ClientRole::App,
        Role::Interactive,
        Stream::Screen { known: None },
    );
    viewport(&mut app, 100, 30);
    assert_eq!(app.role(roles::Request::Focus), Ack::Applied);
    let mut cli = pty.connect(
        ClientRole::Cli,
        Role::Interactive,
        Stream::Raw { from: Seq(0) },
    );
    viewport(&mut app, 120, 40);
    pty.size(120, 40);
    cli.applied(1, b"input\n");
    pty.size(100, 30);
}

#[test]
fn registry_observer_can_read_but_cannot_mutate() {
    let mut wire = Wire::new(ClientRole::Cli, |s| {
        Registry::new(None, None).serve(s, &hello()).unwrap();
    });
    assert_eq!(wire.role(roles::Request::Set(Role::Observer)), Ack::Applied);
    assert_eq!(wire.control(&Request::List), Reply::Sessions(Vec::new()));
    for request in [
        Request::Close {
            session: SessionId(50),
        },
        Request::Resize {
            session: SessionId(50),
            cols: 120,
            rows: 40,
        },
        Request::Create(Create {
            account: None,
            program: Program::Login,
            cwd: Vec::new(),
            cols: 80,
            rows: 24,
        }),
    ] {
        assert_eq!(wire.control(&request), Reply::Failed(Failure::Denied));
    }
    assert_eq!(
        wire.role(roles::Request::Set(Role::Interactive)),
        Ack::Applied
    );
    assert_eq!(
        wire.control(&Request::Close {
            session: SessionId(50)
        }),
        Reply::Failed(Failure::NoHolder)
    );
}

#[test]
fn role_message_requires_negotiated_feature() {
    let pty = Pty::new("printf 'READY\\n'; exec /bin/cat");
    let tap: Tap = pty.session.tap();
    let (mut client, server) = UnixStream::pair().unwrap();
    client.set_read_timeout(Some(WAIT)).unwrap();
    let worker = thread::spawn(move || attach::serve(server, &hello(), |_| Some(tap)).ok());
    ipc::read_frame(&mut client).unwrap().unwrap();
    let mut h = client_hello(ClientRole::Cli, "legacy");
    h.features &= !FEAT_ROLES;
    client.write_all(&h.encode().unwrap()).unwrap();
    client
        .write_all(
            &Attach {
                session: SessionId(50),
                stream: Stream::Screen { known: None },
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    client
        .write_all(&roles::Request::Set(Role::Observer).encode().unwrap())
        .unwrap();
    loop {
        let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        assert_ne!(kind, roles::kind::ROLE_ACK);
        if kind == screen::kind::REFUSED {
            assert_eq!(Refused::decode(kind, &body).unwrap(), Refused::BadRequest);
            break;
        }
    }
    client.shutdown(Shutdown::Both).unwrap();
    drop(client); // shutdown만으로는 서버 read가 드물게 안 깨어난다(OWE-530).
    worker.join().unwrap();
}

#[test]
fn creator_session_can_wait_read_and_send_to_created_child_while_unrelated_session_is_denied() {
    use std::os::fd::AsFd;

    use krill_holder::link::Link;
    use krill_proto::control::CaptureFormat;
    use krill_proto::holder::{Frame, FromHolder, ToHolder, VERSION};
    use krilld::holder::{Holder, Kept};

    let dir = std::env::temp_dir().join(format!("mk060-roles-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();

    let (krilld_end, mut holder_end) = UnixStream::pair().unwrap();
    for m in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
            inbox: false,
        },
        FromHolder::Ready,
    ] {
        holder_end.write_all(&m.encode().unwrap()).unwrap();
    }
    let (holder, link, _, _) = Holder::connect(OwnedFd::from(krilld_end)).unwrap();
    let (master_a, _pty_a) = UnixStream::pair().unwrap();
    let (master_c, _pty_c) = UnixStream::pair().unwrap();
    let session_a = SessionId(1);
    let session_c = SessionId(3);
    // 이 테스트 프로세스 PID를 세션 A 루트로 등록해 연결 피어가 Scope::Session(A)로 판정되게 한다.
    let kept = vec![
        Kept {
            id: session_a,
            pid: std::process::id(),
            status: None,
            master: OwnedFd::from(master_a),
        },
        Kept {
            id: session_c,
            pid: 1,
            status: None,
            master: OwnedFd::from(master_c),
        },
    ];
    let reg = Arc::new(Registry::new(Some((holder, kept)), Some(&dir)));
    let reg_holder = Arc::clone(&reg);
    let reader_thread = thread::spawn(move || reg_holder.run_holder(link));

    let holder_worker = thread::spawn(move || {
        let mut link = Link::new(OwnedFd::from(holder_end));
        let p = link.recv().unwrap().unwrap();
        let ToHolder::Spawn { req, id, size, .. } = ToHolder::decode(Frame {
            kind: p.kind,
            body: &p.body,
            fds: u8::try_from(p.fds.len()).unwrap(),
        })
        .unwrap() else {
            panic!("expected Spawn");
        };
        let pty = rustix_openpty::openpty(
            None,
            Some(&rustix::termios::Winsize {
                ws_row: size.rows,
                ws_col: size.cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
        )
        .unwrap();
        let child = Command::new("/bin/sh")
            .args([
                "-c",
                "stty -echo; printf 'CHILD_READY\\n'; IFS= read -r line; printf 'CHILD_GOT:%s\\n' \"$line\"",
            ])
            .stdin(Stdio::from(pty.user.try_clone().unwrap()))
            .stdout(Stdio::from(pty.user.try_clone().unwrap()))
            .stderr(Stdio::from(pty.user))
            .spawn()
            .unwrap();
        let reply = FromHolder::Spawned {
            req,
            id,
            pid: child.id(),
        }
        .encode()
        .unwrap();
        link.send(&reply, &[pty.controller.as_fd()]).unwrap();
        child
    });

    let reg_ctrl = Arc::clone(&reg);
    let mut ctrl = Wire::new(ClientRole::Cli, move |s| {
        reg_ctrl.serve(s, &hello()).unwrap();
    });
    let Reply::Created {
        session: session_b, ..
    } = ctrl.control(&Request::Create(Create {
        account: None,
        program: Program::Custom {
            program: b"/bin/sh".to_vec(),
            args: Vec::new(),
        },
        cwd: Vec::new(),
        cols: 80,
        rows: 24,
    }))
    else {
        panic!("create child session from session A failed");
    };

    // 세션 A 안에서 자기가 만든 B에 wait·read·send가 모두 허용된다(새 연결과 기존 연결 모두).
    let reg_wait = Arc::clone(&reg);
    let mut wait_conn = Wire::new(ClientRole::Cli, move |s| {
        reg_wait.serve(s, &hello()).unwrap();
    });
    assert!(matches!(
        wait_conn.control(&Request::WaitStatus { session: session_b }),
        Reply::WaitStatus { .. }
    ));
    until(|| {
        matches!(
            wait_conn.control(&Request::Capture {
                session: session_b,
                format: CaptureFormat::Plain,
            }),
            Reply::Captured { bytes, .. } if String::from_utf8_lossy(&bytes).contains("CHILD_READY")
        )
    });

    let reg_send = Arc::clone(&reg);
    let mut send_conn = Wire::new(ClientRole::Cli, move |s| {
        reg_send.serve(s, &hello()).unwrap();
    });
    send_conn
        .socket
        .write_all(
            &Attach {
                session: session_b,
                stream: Stream::Raw { from: Seq(0) },
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    send_conn.frame(screen::kind::ATTACHED);
    send_conn.applied(1, b"from-parent-a\n");
    drop(send_conn);

    until(|| {
        matches!(
            ctrl.control(&Request::Capture {
                session: session_b,
                format: CaptureFormat::Plain,
            }),
            Reply::Captured { bytes, .. }
                if String::from_utf8_lossy(&bytes).contains("CHILD_GOT:from-parent-a")
        )
    });

    // 무관한 세션 C는 계속 거부되고 감사 로그에 남는다(MK-036).
    assert_eq!(
        ctrl.control(&Request::WaitStatus { session: session_c }),
        Reply::Failed(Failure::NoSession)
    );
    assert_eq!(
        ctrl.control(&Request::Capture {
            session: session_c,
            format: CaptureFormat::Plain,
        }),
        Reply::Failed(Failure::NoSession)
    );
    assert_eq!(
        ctrl.control(&Request::Close { session: session_c }),
        Reply::Failed(Failure::Denied)
    );
    let Reply::Sessions(listed) = ctrl.control(&Request::List) else {
        panic!("list");
    };
    let ids: Vec<_> = listed.into_iter().map(|i| i.session).collect();
    assert_eq!(ids, vec![session_a, session_b]);

    let audit = fs::read_to_string(dir.join("audit.log")).unwrap();
    assert!(
        audit.contains("deny op=attach target=3") && audit.contains("scope=session:1"),
        "{audit}"
    );
    assert!(
        audit.contains("deny op=close target=3") && audit.contains("scope=session:1"),
        "{audit}"
    );

    drop(wait_conn);
    drop(ctrl);
    let mut child = holder_worker.join().unwrap();
    reader_thread.join().unwrap();
    child.kill().ok();
    child.wait().ok();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn mobile_input_preserves_desktop_size_until_explicit_resize() {
    let pty = Pty::new("stty -echo; printf 'READY\\n'; cat");
    let mut desktop = pty.connect(
        ClientRole::App,
        Role::Interactive,
        Stream::Screen { known: None },
    );
    viewport(&mut desktop, 120, 40);
    assert_eq!(desktop.role(roles::Request::Focus), Ack::Applied);
    let mut mobile = pty.connect(
        ClientRole::App,
        Role::InteractiveWithSize { owns_size: false },
        Stream::Screen { known: None },
    );
    assert_eq!(mobile.role(roles::Request::Focus), Ack::Applied);
    mobile.applied(1, b"approve\n");
    pty.size(120, 40);
    assert_eq!(desktop.role(roles::Request::Focus), Ack::Applied);
    viewport(&mut mobile, 40, 20);
    pty.size(40, 20);
    assert_eq!(
        mobile.role(roles::Request::Set(Role::InteractiveWithSize {
            owns_size: false
        })),
        Ack::Applied
    );
    pty.size(120, 40);
    viewport(&mut mobile, 45, 22);
    pty.size(45, 22);
    drop(mobile);
    pty.size(120, 40);
}

#[test]
fn fit_phone_without_desktop_restores_original_size_on_release_and_disconnect() {
    let pty = Pty::new("stty -echo; printf 'READY\\n'; cat");
    let mut mobile = pty.connect(
        ClientRole::App,
        Role::InteractiveWithSize { owns_size: false },
        Stream::Screen { known: None },
    );
    viewport(&mut mobile, 42, 24);
    pty.size(42, 24);
    assert_eq!(
        mobile.role(roles::Request::Set(Role::InteractiveWithSize {
            owns_size: false
        })),
        Ack::Applied
    );
    pty.size(80, 24);
    viewport(&mut mobile, 44, 24);
    pty.size(44, 24);
    drop(mobile);
    pty.size(80, 24);
}
