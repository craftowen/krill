//! holder 이벤트 루프: 세션 spawn·waitpid·master fd 보관, krilld 기동·재기동·fd 재전달, exec 핸드오프,
//! 다른 holder가 띄운 세션 `Adopt`(종료는 `EVFILT_PROC NOTE_EXIT`로만 감지, 종료 코드 없음).

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::sys::kq::{Event, EventFilter, EventFlags, ProcessEvents};
use krill_proto::SessionId;
use krill_proto::daemon::KRILLD_DUPLICATE;
use krill_proto::holder::Error::UnknownKind;
use krill_proto::holder::{
    FromHolder, MAX_SESSIONS, SESSION_LIMIT_ERRNO, STATUS_UNKNOWN, ToHolder, VERSION, WinSize,
    request_id,
};
use rustix::io::{Errno, FdFlags, fcntl_setfd};
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};
use rustix::process::{Pid, Signal, WaitOptions};

use crate::link::{Link, Packet};
use crate::sys;
use crate::teardown::Teardown;

/// krilld가 이만큼 살았으면 다음 크래시는 "첫 크래시"로 보고 즉시 재기동한다.
/// ponytail: MK-005 스파이크 값(1s·250ms·30s). 상수 확정은 오웬 확인 대기(MK-005 보고서).
const STABLE: Duration = Duration::from_secs(1);
const BACKOFF_BASE: Duration = Duration::from_millis(250);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// `RLIMIT_NOFILE` 목표. macOS는 `OPEN_MAX`(10240)보다 큰 소프트 한도를 거부한다.
const NOFILE: u64 = 10_240;
/// `UpgradeKrilld`로 바꾼 krilld가 뜨자마자 죽기를 이 횟수 연속으로 반복하면 바꾸기 전 경로로 되돌린다(OWE-589).
/// 백오프 1번째(즉시)·2번째(즉시)·3번째(250ms) 뒤라 롤백까지 1초 안에 끝난다.
const ROLLBACK_AFTER: u32 = 3;

#[derive(Debug)]
pub(crate) struct Session {
    pub id: SessionId,
    pub pid: Pid,
    pub master: OwnedFd,
    pub status: Option<i32>,
    /// `Adopt`로 받은 세션: holder의 자식이 아니라 `NOTE_EXIT`로만 종료를 안다.
    pub adopted: bool,
}

#[derive(Debug)]
struct Krilld {
    pid: Pid,
    /// 링크가 끊기면 `None`. 프로세스는 죽을 때까지 기다렸다 거둔다.
    link: Option<Link>,
    started: Instant,
}

#[derive(Debug)]
struct Holder {
    krilld_path: OsString,
    krilld_args: Vec<OsString>,
    sessions: Vec<Session>,
    listener: Option<OwnedFd>,
    krilld: Option<Krilld>,
    restart_at: Option<Instant>,
    /// 짧게 살고 죽은 연속 횟수(첫 크래시 = 1).
    streak: u32,
    /// `UpgradeKrilld`를 받았다 — 다음 종료는 크래시가 아니다.
    upgrading: bool,
    /// `UpgradeKrilld`로 바꾸기 전 경로. 새 경로가 안정(1초 이상 생존)되기 전 연속 실패가
    /// [`ROLLBACK_AFTER`]에 닿으면 되돌린다. exec 핸드오프를 거치면 잃는다(업그레이드 순서가 holder
    /// exec → krilld 경로 변경이라 잃을 일이 없다).
    prev_krilld: Option<OsString>,
    /// 현 krilld 경로가 뜨자마자 죽거나 뜨지 못한 연속 횟수(경로 변경·안정 생존 때 0).
    unstable: u32,
    /// 진행 중인 `Terminate` 단계들.
    teardowns: Vec<Teardown>,
    kq: OwnedFd,
}

