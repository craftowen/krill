//! Host terminal adapter: safe termios, bounded stream, no terminal engine in krill.

mod presenter;

use std::collections::BTreeMap;
use std::io::{IsTerminal as _, Read as _, Write as _};
use std::time::Instant;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::termios::{self, OptionalActions, Termios};

use super::{Result, capture, connect, fail, frame, raw, request};
use crate::remote::Target;
use krill_proto::SessionId;
use krill_proto::control::{CaptureFormat, FEAT_CLI, FEAT_CONTROL, Reply, Request};
use krill_proto::input::{self, Input, InputAck, InputId, InputResult};
use krill_proto::ipc;
use krill_proto::roles;
use krill_proto::screen::{self, Msg};
use presenter::Presenter;
use std::io;
use std::os::unix::net::UnixStream;

const RESET: &[u8] = b"\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[=0u\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1005l\x1b[?1006l\x1b[?2004l\x1b[?1049l";

#[derive(PartialEq, Eq)]
enum InputMode {
    Reading,
    Detaching,
}

struct AckReader<'a> {
    stream: &'a mut UnixStream,
    deadline: Instant,
}

impl io::Read for AckReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            let remaining = self
                .deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or(io::ErrorKind::TimedOut)?;
            let timeout = Timespec::try_from(remaining).map_err(io::Error::other)?;
            let ready = {
                let mut fds = [PollFd::new(&*self.stream, PollFlags::IN)];
                poll(&mut fds, Some(&timeout))
            };
            match ready {
                Ok(0) => return Err(io::ErrorKind::TimedOut.into()),
                Ok(_) => return self.stream.read(bytes),
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}

struct Host {
    original: Option<Termios>,
}
impl Host {
    fn enter() -> Result<Self> {
        let original = if io::stdin().is_terminal() {
            let original = termios::tcgetattr(io::stdin()).map_err(fail)?;
            let mut raw = original.clone();
            raw.make_raw();
            termios::tcsetattr(io::stdin(), OptionalActions::Now, &raw).map_err(fail)?;
            Some(original)
        } else {
            None
        };
        let host = Self { original };
        if io::stdout().is_terminal() {
            io::stdout().write_all(b"\x1b[?1049h").map_err(fail)?;
        }
        Ok(host)
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        if let Some(original) = &self.original {
            termios::tcsetattr(io::stdin(), OptionalActions::Now, original).ok();
        }
        if io::stdout().is_terminal() {
            let mut out = io::stdout().lock();
            out.write_all(RESET).ok();
            out.flush().ok();
        }
    }
}

pub(super) fn size() -> Option<(u16, u16)> {
    let s = termios::tcgetwinsize(io::stdin()).ok()?;
    (s.ws_col > 0 && s.ws_row > 0).then_some((
        s.ws_col.min(screen::MAX_COLS),
        s.ws_row.min(screen::MAX_LINES),
    ))
}

fn resize(c: &mut UnixStream, session: SessionId, size: (u16, u16)) -> Result<()> {
    match request(
        c,
        &Request::Resize {
            session,
            cols: size.0,
            rows: size.1,
        },
    )? {
        Reply::Resized => Ok(()),
        _ => Err(fail("unexpected resize reply")),
    }
}

fn role_resize(c: &mut UnixStream, size: (u16, u16)) -> Result<()> {
    c.write_all(
        &roles::Request::Resize {
            cols: size.0,
            rows: size.1,
        }
        .encode()
        .map_err(fail)?,
    )
    .map_err(fail)?;
    c.write_all(&roles::Request::Focus.encode().map_err(fail)?)
        .map_err(fail)
}

