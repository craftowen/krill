//! holder ↔ krilld socketpair 전송로: [`krill_proto::holder`] 프레임 + `SCM_RIGHTS` fd. holder와 krilld가 같이 쓴다.

use std::collections::VecDeque;
use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use krill_proto::holder::{Frame, parse_frame};
use rustix::io::{Errno, FdFlags, fcntl_setfd};
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};

/// 한 번의 `recvmsg`로 받을 수 있는 fd 수. 넘으면 커널이 fd를 버리므로(`MSG_CTRUNC`) 오류로 끊는다.
const MAX_RECV_FDS: usize = 64;
/// 한 프레임에 붙일 수 있는 fd 수(프레임 헤더 `fds`는 u8).
const MAX_SEND_FDS: usize = 8;

/// 받은 프레임 하나와 거기 붙은 fd.
#[derive(Debug)]
pub struct Packet {
    pub kind: u8,
    pub body: Vec<u8>,
    pub fds: Vec<OwnedFd>,
}

impl Packet {
    /// [`krill_proto::holder`]의 `decode`에 넘길 형태.
    pub fn frame(&self) -> Frame<'_> {
        Frame {
            kind: self.kind,
            fds: u8::try_from(self.fds.len()).unwrap_or(u8::MAX),
            body: &self.body,
        }
    }
}

/// 스트림 소켓 위의 프레임 송수신. 읽기 버퍼와 도착한 fd 대기열을 가진다.
#[derive(Debug)]
pub struct Link {
    sock: OwnedFd,
    buf: Vec<u8>,
    fds: VecDeque<OwnedFd>,
}

impl Link {
    pub fn new(sock: OwnedFd) -> Self {
        Self {
            sock,
            buf: Vec::new(),
            fds: VecDeque::new(),
        }
    }

    /// 아직 프레임이 되지 못한 읽기 바이트가 남았는지(exec 핸드오프 전에 확인).
    pub fn has_pending(&self) -> bool {
        !self.buf.is_empty() || !self.fds.is_empty()
    }

    /// 인코딩된 프레임 하나를 fd와 함께 보낸다. fd는 프레임 첫 바이트와 같은 `sendmsg`에 실린다.
    pub fn send(&self, frame: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_SEND_FDS))];
        let mut cmsg = SendAncillaryBuffer::new(&mut space);
        if !fds.is_empty() && !cmsg.push(SendAncillaryMessage::ScmRights(fds)) {
            return Err(io::Error::from(Errno::TOOMANYREFS));
        }
        let iov = [IoSlice::new(frame)];
        let mut sent = loop {
            match sendmsg(&self.sock, &iov, &mut cmsg, SendFlags::empty()) {
                Err(Errno::INTR) => {}
                r => break r?,
            }
        };
        while let Some(rest) = frame.get(sent..).filter(|r| !r.is_empty()) {
            match rustix::io::write(&self.sock, rest) {
                Err(Errno::INTR) => {}
                r => sent = sent.saturating_add(r?),
            }
        }
        Ok(())
    }

    /// 완성된 프레임이 올 때까지 막고 기다린다. 상대가 닫았으면 `Ok(None)`.
    pub fn recv(&mut self) -> io::Result<Option<Packet>> {
        loop {
            if let Some(p) = self.try_next()? {
                return Ok(Some(p));
            }
            if !self.fill(RecvFlags::empty())? {
                return Ok(None);
            }
        }
    }

    /// 막지 않고 소켓에 있는 만큼 읽는다(kqueue가 읽기 가능을 알린 뒤). 상대가 닫았으면 `Ok(false)`.
    pub fn fill_nowait(&mut self) -> io::Result<bool> {
        match self.fill(RecvFlags::DONTWAIT) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(true),
            r => r,
        }
    }

    /// 버퍼에 완성된 프레임이 있으면 꺼낸다.
    pub fn try_next(&mut self) -> io::Result<Option<Packet>> {
        let Some((f, total)) = parse_frame(&self.buf).map_err(invalid)? else {
            return Ok(None);
        };
        if usize::from(f.fds) > self.fds.len() {
            return Err(invalid("frame announces more fds than arrived"));
        }
        let p = Packet {
            kind: f.kind,
            body: f.body.to_vec(),
            fds: self.fds.drain(..usize::from(f.fds)).collect(),
        };
        self.buf.drain(..total);
        Ok(Some(p))
    }

    fn fill(&mut self, flags: RecvFlags) -> io::Result<bool> {
        let old = self.buf.len();
        self.buf.resize(old.saturating_add(64 * 1024), 0);
        let tmp = self.buf.get_mut(old..).unwrap_or_default();
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_RECV_FDS))];
        let mut cmsg = RecvAncillaryBuffer::new(&mut space);
        let got = loop {
            match recvmsg(&self.sock, &mut [IoSliceMut::new(tmp)], &mut cmsg, flags) {
                Err(Errno::INTR) => {}
                r => break r,
            }
        };
        let got = match got {
            Ok(g) => g,
            Err(e) => {
                self.buf.truncate(old);
                return Err(e.into());
            }
        };
        self.buf.truncate(old.saturating_add(got.bytes));
        for m in cmsg.drain() {
            if let RecvAncillaryMessage::ScmRights(it) = m {
                for fd in it {
                    // macOS에는 MSG_CMSG_CLOEXEC가 없다(MK-005 결정 2).
                    fcntl_setfd(&fd, FdFlags::CLOEXEC)?;
                    self.fds.push_back(fd);
                }
            }
        }
        if got.flags.contains(ReturnFlags::CTRUNC) {
            return Err(invalid("ancillary data truncated"));
        }
        Ok(got.bytes > 0)
    }
}

impl AsFd for Link {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.sock.as_fd()
    }
}

fn invalid(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}
