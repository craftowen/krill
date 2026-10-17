#![forbid(unsafe_code)]
//! MK-131: 전송 무관 호스트 타깃 추상화(`HostId`, `HostTarget`)와 원격 연결 관리자(`HostManager`).
//!
//! - [`HostTarget`]은 로컬에서 접근 가능한 Unix 소켓 경로와 `allow_local_launch: bool` 정책만 제공한다.
//!   - [`HostId::Local`]: `allow_local_launch == true` — 로컬 `krill-holder`/`krilld` 자동 기동을 허용한다.
//!   - [`HostId::Remote`]: `allow_local_launch == false` — 절대 로컬 `krill-holder`/`krilld`를 띄우지 않고
//!     포워딩된 Unix 소켓에만 핸드셰이크한다.
//! - 진행 중인 MK-130(iroh QUIC)이 추후 같은 [`HostTarget`]에 로컬 포워딩 소켓만 꽂을 수 있도록
//!   UI 계층에는 SSH 전용 타입을 노출하지 않는다.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use krill_proto::daemon::{self, Connection, Paths};
use krill_proto::ipc::ClientHello;
use serde::{Deserialize, Serialize};

pub mod manager;
pub mod ssh;

pub use manager::{ExternalHost, HostConnectionInfo};
pub use manager::{HostManager, RemoteHostSpec};

/// 호스트 식별자: 내 Mac(`Local`) 또는 원격 호스트(`Remote(id)`).
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HostId {
    #[default]
    Local,
    Remote(String),
}

impl HostId {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }

    pub fn remote_id(&self) -> Option<&str> {
        match self {
            Self::Local => None,
            Self::Remote(id) => Some(id),
        }
    }

    pub fn as_key(&self) -> &str {
        match self {
            Self::Local => "local",
            Self::Remote(id) => id,
        }
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => f.write_str("local"),
            Self::Remote(id) => f.write_str(id),
        }
    }
}

/// 원격/로컬 호스트의 연결 상태.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostStatus {
    Connected,
    #[default]
    Connecting,
    Disconnected,
}

type EnsureSocketFn = dyn Fn() -> io::Result<PathBuf> + Send + Sync;
type OutcomeFn = dyn Fn(bool, Option<&io::Error>) + Send + Sync;

/// 원격 전송 계층(SSH 터널, 추후 iroh QUIC 등)이 로컬 포워딩 소켓을 보장하고 연결 결과를 기록하는 훅.
#[derive(Clone)]
pub struct RemoteConnector {
    ensure_socket: Arc<EnsureSocketFn>,
    on_outcome: Arc<OutcomeFn>,
    map_error: Option<Arc<dyn Fn(io::Error) -> io::Error + Send + Sync>>,
}

impl fmt::Debug for RemoteConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteConnector").finish_non_exhaustive()
    }
}

impl RemoteConnector {
    pub fn new(
        ensure_socket: impl Fn() -> io::Result<PathBuf> + Send + Sync + 'static,
        on_outcome: impl Fn(bool, Option<&io::Error>) + Send + Sync + 'static,
    ) -> Self {
        Self {
            ensure_socket: Arc::new(ensure_socket),
            on_outcome: Arc::new(on_outcome),
            map_error: None,
        }
    }

    #[must_use]
    pub fn with_error_mapper(
        mut self,
        mapper: impl Fn(io::Error) -> io::Error + Send + Sync + 'static,
    ) -> Self {
        self.map_error = Some(Arc::new(mapper));
        self
    }
}

/// 전송 무관 호스트 타깃.
///
/// 로컬에서 접근 가능한 Unix 소켓 경로와 `allow_local_launch` 정책만 제공한다.
#[derive(Debug, Clone)]
pub struct HostTarget {
    host: HostId,
    socket: PathBuf,
    allow_local_launch: bool,
    local_paths: Option<Paths>,
    connector: Option<RemoteConnector>,
}

