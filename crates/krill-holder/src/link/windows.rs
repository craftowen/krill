//! Windows holder 전송로: 상속된 익명 파이프 두 개와 v4 핸들 프레임.
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsHandle, BorrowedHandle, BorrowedSocket, OwnedHandle};

use crate::sys;
pub use crate::sys::{duplicate_socket, restore_socket};
use krill_proto::holder::{Frame, FromHolder, parse_frame};

/// `ConPTY` 수명은 signal 사본으로 유지한다. 입출력 핸들만으로는 유지되지 않는다.
#[derive(Debug)]
pub struct Pty {
    pub input: OwnedHandle,
    pub output: OwnedHandle,
    pub signal: OwnedHandle,
    pub reference: Option<OwnedHandle>,
    pub process: Option<OwnedHandle>,
}
impl Pty {
    pub fn from_handles(handles: Vec<OwnedHandle>) -> io::Result<Self> {
        let mut it = handles.into_iter();
        Ok(Self {
            input: it.next().ok_or(io::ErrorKind::InvalidData)?,
            output: it.next().ok_or(io::ErrorKind::InvalidData)?,
            signal: it.next().ok_or(io::ErrorKind::InvalidData)?,
            reference: it.next(),
            process: it.next(),
        })
    }
    pub fn handles(&self) -> Vec<BorrowedHandle<'_>> {
        let mut v = vec![
            self.input.as_handle(),
            self.output.as_handle(),
            self.signal.as_handle(),
        ];
        v.extend(self.reference.as_ref().map(AsHandle::as_handle));
        v.extend(self.process.as_ref().map(AsHandle::as_handle));
        v
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Self::from_handles(
            self.handles()
                .iter()
                .map(BorrowedHandle::try_clone_to_owned)
                .collect::<io::Result<_>>()?,
        )
    }
}
#[derive(Debug)]
pub struct Packet {
    pub kind: u8,
    pub body: Vec<u8>,
    pub fds: Vec<OwnedHandle>,
}
impl Packet {
    /// 핸들 값은 이미 소유권으로 바꿨으므로 Unix 디코더의 논리 fd 하나로 정규화한다.
    pub fn frame(&self) -> Frame<'_> {
        Frame {
            kind: self.kind,
            fds: u8::from(!self.fds.is_empty()),
            body: &self.body,
        }
    }
}
#[derive(Debug)]
pub struct Link {
    pub(crate) input: File,
    pub(crate) output: File,
    buf: Vec<u8>,
    pub peer_pid: Option<u32>,
}
impl Link {
    pub fn pair() -> io::Result<(Self, Self)> {
        let (ar, bw) = sys::pipe()?;
        let (br, aw) = sys::pipe()?;
        Ok((Self::from_handles(ar, aw), Self::from_handles(br, bw)))
    }
    pub fn from_handles(input: OwnedHandle, output: OwnedHandle) -> Self {
        Self {
            input: input.into(),
            output: output.into(),
            buf: Vec::new(),
            peer_pid: None,
        }
    }
    /// 시작할 때 한 번만 상속 핸들을 회수한다. 같은 프로세스에서 두 번 호출하지 않는다.
    pub fn inherited() -> io::Result<Self> {
        sys::inherited_link()
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            input: self.input.try_clone()?,
            output: self.output.try_clone()?,
            buf: Vec::new(),
            peer_pid: None,
        })
    }
    pub fn has_pending(&self) -> bool {
        !self.buf.is_empty()
    }
    /// Windows 소켓은 `WSADuplicateSocket` 경로로 전달해야 한다.
    pub fn send(&self, frame: &[u8], sockets: &[BorrowedSocket<'_>]) -> io::Result<()> {
        if !sockets.is_empty() {
            return Err(io::ErrorKind::Unsupported.into());
        }
        (&self.output).write_all(frame)
    }
    pub fn send_message(
        &self,
        message: &FromHolder,
        handles: &[BorrowedHandle<'_>],
        target: BorrowedHandle<'_>,
    ) -> io::Result<()> {
        let remote = sys::Remote::duplicate(target, handles)?;
        let frame = message.encode_handles(remote.values()).map_err(invalid)?;
        (&self.output).write_all(&frame)?;
        remote.commit();
        Ok(())
    }
    pub fn recv(&mut self) -> io::Result<Option<Packet>> {
        loop {
            if let Some(p) = self.try_next()? {
                return Ok(Some(p));
            }
            if !self.fill()? {
                return if self.buf.is_empty() {
                    Ok(None)
                } else {
                    Err(io::ErrorKind::UnexpectedEof.into())
                };
            }
        }
    }
    pub fn fill_nowait(&mut self) -> io::Result<bool> {
        if sys::available(self.input.as_handle())? == 0 {
            return Ok(true);
        }
        self.fill()
    }
    pub fn try_next(&mut self) -> io::Result<Option<Packet>> {
        let Some((f, total)) = parse_frame(&self.buf).map_err(invalid)? else {
            return Ok(None);
        };
        let (values, body) = f.handles().map_err(invalid)?;
        let packet = Packet {
            kind: f.kind,
            body: body.to_vec(),
            fds: sys::received_handles(values)?,
        };
        self.buf.drain(..total);
        Ok(Some(packet))
    }
    fn fill(&mut self) -> io::Result<bool> {
        let mut tmp = [0; 8192];
        let n = match self.input.read(&mut tmp) {
            Err(e) if e.raw_os_error() == Some(109) => 0,
            other => other?,
        };
        self.buf
            .extend(tmp.get(..n).ok_or(io::ErrorKind::InvalidData)?);
        Ok(n != 0)
    }
}
fn invalid(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frames_stay_separate_and_eof_is_clean() -> io::Result<()> {
        let (a, mut b) = Link::pair()?;
        let first = FromHolder::Done { req: 12 }
            .encode_handles(&[])
            .map_err(invalid)?;
        let second = FromHolder::Ready.encode_handles(&[]).map_err(invalid)?;
        a.send(&first, &[])?;
        a.send(&second, &[])?;
        drop(a);
        let packet = b.recv()?.ok_or(io::ErrorKind::UnexpectedEof)?;
        assert_eq!(
            FromHolder::decode(packet.frame()).map_err(invalid)?,
            FromHolder::Done { req: 12 }
        );
        let packet = b.recv()?.ok_or(io::ErrorKind::UnexpectedEof)?;
        assert_eq!(
            FromHolder::decode(packet.frame()).map_err(invalid)?,
            FromHolder::Ready
        );
        assert!(b.recv()?.is_none());
        Ok(())
    }
    #[test]
    fn truncated_frame_fails() -> io::Result<()> {
        let (a, mut b) = Link::pair()?;
        a.send(&[7, 0, 0], &[])?;
        drop(a);
        assert_eq!(
            b.recv().err().map(|e| e.kind()),
            Some(io::ErrorKind::UnexpectedEof)
        );
        Ok(())
    }
}
