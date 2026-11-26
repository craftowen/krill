//! holder 이벤트 루프: 세션 spawn·waitpid·master fd 보관, krilld 기동·재기동·fd 재전달, exec 핸드오프,
//! 다른 holder가 띄운 세션 `Adopt`(종료는 `EVFILT_PROC NOTE_EXIT`로만 감지, 종료 코드 없음).

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::sys::kq::{Event, EventFilter, EventFlags, ProcessEvents};
use krill_proto::SessionId;
use krill_proto::daemon::{KRILLD_DUPLICATE, KRILLD_FATAL_CONFIG, KRILLD_HANDOVER_FAILED};
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

/// krilld가 이만큼 살았으면 다음 크래시는 첫 크래시로 보고 즉시 재기동한다(MK-005).
const STABLE: Duration = Duration::from_secs(1);
const BACKOFF_BASE: Duration = Duration::from_millis(250);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
const NOFILE: u64 = 10_240;
/// `UpgradeKrilld` 뒤 연속 실패가 이 횟수에 닿으면 이전 경로로 되돌린다(OWE-589).
const ROLLBACK_AFTER: u32 = 3;
/// 살아있는 세션 없는 holder의 연속 실패 포기 한도.
const GIVE_UP: u32 = 5;

#[derive(Debug)]
pub(crate) struct Session {
    pub id: SessionId,
    pub pid: Pid,
    pub master: OwnedFd,
    pub status: Option<i32>,
    /// `Adopt` 세션: `NOTE_EXIT`로만 종료 감지.
    pub adopted: bool,
}

#[derive(Debug)]
struct Krilld {
    pid: Pid,
    link: Option<Link>,
    started: Instant,
}

#[derive(Debug)]
struct Holder {
    krilld_path: OsString,
    krilld_args: Vec<OsString>,
    sessions: Vec<Session>,
    listener: Option<OwnedFd>,
    owner: Option<File>,
    krilld: Option<Krilld>,
    restart_at: Option<Instant>,
    streak: u32,
    upgrading: bool,
    /// `UpgradeKrilld` 직전 경로(안정 전 [`ROLLBACK_AFTER`]회 실패 시 복구).
    prev_krilld: Option<OsString>,
    unstable: u32,
    teardowns: Vec<Teardown>,
    kq: OwnedFd,
}

