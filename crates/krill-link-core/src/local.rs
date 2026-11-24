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

#[cfg(any(windows, test))]
fn write_cancellable(
    writer: &mut impl std::io::Write,
    mut bytes: &[u8],
    is_closed: impl Fn() -> bool,
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    while !bytes.is_empty() {
        if is_closed() {
            return Err(ErrorKind::Interrupted.into());
        }
        match writer.write(bytes) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(count) => {
                bytes = bytes
                    .get(count..)
                    .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::Interrupted | ErrorKind::WouldBlock | ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(windows)]
mod windows {
    use std::io::{self, Read as _};
    use std::net::Shutdown;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::task::{Context, Poll, Waker};
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
        written: AtomicU64,
        write_done: AtomicBool,
        write_failed: AtomicBool,
        write_waiter: std::sync::Mutex<Option<Waker>>,
    }

    impl Shared {
        fn wake_writer(&self) {
            let wake = self
                .write_waiter
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(wake) = wake {
                wake.wake();
            }
        }

        fn poll_written(
            &self,
            cx: &Context<'_>,
            queued: u64,
            shutdown: bool,
        ) -> Poll<io::Result<()>> {
            let mut wake = self
                .write_waiter
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.write_failed.load(Ordering::Acquire) {
                return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
            }
            let ready = if shutdown {
                self.write_done.load(Ordering::Acquire)
            } else {
                self.written.load(Ordering::Acquire) >= queued
            };
            if ready {
                Poll::Ready(Ok(()))
            } else {
                *wake = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    /// tokio 쪽 끝. 드롭하면 소켓을 닫는다.
    #[derive(Debug)]
    pub struct Stream {
        io: DuplexStream,
        shared: Arc<Shared>,
        queued: u64,
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
            sock.set_write_timeout(Some(CLOSE_POLL))?;
            let shared = Arc::new(Shared {
                sock,
                closed: AtomicBool::new(false),
                written: AtomicU64::new(0),
                write_done: AtomicBool::new(false),
                write_failed: AtomicBool::new(false),
                write_waiter: std::sync::Mutex::new(None),
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
            let started = std::thread::Builder::new()
                .name("krill-link-uds-write".to_owned())
                .spawn(move || {
                    let mut buf = vec![0; BRIDGE_BUFFER];
                    // EOF = tokio 쪽 shutdown(반쪽 닫기) 또는 드롭.
                    while let Ok(n @ 1..) = rt.block_on(from_tokio.read(&mut buf)) {
                        if super::write_cancellable(&mut &writer.sock, &buf[..n], || {
                            writer.closed.load(Ordering::Acquire)
                        })
                        .is_err()
                        {
                            writer.write_failed.store(true, Ordering::Release);
                            break;
                        }
                        writer.written.fetch_add(n as u64, Ordering::Release);
                        writer.wake_writer();
                    }
                    if writer.sock.shutdown(Shutdown::Write).is_err() {
                        writer.write_failed.store(true, Ordering::Release);
                    }
                    writer.write_done.store(true, Ordering::Release);
                    writer.wake_writer();
                });
            if let Err(error) = started {
                shared.closed.store(true, Ordering::Release);
                return Err(error);
            }
            Ok(Self {
                io,
                shared,
                queued: 0,
            })
        }
    }

    /// 드롭하면 막힌 소켓 읽기·쓰기도 [`CLOSE_POLL`] 뒤 취소되고 두 스레드가 끝나면 소켓이 닫힌다.
    /// 전송 완료가 필요하면 드롭 전에 flush 또는 shutdown을 기다린다.
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
            let this = self.get_mut();
            match Pin::new(&mut this.io).poll_write(cx, buf) {
                Poll::Ready(Ok(n)) => {
                    this.queued = this.queued.saturating_add(n as u64);
                    Poll::Ready(Ok(n))
                }
                other => other,
            }
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            std::task::ready!(Pin::new(&mut this.io).poll_flush(cx))?;
            // A DuplexStream flush only reaches the bridge buffer. Wait for the
            // socket writer too, so a server can exit after sending its reply.
            this.shared.poll_written(cx, this.queued, false)
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            std::task::ready!(this.shared.poll_written(cx, this.queued, false))?;
            std::task::ready!(Pin::new(&mut this.io).poll_shutdown(cx))?;
            this.shared.poll_written(cx, this.queued, true)
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
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write as _;
        use std::time::Instant;

        #[tokio::test]
        async fn dropping_stream_releases_workers_when_peer_never_reads() {
            let (mut socket, peer) = net::UnixStream::pair().unwrap();
            socket.set_nonblocking(true).unwrap();
            let buffer = vec![1; BRIDGE_BUFFER];
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match socket.write(&buffer) {
                    Ok(_) => assert!(Instant::now() < deadline, "socket did not saturate"),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                        ) =>
                    {
                        break;
                    }
                    Err(error) => panic!("fill socket: {error}"),
                }
            }
            let mut stream = Stream::bridge(socket, &Handle::current()).unwrap();
            let resources = Arc::downgrade(&stream.shared);
            stream.write_all(b"queued after saturation").await.unwrap();
            drop(stream);
            let deadline = Instant::now() + Duration::from_secs(3);
            while resources.upgrade().is_some() {
                assert!(
                    Instant::now() < deadline,
                    "socket workers retained a dropped stream"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            drop(peer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Listener, Stream};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[test]
    fn partial_writes_retry_only_the_unwritten_suffix_and_cancel() {
        use std::collections::VecDeque;
        use std::io::{self, Write};
        struct Sink {
            steps: VecDeque<io::Result<usize>>,
            bytes: Vec<u8>,
        }
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let count = self.steps.pop_front().unwrap_or(Ok(bytes.len()))?;
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut sink = Sink {
            steps: [
                Ok(2),
                Err(io::ErrorKind::TimedOut.into()),
                Ok(1),
                Err(io::ErrorKind::Interrupted.into()),
            ]
            .into(),
            bytes: Vec::new(),
        };
        super::write_cancellable(&mut sink, b"abcdef", || false).unwrap();
        assert_eq!(sink.bytes, b"abcdef");
        assert_eq!(
            super::write_cancellable(&mut sink, b"later", || true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(sink.bytes, b"abcdef");
        let mut partial = Sink {
            steps: [Ok(2)].into(),
            bytes: Vec::new(),
        };
        let checks = std::cell::Cell::new(0);
        let cancelled = super::write_cancellable(&mut partial, b"abcdef", || {
            let previous = checks.get();
            checks.set(previous + 1);
            previous > 0
        });
        assert_eq!(cancelled.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(partial.bytes, b"ab");
    }

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
