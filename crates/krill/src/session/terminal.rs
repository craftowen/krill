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
use krill_proto::roles;
use krill_proto::screen::{self, Msg};
use presenter::Presenter;
use std::io;
use std::os::unix::net::UnixStream;

const RESET: &[u8] = b"\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[=0u\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1005l\x1b[?1006l\x1b[?2004l\x1b[?1049l";

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
    let result = (|| {
        loop {
            let stdin = io::stdin();
            let mut fds = [
                PollFd::new(&stdin, PollFlags::IN),
                PollFd::new(&connection.stream, PollFlags::IN),
            ];
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: 100_000_000,
            };
            match poll(&mut fds, Some(&timeout)) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(fail(e)),
            }
            let input_ready = !fds[0].revents().is_empty();
            let daemon_ready = !fds[1].revents().is_empty();
            if daemon_ready {
                let (kind, body) = frame(&mut connection.stream)?;
                match kind {
                    screen::kind::OUTPUT => {
                        let (_, bytes) = screen::decode_output(kind, &body).map_err(fail)?;
                        let bytes = presenter.push(bytes);
                        output.write_all(&bytes).map_err(fail)?;
                        output.flush().map_err(fail)?;
                    }
                    screen::kind::CLOSED => {
                        screen::Closed::decode(kind, &body).map_err(fail)?;
                        return Ok(());
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
                        if ack.result == InputResult::Closed {
                            return Err(fail("session closed before input was applied"));
                        }
                    }
                    roles::kind::ROLE_ACK => {
                        let ack = roles::Ack::decode(kind, &body).map_err(fail)?;
                        if ack == roles::Ack::Closed {
                            return Ok(());
                        }
                    }
                    _ => return Err(fail("unexpected attach reply")),
                }
            }
            let current = size();
            if current != dimensions {
                if let Some(size) = current {
                    role_resize(&mut connection.stream, size)?;
                }
                dimensions = current;
            }
            if input_ready {
                let mut bytes = [0u8; 4096];
                let n = io::stdin().read(&mut bytes).map_err(fail)?;
                if n == 0 {
                    return Ok(());
                }
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
                if end < n {
                    return Ok(());
                }
            }
            if pending.values().any(|t| t.elapsed() >= input::PROBE) {
                return Err(fail("input acknowledgement stalled; delivery uncertain"));
            }
        }
    })();
    if !pending.is_empty() {
        eprintln!("attach: unacknowledged input; delivery uncertain, do not automatically retry");
    }
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