impl PartialEq for HostTarget {
    fn eq(&self, other: &Self) -> bool {
        self.host == other.host
            && self.socket == other.socket
            && self.allow_local_launch == other.allow_local_launch
            && self.local_paths == other.local_paths
    }
}

impl Eq for HostTarget {}

impl From<&HostTarget> for HostTarget {
    fn from(target: &HostTarget) -> Self {
        target.clone()
    }
}

impl From<Paths> for HostTarget {
    fn from(paths: Paths) -> Self {
        Self::local(paths)
    }
}

impl From<&Paths> for HostTarget {
    fn from(paths: &Paths) -> Self {
        Self::local(paths.clone())
    }
}

impl HostTarget {
    /// 로컬 호스트 타깃(`allow_local_launch = true`).
    pub fn local(paths: Paths) -> Self {
        Self {
            host: HostId::Local,
            socket: paths.socket.clone(),
            allow_local_launch: true,
            local_paths: Some(paths),
            connector: None,
        }
    }

    /// 기본 개발/번들 경로 기반 로컬 호스트 타깃.
    pub fn local_dev() -> io::Result<Self> {
        Paths::dev().map(Self::local)
    }

    /// 정적 포워딩 소켓 경로를 갖는 원격 호스트 타깃(`allow_local_launch = false`).
    pub fn remote(id: impl Into<String>, socket: PathBuf) -> Self {
        Self {
            host: HostId::Remote(id.into()),
            socket,
            allow_local_launch: false,
            local_paths: None,
            connector: None,
        }
    }

    /// [`HostManager`] 등 연결 관리자와 연동되는 원격 호스트 타깃(`allow_local_launch = false`).
    pub fn remote_with_connector(
        id: impl Into<String>,
        socket: PathBuf,
        connector: RemoteConnector,
    ) -> Self {
        Self {
            host: HostId::Remote(id.into()),
            socket,
            allow_local_launch: false,
            local_paths: None,
            connector: Some(connector),
        }
    }

    pub fn host(&self) -> &HostId {
        &self.host
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn allow_local_launch(&self) -> bool {
        self.allow_local_launch
    }

    /// 로컬 실행이 허용된 타깃일 때만 [`Paths`]를 돌려준다(`Remote`는 항상 `None`).
    pub fn local_paths(&self) -> Option<&Paths> {
        self.local_paths
            .as_ref()
            .filter(|_| self.allow_local_launch)
    }

    /// 재사용 중인 소켓의 요청 성공/실패를 연결 관리자에 알린다.
    pub fn record_outcome(&self, ok: bool, err: Option<&io::Error>) {
        if let Some(c) = &self.connector {
            (c.on_outcome)(ok, err);
        }
    }

    /// 호스트 데몬에 연결한다.
    ///
    /// - `allow_local_launch == true` (`Local`): 소켓이 없으면 로컬 `krill-holder`/`krilld`를 기동한다.
    /// - `allow_local_launch == false` (`Remote`): 절대 로컬 `krill-holder`/`krilld`를 띄우지 않고
    ///   포워딩된 소켓에만 Hello 협상을 수행한다.
    pub fn connect(&self, hello: &ClientHello) -> Result<Connection, daemon::Error> {
        if self.allow_local_launch
            && let Some(paths) = &self.local_paths
        {
            return daemon::connect(paths, hello);
        }
        let socket = match &self.connector {
            Some(c) => match (c.ensure_socket)() {
                Ok(sock) => sock,
                Err(err) => {
                    (c.on_outcome)(false, Some(&err));
                    let err = match &c.map_error {
                        Some(mapper) => mapper(err),
                        None => normalize_remote_io_error(err),
                    };
                    return Err(daemon::Error::Io(err));
                }
            },
            None => self.socket.clone(),
        };
        match ssh::dial_tunnel(&socket, hello, ssh::HELLO_TIMEOUT) {
            Ok(conn) => {
                if let Some(c) = &self.connector {
                    (c.on_outcome)(true, None);
                }
                Ok(conn)
            }
            Err(err) => {
                let norm = match self.connector.as_ref().and_then(|c| c.map_error.as_ref()) {
                    Some(mapper) => mapper(err),
                    None => normalize_remote_io_error(err),
                };
                if let Some(c) = &self.connector {
                    (c.on_outcome)(false, Some(&norm));
                }
                Err(daemon::Error::Io(norm))
            }
        }
    }

    /// 연결될 때까지 지수 백오프로 재시도한다.
    pub fn reconnect_until(&self, hello: &ClientHello) -> Connection {
        if self.allow_local_launch
            && let Some(paths) = &self.local_paths
        {
            return daemon::reconnect_until(paths, hello);
        }
        let mut delay = Duration::from_millis(200);
        loop {
            if let Ok(c) = self.connect(hello) {
                return c;
            }
            thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_secs(5));
        }
    }

