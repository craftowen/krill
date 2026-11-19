//! Unix host terminal and readiness adapter.
use super::super::frame;
use super::{Event, RESET, Result, fail};
use krill_proto::os::net::UnixStream;
use krill_proto::{ipc, screen};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::termios::{self, OptionalActions, Termios};
use std::io::{self, IsTerminal as _, Read as _, Write as _};
use std::time::{Duration, Instant};

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

pub(super) struct Host {
    original: Option<Termios>,
}
impl Host {
    pub(super) fn enter() -> Result<Self> {
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

pub(super) fn output() -> io::Stdout {
    io::stdout()
}

pub(super) struct Events {
    input_ready: bool,
}
impl Events {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "shared platform adapter signature"
    )]
    pub(super) fn new(_: &UnixStream) -> Result<Self> {
        Ok(Self { input_ready: false })
    }
    pub(super) fn next(
        &mut self,
        stream: &mut UnixStream,
        reading: bool,
        deadline: Option<Instant>,
    ) -> Result<Event> {
        // Preserve the input readiness observed beside an OUTPUT frame so a busy
        // daemon cannot starve keyboard input on the next iteration.
        if std::mem::take(&mut self.input_ready) && reading {
            return read_input();
        }
        let stdin = io::stdin();
        let mut fds = [
            PollFd::new(&*stream, PollFlags::IN),
            PollFd::new(&stdin, PollFlags::IN),
        ];
        let timeout = Timespec::try_from(Duration::from_millis(100)).map_err(fail)?;
        let count = if reading { 2 } else { 1 };
        match poll(&mut fds[..count], Some(&timeout)) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => return Ok(Event::Tick),
            Err(e) => return Err(fail(e)),
        }
        if !fds[0].revents().is_empty() {
            self.input_ready = reading && !fds[1].revents().is_empty();
            let (kind, body) = if let Some(deadline) = deadline {
                ipc::read_frame(&mut AckReader { stream, deadline })
                    .map_err(fail)?
                    .ok_or_else(|| fail("krilld disconnected"))?
            } else {
                frame(stream)?
            };
            return Ok(Event::Frame(kind, body));
        }
        if reading && !fds[1].revents().is_empty() {
            return read_input();
        }
        Ok(Event::Tick)
    }
}

fn read_input() -> Result<Event> {
    let mut bytes = vec![0; 4096];
    let n = io::stdin().read(&mut bytes).map_err(fail)?;
    bytes.truncate(n);
    Ok(Event::Input(bytes))
}