/// `krill-holder <krilld 경로> [krilld 인자…]`. 정상 동작 중에는 돌아오지 않는다.
pub fn run(mut args: impl Iterator<Item = OsString>) -> io::Result<()> {
    let krilld_path = args.next().ok_or(Errno::INVAL)?;
    raise_nofile();
    let kq = sys::kq::kqueue()?;
    fcntl_setfd(&kq, FdFlags::CLOEXEC)?;
    let mut h = Holder {
        krilld_path,
        krilld_args: args.collect(),
        sessions: Vec::new(),
        listener: None,
        krilld: None,
        restart_at: Some(Instant::now()),
        streak: 0,
        upgrading: false,
        prev_krilld: None,
        unstable: 0,
        teardowns: Vec::new(),
        kq,
    };
    if let Some(state) = sys::take_handoff()? {
        h.restore(state);
    }
    // exec 복원 ACK를 로그 디렉터리·파일 IO 뒤로 늦추지 않는다(OWE-606).
    krill_log::init(krill_log::Component::Holder).ok();
    tracing::info!("holder starting");
    h.event_loop()
}

fn raise_nofile() {
    use rustix::process::{Resource, getrlimit, setrlimit};
    let mut l = getrlimit(Resource::Nofile);
    let want = l.maximum.map_or(NOFILE, |m| m.min(NOFILE));
    if l.current.is_some_and(|c| c < want) {
        l.current = Some(want);
        setrlimit(Resource::Nofile, l).ok(); // 실패해도 기본 한도로 계속 돈다
    }
}

impl Holder {
    fn event_loop(&mut self) -> io::Result<()> {
        let sigchld = Event::new(
            EventFilter::Signal {
                signal: Signal::CHILD,
                times: 0,
            },
            EventFlags::ADD,
            std::ptr::null_mut(),
        );
        let mut events = Vec::with_capacity(8);
        let mut changes = vec![sigchld];
        if self.reap() {
            return Ok(()); // 중복 기동: 건강한 데몬이 있어 끝낸다
        }
        loop {
            if self.restart_at.is_some_and(|at| at <= Instant::now()) {
                self.restart_at = None;
                self.spawn_krilld();
            }
            let now = Instant::now();
            for t in &mut self.teardowns {
                if t.due().is_some_and(|at| at <= now) {
                    t.step();
                }
            }
            self.teardowns.retain(|t| t.due().is_some());
            let link = self.krilld.as_ref().and_then(|k| k.link.as_ref());
            let link = link.map(|l| l.as_fd().as_raw_fd());
            if let Some(fd) = link {
                changes.push(Event::new(
                    EventFilter::Read(fd),
                    EventFlags::ADD,
                    std::ptr::null_mut(),
                ));
            }
            let next = self.teardowns.iter().filter_map(Teardown::due);
            let timeout = next
                .chain(self.restart_at)
                .min()
                .map(|at| at.saturating_duration_since(Instant::now()));
            sys::wait(self.kq.as_fd(), &changes, &mut events, timeout)?;
            changes.clear();
            // 읽기를 먼저 처리한다. 이 묶음 안에서 링크는 닫힐 수만 있고 새로 생기지 않는다.
            if events
                .iter()
                .any(|e| matches!(e.filter(), EventFilter::Read(fd) if Some(fd) == link))
            {
                self.read_link();
            }
            if events
                .iter()
                .any(|e| matches!(e.filter(), EventFilter::Signal { .. }))
                && self.reap()
            {
                return Ok(()); // 중복 기동: 건강한 데몬이 있어 끝낸다
            }
            for e in &events {
                if let EventFilter::Proc { pid, .. } = e.filter() {
                    self.on_adopted_exit(pid);
                }
            }
        }
    }

    fn send(&mut self, m: &FromHolder) {
        send(&mut self.krilld, m, None);
    }

