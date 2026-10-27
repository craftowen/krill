//! Windows SSH: 로컬 `AF_UNIX` 연결마다 `ssh -T -W <원격.sock>` 자식의 stdio에 잇는다.
//!
//! Windows ssh.exe는 Unix 소켓 `-L`을 지원하지 않는다. `Stdio::piped()`·`std::io::pipe()`도
//! 멈추므로 양쪽 끝이 overlapped인 named pipe를 안전 API로 만든다(OWE-915 VM 실측).
//! 인증·생존 확인용 keepalive 1개와 연결당 ssh 1개를 사용한다. 로컬 TCP 포트는 열지 않는다.
//! 자세한 결과·제약: `docs/research/windows-spike-OWE-915.md`.

use super::SshOptions;
use krill_proto::os::net::{UnixListener, UnixStream};
use std::ffi::OsStr;
use std::io::{self, Read as _};
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub(crate) mod stdio;
use stdio::{ChildGuard, Pipe, Process};

const READY: &[u8] = b"__KRILL_SSH_READY__";
const POLL: Duration = Duration::from_millis(50);
const READ_POLL: Duration = Duration::from_millis(250);
const MAX_CONNECTIONS: usize = 128;

#[derive(Debug)]
pub struct Tunnel {
    child: ChildGuard,
    _stdin: Pipe,
    socket: PathBuf,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Tunnel {
    pub fn start_at_with_program(
        opts: &SshOptions,
        local: PathBuf,
        remote: &Path,
        ssh_bin: &OsStr,
    ) -> io::Result<Self> {
        opts.validate()?;
        if !remote.to_str().is_some_and(super::valid_remote_socket) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid remote socket",
            ));
        }
        if let Some(dir) = local.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 인스턴스 전용 경로. 기존 소켓을 다른 터널이 쓰고 있을 수 있어 덮어쓰지 않는다.
        let listener = UnixListener::bind(&local)?;
        let result = Self::start_bound(opts, local.clone(), remote, ssh_bin, listener);
        if result.is_err() {
            std::fs::remove_file(local).ok();
        }
        result
    }

    fn start_bound(
        opts: &SshOptions,
        socket: PathBuf,
        remote: &Path,
        ssh_bin: &OsStr,
        listener: UnixListener,
    ) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        let mut args = opts.common_args(10);
        args.extend([
            "-T".to_owned(),
            opts.destination.clone(),
            "printf __KRILL_SSH_READY__; cat >/dev/null".to_owned(),
        ]);
        let Process {
            child,
            stdin,
            mut stdout,
            mut stderr,
        } = stdio::spawn(ssh_bin, &args)?;
        let (tx, rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("ssh-ready".into())
            .spawn(move || {
                let mut marker = [0; READY.len()];
                let result = stdout.read_exact(&mut marker).and_then(|()| {
                    if marker == READY {
                        Ok(())
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "invalid ssh readiness marker",
                        ))
                    }
                });
                tx.send(result).ok();
                io::copy(&mut stdout, &mut io::sink()).ok();
            })?;
        thread::Builder::new()
            .name("ssh-keepalive-stderr".into())
            .spawn(move || {
                io::copy(&mut stderr, &mut io::sink()).ok();
            })?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|e| io::Error::new(io::ErrorKind::TimedOut, e))?
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("ssh forwarding to {} failed: {error}", opts.destination),
                )
            })?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let opts = opts.clone();
        let remote = remote.to_owned();
        let program = ssh_bin.to_owned();
        let worker = thread::Builder::new()
            .name("ssh-accept".into())
            .spawn(move || accept(&listener, &opts, &remote, &program, &stop))?;
        Ok(Self {
            child,
            _stdin: stdin,
            socket,
            stopped,
            worker: Some(worker),
        })
    }
    pub fn socket(&self) -> &Path {
        &self.socket
    }
    pub fn pid(&self) -> u32 {
        self.child.0.id()
    }
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.0.try_wait(), Ok(None))
            && !self.stopped.load(Ordering::Acquire)
            && self.worker.as_ref().is_some_and(|w| !w.is_finished())
    }
    /// Windows 클라이언트는 원격 중계(`RemoteEndpoint::Bridge`)를 아직 열지 않으므로 항상 `None`.
    #[expect(clippy::unused_self, reason = "Unix Tunnel과 같은 모양의 API")]
    pub fn remote_os(&self) -> Option<&str> {
        None
    }
}
impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.child.stop();
        if let Some(worker) = self.worker.take() {
            worker.join().ok();
        }
        std::fs::remove_file(&self.socket).ok();
    }
}

