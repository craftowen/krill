//! Windows holder: `ConPTY`와 세션 Job을 보관하고 자식 krilld를 재기동한다.
pub use crate::sys::{Process as DetachedProcess, spawn_detached};
use crate::{
    link::{Link, Packet},
    sys::{self, Api, Console, Job, Process},
};
use krill_proto::{
    SessionId,
    daemon::KRILLD_DUPLICATE,
    holder::{
        FromHolder, MAX_SESSIONS, SESSION_LIMIT_ERRNO, STATUS_UNKNOWN, ToHolder, VERSION,
        request_id,
    },
};
use std::ffi::OsString;
use std::io::{self, Write};
use std::os::windows::io::{AsHandle, AsSocket, BorrowedHandle, OwnedHandle, OwnedSocket};
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
impl Session {
    fn handles(&self) -> Vec<BorrowedHandle<'_>> {
        let mut handles = self.console.pty.handles();
        handles.extend([
            self.process.handle.as_handle(),
            self.job.handle.as_handle(),
            self.job.port.as_handle(),
        ]);
        handles
    }
    fn restore(
        api: Api,
        id: SessionId,
        pid: u32,
        status: Option<i32>,
        mut handles: Vec<OwnedHandle>,
    ) -> io::Result<Self> {
        if handles.len() != 8 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut pop = || {
            handles
                .pop()
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
        };
        let port = pop()?;
        let handle = pop()?;
        let process = Process {
            handle: pop()?,
            pid,
        };
        let mut console = Console::pack(api, crate::link::Pty::from_handles(handles)?)?;
        if status.is_some() {
            console.close();
        }
        Ok(Self {
            id,
            process,
            console,
            job: Job { handle, port },
            status,
            deadline: None,
        })
    }
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
    upgrading: bool,
    listener: Option<OwnedSocket>,
}
pub fn run(mut args: impl Iterator<Item = OsString>) -> io::Result<()> {
    let path = args.next().ok_or(io::ErrorKind::InvalidInput)?;
    if path == "--bootstrap" {
        return sys::bootstrap(&args.collect::<Vec<_>>());
    }
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
        upgrading: false,
        listener: None,
    };
    if let Some((metadata, handles, mut ack, listener)) = sys::take_handoff()? {
        let req = h.restore(&metadata, handles)?;
        h.listener = listener;
        h.send(&h.hello(), None)?;
        h.send(&FromHolder::Done { req }, None)?;
        ack.write_all(&[1])?;
    }
    loop {
        if h.poll() {
            return Ok(());
        }
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
        let target = match sys::duplicate_target(process.pid) {
            Ok(target) => target,
            Err(error) => {
                sys::terminate(&process).ok();
                return Err(error);
            }
        };
        let bootstrap = || -> io::Result<()> {
            link.send_message(&self.hello(), &[], target.as_handle())?;
            for s in &self.sessions {
                link.send_message(
                    &FromHolder::Session {
                        id: s.id,
                        pid: s.process.pid,
                        status: s.status,
                    },
                    &s.handles(),
                    target.as_handle(),
                )?;
            }
            if let Some(socket) = &self.listener {
                let info = sys::duplicate_socket(socket.as_socket(), process.pid)?;
                link.send_message(&FromHolder::Listener { info }, &[], target.as_handle())?;
            }
            link.send_message(&FromHolder::Ready, &[], target.as_handle())
        };
        if let Err(error) = bootstrap() {
            sys::terminate(&process).ok();
            return Err(error);
        }
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
                &session.map_or_else(Vec::new, Session::handles),
                d.target.as_handle(),
            )
    }
    fn poll(&mut self) -> bool {
        if self.daemon.is_none() && Instant::now() >= self.restart {
            match self.start() {
                Ok(d) => self.daemon = Some(d),
                Err(error) => {
                    tracing::warn!(kind = ?error.kind(), "krilld start failed");
                    self.failed();
                }
            }
        }
        // Drain notifications (especially HoldListener) even if the daemon has
        // already exited. Dropping its link first would discard queued packets.
        // Observe exit before the final drain: a live peer may write after a
        // drain, so it must be retained until the next poll observes its exit.
        let exited = self.daemon.as_ref().and_then(|d| {
            d.process
                .status()
                .ok()
                .flatten()
                .map(|status| (status, d.started.elapsed()))
        });
        let packets = self
            .daemon
            .as_mut()
            .and_then(|d| d.link.as_mut())
            .map(Link::drain_nowait);
        match packets {
            Some(Ok((packets, alive))) => {
                for p in packets {
                    if self.request(p) {
                        return true;
                    }
                }
                if !alive && let Some(d) = &mut self.daemon {
                    d.link = None;
                }
            }
            Some(Err(_)) => {
                if let Some(d) = &mut self.daemon {
                    d.link = None;
                }
            }
            None => {}
        }
        if let Some((status, uptime)) = exited
            && self.daemon_exited(status, uptime)
        {
            return true;
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
        false
    }
    fn daemon_exited(&mut self, status: i32, uptime: Duration) -> bool {
        if status == i32::from(KRILLD_DUPLICATE) && self.sessions.is_empty() {
            self.daemon = None;
            return true;
        }
        let upgrading = std::mem::take(&mut self.upgrading);
        if uptime >= Duration::from_secs(1) && !upgrading {
            self.streak = 0;
            self.previous = None;
        }
        self.daemon = None;
        if upgrading {
            // The old daemon's deliberate exit is not a crash, nor proof that
            // the new executable is stable. Retain rollback until it runs.
            self.streak = 0;
            self.restart = Instant::now();
        } else {
            self.failed();
        }
        false
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
    fn request(&mut self, packet: Packet) -> bool {
        let req = request_id(&packet.body).unwrap_or(0);
        let decoded = ToHolder::decode(packet.frame());
        if let Ok(ToHolder::UpgradeHolder { req, ref path }) = decoded {
            let result = text(path.clone()).and_then(|path| self.upgrade(&path, req));
            match result {
                Ok(()) => return true,
                Err(e) => {
                    self.send(
                        &FromHolder::Failed {
                            req,
                            errno: e.raw_os_error().unwrap_or(5),
                        },
                        None,
                    )
                    .ok();
                    return false;
                }
            }
        }
        let result = match decoded {
            Ok(m) => self.handle(m, packet.fds),
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
        false
    }
    fn handle(&mut self, m: ToHolder, handles: Vec<OwnedHandle>) -> io::Result<Option<FromHolder>> {
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
                    .filter(|(key, _)| {
                        !key.eq_ignore_ascii_case(b"KRILL_HOLDER_LINK")
                            && !key.eq_ignore_ascii_case(b"KRILL_HOLDER_STATE")
                    })
                    .map(|(k, v)| Ok((text(k)?, text(v)?)))
                    .collect::<io::Result<Vec<_>>>()?;
                let cwd = text(cwd)?;
                let job = Job::new()?;
                let mut console = Console::create(self.api, size)?;
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
                s.console.stop_host().ok();
                s.job.terminate().ok();
                s.deadline = Some(Instant::now() + Duration::from_secs(5));
            }
            ToHolder::Kill { id, signal, .. } => {
                let s = self.live(id)?;
                s.console.stop_host().ok();
                if signal == 9 {
                    s.job.terminate().ok();
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
                self.previous = Some(if path == self.path {
                    // The updater moved the old image to <install>.prev; the
                    // original path now names the new executable, not rollback.
                    backup_path(&self.path)
                } else {
                    self.path.clone()
                });
                self.path = path;
                self.streak = 0;
                self.upgrading = true;
            }
            ToHolder::HoldListener { info, .. } => {
                self.listener = Some(sys::restore_socket(&info)?);
            }
            ToHolder::Adopt { id, pid, .. } => {
                if !self.sessions.iter().any(|s| s.id == id) {
                    self.sessions
                        .push(Session::restore(self.api, id, pid, None, handles)?);
                }
            }
            ToHolder::UpgradeHolder { .. } => {
                return Err(io::Error::from_raw_os_error(38));
            }
        }
        Ok(Some(FromHolder::Done { req }))
    }
    fn upgrade(&mut self, path: &std::ffi::OsStr, req: u32) -> io::Result<()> {
        // Live ConPTY ownership stays with this process during app updates,
        // including bundled OpenConsole mode. Only an empty holder may hand off.
        if !self.sessions.is_empty() {
            return Err(io::Error::from_raw_os_error(16));
        }
        if self.sessions.iter().any(|s| s.deadline.is_some()) {
            return Err(io::Error::from_raw_os_error(16));
        }
        let d = self.daemon.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        let link = d.link.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        if link.has_pending() {
            return Err(io::Error::from_raw_os_error(16));
        }
        let mut metadata = format!("{req} {}\n", d.process.pid);
        let mut handles = vec![
            d.process.handle.as_handle(),
            d.target.as_handle(),
            link.input.as_handle(),
            link.output.as_handle(),
        ];
        for s in &self.sessions {
            use std::fmt::Write as _;
            writeln!(
                metadata,
                "{} {} {}",
                s.id.0,
                s.process.pid,
                s.status
                    .map_or_else(|| "live".to_owned(), |v| v.to_string())
            )
            .map_err(io::Error::other)?;
            handles.push(s.process.handle.as_handle());
            handles.extend(s.console.pty.handles());
            handles.extend([s.job.handle.as_handle(), s.job.port.as_handle()]);
        }
        let args = std::iter::once(self.path.clone())
            .chain(self.args.clone())
            .collect::<Vec<_>>();
        sys::handoff(
            path,
            &args,
            &metadata,
            &handles,
            self.listener.as_ref().map(AsSocket::as_socket),
        )?;
        for s in &mut self.sessions {
            s.console.relinquish();
        }
        Ok(())
    }
    fn restore(&mut self, metadata: &str, handles: Vec<OwnedHandle>) -> io::Result<u32> {
        let mut words = metadata.split_whitespace();
        let req = number(words.next())?;
        let pid = number(words.next())?;
        let remaining = words.clone().count();
        if !remaining.is_multiple_of(3) || handles.len() != 4 + remaining / 3 * 8 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut handles = handles.into_iter();
        let mut next = || handles.next().ok_or(io::ErrorKind::InvalidData.into());
        let process = Process {
            handle: next()?,
            pid,
        };
        let target = next()?;
        let link = Link::from_handles(next()?, next()?);
        self.daemon = Some(Daemon {
            process,
            target,
            link: Some(link),
            started: Instant::now(),
        });
        while let Some(id) = words.next() {
            let id = SessionId(number(Some(id))?);
            let pid = number(words.next())?;
            let status = words.next().ok_or(io::ErrorKind::InvalidData)?;
            let status = if status == "live" {
                None
            } else {
                Some(number(Some(status))?)
            };
            let mut session = (0..8).map(|_| next()).collect::<io::Result<Vec<_>>>()?;
            session
                .get_mut(..6)
                .ok_or(io::ErrorKind::InvalidData)?
                .rotate_left(1);
            self.sessions
                .push(Session::restore(self.api, id, pid, status, session)?);
        }
        if handles.next().is_some() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(req)
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

fn backup_path(path: &std::ffi::OsStr) -> OsString {
    let original = std::path::Path::new(path);
    let Some((parent, name)) = original.parent().zip(original.file_name()) else {
        return path.to_owned();
    };
    let mut prev = parent.as_os_str().to_owned();
    prev.push(".prev");
    let backup = std::path::PathBuf::from(prev).join(name);
    if backup.is_file() {
        backup.into_os_string()
    } else {
        path.to_owned()
    }
}

fn number<T: std::str::FromStr>(word: Option<&str>) -> io::Result<T> {
    word.ok_or(io::ErrorKind::InvalidData)?
        .parse()
        .map_err(|_| io::ErrorKind::InvalidData.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn holder() -> Holder {
        Holder {
            path: "old-krilld.exe".into(),
            args: Vec::new(),
            sessions: Vec::new(),
            api: Api::load(),
            daemon: None,
            streak: 0,
            restart: Instant::now() + Duration::from_secs(60),
            previous: None,
            upgrading: false,
            listener: None,
        }
    }

    /// `cmd /d /c exit <code>`를 자식 krilld 자리에 둔다(bootstrap 링크는 읽지 않고 바로 끝난다).
    /// `sys::child_link`는 인수마다 CRT 따옴표를 붙여 `"/c" "exit" "3"`이 되는데, cmd.exe는 CRT 규칙으로
    /// 파싱하지 않아 `exit 3`이 실행되지 않았다(CI job 40502947206). `std::process::Command`는 공백 없는
    /// 인수를 감싸지 않는다. 종료 상태가 요청한 값인지 holder `poll` 전에 확인한다.
    fn exited_daemon(code: i32) -> io::Result<Daemon> {
        let cmd =
            std::env::var_os("ComSpec").unwrap_or_else(|| r"C:\Windows\System32\cmd.exe".into());
        let mut child = Command::new(cmd)
            .args(["/d", "/c", "exit"])
            .arg(code.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                outcome => {
                    // 시간 초과나 대기 오류: 자식을 거둔 뒤 알린다.
                    child.kill().ok();
                    child.wait().ok();
                    outcome?;
                    panic!("mock krilld did not exit within 10s");
                }
            }
        };
        assert_eq!(status.code(), Some(code), "mock krilld exit status");
        let pid = child.id();
        let process = Process {
            handle: OwnedHandle::from(child),
            pid,
        };
        assert_eq!(process.status()?, Some(code), "holder-side exit status");
        let target = sys::duplicate_target(pid)?;
        let (link, _peer) = Link::pair()?;
        Ok(Daemon {
            process,
            target,
            link: Some(link),
            started: Instant::now(),
        })
    }

    /// foundation-01: 세션 없는 중복 holder는 `KRILLD_DUPLICATE`(3)에서 재기동 없이 끝난다(Unix `reap`과 같은 계약).
    #[test]
    fn empty_duplicate_holder_exits_instead_of_restarting() -> io::Result<()> {
        let mut h = holder();
        h.daemon = Some(exited_daemon(3)?);
        assert!(h.poll());
        assert!(h.daemon.is_none());
        assert_eq!(h.streak, 0, "duplicate exit must not count as a crash");
        Ok(())
    }

    #[test]
    fn crash_exit_still_schedules_restart() -> io::Result<()> {
        let mut h = holder();
        h.daemon = Some(exited_daemon(1)?);
        assert!(!h.poll());
        assert!(h.daemon.is_none());
        assert_eq!(h.streak, 1);
        Ok(())
    }

    #[test]
    fn planned_daemon_exit_retains_rollback_and_has_no_crash_backoff() {
        for uptime in [Duration::ZERO, Duration::from_secs(2)] {
            let mut h = holder();
            h.handle(
                ToHolder::UpgradeKrilld {
                    req: 1,
                    path: b"new-krilld.exe".to_vec(),
                },
                Vec::new(),
            )
            .unwrap();
            assert!(h.upgrading);
            assert!(!h.daemon_exited(0, uptime));
            assert!(!h.upgrading);
            assert_eq!(h.streak, 0);
            assert_eq!(
                h.previous.as_deref(),
                Some(std::ffi::OsStr::new("old-krilld.exe"))
            );
            assert!(h.restart <= Instant::now());
            for _ in 0..3 {
                h.failed();
            }
            assert_eq!(h.path, "old-krilld.exe");
        }
    }

    #[test]
    fn stable_daemon_crash_clears_only_stale_rollback() {
        let mut h = holder();
        h.previous = Some("older-krilld.exe".into());
        h.streak = 2;
        assert!(!h.daemon_exited(1, Duration::from_secs(2)));
        assert!(h.previous.is_none());
        assert_eq!(h.streak, 1);
    }

    #[test]
    fn same_path_update_rolls_back_to_the_previous_install_image() {
        let root =
            std::env::temp_dir().join(format!("krill-holder-rollback-{}", std::process::id()));
        let current = root.join("Krill").join("krilld.exe");
        let previous = root.join("Krill.prev").join("krilld.exe");
        for path in [&current, &previous] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture").unwrap();
        }
        let mut h = holder();
        h.path = current.as_os_str().to_owned();
        h.handle(
            ToHolder::UpgradeKrilld {
                req: 1,
                path: current.as_os_str().as_encoded_bytes().to_vec(),
            },
            Vec::new(),
        )
        .unwrap();
        assert_eq!(h.previous.as_deref(), Some(previous.as_os_str()));
        assert!(!h.daemon_exited(0, Duration::from_secs(2)));
        for _ in 0..3 {
            h.failed();
        }
        assert_eq!(h.path, previous.as_os_str());
        drop(h);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_duplicate_holder_stops_instead_of_restarting() {
        let mut h = holder();
        assert!(h.daemon_exited(
            i32::from(krill_proto::daemon::KRILLD_DUPLICATE),
            Duration::ZERO
        ));
    }

    #[test]
    fn queued_listener_is_kept_before_reaping_dead_daemon() {
        use std::os::windows::process::CommandExt as _;
        use std::process::Stdio;

        // WSADuplicateSocket preserves either family; this test checks both a
        // live poll followed by a late final write and a dead peer at pipe EOF.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/D", "/K"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .spawn()
            .unwrap();
        let process = Process {
            handle: child.as_handle().try_clone_to_owned().unwrap(),
            pid: child.id(),
        };
        let (link, peer) = Link::pair().unwrap();
        peer.send(
            &ToHolder::UpgradeKrilld {
                req: 1,
                path: b"next-krilld.exe".to_vec(),
            }
            .encode()
            .unwrap(),
            &[],
        )
        .unwrap();
        let mut h = holder();
        h.daemon = Some(Daemon {
            process,
            target: sys::duplicate_target(std::process::id()).unwrap(),
            link: Some(link),
            started: Instant::now(),
        });
        assert!(!h.poll());
        assert!(h.upgrading);
        assert!(h.daemon.is_some(), "live daemon must stay attached");

        let info = sys::duplicate_socket(listener.as_socket(), std::process::id()).unwrap();
        peer.send(
            &ToHolder::HoldListener { req: 2, info }.encode().unwrap(),
            &[],
        )
        .unwrap();
        drop(peer);
        drop(child.stdin.take());
        child.wait().unwrap();
        assert!(!h.poll());
        assert!(
            h.listener.is_some(),
            "daemon exit discarded its final HoldListener"
        );
        assert!(h.daemon.is_none());
        drop((h, listener));
    }
}