    fn read_link(&mut self) {
        let Some(link) = self.krilld.as_mut().and_then(|k| k.link.as_mut()) else {
            return;
        };
        let mut alive = link.fill_nowait();
        // 요청마다 처리한다. exec 검사 때 뒤따른 프레임·fd가 Link에 남아 있어야 EBUSY로 거절한다.
        while let Some(link) = self.krilld.as_mut().and_then(|k| k.link.as_mut()) {
            match link.try_next() {
                Ok(Some(p)) => self.on_packet(p),
                Ok(None) => break,
                Err(e) => {
                    alive = Err(e);
                    break;
                }
            }
        }
        if !matches!(alive, Ok(true))
            && let Some(k) = self.krilld.as_mut()
        {
            drop_link(k);
        }
    }

    fn on_packet(&mut self, p: Packet) {
        let (req, result) = match ToHolder::decode(p.frame()) {
            Ok(m) => (m.req(), self.handle(m, p.fds)),
            Err(e) => match (request_id(&p.body), e) {
                (None, _) => return,
                (Some(req), UnknownKind(_)) => (req, Err(Errno::NOSYS.into())),
                (Some(req), _) => (req, Err(Errno::INVAL.into())),
            },
        };
        let reply = result.unwrap_or_else(|e| FromHolder::Failed {
            req,
            errno: e.raw_os_error().unwrap_or(Errno::IO.raw_os_error()),
        });
        let fd = match &reply {
            FromHolder::Spawned { id, .. } => self
                .sessions
                .iter()
                .find(|s| s.id == *id)
                .map(|s| s.master.as_fd()),
            _ => None,
        };
        send(&mut self.krilld, &reply, fd);
    }

    fn handle(&mut self, m: ToHolder, mut fds: Vec<OwnedFd>) -> io::Result<FromHolder> {
        let req = m.req();
        match m {
            ToHolder::Spawn {
                id,
                argv,
                env,
                cwd,
                size,
                ..
            } => {
                if self.find(id).is_some() {
                    return Err(Errno::EXIST.into());
                }
                if self.sessions.len() >= MAX_SESSIONS {
                    // 와이어 값은 OS와 무관하게 macOS EPROCLIM(67)이다(Linux에서는 다른 errno 번호지만 krilld는 값만 비교한다).
                    return Err(Errno::from_raw_os_error(SESSION_LIMIT_ERRNO).into());
                }
                let (pid, master) = spawn_session(&argv, &env, &cwd, size)?;
                self.sessions.push(Session {
                    id,
                    pid,
                    master,
                    status: None,
                    adopted: false,
                });
                return Ok(FromHolder::Spawned {
                    req,
                    id,
                    pid: pid.as_raw_pid().unsigned_abs(),
                });
            }
            ToHolder::Kill { id, signal, .. } => {
                let s = self.find(id).ok_or(Errno::SRCH)?;
                if s.status.is_some() {
                    return Err(Errno::SRCH.into());
                }
                let sig = Signal::from_named_raw(signal).ok_or(Errno::INVAL)?;
                rustix::process::kill_process_group(s.pid, sig)?;
            }
            ToHolder::Terminate { id, .. } => {
                let s = self.find(id).ok_or(Errno::SRCH)?;
                if s.status.is_some() {
                    return Err(Errno::SRCH.into());
                }
                self.teardowns.push(Teardown::start(s.pid));
            }
            // (v4) Unix는 krilld가 master fd에 직접 TIOCSWINSZ한다. Resize는 Windows ConPTY 전용.
            ToHolder::Resize { .. } => return Err(Errno::NOSYS.into()),
            ToHolder::Release { id, .. } => {
                let n = self.sessions.len();
                self.sessions.retain(|s| s.id != id);
                if self.sessions.len() == n {
                    return Err(Errno::SRCH.into());
                }
            }
            ToHolder::HoldListener { .. } => self.listener = fds.pop(),
            ToHolder::UpgradeKrilld { path, .. } => {
                let next = OsStr::from_bytes(&path).to_os_string();
                if next != self.krilld_path {
                    // 마지막으로 안정됐던 경로만 보관한다. 안정되기 전 또 바뀌면 처음 경로로 되돌린다.
                    if self.prev_krilld.is_none() {
                        self.prev_krilld = Some(std::mem::replace(&mut self.krilld_path, next));
                    } else {
                        self.krilld_path = next;
                    }
                    self.unstable = 0;
                }
                self.upgrading = true;
            }
            ToHolder::UpgradeHolder { path, .. } => return Err(self.exec_self(req, &path)),
            ToHolder::Adopt { id, pid, .. } => {
                if self.find(id).is_some() {
                    return Err(Errno::EXIST.into());
                }
                let pid = i32::try_from(pid).ok().and_then(Pid::from_raw);
                let pid = pid.ok_or(Errno::INVAL)?;
                self.watch_exit(pid)?; // 이미 없는 프로세스면 ESRCH
                let master = fds.pop().ok_or(Errno::INVAL)?;
                self.sessions.push(Session {
                    id,
                    pid,
                    master,
                    status: None,
                    adopted: true,
                });
            }
        }
        Ok(FromHolder::Done { req })
    }

