//! IPC 소켓(MK-021): 0700 디렉터리 안 0600 UDS, accept 직후 피어 UID 검사, Hello 협상.
//! 와이어 포맷은 [`krill_proto::ipc`].
//!
//! 게시(MK-022, T2 §3.2 #2): `<sock>.lock`에 flock을 잡은 프로세스만 `<sock>.tmp`에 bind한 뒤 `rename`으로
//! `<sock>`을 원자 교체한다. 소켓 경로에는 항상 살아 있는 소유자의 소켓이 있거나(이전 krilld가 죽었다면) 다음
//! 소유자가 덮어쓸 죽은 소켓 하나만 있다. 종료할 때 unlink하지 않는다 — 떠나는 데몬이 후임의 소켓을 지우지 못하게.

use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use ring::rand::{SecureRandom as _, SystemRandom};

use krill_proto::ipc::{self, ClientHello, Negotiated, ServerHello};
use krill_proto::os::fs::{DirBuilderExt, OpenOptionsExt};
use krill_proto::os::net::{UnixListener, UnixStream};

use crate::os::{AsFd, OwnedFd};
use crate::sys;

pub use krill_proto::daemon::SUN_PATH_MAX;
/// 핸드셰이크 중 클라이언트 응답을 기다리는 최대 시간. 멈춘 클라이언트가 accept 흐름을 붙잡지 못하게 한다.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

mod machine_identity;

static MACHINE_IDENTITY: OnceLock<Option<ipc::MachineIdentity>> = OnceLock::new();

/// 실제 listen 소켓의 영구 신원을 클라이언트를 받기 전에 한 번 준비한다.
pub fn initialize_machine_identity(socket: &Path) {
    MACHINE_IDENTITY.get_or_init(|| match machine_identity::load(socket) {
        Ok(identity) => Some(identity),
        Err(error) => {
            tracing::warn!(kind = ?error.kind(), "machine identity unavailable");
            None
        }
    });
}

/// krilld의 listen 소켓. 자기와 같은 유효 UID의 피어만 받는다. 살아 있는 동안 단일 인스턴스 락을 쥔다.
#[derive(Debug)]
pub struct Listener {
    sock: UnixListener,
    uid: u32,
    /// `<sock>.lock`의 flock. 닫히면(프로세스가 어떻게 죽든) 커널이 푼다.
    _lock: File,
}

/// `path` 뒤에 `suffix`를 붙인 경로.
pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = OsString::from(path);
    s.push(suffix);
    PathBuf::from(s)
}

impl Listener {
    /// `path`에 소켓을 게시한다. 부모 디렉터리가 없으면 0700으로 만들고, 있으면 소유자가 나이고 그룹·기타 권한이
    /// 없는지 확인한다. 소켓·락 파일은 0600. 임시 이름(`<path>.tmp`)도 104바이트 제한을 지켜야 해서 `path`는
    /// 100바이트 미만이어야 한다.
    ///
    /// # Errors
    /// 다른 krilld가 같은 `path`의 락을 쥐고 있으면 `AddrInUse`(호출자는 조용히 끝낸다). 그 밖의 IO 오류.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let tmp = with_suffix(path, ".tmp");
        if tmp.as_os_str().len() >= SUN_PATH_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "socket path must be shorter than 100 bytes",
            ));
        }
        let uid = crate::os::euid();
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .ok_or(io::ErrorKind::InvalidInput)?;
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let m = fs::symlink_metadata(dir)?;
        if !m.is_dir() || !crate::os::private_to_me(&m, 0o077) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket directory must be a 0700 directory owned by this user",
            ));
        }
        let lock = lock(path)?;
        // 락을 쥔 프로세스만 여기 온다. 남은 `.tmp`는 bind와 rename 사이에 죽은 이전 소유자의 것이다.
        match fs::remove_file(&tmp) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let sock = UnixListener::bind(&tmp)?;
        #[cfg(unix)]
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp, path)?;
        Ok(Self {
            sock,
            uid,
            _lock: lock,
        })
    }

    /// holder가 맡아 두었다 돌려준 listen 소켓 `sock`(앞선 krilld가 `path`에 게시한 것)을 이어 쓴다(OWE-491).
    /// 다시 게시하지 않으므로 krilld가 없던 동안 온 연결도 backlog에서 받는다. 락은 [`Listener::bind`]와 같다.
    ///
    /// # Errors
    /// 다른 krilld가 같은 `path`의 락을 쥐고 있으면 `AddrInUse`. 그 밖의 IO 오류.
    #[cfg(unix)]
    pub fn adopt(path: &Path, sock: OwnedFd) -> io::Result<Self> {
        Ok(Self {
            _lock: lock(path)?,
            sock: UnixListener::from(sock),
            uid: crate::os::euid(),
        })
    }

    /// Windows holder가 `WSADuplicateSocket`으로 돌려준 listener를 이어 쓴다.
    ///
    /// # Errors
    /// 소켓 단일 인스턴스 락을 얻지 못하면 `AddrInUse`.
    #[cfg(windows)]
    pub fn adopt(path: &Path, sock: OwnedFd) -> io::Result<Self> {
        Ok(Self {
            _lock: lock(path)?,
            sock: sys::adopt_listener(sock),
            uid: crate::os::euid(),
        })
    }

    /// 다음 연결을 받는다. 피어 UID가 다르면 아무것도 보내지 않고 닫은 뒤 `PermissionDenied`를 돌려준다
    /// (호출자는 기록하고 계속 accept한다).
    pub fn accept(&self) -> io::Result<UnixStream> {
        let (s, _) = self.sock.accept()?;
        let peer = sys::peer_uid(s.as_fd())?;
        if peer != self.uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("peer uid {peer} is not {}", self.uid),
            ));
        }
        Ok(s)
    }
}

