//! 로컬 `AF_UNIX` 소켓(`krilld.sock`·`run/hosts/<id>.sock`·`run/link-ctl.sock`).
//!
//! - Unix: tokio `UnixListener`·`UnixStream` 그대로.
//! - Windows: tokio에 `AF_UNIX`가 없어 `uds_windows` 블로킹 소켓을 스레드로 tokio `DuplexStream`에 잇는다.
//!   연결 하나에 읽기·쓰기 스레드 2개(스트림 상한 [`crate::resume::MAX_STREAMS`]), 리스너 하나에 accept 스레드 1개.
//!
//! 통합 테스트가 같은 타입으로 양쪽 플랫폼을 돌도록 `doc(hidden)` 공개한다.
//!
//! Windows 접근 통제: 소켓은 상태 디렉터리(`%LOCALAPPDATA%\Krill\run\`) 아래에 있고 `mode`는 no-op이다.
//! 다른 표준 사용자는 `%LOCALAPPDATA%` 상속 ACL이 connect를 막는다(OWE-827 실측 10013). 피어 PID·SID 확인은
//! unsafe FFI라 이 크레이트(`forbid(unsafe_code)`)에 넣지 않는다 — 명시 DACL·피어 SID 검사는 krilld FFI와 함께 후속.

#[cfg(unix)]
pub use tokio::net::{UnixListener as Listener, UnixStream as Stream};

#[cfg(windows)]
pub use windows::{Listener, Stream};

