//! A successful detach settles the input already sent on that connection.
#![cfg_attr(
    unix,
    expect(clippy::unwrap_used, reason = "isolated CLI protocol fixture")
)]
#![cfg(unix)]

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::control::{FEAT_CLI, FEAT_CONTROL, Reply, Request};
use krill_proto::input::{self, Input, InputAck, InputResult};
use krill_proto::ipc::{self, ClientHello, ServerHello};
use krill_proto::roles;
use krill_proto::screen::{self, Attach, Attached, Msg as _, Revision, Seq};

const WAIT: Duration = Duration::from_secs(5);

struct Peer {
    dir: PathBuf,
    child: Option<Child>,
    stream: Option<UnixStream>,
}

impl Peer {
    fn new(tag: &str) -> Self {
        let dir = PathBuf::from("/tmp").join(format!("ka-{}-{tag}", std::process::id()));
        fs::create_dir_all(dir.join("run")).unwrap();
        let listener = UnixListener::bind(dir.join("run/krilld.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_krill"))
            .args(["attach", "1"])
            .env("KRILL_STATE_DIR", &dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut fixture = Self {
            dir,
            child: Some(child),
            stream: None,
        };
        let (mut control, kind, body) = Self::request(&listener);
        assert!(matches!(
            Request::decode(kind, &body).unwrap(),
            Request::Capture {
                session: SessionId(1),
                ..
            }
        ));
        control
            .write_all(
                &Reply::Captured {
                    seq: Seq(0),
                    bytes: Vec::new(),
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        let (mut raw, kind, body) = Self::request(&listener);
        assert_eq!(Attach::decode(kind, &body).unwrap().session, SessionId(1));
        raw.write_all(
            &Attached {
                seq: Seq(0),
                revision: Revision(0),
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
        fixture.stream = Some(raw);
        fixture
    }

    fn request(listener: &UnixListener) -> (UnixStream, u8, Vec<u8>) {
        let deadline = Instant::now() + WAIT;
        loop {
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(pair) => break pair,
                    Err(error) => {
                        assert_eq!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock,
                            "accept: {error}"
                        );
                        assert!(Instant::now() < deadline, "CLI did not connect");
                        thread::sleep(Duration::from_millis(5));
                    }
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(WAIT)).unwrap();
            stream
                .write_all(
                    &ServerHello {
                        proto_min: ipc::PROTO_MIN,
                        proto_max: ipc::PROTO_MAX,
                        features: FEAT_CLI
                            | FEAT_CONTROL
                            | screen::FEAT_SCREEN
                            | input::FEAT_INPUT
                            | roles::FEAT_ROLES,
                        build: krill_proto::BUILD.into(),
                        instance_id: 1,
                    }
                    .encode()
                    .unwrap(),
                )
                .unwrap();
            let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
            ClientHello::decode(kind, &body).unwrap();
            if let Some((kind, body)) = ipc::read_frame(&mut stream).unwrap() {
                return (stream, kind, body);
            }
        }
    }

    fn input(&mut self, detach: bool) -> Input {
        let mut stdin = self.child.as_mut().unwrap().stdin.take().unwrap();
        stdin
            .write_all(if detach { b"done\r\x1d" } else { b"done\r" })
            .unwrap();
        if detach {
            self.child.as_mut().unwrap().stdin = Some(stdin);
        }
        let (kind, body) = ipc::read_frame(self.stream.as_mut().unwrap())
            .unwrap()
            .unwrap();
        let input = Input::decode(kind, &body).unwrap();
        assert_eq!(input.bytes, b"done\r");
        input
    }

    fn still_attached(&mut self) {
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline {
            assert!(
                self.child.as_mut().unwrap().try_wait().unwrap().is_none(),
                "detach returned before its input receipt"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn ack(&mut self, input: &Input, result: InputResult) {
        self.stream
            .as_mut()
            .unwrap()
            .write_all(
                &InputAck {
                    id: input.id,
                    result,
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
    }

    fn finish(&mut self) -> Output {
        let deadline = Instant::now() + WAIT;
        loop {
            if self.child.as_mut().unwrap().try_wait().unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "attach did not finish");
            thread::sleep(Duration::from_millis(5));
        }
        let output = self.child.take().unwrap().wait_with_output().unwrap();
        let mut remaining = Vec::new();
        if let Some(stream) = &mut self.stream {
            stream.read_to_end(&mut remaining).unwrap();
        }
        assert!(remaining.is_empty(), "input was resent after detach");
        output
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill().ok();
            child.wait().ok();
        }
        fs::remove_dir_all(&self.dir).ok();
    }
}

#[test]
fn ctrl_detach_and_stdin_eof_wait_for_applied_input() {
    for detach in [true, false] {
        let mut peer = Peer::new(if detach { "ctrl" } else { "eof" });
        let input = peer.input(detach);
        peer.still_attached();
        peer.ack(&input, InputResult::Applied);
        let output = peer.finish();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn applied_receipt_remains_valid_when_peer_closes_immediately() {
    let mut peer = Peer::new("applied-close");
    let input = peer.input(true);
    let pid = rustix::process::Pid::from_child(peer.child.as_ref().unwrap());
    rustix::process::kill_process(pid, rustix::process::Signal::STOP).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some((_, status)) = rustix::process::waitpid(
            Some(pid),
            rustix::process::WaitOptions::UNTRACED | rustix::process::WaitOptions::NOHANG,
        )
        .unwrap()
        {
            assert!(status.stopped());
            break;
        }
        assert!(Instant::now() < deadline, "CLI did not stop");
        thread::sleep(Duration::from_millis(5));
    }
    peer.ack(&input, InputResult::Applied);
    drop(peer.stream.take());
    rustix::process::kill_process(pid, rustix::process::Signal::CONT).unwrap();
    let output = peer.finish();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn session_close_before_applied_receipt_still_settles_input() {
    for role in [false, true] {
        let mut peer = Peer::new(if role { "role-closed" } else { "screen-closed" });
        let input = peer.input(true);
        let closed = if role {
            roles::Ack::Closed.encode().unwrap()
        } else {
            screen::Closed { seq: Seq(0) }.encode().unwrap()
        };
        peer.stream.as_mut().unwrap().write_all(&closed).unwrap();
        peer.still_attached();
        peer.ack(&input, InputResult::Applied);
        assert!(peer.finish().status.success());
    }
}

#[test]
fn session_close_without_receipt_does_not_claim_applied_input() {
    for role in [false, true] {
        let mut peer = Peer::new(if role { "role-no-ack" } else { "screen-no-ack" });
        peer.input(true);
        let closed = if role {
            roles::Ack::Closed.encode().unwrap()
        } else {
            screen::Closed { seq: Seq(0) }.encode().unwrap()
        };
        peer.stream.as_mut().unwrap().write_all(&closed).unwrap();
        let output = peer.finish();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("delivery uncertain"));
    }
}

#[test]
fn closed_session_without_pending_input_detaches_normally() {
    let mut peer = Peer::new("closed-without-input");
    peer.stream
        .as_mut()
        .unwrap()
        .write_all(&screen::Closed { seq: Seq(0) }.encode().unwrap())
        .unwrap();
    assert!(peer.finish().status.success());
}

#[test]
fn rejected_input_cannot_report_successful_detach() {
    for result in [InputResult::Closed, InputResult::Denied] {
        let mut peer = Peer::new(if result == InputResult::Closed {
            "rejected-closed"
        } else {
            "denied"
        });
        let input = peer.input(true);
        peer.ack(&input, result);
        assert!(!peer.finish().status.success());
    }
}

#[test]
fn disconnect_with_unacknowledged_input_reports_uncertainty() {
    let mut peer = Peer::new("disconnect");
    peer.input(true);
    peer.stream
        .as_mut()
        .unwrap()
        .shutdown(Shutdown::Write)
        .unwrap();
    let output = peer.finish();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("delivery uncertain"));
}

#[test]
fn missing_or_partial_receipt_obeys_existing_input_probe() {
    for partial in [false, true] {
        let mut peer = Peer::new(if partial { "partial" } else { "timeout" });
        let input = peer.input(true);
        if partial {
            let ack = InputAck {
                id: input.id,
                result: InputResult::Applied,
            }
            .encode()
            .unwrap();
            peer.stream
                .as_mut()
                .unwrap()
                .write_all(&ack[..ack.len() - 1])
                .unwrap();
        }
        let started = Instant::now();
        let output = peer.finish();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("delivery uncertain"));
        assert!(started.elapsed() < input::PROBE + Duration::from_secs(1));
    }
}