#[cfg(unix)]
impl std::os::fd::AsFd for Listener {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.sock.as_fd()
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsSocket for Listener {
    fn as_socket(&self) -> std::os::windows::io::BorrowedSocket<'_> {
        sys::listener_socket(&self.sock)
    }
}

/// `<path>.lock`을 0600으로 열어 flock을 잡는다. 이미 잡혀 있으면 `AddrInUse`.
fn lock(path: &Path) -> io::Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(with_suffix(path, ".lock"))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "another krilld holds the socket lock",
        )),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

fn daemon_identity() -> Option<ipc::DaemonIdentity> {
    static IDENTITY: OnceLock<Option<ipc::DaemonIdentity>> = OnceLock::new();
    *IDENTITY.get_or_init(|| {
        let mut bytes = [0; 16];
        if SystemRandom::new().fill(&mut bytes).is_err() {
            tracing::warn!("daemon identity unavailable");
            return None;
        }
        Some(ipc::DaemonIdentity(bytes))
    })
}

/// `hello`를 보내고 [`ClientHello`]를 받아 협상한다. 실패하면 호출자가 연결을 닫는다.
pub fn handshake(s: &mut UnixStream, hello: &ServerHello) -> io::Result<(ClientHello, Negotiated)> {
    s.set_read_timeout(Some(HELLO_TIMEOUT))?;
    let frame = match daemon_identity() {
        Some(identity) => {
            hello.encode_with_identities(identity, MACHINE_IDENTITY.get().copied().flatten())?
        }
        None => hello.encode()?,
    };
    s.write_all(&frame)?;
    let (kind, body) = ipc::read_frame(s)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    let client = ClientHello::decode(kind, &body)?;
    let n = ipc::negotiate(hello, &client).ok_or(ipc::Error::Incompatible)?;
    // macOS는 피어가 이미 닫은 소켓의 setsockopt를 EINVAL로 거부한다(OWE-487). 협상은 끝났고 닫힘은 다음
    // 읽기에서 EOF로 드러나므로 실패로 보지 않는다.
    s.set_read_timeout(None).ok();
    Ok((client, n))
}

#[cfg(test)]
#[cfg(unix)]
mod tests;

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::os::windows::io::AsSocket as _;

    #[test]
    fn adopted_listener_keeps_socket_and_reacquires_instance_lock() {
        let dir = std::env::temp_dir().join(format!("kd-adopt-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let path = dir.join("k.sock");
        let original = Listener::bind(&path).unwrap();
        let socket = original.as_socket().try_clone_to_owned().unwrap();
        drop(original);
        let adopted = Listener::adopt(&path, socket).unwrap();
        assert_eq!(
            Listener::bind(&path).unwrap_err().kind(),
            io::ErrorKind::AddrInUse
        );
        let client = UnixStream::connect(&path).unwrap();
        let server = adopted.accept().unwrap();
        drop((client, server, adopted));
        fs::remove_dir_all(dir).unwrap();
    }
}
