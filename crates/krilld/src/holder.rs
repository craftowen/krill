//! holder 링크(OWE-486). holder가 krilld를 띄울 때 만든 socketpair가 krilld의 stdin이다. 프로토콜은
//! [`krill_proto::holder`], 전송로는 holder와 같은 [`krill_holder::link`].
//!
//! - 기동: holder가 먼저 `Hello` → 보관 중인 세션마다 `Session`(+master fd) → (`Listener`) → `Ready`를 보낸다.
//!   [`Holder::connect`]가 `Ready`까지 받는다. krilld가 `kill -9`로 죽었다 다시 떠도 세션은 여기로 돌아온다.
//! - listen 소켓: 새로 게시한 krilld가 [`Holder::hold_listener`]로 holder에 맡기고, 다시 뜬 krilld는 `Listener`로
//!   돌려받아 이어 쓴다(OWE-491). holder가 살아 있는 동안 소켓이 죽지 않아 클라이언트가 holder를 하나 더 띄우지 않는다.
//! - 요청은 아무 스레드에서나 [`Holder::request`]로 보내고 같은 `req`의 답을 기다린다. 답과 `Exited`는
//!   [`Holder::run`]을 도는 리더 스레드 하나가 받아 나눠 준다.
//! - 링크가 끊기면(holder 크래시) 기다리던 요청과 새 요청은 모두 `NotConnected`다. krilld는 끝나지 않고 들고 있는
//!   세션을 계속 서빙한다(docs/spikes/MK-018.md '재결합 절차' 2). 그 뒤의 고아 운영과 재결합은 [`crate::rejoin`].

use std::collections::HashMap;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};

use krill_holder::link::{Link, Packet};
use krill_proto::SessionId;
use krill_proto::holder::{Error as WireError, FromHolder, ToHolder};

use crate::session::lock;

/// holder가 다시 넘겨준 세션(krilld 재기동).
#[derive(Debug)]
pub struct Kept {
    pub id: SessionId,
    pub pid: u32,
    /// 이미 끝났으면 `waitpid` 원시 상태.
    pub status: Option<i32>,
    pub master: OwnedFd,
}

/// 답(`Spawned`면 master fd가 붙는다).
pub type Answer = (FromHolder, Option<OwnedFd>);

/// 답을 기다리는 요청들. `None`이면 링크가 끊겼다.
type Pending = Option<HashMap<u32, Sender<Answer>>>;

/// krilld 쪽 holder 링크. 보내기는 여러 스레드가 나눠 쓰고 받기는 [`Holder::run`] 하나가 한다.
#[derive(Debug)]
pub struct Holder {
    version: u16,
    /// 받는 쪽과 같은 소켓의 사본. 프레임 하나를 한 번에 보내도록 잠근다.
    out: Mutex<Link>,
    pending: Mutex<Pending>,
    next: AtomicU32,
}

impl Holder {
    /// holder 링크 `sock`에서 `Ready`까지 받는다. 돌려준 [`Link`]는 리더 스레드의 [`Holder::run`]에 넘긴다.
    ///
    /// # Errors
    /// `Ready` 전에 링크가 끊기거나 `Hello`가 없으면 오류.
    ///
    /// 마지막 값은 holder가 맡아 둔 listen 소켓(앞선 krilld가 [`Holder::hold_listener`]로 맡긴 것)이다.
    pub fn connect(sock: OwnedFd) -> io::Result<(Self, Link, Vec<Kept>, Option<OwnedFd>)> {
        let out = Link::new(sock.try_clone()?);
        let mut link = Link::new(sock);
        let (version, kept, listener) = receive(&mut link)?;
        let holder = Self {
            version,
            out: Mutex::new(out),
            pending: Mutex::new(Some(HashMap::new())),
            next: AtomicU32::new(1),
        };
        Ok((holder, link, kept, listener))
    }

