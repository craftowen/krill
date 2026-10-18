//! Windows holder: `ConPTY`와 세션 Job을 보관하고 자식 krilld를 재기동한다.
use crate::{
    link::{Link, Packet},
    sys::{self, Api, Console, Job, Process},
};
use krill_proto::{
    SessionId,
    holder::{
        FromHolder, MAX_SESSIONS, SESSION_LIMIT_ERRNO, STATUS_UNKNOWN, ToHolder, VERSION,
        request_id,
    },
};
use std::ffi::OsString;
use std::io;
use std::os::windows::io::{AsHandle, OwnedHandle};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Session {
    id: SessionId,
    process: Process,
    console: Console,
    job: Job,
    status: Option<i32>,
    deadline: Option<Instant>,
}
struct Daemon {
    process: Process,
    target: OwnedHandle,
    link: Option<Link>,
    started: Instant,
}
struct Holder {
    path: OsString,
    args: Vec<OsString>,
    sessions: Vec<Session>,
    api: Api,
    daemon: Option<Daemon>,
    streak: u32,
    restart: Instant,
    previous: Option<OsString>,
}
pub fn run(mut args: impl Iterator<Item = OsString>) -> io::Result<()> {
    let path = args.next().ok_or(io::ErrorKind::InvalidInput)?;
    krill_log::init(krill_log::Component::Holder).ok();
    let mut h = Holder {
        path,
        args: args.collect(),
        sessions: Vec::new(),
        api: Api::load(),
        daemon: None,
        streak: 0,
        restart: Instant::now(),
        previous: None,
    };
    loop {
        h.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
}
/// Windows는 생성할 때 `HANDLE_LIST`로 상속 범위를 제한한다.
#[expect(clippy::unnecessary_wraps, reason = "Unix와 같은 detach 계약")]
pub fn close_inherited() -> io::Result<()> {
    Ok(())
}
impl Holder {
    fn hello(&self) -> FromHolder {
        FromHolder::Hello {
            version: VERSION,
            holder_pid: std::process::id(),
            inbox: self.api.inbox(),
        }
    }
    fn start(&self) -> io::Result<Daemon> {
        let (process, link) = sys::child_link(&self.path, &self.args)?;
        let target = sys::duplicate_target(process.pid)?;
        link.send_message(&self.hello(), &[], target.as_handle())?;
        for s in &self.sessions {
            link.send_message(
                &FromHolder::Session {
                    id: s.id,
                    pid: s.process.pid,
                    status: s.status,
                },
                &s.console.pty.handles(),
                target.as_handle(),
            )?;
        }
        link.send_message(&FromHolder::Ready, &[], target.as_handle())?;
        Ok(Daemon {
            process,
            target,
            link: Some(link),
            started: Instant::now(),
        })
    }
    fn send(&self, m: &FromHolder, session: Option<&Session>) -> io::Result<()> {
        let d = self.daemon.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        d.link
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .send_message(
                m,
                &session.map_or_else(Vec::new, |s| s.console.pty.handles()),
                d.target.as_handle(),
            )
    }
    fn poll(&mut self) {
        if self.daemon.is_none() && Instant::now() >= self.restart {
            match self.start() {
                Ok(d) => self.daemon = Some(d),
                Err(error) => {
                    tracing::warn!(kind = ?error.kind(), "krilld start failed");
                    self.failed();
                }
            }
        }
        let dead = self
            .daemon
            .as_ref()
            .is_some_and(|d| d.process.status().ok().flatten().is_some());
        if dead {
            if self
                .daemon
                .as_ref()
                .is_some_and(|d| d.started.elapsed() >= Duration::from_secs(1))
            {
                self.streak = 0;
                self.previous = None;
            }
            self.daemon = None;
            self.failed();
        }
        let packets = self
            .daemon
            .as_mut()
            .and_then(|d| d.link.as_mut())
            .map(|link| {
                let mut packets = Vec::new();
                let alive = link.fill_nowait()?;
                if !alive {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                while let Some(p) = link.try_next()? {
                    packets.push(p);
                }
                Ok::<_, io::Error>(packets)
            });
        match packets {
            Some(Ok(packets)) => {
                for p in packets {
                    self.request(p);
                }
            }
            Some(Err(_)) => {
                if let Some(d) = &mut self.daemon {
                    d.link = None;
                }
            }
            None => {}
        }
        for s in &mut self.sessions {
            if s.deadline.is_some_and(|t| Instant::now() >= t) {
                s.job.terminate().ok();
                s.deadline = None;
            }
        }
        let mut events = Vec::new();
        for s in &mut self.sessions {
            if s.status.is_none() && s.job.empty().unwrap_or(false) {
                let status = s.process.status().ok().flatten().unwrap_or(STATUS_UNKNOWN);
                s.status = Some(status);
                s.console.stop_host().ok();
                s.console.close();
                events.push(FromHolder::Exited { id: s.id, status });
            }
        }
        for m in events {
            self.send(&m, None).ok();
        }
    }
    fn failed(&mut self) {
        self.streak = self.streak.saturating_add(1);
        if self.streak >= 3
            && let Some(path) = self.previous.take()
        {
            self.path = path;
        }
        self.restart = Instant::now()
            + Duration::from_millis(if self.streak <= 1 {
                0
            } else {
                250_u64
                    .saturating_mul(
                        1_u64
                            .checked_shl(self.streak.saturating_sub(2))
                            .unwrap_or(u64::MAX),
                    )
                    .min(30_000)
            });
    }
    fn request(&mut self, packet: Packet) {
        let req = request_id(&packet.body).unwrap_or(0);
        let decoded = ToHolder::decode(packet.frame());
        drop(packet);
        let result = match decoded {
            Ok(m) => self.handle(m),
            Err(_) => Err(io::Error::from_raw_os_error(38)),
        };
        match result {
            Ok(Some(m)) => {
                self.send(&m, None).ok();
            }
            Ok(None) => {}
            Err(e) => {
                self.send(
                    &FromHolder::Failed {
                        req,
                        errno: e.raw_os_error().unwrap_or(5),
                    },
                    None,
                )
                .ok();
            }
        }
    }
    fn handle(&mut self, m: ToHolder) -> io::Result<Option<FromHolder>> {
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
                if self.sessions.len() >= MAX_SESSIONS {
                    return Err(io::Error::from_raw_os_error(SESSION_LIMIT_ERRNO));
                }
                if self.sessions.iter().any(|s| s.id == id) {
                    return Err(io::Error::from_raw_os_error(17));
                }
                let argv = argv.into_iter().map(text).collect::<io::Result<Vec<_>>>()?;
                let (path, args) = argv.split_first().ok_or(io::ErrorKind::InvalidInput)?;
                let env = env
                    .into_iter()
                    .map(|(k, v)| Ok((text(k)?, text(v)?)))
                    .collect::<io::Result<Vec<_>>>()?;
                let cwd = text(cwd)?;
                let mut console = Console::create(self.api, size)?;
                let job = Job::new()?;
                let process = match sys::spawn(
                    path,
                    args,
                    &env,
                    (!cwd.is_empty()).then_some(cwd.as_os_str()),
                    Some(&console),
                    &[],
                    Some(&job),
                ) {
                    Ok(p) => p,
                    Err(e) => {
                        console.stop_host().ok();
                        console.close();
                        return Err(e);
                    }
                };
                let session = Session {
                    id,
                    process,
                    console,
                    job,
                    status: None,
                    deadline: None,
                };
                let message = FromHolder::Spawned {
                    req,
                    id,
                    pid: session.process.pid,
                };
                // 전송 실패해도 세션은 holder에 남고 다음 krilld가 받는다.
                self.sessions.push(session);
                self.send(&message, self.sessions.last())?;
                return Ok(None);
            }
            ToHolder::Resize { id, size, .. } => self.live(id)?.console.resize(size)?,
            ToHolder::Terminate { id, .. } => {
                let s = self.live(id)?;
                s.console.stop_host()?;
                s.deadline = Some(Instant::now() + Duration::from_secs(5));
            }
            ToHolder::Kill { id, signal, .. } => {
                let s = self.live(id)?;
                s.console.stop_host()?;
                if signal == 9 {
                    s.job.terminate()?;
                } else {
                    s.deadline = Some(Instant::now() + Duration::from_secs(5));
                }
            }
            ToHolder::Release { id, .. } => {
                if self
                    .sessions
                    .iter()
                    .any(|s| s.id == id && s.status.is_none())
                {
                    return Err(io::Error::from_raw_os_error(16));
                }
                self.sessions.retain(|s| s.id != id);
            }
            ToHolder::UpgradeKrilld { path, .. } => {
                let path = text(path)?;
                self.previous = Some(std::mem::replace(&mut self.path, path));
                self.streak = 0;
            }
            ToHolder::UpgradeHolder { .. } => {
                return Err(io::Error::from_raw_os_error(
                    if self.api.inbox() && !self.sessions.is_empty() {
                        16
                    } else {
                        38
                    },
                ));
            }
            ToHolder::HoldListener { .. } | ToHolder::Adopt { .. } => {
                return Err(io::Error::from_raw_os_error(38));
            }
        }
        Ok(Some(FromHolder::Done { req }))
    }
    fn live(&mut self, id: SessionId) -> io::Result<&mut Session> {
        self.sessions
            .iter_mut()
            .find(|s| s.id == id && s.status.is_none())
            .ok_or_else(|| io::Error::from_raw_os_error(3))
    }
}
fn text(bytes: Vec<u8>) -> io::Result<OsString> {
    let s = String::from_utf8(bytes).map_err(|_| io::ErrorKind::InvalidInput)?;
    if s.contains('\0') {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(s.into())
}
