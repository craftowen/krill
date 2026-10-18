//! 연결 취소를 관찰하는 입력 직렬화와 프레임 경계를 넘는 bracketed paste 복구.

use crate::os::OwnedFd;
use std::sync::TryLockError;

use super::{BorrowedFd, Duration, MutexGuard, Tap, io, lock, thread};
use crate::os::AsFd as _;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
#[cfg(unix)]
use rustix::io::write;

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
            Err(TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
            Err(TryLockError::WouldBlock) => {
                waited = true;
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn write_all(
    tap: &Tap,
    mut bytes: &[u8],
    peer: Option<BorrowedFd<'_>>,
    state: &mut State,
) -> io::Result<()> {
    while !bytes.is_empty() {
        alive(tap, None)?;
        let result = {
            #[cfg(unix)]
            let _live = lock(&tap.0.live);
            #[cfg(unix)]
            let result = write(&tap.0.master, bytes).map_err(io::Error::from);
            #[cfg(windows)]
            let result = {
                use std::io::Write as _;
                let handle = tap.0.master.input.try_clone()?;
                std::fs::File::from(handle).write(bytes)
            };
            result
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
                #[cfg(unix)]
                {
                    let mut fds = [PollFd::new(&tap.0.master, PollFlags::OUT)]
                        .into_iter()
                        .chain(peer.map(|fd| PollFd::from_borrowed_fd(fd, PollFlags::HUP)))
                        .collect::<Vec<_>>();
                    match poll(&mut fds, Some(&WAIT)) {
                        Ok(_) | Err(Errno::INTR) => {}
                        Err(err) => return Err(err.into()),
                    }
                }
                #[cfg(windows)]
                thread::sleep(Duration::from_millis(20));
                alive(tap, peer)?;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

pub(super) fn write_input(tap: &Tap, bytes: &[u8], peer: Option<BorrowedFd<'_>>) -> io::Result<()> {
    let mut state = turn(tap, peer)?;
    {
        let clients = lock(&tap.0.clients);
        if clients.sleeping {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let mut live = lock(&tap.0.live);
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
        Err(TryLockError::Poisoned(error)) => error.into_inner(),
        Err(TryLockError::WouldBlock) => return,
    };
    if state.owner == Some(thread::current().id()) {
        state.finish(tap).ok();
        state.matched = 0;
        state.owner = None;
        state.peer = None;
    }
}