    fn find(&self, id: SessionId) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// `waitpid`로 거둔 자식 `pid`의 종료를 붙일 세션. `Adopt` 세션은 holder의 자식이 아니라
    /// `waitpid` 대상이 아니다(`on_adopted_exit`가 처리). 이미 끝난(`status.is_some()`) 세션은
    /// 건너뛴다: pid가 순환해 새 세션에 재사용되면 종료가 옛 id로 가는 것(PH-01)을 막는다
    /// (`on_adopted_exit`과 같은 조건).
    fn reap_target(sessions: &mut [Session], pid: Pid) -> Option<&mut Session> {
        sessions
            .iter_mut()
            .find(|s| s.pid == pid && !s.adopted && s.status.is_none())
    }

    /// 끝난 자식을 모두 거둔다. `waitpid(-1)`: rustix `waitpid(None)`은 같은 프로세스 그룹만 본다(MK-004 결정 4).
    /// 중복 기동(세션 없이 떴는데 krilld가 [`KRILLD_DUPLICATE`]로 끝남)이면 `true` — 호출자는 재기동하지 않고 끝낸다.
    fn reap(&mut self) -> bool {
        let mut duplicate = false;
        while let Ok(Some((pid, st))) = rustix::process::wait(WaitOptions::NOHANG) {
            let status = st.as_raw();
            if self.krilld.as_ref().is_some_and(|k| k.pid == pid) {
                if st.exit_status() == Some(i32::from(KRILLD_DUPLICATE)) && self.sessions.is_empty()
                {
                    // 건강한 데몬이 소켓을 쥐고 있다. 세션이 없어 잃을 것이 없으니 재기동하지 않고 끝낸다.
                    self.krilld = None;
                    self.restart_at = None;
                    duplicate = true;
                } else {
                    self.on_krilld_exit();
                }
            } else if let Some(s) = Self::reap_target(&mut self.sessions, pid) {
                s.status = Some(status);
                let id = s.id;
                self.send(&FromHolder::Exited { id, status });
                self.teardowns.iter_mut().for_each(|t| t.exited(pid));
            }
        }
        duplicate
    }

    /// `pid`의 `NOTE_EXIT`를 지금 등록한다. `EV_RECEIPT`라 대기 중인 이벤트를 꺼내지 않고 등록 결과만
    /// `EV_ERROR` 이벤트(data = 0 또는 errno, 없는 프로세스면 ESRCH)로 받는다.
    fn watch_exit(&self, pid: Pid) -> io::Result<()> {
        let flags = ProcessEvents::EXIT;
        let ev = Event::new(
            EventFilter::Proc { pid, flags },
            EventFlags::ADD | EventFlags::RECEIPT,
            std::ptr::null_mut(),
        );
        let mut out = Vec::with_capacity(1);
        sys::wait(self.kq.as_fd(), &[ev], &mut out, Some(Duration::ZERO))?;
        match out.first().map(|e| i32::try_from(e.data())) {
            Some(Ok(0)) => Ok(()),
            Some(Ok(errno)) => Err(io::Error::from_raw_os_error(errno)),
            _ => Err(Errno::IO.into()),
        }
    }