/// `krill-holder <krilld 경로> [krilld 인자…]`. 정상 동작 중에는 돌아오지 않는다.
pub fn run(mut args: impl Iterator<Item = OsString>) -> io::Result<()> {
    let krilld_path = args.next().ok_or(Errno::INVAL)?;
    raise_nofile();
    let kq = sys::kq::kqueue()?;
    fcntl_setfd(&kq, FdFlags::CLOEXEC)?;
    sys::catch_stop_signals();
    let mut h = Holder {
        krilld_path,
        krilld_args: args.collect(),
        sessions: Vec::new(),
        listener: None,
        owner: None,
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
    // exec 복원 ACK를 로그 디렉터리·소켓 소유 락 파일 IO 뒤로 늦추지 않는다(OWE-606).
    krill_log::init(krill_log::Component::Holder).ok();
    h.owner = h.listener.as_ref().and_then(own_socket);
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
        let null = std::ptr::null_mut();
        let signals = [Signal::CHILD, Signal::TERM, Signal::HUP, Signal::INT].map(|signal| {
            let filter = EventFilter::Signal { signal, times: 0 };
            Event::new(filter, EventFlags::ADD | EventFlags::RECEIPT, null)
        });
        let mut events = Vec::with_capacity(8);
        sys::wait(self.kq.as_fd(), &signals, &mut events, Some(Duration::ZERO))?;
        let err = |e: &Event| i32::try_from(e.data()).ok().filter(|&n| n != 0);
        if let Some(code) = events.iter().find_map(err) {
            return Err(io::Error::from_raw_os_error(code));
        }
        self.reap();
        loop {
            if self.krilld.is_none()
                && self.restart_at.is_none()
                && self.sessions.iter().all(|s| s.status.is_some())
            {
                return Ok(()); // 재기동을 멈췄고(중복 기동·포기) 쥔 세션도 모두 끝났다
            }
            // 종료 신호(기동 직후·대기 중 모두 루프 맨 앞에서 본다): 사유를 남기고 끝낸다. 세션은 master fd를 쥔 krilld가 고아로 이어 받는다(rejoin). SIGKILL은 못 남긴다.
            let stop = sys::STOPPED.load(std::sync::atomic::Ordering::Relaxed);
            if stop != 0 {
                // krilld가 없으면(재기동 대기·링크 끊김) master 사본은 우리 것뿐이라 새 krilld에 넘기고 끝낸다(C-5).
                self.reap();
                if !self.sessions.is_empty()
                    && self.krilld.as_ref().is_none_or(|k| k.link.is_none())
                {
                    self.spawn_krilld();
                }
                let (signal, from, sessions) =
                    (stop >> 32, stop & 0xffff_ffff, self.sessions.len());
                tracing::warn!(signal, from, sessions, "holder stopping");
                return Ok(());
            }
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
            let read = link.map(|fd| Event::new(EventFilter::Read(fd), EventFlags::ADD, null));
            let next = self.teardowns.iter().filter_map(Teardown::due);
            let timeout = next
                .chain(self.restart_at)
                .min()
                .map(|at| at.saturating_duration_since(Instant::now()));
            sys::wait(self.kq.as_fd(), read.as_slice(), &mut events, timeout)?;
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
            {
                self.reap();
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
            ToHolder::HoldListener { .. } => {
                self.owner = None; // 같은 파일을 새로 잠그기 전에 놓는다
                self.listener = fds.pop();
                self.owner = self.listener.as_ref().and_then(own_socket);
            }
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
    fn reap(&mut self) {
        while let Ok(Some((pid, st))) = rustix::process::wait(WaitOptions::NOHANG) {
            let status = st.as_raw();
            if self.krilld.as_ref().is_some_and(|k| k.pid == pid) {
                let code = st.exit_status().and_then(|c| u8::try_from(c).ok());
                let idle = self.sessions.iter().all(|s| s.status.is_some());
                if (code == Some(KRILLD_DUPLICATE)
                    && (self.owner.is_none() || self.streak >= GIVE_UP))
                    || (code == Some(KRILLD_FATAL_CONFIG) && idle)
                {
                    tracing::warn!(sessions = self.sessions.len(), ?code, "daemon duplicate");
                    (self.owner, self.listener, self.krilld, self.restart_at) =
                        (None, None, None, None);
                } else {
                    self.on_krilld_exit(code == Some(KRILLD_HANDOVER_FAILED));
                }
            } else {
                self.teardowns.iter_mut().for_each(|t| t.exited(pid));
                if let Some(s) = Self::reap_target(&mut self.sessions, pid) {
                    s.status = Some(status);
                    let id = s.id;
                    self.send(&FromHolder::Exited { id, status });
                }
            }
        }
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
        self.teardowns.iter_mut().for_each(|t| t.exited(pid));
        let mut live = self
            .sessions
            .iter_mut()
            .filter(|s| s.adopted && s.status.is_none());
        if let Some(s) = live.find(|s| s.pid == pid) {
            let (id, status) = (s.id, STATUS_UNKNOWN);
            s.status = Some(status);
            self.send(&FromHolder::Exited { id, status });
        }
    }

    fn on_krilld_exit(&mut self, failed_handover: bool) {
        let k = self.krilld.take();
        let stable = k.is_some_and(|k| !failed_handover && k.started.elapsed() >= STABLE);
        if std::mem::take(&mut self.upgrading) {
            self.streak = 0;
            tracing::info!(streak = 0u32, give_up = false, "daemon restart scheduled");
            self.restart_at = Some(Instant::now());
        } else {
            if stable {
                (self.streak, self.prev_krilld, self.unstable) = (1, None, 0);
            } else {
                self.streak = self.streak.saturating_add(1);
                self.note_unstable();
            }
            self.schedule();
        }
    }

    fn schedule(&mut self) {
        let give_up = self.sessions.iter().all(|s| s.status.is_some()) && self.streak >= GIVE_UP;
        if self.streak <= GIVE_UP || self.streak.is_power_of_two() {
            tracing::warn!(streak = self.streak, give_up, "daemon restart scheduled");
        }
        self.restart_at = (!give_up).then(|| Instant::now() + backoff(self.streak));
    }

    /// 현 krilld 경로가 연속 [`ROLLBACK_AFTER`]회 실패하면 바꾸기 전 경로로 되돌린다.
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
            (self.unstable, self.streak) = (0, 0);
        }
    }

    /// krilld를 띄우고 보관 중인 fd를 모두 넘긴다. 실패는 크래시와 같게 백오프한다.
    fn spawn_krilld(&mut self) {
        let started = (|| {
            // macOS에는 SOCK_CLOEXEC가 없다(holder는 단일 스레드).
            let (ours, theirs) = socketpair(
                AddressFamily::UNIX,
                SocketType::STREAM,
                SocketFlags::empty(),
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
        let (pid, sock) = match started {
            Ok(pair) => pair,
            Err(e) => {
                self.streak = self.streak.saturating_add(1);
                if self.streak <= GIVE_UP || self.streak.is_power_of_two() {
                    tracing::warn!(streak = self.streak, kind = ?e.kind(), errno = e.raw_os_error(), "krilld spawn failed");
                }
                self.note_unstable();
                self.schedule();
                return;
            }
        };
        let link = Link::new(sock);
        let sessions = self.sessions.iter().map(|s| {
            let msg = FromHolder::Session {
                id: s.id,
                pid: s.pid.as_raw_pid().unsigned_abs(),
                status: s.status,
                size: None,
            };
            (msg, Some(s.master.as_fd()))
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
            .filter(|s| s.adopted && s.status.is_none())
            .map(|s| s.pid)
            .collect();
        for p in adopted {
            if self.watch_exit(p).is_err() {
                self.on_adopted_exit(p);
            }
        }
    }
}

/// krilld가 게시한 listen 소켓(`<소켓>.tmp`에 bind한 뒤 rename)을 맡으면 `<소켓>.holder.lock` flock을 잡는다.
/// krilld가 재기동을 기다려 소켓 락이 비어도 다른 holder의 krilld는 이 락을 보고 살아 있는 소켓을 덮어쓰지 않는다
/// (krilld `Listener::bind`). 덮어쓰면 이 holder의 세션에 아무도 붙지 못한다. exec 핸드오프 뒤 다시 잡는다.
fn own_socket(listener: &OwnedFd) -> Option<File> {
    let l = std::os::unix::net::UnixListener::from(listener.try_clone().ok()?);
    let addr = l.local_addr().ok()?;
    let bound = addr.as_pathname()?.as_os_str().as_bytes();
    let sock = bound.strip_suffix(b".tmp")?;
    let path = OsStr::from_bytes(&[sock, b".holder.lock"].concat()).to_os_string();
    let mut open = File::options();
    let f = open.read(true).write(true).create(true).truncate(false);
    let f = f.mode(0o600).open(path);
    let f = f.and_then(|f| f.try_lock().map(|()| f).map_err(io::Error::from));
    f.inspect_err(|e| tracing::warn!(kind = ?e.kind(), "socket owner lock not taken"))
        .ok()
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
    // 죽인 뒤 닫는다: 먼저 닫으면 krilld가 EOF를 holder의 죽음으로 보고 고아가 될 틈이 생긴다.
    rustix::process::kill_process(k.pid, Signal::KILL).ok();
    k.link = None;
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
        Session {
            id: SessionId(id),
            pid: Pid::from_raw(pid).unwrap(),
            master: OwnedFd::from(std::fs::File::open("/dev/null").unwrap()),
            status,
            adopted: false,
        }
    }

    #[test]
    fn reap_ignores_exited_session_when_pid_is_reused() {
        // PH-01: 끝난 채 열린 탭(옛 id, status 있음)과 pid 순환으로 같은 pid를 받은 새 세션 —
        // 거둔 종료는 살아 있는 새 세션에 붙어야 한다.
        let mut sessions = vec![
            test_session(1, 4242, Some(0x700)),
            test_session(101, 4242, None),
        ];
        let hit = super::Holder::reap_target(&mut sessions, Pid::from_raw(4242).unwrap());
        assert_eq!(hit.map(|s| s.id), Some(SessionId(101)));
    }
}
