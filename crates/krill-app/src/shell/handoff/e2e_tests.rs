//! OWE-661 R5: GUI의 준비 대기 → 입력 장부 → 실제 PTY → 가짜 TUI 수신까지 검증한다.
//! CLI `orchestration_worker_start_waits_for_tui_and_stop_closes_session`과 같이
//! 부팅 중 입력은 버린다. 부팅 해제는 시간 지연 대신 캡처 응답 횟수로 제어한다.

use std::fs::{self, File};
use std::io::Read as _;
use std::net::Shutdown;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use gpui::{Entity, TestAppContext, VisualTestContext};
use krill_proto::input::Input;
use krill_proto::ipc::{ClientHello, ServerHello};
use krill_proto::screen::Seq;
use rustix::termios::{OptionalActions, tcgetattr, tcsetattr};

use super::*;
use crate::shell::tests::{open, state_path};

const SESSION: SessionId = SessionId(661);
const BRIEF: &[u8] = "[MK SESSION HANDOFF]\n한글 브리프\n두 번째 줄".as_bytes();
const WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Default)]
enum Stage {
    #[default]
    Booting,
    Ready,
    Blocked,
    Exited,
}

#[derive(Default)]
struct TuiState {
    stage: Stage,
    after_boot: Option<Stage>,
    captures: usize,
    received: Vec<Vec<u8>>,
    discarded: Vec<Vec<u8>>,
}

/// 제어 소켓은 `WaitStatus`/`Capture`, 화면 소켓은 실제 `Pane::input` 프레임을 받는다.
/// 입력 바이트는 raw PTY를 통과한 뒤에만 가짜 TUI에 도달한다.
struct FakeTui {
    paths: Paths,
    state: Arc<Mutex<TuiState>>,
    input: UnixStream,
    control_thread: Option<JoinHandle<()>>,
    input_thread: Option<JoinHandle<()>>,
}