    /// `Adopt`한 세션이 끝났다. 자식이 아니라 상태를 거둘 수 없다 → [`STATUS_UNKNOWN`].
    fn on_adopted_exit(&mut self, pid: Pid) {
        let mut live = self
            .sessions
            .iter_mut()
            .filter(|s| s.adopted && s.status.is_none());
        if let Some(s) = live.find(|s| s.pid == pid) {
            s.status = Some(STATUS_UNKNOWN);
            let id = s.id;
            self.send(&FromHolder::Exited {
                id,
                status: STATUS_UNKNOWN,
            });
            self.teardowns.iter_mut().for_each(|t| t.exited(pid));
        }
    }

    fn on_krilld_exit(&mut self) {
        let uptime = self
            .krilld
            .take()
            .map_or(Duration::ZERO, |k| k.started.elapsed());
        let handover = std::mem::take(&mut self.upgrading);
        self.streak = match (handover, uptime >= STABLE) {
            (true, _) => 0,
            (false, true) => 1,
            (false, false) => self.streak.saturating_add(1),
        };
        if !handover {
            if uptime >= STABLE {
                // 새 경로가 안정됐다: 롤백 대상을 버린다.
                self.prev_krilld = None;
                self.unstable = 0;
            } else {
                self.note_unstable();
            }
        }
        tracing::warn!(streak = self.streak, "daemon restart scheduled");
        self.restart_at = Some(Instant::now() + backoff(self.streak));
    }

    /// 현 krilld 경로가 한 번 실패했다(뜨자마자 죽거나 뜨지 못함). `UpgradeKrilld` 뒤 연속 실패가
    /// [`ROLLBACK_AFTER`]에 닿으면 바꾸기 전 경로로 되돌리고 처음부터 다시 센다.
    fn note_unstable(&mut self) {
        if self.prev_krilld.is_none() {
            return;
        }
        self.unstable = self.unstable.saturating_add(1);
        if self.unstable >= ROLLBACK_AFTER {
            if let Some(prev) = self.prev_krilld.take() {
                tracing::warn!("new krilld keeps failing, rolling back");
                self.krilld_path = prev;
            }
            self.unstable = 0;
            self.streak = 0;
        }
    }

    /// krilld를 띄우고 보관 중인 fd를 모두 넘긴다. 실패는 크래시와 같게 백오프한다.
    fn spawn_krilld(&mut self) {
        let started = (|| {
            let (ours, theirs) = socketpair(
                AddressFamily::UNIX,
                SocketType::STREAM,
                SocketFlags::empty(), // macOS에는 SOCK_CLOEXEC가 없다(holder는 단일 스레드)
                None,
            )?;
            fcntl_setfd(&ours, FdFlags::CLOEXEC)?;
            fcntl_setfd(&theirs, FdFlags::CLOEXEC)?;
            let child = Command::new(&self.krilld_path)
                .args(&self.krilld_args)
                .env_remove(sys::STATE_ENV)
                .env_remove(sys::STATE_ENV_ALT)
                .stdin(Stdio::from(theirs))
                .spawn()?;
            io::Result::Ok((child_pid(&child)?, ours))
        })();
        let Ok((pid, sock)) = started else {
            self.streak = self.streak.saturating_add(1);
            // 뜨지 못한 것도 실패로 센다(업그레이드 preflight를 피한 교체 등).
            self.note_unstable();
            self.restart_at = Some(Instant::now() + backoff(self.streak));
            return;
        };
        let link = Link::new(sock);
        let sessions = self.sessions.iter().map(|s| {
            let pid = s.pid.as_raw_pid().unsigned_abs();
            let (id, status) = (s.id, s.status);
            (
                FromHolder::Session { id, pid, status },
                Some(s.master.as_fd()),
            )
        });
        let listener = self
            .listener
            .as_ref()
            .map(|l| (FromHolder::Listener { info: Vec::new() }, Some(l.as_fd())));
        let ok = std::iter::once((hello(), None))
            .chain(sessions)
            .chain(listener)
            .chain([(FromHolder::Ready, None)])
            .try_for_each(|(m, fd)| send_to(&link, &m, fd))
            .is_ok();
        let mut k = Krilld {
            pid,
            link: Some(link),
            started: Instant::now(),
        };
        if !ok {
            drop_link(&mut k);
        }
        self.krilld = Some(k);
    }

