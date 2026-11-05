//! Shared attach protocol; platform adapters own the host terminal and readiness.

mod presenter;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
pub(super) fn size() -> Option<(u16, u16)> {
    host::size()
}
#[cfg(unix)]
use unix as host;
#[cfg(windows)]
use windows as host;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::time::Instant;

use super::{Result, capture, connect, fail, raw, request};
use crate::remote::Target;
use krill_proto::SessionId;
use krill_proto::control::{CaptureFormat, FEAT_CLI, FEAT_CONTROL, Reply, Request};
use krill_proto::input::{self, Input, InputAck, InputId, InputResult};
use krill_proto::os::net::UnixStream;
use krill_proto::roles;
use krill_proto::screen::{self, Msg};
use presenter::Presenter;

const RESET: &[u8] = b"\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[=0u\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1005l\x1b[?1006l\x1b[?2004l\x1b[?1049l";

enum Event {
    Frame(u8, Vec<u8>),
    Input(Vec<u8>),
    Tick,
}

#[derive(PartialEq, Eq)]
enum InputMode {
    Reading,
    Detaching,
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
    let _host = host::Host::enter()?;
    let mut events = host::Events::new(&connection.stream)?;
    let mut output = host::output();
    output.write_all(&bytes).map_err(fail)?;
    output.flush().map_err(fail)?;
    let mut presenter = Presenter::default();
    let mut next = 1u64;
    let mut pending = BTreeMap::<InputId, Instant>::new();
    let mut mode = InputMode::Reading;
    let result = (|| {
        loop {
            let event = events.next(
                &mut connection.stream,
                mode == InputMode::Reading,
                pending.values().min().map(|sent| *sent + input::PROBE),
            )?;
            let mut input_bytes = None;
            match event {
                Event::Tick => {}
                Event::Input(bytes) => input_bytes = Some(bytes),
                Event::Frame(kind, body) => match kind {
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
                },
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
            if let Some(bytes) = input_bytes.filter(|_| mode == InputMode::Reading) {
                let n = bytes.len();
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
