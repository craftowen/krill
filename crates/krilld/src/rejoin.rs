//! holder 재결합(OWE-479, docs/spikes/MK-018.md '재결합 절차'): holder를 잃은 krilld의 고아 운영과, 새 holder가
//! 띄운 krilld로의 인계.
//!
//! - **고아 운영**([`orphan`]): holder 링크가 끊기면 krilld는 끝나지 않고 세션을 계속 서빙한다. 세션 종료는
//!   `EVFILT_PROC NOTE_EXIT`로 감지하고 상태는 [`STATUS_UNKNOWN`]이다. 새 세션은 holder가 없어 거절된다
//!   (`Failure::NoHolder`). 그리고 `<소켓>.hnd`에서 다음 krilld를 기다린다.
//! - **인계**: 새 holder가 띄운 krilld#2는 소켓 락(MK-022)을 못 잡으면 `<소켓>.hnd`에 붙는다([`Handover::take`]).
//!   krilld#1은 PTY 리더를 모두 멈추고(세그먼트 봉인) holder 기동과 같은 순서(`Hello` → `Session`+master… → `Ready`,
//!   [`krill_proto::holder`] 메시지)로 세션을 넘긴다. krilld#2는 세션을 띄우고 holder에 `Adopt`로 맡긴 뒤
//!   `Ready`를 돌려준다([`adopt`], [`Handover::finish`]). 그 답을 받은 krilld#1이 끝나 소켓 락이 풀리면 krilld#2가
//!   소켓을 게시한다.
//! - krilld#2가 `Ready` 전에 끊기면 krilld#1은 master를 그대로 쥐고(리더는 멈춘 채) 다음 krilld를 기다린다. 어느
//!   순간에도 master 사본이 하나 이상 있어 세션은 SIGHUP을 받지 않는다. 리더를 멈춘 뒤 넘기므로 원본은 두 krilld에
//!   나뉘어 읽히지 않고, krilld#2는 봉인된 세그먼트 끝에서 이어 기록한다(무손실, ARCHITECTURE §4.1 'holder만 크래시').
//!
//! 잠정 결정 의존: holder 프로토콜 v2 `Adopt`·[`STATUS_UNKNOWN`](OWE-478, 사람 확인 대기). 거부되면 이 모듈과
//! `main.rs`의 인계 분기, `sys::ExitWatch`, `session::halt`를 되돌린다.
//! 개발 경로의 재기동 주체는 클라이언트(OWE-493): 인계 소켓이 준비되면 감시 연결에 holder 상실을 알리고,
//! `krill_proto::daemon::connect`가 기동 락 아래 새 `krill-holder --detach`를 띄운다. 서명 경로는 MK-106에서 전환한다.

use std::collections::HashMap;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::holder::{FromHolder, STATUS_UNKNOWN, ToHolder};
use krill_proto::os::net::{UnixListener, UnixStream};

#[cfg(unix)]
use crate::holder;
use crate::holder::Kept;
use crate::holder::link::Link;
use crate::ipc::{Listener, with_suffix};
use crate::os::{AsFd, OwnedFd};
use crate::registry::Registry;
use crate::sys::{self, ExitWatch};

/// 인계 소켓 이름 = 소켓 경로 + 이 접미사. `.tmp`와 길이가 같아 [`Listener::bind`]의 경로 길이 검사 안에 든다.
const SUFFIX: &str = ".hnd";
/// 인계 중 상대를 기다리는 최대 시간(멈춘 상대가 고아 krilld를 붙잡지 못하게).
#[cfg(unix)]
const PEER_TIMEOUT: Duration = Duration::from_secs(10);
/// 리더가 멈추기를 기다리는 최대 시간. 리더는 읽은 묶음 하나(최대 64KiB)를 엔진에 넣고 멈춘다.
const HALT_TIMEOUT: Duration = Duration::from_secs(2);
/// krilld#1이 끝나 소켓 락이 풀리기를 기다리는 최대 시간.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

fn handoff_path(sock: &Path) -> PathBuf {
    with_suffix(sock, SUFFIX)
}

