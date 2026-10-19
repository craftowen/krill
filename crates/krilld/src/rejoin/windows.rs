//! Same-user `AF_UNIX` handover; only handle values valid in the authenticated peer cross the wire.
use std::io::{self, Read as _, Write as _};
use std::os::windows::io::{AsHandle as _, AsSocket as _, OwnedHandle};

use krill_proto::holder::{FromHolder, MAX_FRAME, parse_frame};
use krill_proto::os::net::UnixStream;

use crate::holder::{self, Kept, Master, link};
use crate::sys;

#[derive(Debug)]
pub(super) struct SocketLink {
    stream: UnixStream,
    target: OwnedHandle,
}

impl SocketLink {
    pub(super) fn new(stream: UnixStream) -> io::Result<Self> {
        if sys::peer_uid(stream.as_socket())? != crate::os::euid() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let target = link::duplicate_target(sys::peer_pid(stream.as_socket())?)?;
        Ok(Self { stream, target })
    }

    pub(super) fn send(&self, frame: &[u8], _: &[crate::os::BorrowedFd<'_>]) -> io::Result<()> {
        (&self.stream).write_all(frame)
    }

    pub(super) fn send_session(&self, message: &FromHolder, master: &Master) -> io::Result<()> {
        let remote = link::Remote::duplicate(self.target.as_handle(), &master.handles())?;
        self.send(&remote.encode(message)?, &[])?;
        remote.commit();
        Ok(())
    }

    pub(super) fn recv(&mut self) -> io::Result<Option<link::Packet>> {
        let mut size = [0; 4];
        if self.stream.read(&mut size[..1])? == 0 {
            return Ok(None);
        }
        self.stream.read_exact(&mut size[1..])?;
        let len =
            usize::try_from(u32::from_le_bytes(size)).map_err(|_| io::ErrorKind::InvalidData)?;
        if !(2..=MAX_FRAME).contains(&len) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut bytes = Vec::from(size);
        bytes.resize(4 + len, 0);
        self.stream
            .read_exact(bytes.get_mut(4..).ok_or(io::ErrorKind::InvalidData)?)?;
        let (frame, _) = parse_frame(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .ok_or(io::ErrorKind::InvalidData)?;
        link::packet(frame).map(Some)
    }

    pub(super) fn receive(&mut self) -> io::Result<Vec<Kept>> {
        let mut sessions = Vec::new();
        let mut hello = false;
        loop {
            let packet = self.recv()?.ok_or(io::ErrorKind::UnexpectedEof)?;
            match holder::decode(packet)? {
                Some((FromHolder::Hello { version, .. }, _)) if version >= 4 => hello = true,
                Some((FromHolder::Session { id, pid, status }, Some(master))) if hello => {
                    sessions.push(Kept {
                        id,
                        pid,
                        status,
                        master,
                    });
                }
                Some((FromHolder::Ready, _)) if hello => return Ok(sessions),
                _ => return Err(io::ErrorKind::InvalidData.into()),
            }
        }
    }
}
