//! 연결 수명에 맞춘 역할·뷰포트와 세션별 입력 직렬화. PTY 리더는 이 잠금을 잡지 않는다.

use crate::os::BorrowedFd;
use krill_proto::os::net::UnixStream;
use std::io::{self, Write};

use krill_proto::holder::WinSize;
use krill_proto::ipc;
use krill_proto::roles::{self, Ack, FEAT_ROLES, Request, Role};
use krill_proto::screen::{MAX_COLS, MAX_LINES, Msg};

use super::Tap;

#[derive(Default)]
pub(crate) struct Clients {
    next: u64,
    pub(crate) sleeping: bool,
    pub(crate) last_attached: Option<std::time::Instant>,
    applied: Option<WinSize>,
    /// Width before the first temporary mobile size owner (also works without a desktop).
    mobile_fallback: Option<WinSize>,
    /// 활동 순서. 마지막으로 활동한 interactive가 크기를 정한다.
    entries: Vec<Entry>,
}

struct Entry {
    id: u64,
    role: Role,
    size: WinSize,
    active: bool,
}

impl Clients {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn owner(&self) -> Option<&Entry> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.active && e.role.owns_size())
    }

    fn apply(&mut self, tap: &Tap) -> io::Result<()> {
        self.apply_with(tap, Tap::resize)
    }

    fn apply_with(
        &mut self,
        tap: &Tap,
        resize: impl FnOnce(&Tap, WinSize) -> io::Result<()>,
    ) -> io::Result<()> {
        let size = self.owner().map(|e| e.size).or(self.mobile_fallback);
        if let Some(size) = size
            && self.applied != Some(size)
        {
            // 오류가 OS 변경 뒤에 올 수도 있다. 다음 소유자는 실제 크기를 다시 적용해야 한다.
            self.applied = None;
            resize(tap, size)?;
            self.applied = Some(size);
        }
        if !self.entries.iter().any(|entry| {
            entry.active && matches!(entry.role, Role::InteractiveWithSize { owns_size: true })
        }) {
            self.mobile_fallback = None;
        }
        Ok(())
    }
}

/// 연결이 끝나면 장부에서 지운다. 화면·PTY·holder의 수명은 건드리지 않는다.
#[must_use = "연결이 끝날 때까지 역할 등록을 보관해야 한다"]
pub(crate) struct Client {
    tap: Tap,
    id: u64,
}

impl Client {
    pub(crate) fn new(tap: Tap, role: Role) -> io::Result<Self> {
        let mut clients = tap
            .clients()
            .lock()
            .map_err(|_| io::Error::other("terminal client state poisoned"))?;
        if clients.sleeping {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        clients.last_attached = Some(std::time::Instant::now());
        let size = tap.size()?;
        clients.next = clients
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("client ids exhausted"))?;
        let id = clients.next;
        clients.entries.push(Entry {
            id,
            role,
            size,
            active: false,
        });
        drop(clients);
        Ok(Self { tap, id })
    }

    pub(crate) fn request(&self, request: Request) -> Ack {
        let Ok(mut clients) = self.tap.clients().lock() else {
            return Ack::Closed;
        };
        let Some(at) = clients.entries.iter().position(|e| e.id == self.id) else {
            return Ack::Closed;
        };
        let mut e = clients.entries.remove(at);
        let acquire = matches!(request, Request::Resize { .. })
            && matches!(e.role, Role::InteractiveWithSize { owns_size: false });
        if acquire && clients.mobile_fallback.is_none() {
            clients.mobile_fallback = self.tap.size().ok();
        }
        match request {
            Request::Set(role) => {
                if e.role != role {
                    e.active = false;
                }
                e.role = role;
            }
            _ if e.role == Role::Observer => {
                clients.entries.insert(at, e);
                return Ack::Denied;
            }
            Request::Resize { cols, rows } => {
                if cols == 0 || rows == 0 || cols > MAX_COLS || rows > MAX_LINES {
                    clients.entries.insert(at, e);
                    return Ack::Invalid;
                }
                e.size = WinSize { cols, rows };
                if matches!(e.role, Role::InteractiveWithSize { .. }) {
                    e.role = Role::InteractiveWithSize { owns_size: true };
                    e.active = true;
                }
            }
            Request::Focus => e.active = true,
        }
        if request == Request::Focus || acquire {
            clients.entries.push(e);
        } else {
            clients.entries.insert(at, e);
        }
        if clients.apply(&self.tap).is_ok() {
            Ack::Applied
        } else {
            Ack::Closed
        }
    }

    /// 역할 검사·크기 변경은 장부 잠금 안에서, 쓰기는 그 밖에서 한다(PTY가 막혀도 새 연결이 붙는다, OWE-579).
    /// `peer`는 보낸 연결이다([`Tap::write_input`]).
    pub(super) fn input(&self, bytes: &[u8], peer: BorrowedFd<'_>) -> io::Result<()> {
        let mut clients = self
            .tap
            .clients()
            .lock()
            .map_err(|_| io::Error::other("terminal client state poisoned"))?;
        let Some(at) = clients.entries.iter().position(|e| e.id == self.id) else {
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        let mut e = clients.entries.remove(at);
        if e.role == Role::Observer {
            clients.entries.insert(at, e);
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        e.active = true;
        clients.entries.push(e);
        clients.apply(&self.tap)?;
        drop(clients);
        self.tap.write_input(bytes, Some(peer))
    }

    pub(super) fn can_modify(&self) -> bool {
        let Ok(clients) = self.tap.clients().lock() else {
            return false;
        };
        clients
            .entries
            .iter()
            .find(|e| e.id == self.id)
            .is_some_and(|e| e.role != Role::Observer)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let Ok(mut clients) = self.tap.clients().lock() else {
            return;
        };
        clients.entries.retain(|e| e.id != self.id);
        clients.last_attached = Some(std::time::Instant::now());
        clients.apply(&self.tap).ok();
    }
}

type Frame = (u8, Vec<u8>);

/// Attach/제어 요청 앞의 선택적 역할 지정. 이전 클라이언트는 첫 프레임을 그대로 처리한다.
pub(crate) fn first(stream: &mut UnixStream, features: u64) -> io::Result<(Role, Option<Frame>)> {
    let first = ipc::read_frame(stream)?;
    if let Some((k, body)) = &first
        && *k == roles::kind::ROLE
        && features & FEAT_ROLES != 0
    {
        let request = Request::decode(*k, body)?;
        if let Request::Set(role) = request {
            stream.write_all(&Ack::Applied.encode()?)?;
            return Ok((role, ipc::read_frame(stream)?));
        }
        stream.write_all(&Ack::Invalid.encode()?)?;
        return Ok((Role::Observer, None));
    }
    Ok((Role::Interactive, first))
}

#[cfg(all(test, unix))]
mod tests;
