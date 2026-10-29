//! 연결 취소를 관찰하는 입력 직렬화와 프레임 경계를 넘는 bracketed paste 복구.

use crate::os::OwnedFd;
use std::sync::TryLockError;

use super::{BorrowedFd, Duration, MutexGuard, Tap, io, thread};
use crate::os::AsFd as _;
#[cfg(unix)]
use rustix::event::{PollFd, PollFlags, Timespec, poll};
#[cfg(unix)]
use rustix::io::{Errno, write};

const START: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";
#[cfg(unix)]
const WAIT: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 20_000_000,
};

#[derive(Default)]
pub(super) struct State {
    paste: bool,
    matched: usize,
    owner: Option<thread::ThreadId>,
    peer: Option<OwnedFd>,
}

impl State {
    fn orphaned(&self, writer: thread::ThreadId) -> bool {
        self.owner != Some(writer)
            && self
                .peer
                .as_ref()
                .is_some_and(|fd| disconnected(fd.as_fd()).unwrap_or(false))
    }

    fn record(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let marker = if self.paste { END } else { START };
            if marker.get(self.matched) == Some(&byte) {
                self.matched += 1;
                if self.matched == marker.len() {
                    self.paste = !self.paste;
                    self.matched = 0;
                }
            } else {
                self.matched = usize::from(byte == 0x1b);
            }
        }
    }

    fn finish(&mut self, tap: &Tap) -> io::Result<()> {
        // 이미 쓴 종료 표지의 접두사는 반복하지 않는다.
        if self.paste {
            write_all(tap, END.get(self.matched..).unwrap_or(END), None, self)?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn disconnected(peer: BorrowedFd<'_>) -> io::Result<bool> {
    let mut fds = [PollFd::from_borrowed_fd(peer, PollFlags::HUP)];
    match poll(
        &mut fds,
        Some(&Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        }),
    ) {
        Ok(_) => Ok(fds.first().is_some_and(|fd| !fd.revents().is_empty())),
        Err(Errno::INTR) => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// Windows: `WSAPoll`은 요청 마스크의 `HUP`를 거부하고(WSAEINVAL) 정상 종료(FIN)는 `HUP` 없이 "읽을 수 있음"으로만
/// 보일 수 있어, 소켓 전용 판정([`crate::sys::socket_disconnected`])을 쓴다. Unix의 `HUP` 폴링은 그대로다.
#[cfg(windows)]
fn disconnected(peer: BorrowedFd<'_>) -> io::Result<bool> {
    crate::sys::socket_disconnected(peer)
}

fn alive(tap: &Tap, peer: Option<BorrowedFd<'_>>) -> io::Result<()> {
    if tap.pulse().closed {
        return Err(io::ErrorKind::BrokenPipe.into());
    }
    if let Some(peer) = peer
        && disconnected(peer)?
    {
        return Err(io::ErrorKind::ConnectionAborted.into());
    }
    Ok(())
}

fn turn<'a>(tap: &'a Tap, peer: Option<BorrowedFd<'_>>) -> io::Result<MutexGuard<'a, State>> {
    let mut waited = false;
    loop {
        // 이미 받은 완전한 프레임은 즉시 처리한다. detach 직전 보낸 입력을 HUP만으로 버리지 않는다.
        alive(tap, if waited { peer } else { None })?;
        match tap.0.input.try_lock() {
            Ok(guard) => return Ok(guard),
            // 이미 PTY에 쓴 접두사와 장부의 대응을 증명할 수 없으므로 입력만 거부한다.
            Err(TryLockError::Poisoned(_)) => {
                return Err(io::Error::other("terminal input state poisoned"));
            }
            Err(TryLockError::WouldBlock) => {
                waited = true;
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

#[cfg(unix)]
fn write_all(
    tap: &Tap,
    mut bytes: &[u8],
    peer: Option<BorrowedFd<'_>>,
    state: &mut State,
) -> io::Result<()> {
    while !bytes.is_empty() {
        alive(tap, None)?;
        let result = {
            let _live = tap.0.live();
            write_once(tap, bytes)
        };
        match result {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                state.record(bytes.get(..n).unwrap_or_default());
                if state.peer.is_none() && (state.paste || state.matched > 0) {
                    state.peer = peer.map(|fd| fd.try_clone_to_owned()).transpose()?;
                }
                bytes = bytes.get(n..).unwrap_or_default();
            }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                let mut fds = [PollFd::new(&tap.0.master, PollFlags::OUT)]
                    .into_iter()
                    .chain(peer.map(|fd| PollFd::from_borrowed_fd(fd, PollFlags::HUP)))
                    .collect::<Vec<_>>();
                match poll(&mut fds, Some(&WAIT)) {
                    Ok(_) | Err(Errno::INTR) => {}
                    Err(err) => return Err(err.into()),
                }
                alive(tap, peer)?;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// Windows(daemon-02): 막히는 `WriteFile`은 세션 writer 스레드가 하고, 이 스레드는 20ms마다 세션 종료·연결 HUP만
/// 본다. 취소되면 writer가 실제로 쓴 접두사만 장부에 적는다(큐에 넣은 바이트가 곧 쓴 바이트가 아니다).
#[cfg(windows)]
fn write_all(
    tap: &Tap,
    bytes: &[u8],
    peer: Option<BorrowedFd<'_>>,
    state: &mut State,
) -> io::Result<()> {
    alive(tap, None)?;
    let (written, result) = tap.0.writer.write(bytes, || alive(tap, peer));
    state.record(bytes.get(..written).unwrap_or_default());
    if state.peer.is_none() && (state.paste || state.matched > 0) {
        state.peer = peer.map(|fd| fd.try_clone_to_owned()).transpose()?;
    }
    result
}

#[cfg(unix)]
fn write_once(tap: &Tap, bytes: &[u8]) -> io::Result<usize> {
    write(&tap.0.master, bytes).map_err(io::Error::from)
}

/// A short local CLI command, serialized with every other writer. The caller must recognize
/// both the empty composer and its exact echoed command before Enter is permitted.
#[cfg(unix)]
pub(super) fn guarded_command(
    tap: &Tap,
    revision: u64,
    command: &[u8],
    accepts: impl Fn(&krill_proto::screen::Screen, bool) -> bool,
) -> io::Result<()> {
    if command.is_empty() || command.len() > 256 || command.iter().any(u8::is_ascii_control) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut state = tap
        .0
        .input
        .try_lock()
        .map_err(|_| io::Error::other("Terminal input is busy"))?;
    let clients = tap
        .0
        .clients
        .lock()
        .map_err(|_| io::Error::other("Terminal client state unavailable"))?;
    let mut live = tap.0.live();
    if clients.sleeping
        || state.paste
        || state.matched != 0
        || live.revision != revision
        || crate::status::now_ns().saturating_sub(live.last_input_ns) < 250_000_000
        || live.last_output.elapsed() < Duration::from_millis(100)
        || !accepts(&live.engine.screen(), false)
    {
        return Err(io::Error::other(
            "Wait for an idle terminal with an empty native composer",
        ));
    }
    let bytes = if live
        .engine
        .input_modes()
        .contains(krill_proto::term_modes::Modes::BRACKETED_PASTE)
    {
        [START, command, END].concat()
    } else {
        command.to_vec()
    };
    // The screen authority and the first write share the live lock; a resize/output cannot
    // invalidate the inspected empty composer before these bytes enter the PTY.
    let written = write_once(tap, &bytes)?;
    state.record(bytes.get(..written).unwrap_or_default());
    state.owner = Some(thread::current().id());
    live.input_since_done = true;
    live.last_input_ns = crate::status::now_ns();
    drop(live);
    drop(clients);
    if written != bytes.len() {
        state.finish(tap).ok();
        return Err(io::Error::other(
            "Command delivery was partial; no Enter was sent",
        ));
    }
    // Codex coalesces fast text+Enter as paste. Wait for its actual echo, without allowing
    // another user's input to be interleaved between text and submission.
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_millis(1500) {
        thread::sleep(Duration::from_millis(25));
        let mut live = tap.0.live();
        if started.elapsed() < Duration::from_millis(250) || live.revision <= revision {
            continue;
        }
        if accepts(&live.engine.screen(), true) {
            let written = write_once(tap, b"\r")?;
            if written != 1 {
                return Err(io::Error::other("Command submission was not confirmed"));
            }
            live.last_input_ns = crate::status::now_ns();
            return Ok(());
        }
    }
    Err(io::Error::other(
        "Native command echo was not recognized; no Enter was sent",
    ))
}

#[cfg(windows)]
pub(super) fn guarded_command(
    _tap: &Tap,
    _revision: u64,
    _command: &[u8],
    _accepts: impl Fn(&krill_proto::screen::Screen, bool) -> bool,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Verified atomic model input is unavailable on Windows",
    ))
}

pub(super) fn write_input(tap: &Tap, bytes: &[u8], peer: Option<BorrowedFd<'_>>) -> io::Result<()> {
    let mut state = turn(tap, peer)?;
    {
        let clients = tap
            .0
            .clients
            .lock()
            .map_err(|_| io::Error::other("terminal client state poisoned"))?;
        if clients.sleeping {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let mut live = tap.0.live();
        live.input_since_done = true;
        live.last_input_ns = crate::status::now_ns();
    }
    let owner = thread::current().id();
    // 이전 연결의 정리가 다른 입력과 경합해 잠금을 못 얻었어도, 그 연결의 paste를 이어받지 않는다.
    if state.orphaned(owner) {
        state.finish(tap)?;
        state.matched = 0;
        state.peer = None;
    }
    if !state.paste && state.matched == 0 {
        state.owner = Some(owner);
        state.peer = None;
    }
    let result = write_all(tap, bytes, peer, &mut state);
    if (result.is_err() && state.owner == Some(owner)) || state.orphaned(owner) {
        // 본문은 버리고 종료 표지만 보충한다. 입력 큐가 차도 엔진 잠금은 쥐지 않는다.
        // 자식이 읽지 않는 동안 이 쓰기 스레드만 기다리며, 다른 잠금 대기자는 HUP으로 취소할 수 있다.
        state.finish(tap).ok();
    }
    result
}

pub(super) fn finish_input(tap: &Tap) {
    let mut state = match tap.0.input.try_lock() {
        Ok(state) => state,
        // 상태가 불확실하면 종료 표지조차 임의로 PTY에 쓰지 않는다.
        Err(TryLockError::Poisoned(_) | TryLockError::WouldBlock) => return,
    };
    if state.owner == Some(thread::current().id()) {
        state.finish(tap).ok();
        state.matched = 0;
        state.owner = None;
        state.peer = None;
    }
}

#[cfg(all(test, unix))]
mod tests;