    /// holder의 프로토콜 버전(`Hello.version`). v3 미만이면 `Terminate`가 없다.
    pub fn version(&self) -> u16 {
        self.version
    }

    /// An orphan daemon must not claim scheduled work for a disconnected holder.
    pub(crate) fn is_connected(&self) -> bool {
        lock(&self.pending).is_some()
    }

    /// `make(req)`를 보내고 같은 `req`의 답을 기다린다. holder는 요청마다 바로 답한다.
    ///
    /// # Errors
    /// 링크가 끊겼으면 `NotConnected`.
    pub fn request(&self, make: impl FnOnce(u32) -> ToHolder) -> io::Result<Answer> {
        self.request_with(make, &[])
    }

    /// [`Holder::request`]와 같고 요청에 `fds`를 붙인다(`Adopt`의 master).
    ///
    /// # Errors
    /// 링크가 끊겼으면 `NotConnected`.
    pub fn request_with(
        &self,
        make: impl FnOnce(u32) -> ToHolder,
        fds: &[BorrowedFd<'_>],
    ) -> io::Result<Answer> {
        let (req, rx) = self.register()?;
        if let Err(e) = self.send(&make(req), fds) {
            self.unregister(req);
            return Err(e);
        }
        rx.recv().map_err(|_| io::ErrorKind::NotConnected.into())
    }

    /// `UpgradeHolder{path}`(exec 핸드오프, MK-062). holder 계약대로 답이 올 때까지 다른 스레드가 아무것도 보내지
    /// 못하게 송신 잠금을 쥔 채 기다린다. 답은 새 holder 이미지가 보낸다.
    ///
    /// # Errors
    /// 링크가 끊겼으면 `NotConnected`.
    pub fn upgrade_holder(&self, path: Vec<u8>) -> io::Result<Answer> {
        let out = lock(&self.out);
        let (req, rx) = self.register()?;
        let sent = ToHolder::UpgradeHolder { req, path }
            .encode()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
            .and_then(|frame| out.send(&frame, &[]));
        if let Err(e) = sent {
            self.unregister(req);
            return Err(e);
        }
        rx.recv().map_err(|_| io::ErrorKind::NotConnected.into())
    }

    fn register(&self) -> io::Result<(u32, mpsc::Receiver<Answer>)> {
        let req = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        lock(&self.pending)
            .as_mut()
            .ok_or(io::ErrorKind::NotConnected)?
            .insert(req, tx);
        Ok((req, rx))
    }

    fn unregister(&self, req: u32) {
        if let Some(p) = lock(&self.pending).as_mut() {
            p.remove(&req);
        }
    }

    /// 답을 기다리지 않고 보낸다(리더 스레드 안에서는 [`Holder::request`]를 쓰면 스스로를 기다린다). 답은 버려진다.
    pub fn notify(&self, make: impl FnOnce(u32) -> ToHolder) -> io::Result<()> {
        self.send(&make(self.next.fetch_add(1, Ordering::Relaxed)), &[])
    }

    /// listen 소켓을 holder에 맡긴다(`HoldListener`, 답은 버린다). 클라이언트를 받기 전에 보내야 세션을 가진
    /// holder는 늘 소켓도 쥐고 있다.
    pub fn hold_listener(&self, fd: BorrowedFd<'_>) -> io::Result<()> {
        let req = self.next.fetch_add(1, Ordering::Relaxed);
        self.send(&ToHolder::HoldListener { req }, &[fd])
    }

    fn send(&self, m: &ToHolder, fds: &[BorrowedFd<'_>]) -> io::Result<()> {
        let frame = m
            .encode()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        lock(&self.out).send(&frame, fds)
    }

    /// 리더: 답을 기다리는 요청에 나눠 주고 `Exited` 처리는 별도 큐로 넘긴다. 그 큐의 `Release` 송신이
    /// exec 핸드오프의 송신 잠금을 기다려도 리더는 `Done`을 받아 잠금을 풀 수 있다. 링크가 끊기면 기다리던
    /// 요청을 모두 `NotConnected`로 끝낸 뒤 종료 이벤트 큐를 비운다.
    pub fn run(&self, mut link: Link, mut exited: impl FnMut(SessionId, i32) + Send) {
        std::thread::scope(|scope| {
            let (tx, rx) = mpsc::channel();
            let worker = std::thread::Builder::new()
                .name("holder-events".to_owned())
                .spawn_scoped(scope, move || {
                    for (id, status) in rx {
                        // 한 세션의 종료 처리가 panic해도 워커는 살아남는다. 워커가 죽으면
                        // 이후 `Exited`가 버려져 closing 세션이 Release되지 않는다(K-06).
                        if panic::catch_unwind(AssertUnwindSafe(|| exited(id, status))).is_err() {
                            tracing::warn!(session = id.0, "holder exited handler panicked");
                        }
                    }
                });
            if worker.is_ok() {
                // `decode`·송신 panic에도 `pending` 정리가 실행되게 격리한다.
                // 정리 없이 죽으면 holder 요청이 영원히 걸린다(K-06).
                if panic::catch_unwind(AssertUnwindSafe(|| {
                    self.read(&mut link, |id, status| {
                        tx.send((id, status)).ok();
                    });
                }))
                .is_err()
                {
                    tracing::warn!("holder link reader panicked; treating link as down");
                }
            }
            *lock(&self.pending) = None;
            drop(tx);
        });
    }

    fn read(&self, link: &mut Link, mut exited: impl FnMut(SessionId, i32)) {
        while let Ok(Some(p)) = link.recv() {
            let Ok(m) = decode(p) else { break }; // 프로토콜 위반: 끊긴 것으로 본다
            match m {
                Some((FromHolder::Exited { id, status }, _)) => exited(id, status),
                Some((
                    m @ (FromHolder::Spawned { req, .. }
                    | FromHolder::Done { req }
                    | FromHolder::Failed { req, .. }),
                    fd,
                )) => {
                    let tx = lock(&self.pending).as_mut().and_then(|p| p.remove(&req));
                    if let Some(tx) = tx {
                        tx.send((m, fd)).ok();
                    }
                }
                _ => {}
            }
        }
    }
}

/// `Hello` → `Session`(+master)… → (`Listener`) → `Ready`를 받아 `Hello.version`, 세션 목록, 맡긴 listen 소켓을
/// 돌려준다. holder 기동 순서와 같고, krilld 간 인계([`crate::rejoin`])도 같은 순서로 보낸다(`Listener` 없이).
///
/// # Errors
/// `Ready` 전에 링크가 끊기거나 `Hello`가 없으면 오류.
pub(crate) fn receive(link: &mut Link) -> io::Result<(u16, Vec<Kept>, Option<OwnedFd>)> {
    let mut version = None;
    let mut kept = Vec::new();
    let mut listener = None;
    loop {
        let p = link.recv()?.ok_or(io::ErrorKind::UnexpectedEof)?;
        match decode(p)? {
            Some((FromHolder::Hello { version: v, .. }, _)) => version = Some(v),
            Some((FromHolder::Session { id, pid, status }, Some(master))) => {
                kept.push(Kept {
                    id,
                    pid,
                    status,
                    master,
                });
            }
            Some((FromHolder::Listener, fd)) => listener = fd,
            Some((FromHolder::Ready, _)) => break,
            _ => {}
        }
    }
    Ok((version.ok_or(io::ErrorKind::InvalidData)?, kept, listener))
}

/// 모르는 kind(새 holder)는 붙은 fd를 닫고 `None`. 그 밖의 디코드 실패는 오류.
pub(crate) fn decode(mut p: Packet) -> io::Result<Option<Answer>> {
    match FromHolder::decode(p.frame()) {
        Ok(m) => Ok(Some((m, p.fds.pop()))),
        Err(WireError::UnknownKind(_)) => Ok(None),
        Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
    }
}