    /// exec 핸드오프(MK-004 결정 1). 성공하면 돌아오지 않는다.
    fn exec_self(&mut self, req: u32, path: &[u8]) -> io::Error {
        let Some((k, Some(link))) = self.krilld.as_ref().map(|k| (k, k.link.as_ref())) else {
            return Errno::NOTCONN.into();
        };
        if link.has_pending() {
            return Errno::BUSY.into(); // 계약 위반: UpgradeHolder 뒤에 더 보냈다
        }
        // ponytail: 진행 중인 종료 단계는 새 이미지로 넘기지 않고 SIGKILL로 당겨 끝낸다(유예가 짧아진다).
        // 업그레이드 중 유예를 지켜야 하면 단계 상태를 STATE_ENV에 싣는다.
        self.teardowns.iter_mut().for_each(Teardown::finish);
        let listener = self.listener.as_ref().map_or(-1, AsRawFd::as_raw_fd);
        let mut state = format!(
            "{req} {} {} {listener} {} {}\n",
            k.pid.as_raw_pid(),
            link.as_fd().as_raw_fd(),
            self.streak,
            u8::from(self.upgrading),
        );
        for s in &self.sessions {
            let pid = s.pid.as_raw_pid();
            let pid = if s.adopted { -pid } else { pid }; // 음수 = Adopt한 세션(sys::STATE_ENV)
            let (id, fd) = (s.id.0, s.master.as_raw_fd());
            let st = s.status.map(|v| format!(" {v}")).unwrap_or_default();
            writeln!(state, "{id} {pid} {fd}{st}").ok();
        }
        let mut keep: Vec<BorrowedFd<'_>> = vec![link.as_fd()];
        keep.extend(self.listener.as_ref().map(AsFd::as_fd));
        keep.extend(self.sessions.iter().map(|s| s.master.as_fd()));
        let flip = |flags| {
            keep.iter()
                .try_for_each(|fd| fcntl_setfd(fd, flags).map_err(io::Error::from))
        };
        let err = match flip(FdFlags::empty()) {
            Ok(()) => Command::new(OsStr::from_bytes(path))
                .arg(&self.krilld_path)
                .args(&self.krilld_args)
                .env(sys::STATE_ENV, state)
                .exec(),
            Err(e) => e,
        };
        flip(FdFlags::CLOEXEC).ok();
        err
    }

    /// exec 핸드오프 뒤 새 이미지: 상태를 되찾고 krilld에 `Hello` → `Done{req}`를 보낸다.
    fn restore(&mut self, h: sys::Handoff) {
        self.sessions = h.sessions;
        self.listener = h.listener;
        self.krilld = Some(Krilld {
            pid: h.krilld,
            link: Some(Link::new(h.link)),
            started: Instant::now(),
        });
        self.restart_at = None;
        self.streak = h.streak;
        self.upgrading = h.upgrading;
        self.send(&hello());
        self.send(&FromHolder::Done { req: h.req });
        // 새 이미지의 kqueue는 비어 있다. Adopt한 세션의 NOTE_EXIT를 다시 걸고, 그새 끝났으면 알린다.
        let adopted: Vec<Pid> = self
            .sessions
            .iter()
            .filter(|s| s.adopted)
            .map(|s| s.pid)
            .collect();
        for p in adopted {
            if self.watch_exit(p).is_err() {
                self.on_adopted_exit(p);
            }
        }
    }
}

fn hello() -> FromHolder {
    FromHolder::Hello {
        version: VERSION,
        holder_pid: rustix::process::getpid().as_raw_pid().unsigned_abs(),
        inbox: false,
    }
}