impl FakeTui {
    fn new(name: &str) -> (Self, UnixStream) {
        // macOS UDS 경로 제한을 지키고, 각 nextest 프로세스·테스트를 격리한다.
        let dir = PathBuf::from(format!("/tmp/mk661-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let paths = Paths {
            socket: dir.join("daemon.sock"),
            holder: dir.join("unused-holder"),
            krilld: dir.join("unused-krilld"),
        };
        let listener = UnixListener::bind(&paths.socket).unwrap();
        let state = Arc::new(Mutex::new(TuiState::default()));
        let control_state = state.clone();
        let control_thread = thread::spawn(move || {
            // daemon::connect는 holder 감시 핸드셰이크 뒤 제어 연결을 새로 연다.
            for _ in 0..2 {
                let (mut sock, _) = listener.accept().unwrap();
                if sock.set_read_timeout(Some(WAIT)).is_err() {
                    break;
                }
                let hello = ServerHello {
                    proto_min: ipc::PROTO_MIN,
                    proto_max: ipc::PROTO_MAX,
                    features: ipc::FEATURES & !daemon::FEAT_HOLDER_WATCH,
                    build: "handoff-fake-tui".into(),
                    instance_id: 1,
                };
                if sock.write_all(&hello.encode().unwrap()).is_err() {
                    break;
                }
                let Ok(Some((k, body))) = ipc::read_frame(&mut sock) else {
                    break;
                };
                ClientHello::decode(k, &body).unwrap();
                while let Ok(Some((k, body))) = ipc::read_frame(&mut sock) {
                    let request = Request::decode(k, &body).unwrap();
                    let mut tui = control_state.lock().unwrap();
                    let reply = match request {
                        Request::WaitStatus { session } => {
                            assert_eq!(session, SESSION);
                            Reply::WaitStatus {
                                state: match tui.stage {
                                    Stage::Exited => AgentState::Exited,
                                    // Ready도 화면 프롬프트를 통해 판정하도록 한다.
                                    _ => AgentState::Unknown,
                                },
                                quiet_ms: 0,
                            }
                        }
                        Request::Capture { session, format } => {
                            assert_eq!(session, SESSION);
                            assert_eq!(format, CaptureFormat::Plain);
                            tui.captures += 1;
                            let bytes = match tui.stage {
                                    Stage::Booting => b"Loading fake TUI...".to_vec(),
                                    Stage::Ready => b"Fake TUI\n> ".to_vec(),
                                    Stage::Blocked => b"Do you trust the contents of this project?\n> Yes, I trust this folder".to_vec(),
                                    Stage::Exited => Vec::new(),
                                };
                            // GPUI 테스트 실행기는 같은 스레드에서 blocking 대기도 실행한다.
                            // 피어가 두 번의 부팅 캡처 뒤 스스로 전환해야 GUI 스레드와 교착하지 않는다.
                            if tui.captures >= 2
                                && let Some(stage) = tui.after_boot.take()
                            {
                                tui.stage = stage;
                            }
                            Reply::Captured { seq: Seq(0), bytes }
                        }
                        other => panic!("unexpected control request: {other:?}"),
                    };
                    drop(tui);
                    if sock.write_all(&reply.encode().unwrap()).is_err() {
                        break;
                    }
                }
            }
        });

        let pty = rustix_openpty::openpty(None, None).unwrap();
        let mut attrs = tcgetattr(&pty.user).unwrap();
        attrs.make_raw();
        tcsetattr(&pty.user, OptionalActions::Now, &attrs).unwrap();
        let mut controller = File::from(pty.controller);
        let mut user = File::from(pty.user);
        let (ours, mut peer) = UnixStream::pair().unwrap();
        let input = peer.try_clone().unwrap();
        let input_state = state.clone();
        let input_thread = thread::spawn(move || {
            while let Ok(Some((k, body))) = ipc::read_frame(&mut peer) {
                let input = Input::decode(k, &body).unwrap();
                let mut tui = input_state.lock().unwrap();
                controller.write_all(&input.bytes).unwrap();
                let mut bytes = vec![0; input.bytes.len()];
                user.read_exact(&mut bytes).unwrap();
                if matches!(tui.stage, Stage::Ready) {
                    tui.received.push(bytes);
                } else {
                    tui.discarded.push(bytes);
                }
            }
        });
        (
            Self {
                paths,
                state,
                input,
                control_thread: Some(control_thread),
                input_thread: Some(input_thread),
            },
            ours,
        )
    }

    fn attach(
        &self,
        view: &Entity<Shell>,
        cx: &mut VisualTestContext,
        sock: UnixStream,
        modes: Modes,
        timeout: Duration,
    ) {
        view.update(cx, |shell, cx| {
            shell.paths = Some(self.paths.clone());
            let pane = shell.layout.active_tab().focused;
            let launch = shell.launch_for(pane);
            shell.layout.set_launch(pane, launch);
            shell.layout.set_session(pane, SESSION);
            shell.attach_test(pane, SESSION, sock, cx);
            let body = postcard::to_stdvec(&modes).unwrap();
            assert!(shell.on_frame(pane, krill_proto::term_modes::TERM_MODES, &body, cx));
            shell.start_handoff_delivery(pane, "codex".into(), BRIEF.to_vec(), timeout, cx);
        });
    }

    fn stage(&self, stage: Stage) {
        self.state.lock().unwrap().stage = stage;
    }

    fn after_boot(&self, stage: Stage) {
        self.state.lock().unwrap().after_boot = Some(stage);
    }

    /// Writer 큐를 끝까지 비운 뒤 EOF까지 읽어, 늦게 도착한 입력·중복도 대조한다.
    fn finish_input(&mut self, view: &Entity<Shell>, cx: &mut VisualTestContext) {
        view.update(cx, |shell, _| {
            shell.paths = None;
            shell.live.clear();
        });
        self.input_thread.take().unwrap().join().unwrap();
    }

    fn assert_no_input(&self) {
        let tui = self.state.lock().unwrap();
        assert!(tui.received.is_empty(), "brief was received on failure");
        assert!(tui.discarded.is_empty(), "brief was sent before readiness");
    }
}

impl Drop for FakeTui {
    fn drop(&mut self) {
        self.input.shutdown(Shutdown::Both).ok();
        if let Some(handle) = self.input_thread.take() {
            handle.join().unwrap();
        }
        // 준비 대기를 우회한 되돌림 실험에서도 accept를 깨워 스레드를 남기지 않는다.
        for _ in 0..2 {
            if let Ok(sock) = UnixStream::connect(&self.paths.socket) {
                sock.shutdown(Shutdown::Both).ok();
            }
        }
        self.control_thread.take().unwrap().join().unwrap();
        fs::remove_dir_all(self.paths.socket.parent().unwrap()).unwrap();
    }
}

fn pump_until(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "GUI handoff did not complete");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_notice(view: &Entity<Shell>, cx: &mut VisualTestContext, expected: &str) {
    pump_until(cx, |cx| {
        view.read_with(cx, |shell, _| shell.notice.as_deref() == Some(expected))
    });
}

fn bracketed() -> Modes {
    Modes {
        kitty: 0,
        flags: Modes::BRACKETED_PASTE,
    }
}

#[gpui::test]
fn handoff_tui_e2e_waits_before_delivering_one_multiline_brief(cx: &mut TestAppContext) {
    let (mut tui, sock) = FakeTui::new("ready");
    let (view, cx) = open(cx, state_path("handoff-tui-ready"));
    tui.after_boot(Stage::Ready);
    tui.attach(&view, cx, sock, bracketed(), WAIT);
    wait_notice(
        &view,
        cx,
        &text::HANDOFF_SPAWNED.format(&[("agent", "codex")]),
    );
    pump_until(cx, |_| !tui.state.lock().unwrap().received.is_empty());
    tui.finish_input(&view, cx);
    let state = tui.state.lock().unwrap();
    assert!(
        state.captures >= 3,
        "booting and ready screens must be polled"
    );
    assert!(state.discarded.is_empty(), "early input was lost");
    // plan_paste로 기대값을 만들지 않고 본문·래퍼·Enter·전송 횟수를 독립적으로 대조한다.
    let mut expected = b"\x1b[200~".to_vec();
    expected.extend_from_slice(BRIEF);
    expected.extend_from_slice(b"\x1b[201~\r");
    assert_eq!(state.received, [expected]);
}

#[gpui::test]
fn handoff_tui_e2e_startup_exit_shows_failure_without_input(cx: &mut TestAppContext) {
    let (mut tui, sock) = FakeTui::new("exited");
    let (view, cx) = open(cx, state_path("handoff-tui-exited"));
    tui.after_boot(Stage::Exited);
    tui.attach(&view, cx, sock, bracketed(), WAIT);
    wait_notice(
        &view,
        cx,
        &text::HANDOFF_ERR_EXITED.format(&[("agent", "codex")]),
    );
    tui.finish_input(&view, cx);
    tui.assert_no_input();
}

#[gpui::test]
fn handoff_tui_e2e_trust_prompt_shows_failure_without_input(cx: &mut TestAppContext) {
    let (mut tui, sock) = FakeTui::new("blocked");
    let (view, cx) = open(cx, state_path("handoff-tui-blocked"));
    tui.after_boot(Stage::Blocked);
    tui.attach(&view, cx, sock, bracketed(), WAIT);
    wait_notice(
        &view,
        cx,
        &text::HANDOFF_ERR_BLOCKED.format(&[("agent", "codex")]),
    );
    tui.finish_input(&view, cx);
    tui.assert_no_input();
}

#[gpui::test]
fn handoff_tui_e2e_boot_timeout_shows_failure_without_input(cx: &mut TestAppContext) {
    let (mut tui, sock) = FakeTui::new("timeout");
    let (view, cx) = open(cx, state_path("handoff-tui-timeout"));
    tui.attach(&view, cx, sock, bracketed(), Duration::from_millis(350));
    wait_notice(
        &view,
        cx,
        &text::HANDOFF_ERR_TIMEOUT.format(&[("agent", "codex")]),
    );
    assert!(tui.state.lock().unwrap().captures > 0);
    tui.finish_input(&view, cx);
    tui.assert_no_input();
}

#[gpui::test]
fn handoff_tui_e2e_plain_multiline_is_refused_after_ready(cx: &mut TestAppContext) {
    let (mut tui, sock) = FakeTui::new("plain");
    let (view, cx) = open(cx, state_path("handoff-tui-plain"));
    tui.stage(Stage::Ready);
    tui.attach(&view, cx, sock, Modes::default(), WAIT);
    let expected = fail_text(
        &FailKind::Transport("paste refused: the program does not use bracketed paste".into()),
        "codex",
    );
    wait_notice(&view, cx, &expected);
    tui.finish_input(&view, cx);
    tui.assert_no_input();
}