    /// 로컬 타깃일 때만 데몬 빌드 업그레이드를 수행한다(`Remote`는 항상 `Ok(None)`).
    pub fn upgrade(
        &self,
        hello: &ClientHello,
    ) -> io::Result<Option<krill_proto::ipc::ServerHello>> {
        if self.allow_local_launch
            && let Some(paths) = &self.local_paths
        {
            return daemon::upgrade(paths, hello);
        }
        Ok(None)
    }
}

/// 원격 포워딩 소켓이 아직 열리지 않았을 때의 `NotFound`를 일시적 연결 거부(`ConnectionRefused`)로 정규화해
/// 상위 재연결 루프가 영구 실패로 오판하지 않게 한다.
fn normalize_remote_io_error(err: io::Error) -> io::Error {
    if matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::WouldBlock
    ) {
        io::Error::new(io::ErrorKind::ConnectionRefused, err.to_string())
    } else {
        err
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::ipc::ClientRole;

    #[test]
    fn remote_target_disallows_local_launch_and_never_spawns_daemon() {
        let dir = std::env::temp_dir().join(format!("krill-hosts-remote-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let sock = dir.join("run/missing-remote.sock");
        let target = HostTarget::remote("studio", sock.clone());

        assert_eq!(target.host(), &HostId::Remote("studio".to_owned()));
        assert!(!target.allow_local_launch());
        assert!(target.local_paths().is_none());
        assert_eq!(target.socket(), sock.as_path());

        let hello = daemon::client_hello(ClientRole::App, "krill-hosts-test");
        let err = target
            .connect(&hello)
            .expect_err("must fail without spawning");
        assert!(
            matches!(err, daemon::Error::Io(ref e) if e.kind() == io::ErrorKind::ConnectionRefused),
            "unexpected error: {err:?}"
        );
        assert!(
            !sock.exists(),
            "remote target must never create a local socket"
        );
        assert_eq!(target.upgrade(&hello).expect("upgrade noop"), None);
    }

    #[test]
    fn local_target_allows_local_launch() {
        let paths = Paths {
            socket: PathBuf::from("/tmp/local-krilld.sock"),
            holder: PathBuf::from("/tmp/krill-holder"),
            krilld: PathBuf::from("/tmp/krilld"),
        };
        let target = HostTarget::local(paths.clone());
        assert_eq!(target.host(), &HostId::Local);
        assert!(target.allow_local_launch());
        assert_eq!(target.local_paths(), Some(&paths));
    }

    #[test]
    fn exponential_backoff_doubles_and_caps_at_five_seconds() {
        assert_eq!(HostManager::backoff_delay(1), Duration::from_millis(200));
        assert_eq!(HostManager::backoff_delay(2), Duration::from_millis(400));
        assert_eq!(HostManager::backoff_delay(3), Duration::from_millis(800));
        assert_eq!(HostManager::backoff_delay(4), Duration::from_millis(1_600));
        assert_eq!(HostManager::backoff_delay(5), Duration::from_millis(3_200));
        assert_eq!(HostManager::backoff_delay(6), Duration::from_secs(5));
        assert_eq!(HostManager::backoff_delay(99), Duration::from_secs(5));
    }
}