/// krilld에 보낸다. 실패하면 링크를 버린다.
fn send(k: &mut Option<Krilld>, m: &FromHolder, fd: Option<BorrowedFd<'_>>) {
    let Some(k) = k.as_mut() else { return };
    let Some(link) = k.link.as_ref() else { return };
    if send_to(link, m, fd).is_err() {
        drop_link(k);
    }
}

fn send_to(link: &Link, m: &FromHolder, fd: Option<BorrowedFd<'_>>) -> io::Result<()> {
    let f = m.encode().map_err(|_| io::Error::from(Errno::MSGSIZE))?;
    link.send(&f, fd.as_slice())
}

/// 링크가 깨진 krilld는 쓸모가 없다. 죽여서 재기동 경로로 보낸다.
fn drop_link(k: &mut Krilld) {
    k.link = None;
    rustix::process::kill_process(k.pid, Signal::KILL).ok();
}

/// 연속 k번째 크래시의 재기동 지연: 1번째 0, 이후 base·2^(k-2), 상한 cap.
fn backoff(streak: u32) -> Duration {
    match streak.checked_sub(2) {
        None => Duration::ZERO,
        Some(n) => BACKOFF_BASE
            .saturating_mul(1u32.checked_shl(n.min(20)).unwrap_or(u32::MAX))
            .min(BACKOFF_CAP),
    }
}

fn spawn_session(
    argv: &[Vec<u8>],
    env: &[(Vec<u8>, Vec<u8>)],
    cwd: &[u8],
    size: WinSize,
) -> io::Result<(Pid, OwnedFd)> {
    let (prog, args) = argv.split_first().ok_or(Errno::INVAL)?;
    let ws = rustix::termios::Winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = rustix_openpty::openpty(None, Some(&ws))?;
    let mut cmd = Command::new(OsStr::from_bytes(prog));
    cmd.args(args.iter().map(|a| OsStr::from_bytes(a)))
        .env_clear()
        .envs(
            env.iter()
                .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
        )
        .stdin(Stdio::from(pty.user.try_clone()?))
        .stdout(Stdio::from(pty.user.try_clone()?))
        .stderr(Stdio::from(pty.user));
    if !cwd.is_empty() {
        cmd.current_dir(OsStr::from_bytes(cwd));
    }
    sys::set_controlling_tty(&mut cmd);
    Ok((child_pid(&cmd.spawn()?)?, pty.controller))
}

fn child_pid(c: &std::process::Child) -> io::Result<Pid> {
    let pid = i32::try_from(c.id()).map_err(|_| Errno::SRCH)?;
    Pid::from_raw(pid).ok_or_else(|| Errno::SRCH.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_crash_is_immediate_then_backoff() {
        let got: Vec<u128> = (1..=8).map(|k| backoff(k).as_millis()).collect();
        assert_eq!(got, [0, 250, 500, 1000, 2000, 4000, 8000, 16000]);
        assert_eq!(backoff(9), BACKOFF_CAP);
        assert_eq!(backoff(u32::MAX), BACKOFF_CAP);
    }

    fn test_session(id: u64, pid: i32, status: Option<i32>) -> Session {
        let file = std::fs::File::open("/dev/null").unwrap();
        Session {
            id: SessionId(id),
            pid: Pid::from_raw(pid).unwrap(),
            master: OwnedFd::from(file),
            status,
            adopted: false,
        }
    }

    #[test]
    fn reap_ignores_exited_session_when_pid_is_reused() {
        // PH-01: 끝난 채 열린 탭(옛 id, status 있음)과 pid 순환으로 같은 pid를 받은 새 세션 —
        // 거둔 종료는 살아 있는 새 세션에 붙어야 한다.
        let pid = Pid::from_raw(4242).unwrap();
        let mut sessions = vec![
            test_session(1, 4242, Some(0x700)),
            test_session(101, 4242, None),
        ];
        let hit = super::Holder::reap_target(&mut sessions, pid).map(|s| s.id);
        assert_eq!(hit, Some(SessionId(101)));
    }
}