pub(super) fn attach(target: &Target, session: SessionId) -> Result<()> {
    let mut control = connect(target, FEAT_CONTROL | FEAT_CLI)?;
    let mut dimensions = size();
    if let Some(size) = dimensions {
        resize(&mut control.stream, session, size)?;
    }
    let (seq, bytes) = capture(&mut control.stream, session, CaptureFormat::Ansi)?;
    let mut connection = raw(target, session, seq)?;
    if let Some(size) = dimensions {
        role_resize(&mut connection.stream, size)?;
    }
    drop(control);
    let _host = Host::enter()?;
    let mut output = io::stdout();
    output.write_all(&bytes).map_err(fail)?;
    output.flush().map_err(fail)?;
    let mut presenter = Presenter::default();
    let mut next = 1u64;
    let mut pending = BTreeMap::<InputId, Instant>::new();
    let mut mode = InputMode::Reading;
    let result = (|| {
        loop {
            let stdin = io::stdin();
            let mut fds = [
                PollFd::new(&connection.stream, PollFlags::IN),
                PollFd::new(&stdin, PollFlags::IN),
            ];
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: 100_000_000,
            };
            let count = if mode == InputMode::Reading { 2 } else { 1 };
            match poll(&mut fds[..count], Some(&timeout)) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(fail(e)),
            }
            let input_ready = mode == InputMode::Reading && !fds[1].revents().is_empty();
            let daemon_ready = !fds[0].revents().is_empty();
            if daemon_ready {
                let (kind, body) = if let Some(sent) = pending.values().min() {
                    ipc::read_frame(&mut AckReader {
                        stream: &mut connection.stream,
                        deadline: *sent + input::PROBE,
                    })
                    .map_err(fail)?
                    .ok_or_else(|| fail("krilld disconnected"))?
                } else {
                    frame(&mut connection.stream)?
                };
                match kind {
                    screen::kind::OUTPUT => {
                        let (_, bytes) = screen::decode_output(kind, &body).map_err(fail)?;
                        let bytes = presenter.push(bytes);
                        output.write_all(&bytes).map_err(fail)?;
                        output.flush().map_err(fail)?;
                    }
                    screen::kind::CLOSED => {
                        screen::Closed::decode(kind, &body).map_err(fail)?;
                        mode = InputMode::Detaching;
                    }
                    screen::kind::GAP => {
                        let gap = screen::GapMsg::decode(kind, &body).map_err(fail)?;
                        return Err(fail(format!(
                            "output gap at {}; detach and reattach for a fresh screen",
                            gap.start.0
                        )));
                    }
                    input::kind::INPUT_ACK => {
                        let ack = InputAck::decode(kind, &body).map_err(fail)?;
                        pending.remove(&ack.id);
                        match ack.result {
                            InputResult::Applied => {}
                            InputResult::Closed => {
                                return Err(fail("session closed before input was applied"));
                            }
                            InputResult::Denied => return Err(fail("input permission denied")),
                        }
                    }
                    roles::kind::ROLE_ACK => {
                        let ack = roles::Ack::decode(kind, &body).map_err(fail)?;
                        if ack == roles::Ack::Closed {
                            mode = InputMode::Detaching;
                        }
                    }
                    _ => return Err(fail("unexpected attach reply")),
                }
            }
            if mode == InputMode::Reading {
                let current = size();
                if current != dimensions {
                    if let Some(size) = current {
                        role_resize(&mut connection.stream, size)?;
                    }
                    dimensions = current;
                }
            }
            if input_ready && mode == InputMode::Reading {
                let mut bytes = [0u8; 4096];
                let n = io::stdin().read(&mut bytes).map_err(fail)?;
                let bytes = &bytes[..n];
                let end = bytes.iter().position(|b| *b == 0x1d).unwrap_or(n);
                if end > 0 {
                    if pending.len() >= 16 {
                        return Err(fail("input acknowledgement stalled"));
                    }
                    let id = InputId(next);
                    next = next
                        .checked_add(1)
                        .ok_or_else(|| fail("input id exhausted"))?;
                    connection
                        .stream
                        .write_all(
                            &Input {
                                id,
                                bytes: bytes[..end].to_vec(),
                            }
                            .encode()
                            .map_err(fail)?,
                        )
                        .map_err(fail)?;
                    pending.insert(id, Instant::now());
                }
                if n == 0 || end < n {
                    mode = InputMode::Detaching;
                }
            }
            if mode == InputMode::Detaching && pending.is_empty() {
                return Ok(());
            }
            if pending.values().any(|t| t.elapsed() >= input::PROBE) {
                return Err(fail("input acknowledgement stalled; delivery uncertain"));
            }
        }
    })();
    let result = if pending.is_empty() {
        result
    } else {
        eprintln!("attach: unacknowledged input; delivery uncertain, do not automatically retry");
        result.and_then(|()| Err(fail("input acknowledgement missing; delivery uncertain")))
    };
    result.map_err(|e| {
        if !target.is_remote() {
            return e;
        }
        super::Error {
            code: e.code,
            text: format!(
                "{} (remote {}; the session is preserved on the host, reattach to resume)",
                e.text,
                target.host().unwrap_or("?")
            ),
        }
    })
}