#[cfg(windows)]
mod windows {
    use std::io::{self, Read as _, Write as _};
    use std::net::Shutdown;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};
    use std::time::Duration;

    use krill_proto::os::net;
    use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, DuplexStream};
    use tokio::runtime::Handle;
    use tokio::sync::{Mutex, mpsc};

    const BRIDGE_BUFFER: usize = 64 * 1024;
    // ponytail: 블로킹 recv는 다른 스레드의 shutdown(Both)으로 깨지 않는다(VM 실측). 닫힘 확인 주기 = 종료 지연 상한.
    const CLOSE_POLL: Duration = Duration::from_millis(250);
    // ponytail: 논블로킹 accept 폴링. 새 연결 지연 상한 50ms, 줄여야 하면 깨우기용 자기 connect로 바꾼다.
    const ACCEPT_POLL: Duration = Duration::from_millis(50);

    fn no_runtime(_: tokio::runtime::TryCurrentError) -> io::Error {
        io::Error::other("local socket requires a tokio runtime")
    }

    #[derive(Debug)]
    struct Shared {
        sock: net::UnixStream,
        closed: AtomicBool,
    }

    /// tokio 쪽 끝. 드롭하면 소켓을 닫는다.
    #[derive(Debug)]
    pub struct Stream {
        io: DuplexStream,
        shared: Arc<Shared>,
    }

    impl Stream {
        pub async fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
            let path: PathBuf = path.as_ref().to_path_buf();
            let sock = tokio::task::spawn_blocking(move || net::UnixStream::connect(path))
                .await
                .map_err(io::Error::other)??;
            Self::bridge(sock, &Handle::try_current().map_err(no_runtime)?)
        }

        fn bridge(sock: net::UnixStream, handle: &Handle) -> io::Result<Self> {
            sock.set_nonblocking(false)?;
            sock.set_read_timeout(Some(CLOSE_POLL))?;
            let shared = Arc::new(Shared {
                sock,
                closed: AtomicBool::new(false),
            });
            let (io, bridge) = tokio::io::duplex(BRIDGE_BUFFER);
            let (mut from_tokio, mut to_tokio) = tokio::io::split(bridge);
            let reader = Arc::clone(&shared);
            let rt = handle.clone();
            std::thread::Builder::new()
                .name("krill-link-uds-read".to_owned())
                .spawn(move || {
                    let mut buf = vec![0; BRIDGE_BUFFER];
                    loop {
                        match (&reader.sock).read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                if rt.block_on(to_tokio.write_all(&buf[..n])).is_err() {
                                    break;
                                }
                            }
                            Err(e)
                                if matches!(
                                    e.kind(),
                                    io::ErrorKind::TimedOut
                                        | io::ErrorKind::WouldBlock
                                        | io::ErrorKind::Interrupted
                                ) =>
                            {
                                if reader.closed.load(Ordering::Acquire) {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    rt.block_on(to_tokio.shutdown()).ok();
                })?;
            let writer = Arc::clone(&shared);
            let rt = handle.clone();
            std::thread::Builder::new()
                .name("krill-link-uds-write".to_owned())
                .spawn(move || {
                    let mut buf = vec![0; BRIDGE_BUFFER];
                    // EOF = tokio 쪽 shutdown(반쪽 닫기) 또는 드롭.
                    while let Ok(n @ 1..) = rt.block_on(from_tokio.read(&mut buf)) {
                        if (&writer.sock).write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    writer.sock.shutdown(Shutdown::Write).ok();
                })?;
            Ok(Self { io, shared })
        }
    }

    /// 쓰기 스레드는 duplex에 남은 바이트를 마저 보내고 반쪽 닫는다(Unix close처럼 보낸 데이터를 버리지 않음).
    /// 읽기 스레드는 [`CLOSE_POLL`] 안에 끝나고, 두 스레드가 다 끝나면 소켓이 닫힌다.
    impl Drop for Stream {
        fn drop(&mut self) {
            self.shared.closed.store(true, Ordering::Release);
        }
    }

    impl AsyncRead for Stream {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().io).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for Stream {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.get_mut().io).poll_write(cx, buf)
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().io).poll_flush(cx)
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
        }
    }

    /// accept 스레드가 받은 연결을 넘겨준다. 드롭하면 스레드가 [`ACCEPT_POLL`] 안에 리스너를 닫는다.
    #[derive(Debug)]
    pub struct Listener {
        rx: Mutex<mpsc::Receiver<io::Result<(Stream, net::SocketAddr)>>>,
    }

    impl Listener {
        pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
            let handle = Handle::try_current().map_err(no_runtime)?;
            let listener = net::UnixListener::bind(path)?;
            listener.set_nonblocking(true)?;
            let (tx, rx) = mpsc::channel(8);
            std::thread::Builder::new()
                .name("krill-link-uds-accept".to_owned())
                .spawn(move || {
                    while !tx.is_closed() {
                        let accepted = match listener.accept() {
                            Ok((sock, addr)) => Stream::bridge(sock, &handle).map(|s| (s, addr)),
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                                std::thread::sleep(ACCEPT_POLL);
                                continue;
                            }
                            Err(e) => Err(e),
                        };
                        if tx.blocking_send(accepted).is_err() {
                            break;
                        }
                    }
                })?;
            Ok(Self { rx: Mutex::new(rx) })
        }

        /// tokio `UnixListener::accept`와 같은 모양.
        pub async fn accept(&self) -> io::Result<(Stream, net::SocketAddr)> {
            match self.rx.lock().await.recv().await {
                Some(accepted) => accepted,
                None => Err(io::Error::other("local listener stopped")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Listener, Stream};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn local_socket_round_trips_and_half_closes() {
        let dir = std::env::temp_dir().join(format!("kl-local-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.sock");
        std::fs::remove_file(&path).ok();
        let listener = Listener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut got = Vec::new();
            s.read_to_end(&mut got).await.unwrap();
            s.write_all(&got).await.unwrap();
            s.shutdown().await.unwrap();
        });
        let mut c = Stream::connect(&path).await.unwrap();
        let payload = vec![7u8; 200_000];
        c.write_all(&payload).await.unwrap();
        c.shutdown().await.unwrap();
        let mut back = Vec::new();
        c.read_to_end(&mut back).await.unwrap();
        assert_eq!(back, payload);
        server.await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