fn accept(
    listener: &UnixListener,
    opts: &SshOptions,
    remote: &Path,
    program: &OsStr,
    stopped: &Arc<AtomicBool>,
) {
    let mut workers = Vec::<JoinHandle<()>>::new();
    while !stopped.load(Ordering::Acquire) {
        workers.retain(|worker| !worker.is_finished());
        match listener.accept() {
            Ok((socket, _)) if workers.len() < MAX_CONNECTIONS => {
                let opts = opts.clone();
                let remote = remote.to_owned();
                let program = program.to_owned();
                let stopped = Arc::clone(stopped);
                if let Ok(worker) =
                    thread::Builder::new()
                        .name("ssh-forward".into())
                        .spawn(move || {
                            bridge(socket, &opts, &remote, &program, &stopped).ok();
                        })
                {
                    workers.push(worker);
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(_) => break,
        }
    }
    stopped.store(true, Ordering::Release);
    for worker in workers {
        worker.join().ok();
    }
}

fn bridge(
    mut socket: UnixStream,
    opts: &SshOptions,
    remote: &Path,
    program: &OsStr,
    stopped: &AtomicBool,
) -> io::Result<()> {
    socket.set_read_timeout(Some(READ_POLL))?;
    socket.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut back = socket.try_clone()?;
    let mut args = opts.common_args(10);
    args.extend([
        "-T".into(),
        "-W".into(),
        remote.to_string_lossy().into_owned(),
        opts.destination.clone(),
    ]);
    let Process {
        mut child,
        mut stdin,
        mut stdout,
        mut stderr,
    } = stdio::spawn(program, &args)?;
    let finished = AtomicBool::new(false);
    thread::scope(|scope| {
        let from_local = scope.spawn(|| {
            let mut buffer = vec![0; 64 * 1024];
            while !finished.load(Ordering::Acquire) && !stopped.load(Ordering::Acquire) {
                match socket.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if std::io::Write::write_all(&mut stdin, &buffer[..n]).is_err() {
                            break;
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::TimedOut
                                | io::ErrorKind::WouldBlock
                                | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => break,
                }
            }
            drop(stdin);
        });
        let from_remote = scope.spawn(|| {
            io::copy(&mut stdout, &mut back).ok();
            back.shutdown(Shutdown::Write).ok();
            finished.store(true, Ordering::Release);
        });
        let errors = scope.spawn(|| {
            io::copy(&mut stderr, &mut io::sink()).ok();
        });
        while !stopped.load(Ordering::Acquire) && !finished.load(Ordering::Acquire) {
            if !matches!(child.0.try_wait(), Ok(None)) {
                break;
            }
            thread::sleep(POLL);
        }
        child.stop();
        finished.store(true, Ordering::Release);
        from_local.join().ok();
        from_remote.join().ok();
        errors.join().ok();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::screen::Msg as _;
    use krill_proto::{daemon, ipc, screen};
    use std::io::Write as _;

    #[test]
    fn missing_ssh_removes_only_new_forward_socket() {
        let path = std::env::temp_dir().join(format!("kh915-{}.sock", std::process::id()));
        let result = Tunnel::start_at_with_program(
            &SshOptions::new("owner@test"),
            path.clone(),
            Path::new("/remote.sock"),
            OsStr::new("Z:\\no-such-ssh.exe"),
        );
        assert!(result.is_err());
        assert!(!path.exists());
    }

    // 외부 네트워크를 쓰는 실측은 명시 실행한다. 일반 테스트는 네트워크에 접속하지 않는다.
    #[test]
    #[ignore = "requires OWE-915 dedicated POSIX sshd and remote test session"]
    fn real_ssh_home_query_and_remote_session_attach() {
        let mut opts = SshOptions::new(std::env::var("KRILL_SSH_E2E_DEST").unwrap());
        opts.port = Some(
            std::env::var("KRILL_SSH_E2E_PORT")
                .unwrap()
                .parse()
                .unwrap(),
        );
        opts.identity_file = Some(std::env::var("KRILL_SSH_E2E_KEY").unwrap());
        let program = std::env::var_os("KRILL_SSH_E2E_PROGRAM").unwrap_or_else(|| "ssh.exe".into());
        let home = super::super::query_remote_home_with_program(&opts, &program).unwrap();
        assert!(home.starts_with('/'));
        let remote = PathBuf::from(std::env::var("KRILL_SSH_E2E_SOCKET").unwrap());
        let session = krill_proto::SessionId(
            std::env::var("KRILL_SSH_E2E_SESSION")
                .unwrap()
                .parse()
                .unwrap(),
        );
        let local = std::env::temp_dir().join(format!("kh915-e2e-{}.sock", std::process::id()));
        let mut tunnel =
            Tunnel::start_at_with_program(&opts, local.clone(), &remote, &program).unwrap();
        assert!(tunnel.is_alive());
        let hello = daemon::client_hello(ipc::ClientRole::Cli, "OWE-915");
        let mut connection =
            super::super::dial_tunnel(tunnel.socket(), &hello, Duration::from_secs(15)).unwrap();
        assert!(!connection.launched);
        connection
            .stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        connection
            .stream
            .write_all(
                &screen::Attach {
                    session,
                    stream: screen::Stream::Raw {
                        from: screen::Seq(0),
                    },
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        let mut attached = false;
        let mut bytes = Vec::new();
        while !bytes
            .windows(b"OWE915_READY".len())
            .any(|b| b == b"OWE915_READY")
        {
            let (kind, body) = ipc::read_frame(&mut connection.stream).unwrap().unwrap();
            if kind == screen::kind::ATTACHED {
                attached = true;
            }
            if kind == screen::kind::OUTPUT {
                bytes.extend_from_slice(screen::decode_output(kind, &body).unwrap().1);
            }
        }
        assert!(attached);
        drop(connection);
        drop(tunnel);
        assert!(!local.exists());
    }
}