/// holder 링크가 끊긴 뒤(리더 스레드에서) 부른다. 세션 종료 감시를 걸고 다음 krilld를 기다린다. 세션을 넘겼으면
/// `Ok` — 호출자는 프로세스를 끝낸다. 인계 소켓을 열지 못하면 오류(고아 운영만 계속한다).
pub fn orphan(reg: &Arc<Registry>, sock: &Path) -> io::Result<()> {
    watch_exits(reg);
    let path = handoff_path(sock);
    // 소켓 락을 쥔 이 krilld만 이 이름을 쓴다. 남은 파일은 고아인 채 죽은 이전 krilld의 것이다.
    match fs::remove_file(&path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let lis = UnixListener::bind(&path)?;
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    reg.holder_lost();
    let uid = crate::os::euid();
    loop {
        let Ok((s, _)) = lis.accept() else { continue };
        if sys::peer_uid(s.as_fd()).ok() != Some(uid) {
            continue;
        }
        if give(reg, s).is_ok() {
            fs::remove_file(&path).ok();
            return Ok(());
        }
    }
}

/// 끝나지 않은 세션마다 `NOTE_EXIT`를 걸고, 끝나면 [`STATUS_UNKNOWN`]으로 알린다(holder가 없어 종료 코드는 없다).
// ponytail: holder가 죽은 뒤 등록하기 전에 세션이 끝나고 pid가 재사용되면 엉뚱한 프로세스를 본다. 창은 holder 링크
// EOF부터 등록까지(수 ms 추정). 줄이려면 등록 뒤 proc 시작 시각을 세션 시작 시각과 대조한다.
fn watch_exits(reg: &Arc<Registry>) {
    let Ok(watch) = ExitWatch::new() else { return };
    let mut by_pid = HashMap::new();
    for (id, pid) in reg.running() {
        match watch.add(pid) {
            Ok(()) => {
                by_pid.insert(pid, id);
            }
            Err(_) => reg.on_exited(id, STATUS_UNKNOWN), // ESRCH: 이미 끝났다
        }
    }
    if by_pid.is_empty() {
        return;
    }
    let reg = Arc::clone(reg);
    thread::Builder::new()
        .name("orphan-exit".to_owned())
        .spawn(move || {
            while !by_pid.is_empty() {
                let Ok(pid) = watch.next() else { return };
                if let Some(id) = by_pid.remove(&pid) {
                    reg.on_exited(id, STATUS_UNKNOWN);
                }
            }
        })
        .ok();
}

/// 리더를 멈추고 세션을 모두 넘긴 뒤 상대의 `Ready`를 기다린다.
#[cfg(unix)]
fn give(reg: &Registry, s: UnixStream) -> io::Result<()> {
    halt_all(reg)?;
    s.set_read_timeout(Some(PEER_TIMEOUT))?;
    s.set_write_timeout(Some(PEER_TIMEOUT))?;
    let mut link = Link::new(crate::os::link_fd(s)?);
    let hello = FromHolder::Hello {
        version: krill_proto::holder::VERSION,
        holder_pid: std::process::id(),
        inbox: false,
    };
    send(&link, &hello, None)?;
    let mut sent = Ok(());
    reg.each(|id, pid, status, session| {
        if sent.is_ok() {
            let m = FromHolder::Session { id, pid, status };
            sent = send(&link, &m, Some(session));
        }
    });
    sent?;
    send(&link, &FromHolder::Ready, None)?;
    let p = link.recv()?.ok_or(io::ErrorKind::UnexpectedEof)?;
    match holder::decode(p)? {
        Some((FromHolder::Ready, _)) => Ok(()),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

/// Windows 고아 재결합 전송로는 아직 연결하지 않는다(OWE-833-8).
#[cfg(windows)]
fn give(_reg: &Registry, _stream: UnixStream) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// PTY 리더를 모두 멈추고(읽은 묶음은 기록·세그먼트 봉인까지) 끝나기를 기다린다. 되돌릴 수 없다.
/// 인계와 업그레이드([`crate::upgrade`])가 쓴다.
pub(crate) fn halt_all(reg: &Registry) -> io::Result<()> {
    crate::session::halt()?;
    let deadline = Instant::now() + HALT_TIMEOUT;
    while !all_halted(reg) {
        if Instant::now() > deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn all_halted(reg: &Registry) -> bool {
    let mut all = true;
    reg.each(|_, _, _, s| all &= s.is_closed());
    all
}

fn send(link: &Link, m: &FromHolder, with: Option<&crate::Session>) -> io::Result<()> {
    let frame = m
        .encode()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    match with {
        Some(s) => link.send(&frame, &[s.master()]),
        None => link.send(&frame, &[]),
    }
}

/// krilld#2 쪽: 고아 krilld에게서 받은 세션. [`Handover::merge`] → 레지스트리 생성 → [`adopt`] →
/// [`Handover::finish`] 순서로 쓴다.
#[derive(Debug)]
pub struct Handover {
    link: Link,
    #[cfg(unix)]
    sessions: Vec<Kept>,
}

impl Handover {
    /// `sock`의 인계 소켓에 붙어 세션을 받는다. 고아 krilld가 없으면 오류(다른 krilld가 정상으로 떠 있다).
    ///
    /// # Errors
    /// 인계 소켓이 없거나 `Ready` 전에 끊기면 오류.
    #[cfg(unix)]
    pub fn take(sock: &Path) -> io::Result<Self> {
        let s = UnixStream::connect(handoff_path(sock))?;
        s.set_read_timeout(Some(PEER_TIMEOUT))?;
        s.set_write_timeout(Some(PEER_TIMEOUT))?;
        let mut link = Link::new(crate::os::link_fd(s)?);
        let (_, sessions, _) = holder::receive(&mut link)?;
        Ok(Self { link, sessions })
    }

    /// Windows에서는 Unix 소켓 기반 fd 재결합을 사용하지 않는다.
    #[cfg(windows)]
    pub fn take(_sock: &Path) -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// 받은 세션을 holder가 돌려준 목록 `kept`에 더하고, holder에 맡길 세션(끝나지 않은 것)의 id·pid·master 사본을
    /// 돌려준다. holder가 이미 가진 id(맡긴 뒤 krilld#2가 죽어 다시 인계받은 경우)는 버린다.
    ///
    /// # Errors
    /// master fd 복제 실패.
    #[cfg(unix)]
    pub fn merge(&mut self, kept: &mut Vec<Kept>) -> io::Result<Vec<(SessionId, u32, OwnedFd)>> {
        let mut adopt = Vec::new();
        for k in self.sessions.drain(..) {
            if kept.iter().any(|h| h.id == k.id) {
                continue;
            }
            if k.status.is_none() {
                adopt.push((k.id, k.pid, k.master.try_clone()?));
            }
            kept.push(k);
        }
        Ok(adopt)
    }

    /// Windows 세션 재결합은 별도 핸들 전송 경로가 필요하다.
    #[cfg(windows)]
    #[expect(clippy::unused_self, reason = "Unix merge와 같은 인계 진입점")]
    pub fn merge(&mut self, _kept: &mut Vec<Kept>) -> io::Result<Vec<(SessionId, u32, OwnedFd)>> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// `Ready`를 돌려주고 krilld#1이 끝나기를 기다린 뒤 소켓을 게시한다.
    ///
    /// # Errors
    /// krilld#1이 [`RELEASE_TIMEOUT`] 안에 소켓 락을 놓지 않으면 `AddrInUse`. 그 밖의 IO 오류.
    pub fn finish(mut self, sock: &Path) -> io::Result<Listener> {
        send(&self.link, &FromHolder::Ready, None)?;
        // krilld#1이 끝나며 연결이 닫힌다(시간 초과도 그냥 넘어간다: 락 대기에 상한이 있다).
        while let Ok(Some(_)) = self.link.recv() {}
        let deadline = Instant::now() + RELEASE_TIMEOUT;
        loop {
            match Listener::bind(sock) {
                Err(e) if e.kind() == io::ErrorKind::AddrInUse && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                r => return r,
            }
        }
    }
}

/// 넘겨받은 세션을 holder에 `Adopt`로 맡긴다(holder 리더 스레드가 돈 뒤). 그새 끝난 세션(`ESRCH`)은 끝난 것으로
/// 표시한다.
// ponytail: 그 밖의 실패(v1 holder의 ENOSYS 등)는 무시한다. 그 세션은 krilld가 master를 쥔 동안만 산다. 같은 빌드의
// holder는 v2 이상이라 생기지 않는다.
pub fn adopt(reg: &Registry, sessions: Vec<(SessionId, u32, OwnedFd)>) {
    let Some(h) = reg.holder() else { return };
    for (id, pid, master) in sessions {
        let r = h.request_with(|req| ToHolder::Adopt { req, id, pid }, &[master.as_fd()]);
        if let Ok((FromHolder::Failed { errno, .. }, _)) = r
            && errno == libc::ESRCH
        {
            reg.on_exited(id, STATUS_UNKNOWN);
        }
    }
}
