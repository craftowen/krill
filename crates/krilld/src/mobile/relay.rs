//! `krill-rendezvous` Durable Object WebSocket Hibernation 멀티플렉싱 E2EE 릴레이 클라이언트.
//! 데몬(`krilld`)이 아웃바운드 WebSocket 1개로 DO에 연결해 다중 모바일 클라이언트(`conn_id`)의
//! 1-RTT 파이프라인 핸드셰이크·RPC·터미널 스트림을 종단간 암호화(E2EE v2) 그대로 중계한다.
use super::devices::{decode_hex, hex};
use super::e2ee::{Channel, Endpoint, MAX_PLAINTEXT, invalid, random};
use super::gateway::{
    AuthHello, AuthProof, Authenticated, Connection, MAX_CONNECTIONS, MAX_PENDING, RPC_STACK,
    Shared, ShutdownHandle, TIMEOUT, now_ms,
};
use super::tunnel::Publication;
use super::ws;
use crate::registry::Registry;
use crate::session::lock;
use krill_proto::ipc::{self, ServerHello};
use krill_proto::os::net::UnixStream;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

pub const MUX_OPEN: u8 = 1;
pub const MUX_TEXT: u8 = 2;
pub const MUX_BINARY: u8 = 3;
pub const MUX_CLOSE: u8 = 4;

mod mailbox;
use mailbox::Mailbox;

const STACK: usize = 128 * 1024;
const OUTBOX_BYTES: usize = 4 * MAX_PLAINTEXT;
const PEER_QUEUE_BYTES: usize = 2 * MAX_PLAINTEXT;
const READ_BUDGET: usize = 64 * 1024;
const MAX_RX_BUFFER: usize = MAX_PLAINTEXT + 80;
const PING: Duration = Duration::from_secs(30);

pub fn encode_mux(kind: u8, conn_id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len().saturating_add(5));
    out.push(kind);
    out.extend_from_slice(&conn_id.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn decode_mux(bytes: &[u8]) -> io::Result<(u8, u32, &[u8])> {
    let kind = *bytes.first().ok_or_else(invalid)?;
    let id_slice = bytes.get(1..5).ok_or_else(invalid)?;
    let conn_id = u32::from_be_bytes(id_slice.try_into().map_err(|_| invalid())?);
    let payload = bytes.get(5..).ok_or_else(invalid)?;
    Ok((kind, conn_id, payload))
}

#[derive(Debug, Default)]
struct Stop {
    stopped: Mutex<bool>,
    changed: Condvar,
}
impl Stop {
    fn cancelled(&self) -> bool {
        *lock(&self.stopped)
    }
    fn wait(&self, delay: Duration) -> bool {
        let guard = lock(&self.stopped);
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, delay, |stopped| !*stopped)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard
    }
    fn cancel(&self) {
        *lock(&self.stopped) = true;
        self.changed.notify_all();
    }
}

pub(super) struct RelayClient {
    connected: Arc<AtomicBool>,
    settled: Arc<AtomicBool>,
    assets: Arc<AtomicBool>,
    stop: Arc<Stop>,
    wake_tx: Option<UnixStream>,
}
impl std::fmt::Debug for RelayClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayClient")
            .field("connected", &self.connected.load(Ordering::Acquire))
            .field("settled", &self.settled.load(Ordering::Acquire))
            .field("assets", &self.assets.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}
impl Drop for RelayClient {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(mut tx) = self.wake_tx.take() {
            tx.write_all(&[1]).ok();
        }
    }
}

#[derive(Clone)]
struct Outbox {
    tx: Arc<Mailbox<Vec<u8>>>,
    wake: Arc<Mutex<UnixStream>>,
}
impl Outbox {
    fn send_mux(&self, kind: u8, conn_id: u32, payload: &[u8]) -> io::Result<()> {
        self.tx.send(
            conn_id,
            encode_mux(kind, conn_id, payload),
            payload.len().saturating_add(5),
        )?;
        lock(&self.wake).write_all(&[1]).ok();
        Ok(())
    }
    fn send_close(&self, conn_id: u32, code: u16) {
        self.send_mux(MUX_CLOSE, conn_id, &code.to_be_bytes()).ok();
    }
}

struct MuxSession {
    tx: Arc<Mailbox<(u8, Vec<u8>)>>,
    stream: UnixStream,
}

enum Transport {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}
impl Transport {
    fn tcp(&self) -> &TcpStream {
        match self {
            Self::Plain(s) => s,
            Self::Tls(s) => &s.sock,
        }
    }
    fn write_masked(&mut self, opcode: u8, body: &[u8]) -> io::Result<()> {
        let mask_bytes = random()?;
        let mask: [u8; 4] = mask_bytes
            .get(..4)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?;
        let frame = ws::encode_masked(opcode, body, mask)?;
        self.tcp().set_nonblocking(false)?;
        let res = match self {
            Self::Plain(s) => s.write_all(&frame).and_then(|()| s.flush()),
            // StreamOwned drains TLS records between partial plaintext writes.
            // Writing the whole frame into conn.writer() first exhausts rustls'
            // 64 KiB buffer on large snapshots and disconnects every client.
            Self::Tls(s) => s.write_all(&frame).and_then(|()| s.flush()),
        };
        self.tcp().set_nonblocking(true).ok();
        res
    }
    fn read_available(&mut self, rx_buf: &mut Vec<u8>) -> io::Result<(bool, bool)> {
        self.tcp().set_nonblocking(true)?;
        let mut budget = READ_BUDGET;
        let mut chunk = [0_u8; 8192];
        match self {
            Self::Plain(socket) => loop {
                let room = read_room(rx_buf, budget);
                if room == 0 {
                    return Ok((true, true));
                }
                match socket.read(chunk.get_mut(..room).ok_or_else(invalid)?) {
                    Ok(0) => return Ok((false, false)),
                    Ok(n) => {
                        rx_buf.extend_from_slice(chunk.get(..n).ok_or_else(invalid)?);
                        budget -= n;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok((true, false)),
                    Err(e) => return Err(e),
                }
            },
            Self::Tls(stream) => loop {
                loop {
                    let room = read_room(rx_buf, budget);
                    if room == 0 {
                        return Ok((true, true));
                    }
                    match stream
                        .conn
                        .reader()
                        .read(chunk.get_mut(..room).ok_or_else(invalid)?)
                    {
                        Ok(0) => break,
                        Ok(n) => {
                            rx_buf.extend_from_slice(chunk.get(..n).ok_or_else(invalid)?);
                            budget -= n;
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => return Err(e),
                    }
                }
                match stream.conn.read_tls(&mut stream.sock) {
                    Ok(0) => return Ok((false, false)),
                    Ok(_) => {
                        stream
                            .conn
                            .process_new_packets()
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok((true, false)),
                    Err(e) => return Err(e),
                }
            },
        }
    }
}
fn read_room(rx_buf: &[u8], budget: usize) -> usize {
    8192.min(budget)
        .min(MAX_RX_BUFFER.saturating_sub(rx_buf.len()))
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Tls(s) => s.read(buf),
        }
    }
}
impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            Self::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

fn tls_config() -> io::Result<Arc<rustls::ClientConfig>> {
    static CONFIG: OnceLock<Option<Arc<rustls::ClientConfig>>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots =
                rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let cfg = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .ok()?
                .with_root_certificates(roots)
                .with_no_client_auth();
            Some(Arc::new(cfg))
        })
        .clone()
        .ok_or_else(|| io::Error::other("rustls ClientConfig 초기화 실패"))
}

fn parse_origin(origin: &str) -> io::Result<(bool, String, String, u16)> {
    let (tls, rest, default_port) = if let Some(r) = origin.strip_prefix("https://") {
        (true, r, 443_u16)
    } else if let Some(r) = origin.strip_prefix("http://") {
        (false, r, 80_u16)
    } else {
        return Err(invalid());
    };
    let host_port = rest.split('/').next().ok_or_else(invalid)?;
    if host_port.is_empty() {
        return Err(invalid());
    }
    let (host, port) = if let Some((h, p)) = host_port.rsplit_once(':') {
        let port: u16 = p.parse().map_err(|_| invalid())?;
        (h.to_owned(), port)
    } else {
        (host_port.to_owned(), default_port)
    };
    Ok((tls, host_port.to_owned(), host, port))
}

impl RelayClient {
    pub fn start(
        publication: Publication,
        shared: &Arc<Shared>,
        registry: &Weak<Registry>,
        hello: &ServerHello,
    ) -> io::Result<Self> {
        let (wake_tx, wake_rx) = UnixStream::pair()?;
        wake_tx.set_nonblocking(true)?;
        wake_rx.set_nonblocking(true)?;
        let wake_clone = wake_tx.try_clone()?;
        let connected = Arc::new(AtomicBool::new(false));
        let settled = Arc::new(AtomicBool::new(false));
        let assets = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(Stop::default());
        let (c_flag, set_flag, a_flag, s_flag) = (
            Arc::clone(&connected),
            Arc::clone(&settled),
            Arc::clone(&assets),
            Arc::clone(&stop),
        );
        let (sh, reg, h) = (Arc::clone(shared), registry.clone(), hello.clone());
        thread::Builder::new()
            .name("mobile-relay".into())
            .stack_size(STACK)
            .spawn(move || {
                crate::alloc::mobile_thread().ok();
                supervise_loop(
                    &publication,
                    wake_tx,
                    wake_rx,
                    &c_flag,
                    &set_flag,
                    &a_flag,
                    &s_flag,
                    &sh,
                    &reg,
                    &h,
                );
            })?;
        Ok(Self {
            connected,
            settled,
            assets,
            stop,
            wake_tx: Some(wake_clone),
        })
    }
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }
    pub fn settled(&self) -> bool {
        self.settled.load(Ordering::Acquire)
    }
    /// 릴레이로 새 기기를 짝지을 수 있는지: 연결됐고 고정 출처에 현재 에셋 해시가 배포돼 있다(QR이 404가 아니다).
    pub fn pairing(&self) -> bool {
        self.connected() && self.assets.load(Ordering::Acquire)
    }
}

#[expect(clippy::too_many_arguments, reason = "내부 감시 루프 컨텍스트 전달")]
fn supervise_loop(
    publication: &Publication,
    wake_tx: UnixStream,
    mut wake_rx: UnixStream,
    connected: &Arc<AtomicBool>,
    settled: &Arc<AtomicBool>,
    assets: &Arc<AtomicBool>,
    stop: &Arc<Stop>,
    shared: &Arc<Shared>,
    registry: &Weak<Registry>,
    hello: &ServerHello,
) {
    // 고정 출처에 이 앱 버전이 없으면 새 QR만 404가 된다(게이트웨이가 터널 QR로 낸다). 릴레이는 그래도 붙는다:
    // 이미 짝지은 PWA(이전 해시)는 릴레이로 계속 접속해야 하고, 해시는 UI 식별용이지 프로토콜이 아니다.
    // 첫 확인은 연결 전에 끝내 배포된 버전에서 터널을 괜히 띄우지 않게 한다.
    if super::tunnel::publication_available(publication) {
        assets.store(true, Ordering::Release);
    } else {
        watch_assets(
            publication.clone(),
            Arc::clone(assets),
            Arc::clone(stop),
            Arc::clone(shared),
        );
    }
    let wake_writer = Arc::new(Mutex::new(wake_tx));
    let mut delay = Duration::from_millis(200);
    while !stop.cancelled() {
        let res = run_once(
            publication,
            &wake_writer,
            &mut wake_rx,
            connected,
            settled,
            stop,
            shared,
            registry,
            hello,
        );
        let first_settle = !settled.swap(true, Ordering::AcqRel);
        let was_connected = connected.swap(false, Ordering::AcqRel);
        if was_connected || first_settle {
            shared.wake();
        }
        if was_connected {
            delay = Duration::from_millis(200);
        }
        if stop.cancelled() {
            return;
        }
        tracing::debug!(?res, "mobile DO relay disconnected; backing off");
        if stop.wait(delay) {
            return;
        }
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

/// 늦게 배포된 에셋을 알아챈다. 배포를 확인하면 게이트웨이를 깨워 QR을 고정 출처로 바꾸고 끝난다.
fn watch_assets(
    publication: Publication,
    assets: Arc<AtomicBool>,
    stop: Arc<Stop>,
    shared: Arc<Shared>,
) {
    thread::Builder::new()
        .name("mobile-assets".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            while !stop.wait(Duration::from_secs(60)) {
                if super::tunnel::publication_available(&publication) {
                    assets.store(true, Ordering::Release);
                    shared.wake();
                    return;
                }
            }
        })
        .ok();
}

#[expect(clippy::too_many_arguments, reason = "단일 세션 루프 인자 전달")]
fn run_once(
    publication: &Publication,
    wake_writer: &Arc<Mutex<UnixStream>>,
    wake_rx: &mut UnixStream,
    connected: &Arc<AtomicBool>,
    settled: &Arc<AtomicBool>,
    stop: &Arc<Stop>,
    shared: &Arc<Shared>,
    registry: &Weak<Registry>,
    hello: &ServerHello,
) -> io::Result<()> {
    let (tls, host_header, host, port) = parse_origin(&publication.origin)?;
    let addr = (host.as_str(), port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "relay DNS empty"))?;
    let tcp = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(TIMEOUT))?;
    tcp.set_write_timeout(Some(TIMEOUT))?;
    let mut transport = if tls {
        let cfg = tls_config()?;
        let server_name = rustls_pki_types::ServerName::try_from(host).map_err(|_| invalid())?;
        let conn = rustls::ClientConnection::new(cfg, server_name)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Transport::Tls(Box::new(rustls::StreamOwned::new(conn, tcp)))
    } else {
        Transport::Plain(tcp)
    };
    let tunnel_query = lock(&shared.state)
        .tunnel
        .as_ref()
        .and_then(|t| t.ready_url())
        .filter(|u| super::tunnel::parse_url(u).is_some())
        .map(|u| format!("&url={u}"))
        .unwrap_or_default();
    let path = format!(
        "/r/{}?hash={}{tunnel_query}",
        publication.rid,
        super::pwa::asset_hash()
    );
    ws::connect_client(&mut transport, &host_header, &path, &publication.key)?;
    connected.store(true, Ordering::Release);
    settled.store(true, Ordering::Release);
    shared.wake();

    let out_rx = Arc::new(Mailbox::new(OUTBOX_BYTES, PEER_QUEUE_BYTES, 128));
    let outbox = Outbox {
        tx: Arc::clone(&out_rx),
        wake: Arc::clone(wake_writer),
    };
    let mut sessions: HashMap<u32, MuxSession> = HashMap::new();
    let mut rx_buf = Vec::with_capacity(8192);
    let max_frame = MAX_PLAINTEXT.saturating_add(64);
    let (mut transport_alive, mut read_pending) = transport.read_available(&mut rx_buf)?;
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now();

    let res = (|| -> io::Result<()> {
        while !stop.cancelled() {
            drain_outbox(&mut transport, &out_rx)?;
            let mut decoded = 0usize;
            while decoded < 64 {
                let Some((opcode, body)) = ws::try_decode_unmasked(&mut rx_buf, max_frame)? else {
                    break;
                };
                decoded += 1;
                last_rx = Instant::now();
                match opcode {
                    ws::BINARY => {
                        handle_mux_frame(&body, &mut sessions, &outbox, shared, registry, hello);
                    }
                    9 => {
                        transport.write_masked(10, &body)?;
                    }
                    10 => {}
                    8 => return Ok(()),
                    _ => return Err(invalid()),
                }
            }
            if last_rx.elapsed() >= PING.saturating_mul(3) {
                return Err(io::ErrorKind::TimedOut.into());
            }
            if last_ping.elapsed() >= PING {
                transport.write_masked(9, &[])?;
                last_ping = Instant::now();
            }
            if !transport_alive {
                if decoded == 64 {
                    continue;
                }
                return if rx_buf.is_empty() {
                    Ok(())
                } else {
                    Err(invalid())
                };
            }
            if read_pending || decoded == 64 || !out_rx.is_empty() {
                (transport_alive, read_pending) = transport.read_available(&mut rx_buf)?;
                continue;
            }
            let wait_dur = PING.saturating_sub(last_ping.elapsed());
            wait_io(transport.tcp(), wake_rx, wait_dur);
            if stop.cancelled() {
                return Ok(());
            }
            drain_outbox(&mut transport, &out_rx)?;
            (transport_alive, read_pending) = transport.read_available(&mut rx_buf)?;
        }
        Ok(())
    })();
    for (_, sess) in sessions.drain() {
        sess.tx.close();
        sess.stream.shutdown(Shutdown::Both).ok();
    }
    out_rx.close();
    res
}

fn drain_outbox(transport: &mut Transport, queue: &Mailbox<Vec<u8>>) -> io::Result<()> {
    let mut sent = 0usize;
    for _ in 0..16 {
        let Ok(payload) = queue.try_recv() else { break };
        sent = sent.saturating_add(payload.len());
        transport.write_masked(ws::BINARY, &payload)?;
        if sent >= 128 * 1024 {
            break;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn wait_io(tcp: &TcpStream, wake_rx: &mut UnixStream, timeout: Duration) {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let mut fds = [
        PollFd::new(tcp, PollFlags::IN),
        PollFd::new(&*wake_rx, PollFlags::IN),
    ];
    let ts = Timespec::try_from(timeout).ok();
    poll(&mut fds, ts.as_ref()).ok();
    let mut drain = [0_u8; 64];
    while wake_rx.read(&mut drain).is_ok_and(|n| n > 0) {}
}

#[cfg(not(unix))]
fn wait_io(_tcp: &TcpStream, wake_rx: &mut UnixStream, _timeout: Duration) {
    let mut drain = [0_u8; 64];
    while wake_rx.read(&mut drain).is_ok_and(|n| n > 0) {}
    thread::sleep(Duration::from_millis(10));
}

fn handle_mux_frame(
    body: &[u8],
    sessions: &mut HashMap<u32, MuxSession>,
    outbox: &Outbox,
    shared: &Arc<Shared>,
    registry: &Weak<Registry>,
    hello: &ServerHello,
) {
    let Ok((kind, conn_id, payload)) = decode_mux(body) else {
        return;
    };
    sessions.retain(|_, s| {
        if s.tx.is_closed() {
            s.stream.shutdown(Shutdown::Both).ok();
            false
        } else {
            true
        }
    });
    match kind {
        // 챌린지는 데몬이 만든다(serve_mux_client). DO가 정하면 같은 챌린지로 녹화한 세션을 재생할 수 있다.
        // DO는 빈 OPEN만 보낸다. 내용이 있거나(서버 지정 챌린지) 이미 열린 conn_id면 거부한다.
        MUX_OPEN => {
            // DO는 빈 OPEN만 보낸다. 챌린지는 E2EE 종단인 데몬이 생성한다.
            if !payload.is_empty() || sessions.contains_key(&conn_id) {
                outbox.send_close(conn_id, 1002);
                return;
            }
            let Ok(challenge) = random() else {
                outbox.send_close(conn_id, 1011);
                return;
            };
            let tx = Arc::new(Mailbox::new(PEER_QUEUE_BYTES, PEER_QUEUE_BYTES, 32));
            let rx = Arc::clone(&tx);
            let Ok((local, server)) = UnixStream::pair() else {
                outbox.send_close(conn_id, 1011);
                return;
            };
            let Ok(kept) = local.try_clone() else {
                outbox.send_close(conn_id, 1011);
                return;
            };
            let Ok(sess_stream) = local.try_clone() else {
                outbox.send_close(conn_id, 1011);
                return;
            };
            let id = {
                let mut state = lock(&shared.state);
                let pending = state
                    .connections
                    .values()
                    .filter(|c| c.device.is_none())
                    .count();
                if pending >= MAX_PENDING || state.connections.len() >= MAX_CONNECTIONS {
                    outbox.send_close(conn_id, 1013);
                    return;
                }
                let Some(id) = state.next.checked_add(1) else {
                    outbox.send_close(conn_id, 1011);
                    return;
                };
                state.next = id;
                state.connections.insert(
                    id,
                    Connection {
                        accepted: Instant::now(),
                        device: None,
                        socket: ShutdownHandle::Unix(kept),
                    },
                );
                id
            };
            let close_tx = tx.clone();
            sessions.insert(
                conn_id,
                MuxSession {
                    tx,
                    stream: sess_stream,
                },
            );
            let (sh, reg, h, out) = (
                Arc::clone(shared),
                registry.clone(),
                hello.clone(),
                outbox.clone(),
            );
            if thread::Builder::new()
                .name("mobile-relay-ws".into())
                .stack_size(STACK)
                .spawn(move || {
                    crate::alloc::mobile_thread().ok();
                    let code = match serve_mux_client(
                        conn_id, challenge, &rx, &close_tx, local, server, &out, &sh, id, &reg, &h,
                    ) {
                        Err(err) => super::gateway::auth_close_code(&err).unwrap_or(1011),
                        _ => 1000,
                    };
                    out.send_close(conn_id, code);
                    let mut s = lock(&sh.state);
                    if let Some(c) = s.connections.remove(&id) {
                        c.socket.shutdown();
                    }
                    sh.wake();
                })
                .is_err()
            {
                sessions.remove(&conn_id);
                lock(&shared.state).connections.remove(&id);
                outbox.send_close(conn_id, 1011);
            }
        }
        MUX_TEXT => {
            if let Some(s) = sessions.get(&conn_id)
                && s.tx
                    .send(0, (ws::TEXT, payload.to_vec()), payload.len())
                    .is_err()
            {
                s.tx.close();
                s.stream.shutdown(Shutdown::Both).ok();
                outbox.send_close(conn_id, 1013);
            }
        }
        MUX_BINARY => {
            if let Some(s) = sessions.get(&conn_id)
                && s.tx
                    .send(0, (ws::BINARY, payload.to_vec()), payload.len())
                    .is_err()
            {
                s.tx.close();
                s.stream.shutdown(Shutdown::Both).ok();
                outbox.send_close(conn_id, 1013);
            }
        }
        MUX_CLOSE => {
            if let Some(s) = sessions.remove(&conn_id) {
                s.tx.close();
                s.stream.shutdown(Shutdown::Both).ok();
            }
        }
        _ => {}
    }
}

fn recv_client_frame(rx: &Mailbox<(u8, Vec<u8>)>, timeout: Duration) -> io::Result<(u8, Vec<u8>)> {
    let deadline = Instant::now() + timeout;
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match rx.recv_timeout(remain) {
            Ok((0, _)) => {}
            Ok(frame) => return Ok(frame),
            Err(mpsc::RecvTimeoutError::Timeout) => return Err(io::ErrorKind::TimedOut.into()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
    }
}

fn recv_open_frame(rx: &Mailbox<(u8, Vec<u8>)>) -> io::Result<(u8, Vec<u8>)> {
    loop {
        match rx.recv() {
            Ok((0, _)) => {}
            Ok(frame) => return Ok(frame),
            Err(mpsc::RecvError) => return Err(io::ErrorKind::UnexpectedEof.into()),
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "멀티플렉싱 클라이언트 브릿지 컨텍스트 전달"
)]
fn serve_mux_client(
    conn_id: u32,
    challenge: [u8; 32],
    rx: &Mailbox<(u8, Vec<u8>)>,
    close_tx: &Arc<Mailbox<(u8, Vec<u8>)>>,
    mut local: UnixStream,
    server: UnixStream,
    outbox: &Outbox,
    shared: &Arc<Shared>,
    id: u64,
    registry: &Weak<Registry>,
    hello: &ServerHello,
) -> io::Result<()> {
    lock(&shared.state).devices.attempt(now_ms())?;
    let greeting = serde_json::json!({"version": 2, "challenge": hex(&challenge)});
    outbox.send_mux(MUX_TEXT, conn_id, greeting.to_string().as_bytes())?;
    let (opcode, body) = recv_client_frame(rx, TIMEOUT)?;
    if opcode != ws::TEXT || body.len() > 4096 {
        return Err(invalid());
    }
    let auth: AuthHello = serde_json::from_slice(&body).map_err(|_| invalid())?;
    let client_nonce = decode_hex::<32>(&auth.client_nonce)?;
    let token = auth.token.as_deref().map(decode_hex::<32>).transpose()?;
    let secret = {
        let s = lock(&shared.state);
        match (&token, &auth.device_id) {
            (Some(t), None) => s.devices.pairing_secret(t, now_ms())?,
            (None, Some(d)) => s.devices.device_secret(d)?,
            _ => return Err(invalid()),
        }
    };
    let mut salt = [0_u8; 64];
    for (a, b) in salt
        .iter_mut()
        .zip(challenge.into_iter().chain(client_nonce))
    {
        *a = b;
    }
    let mut channel = Channel::new(&secret, &salt, Endpoint::Daemon)?;
    let (opcode, body) = recv_client_frame(rx, TIMEOUT)?;
    if opcode != ws::BINARY || body.len() > 512 {
        return Err(invalid());
    }
    let proof: AuthProof = serde_json::from_slice(&channel.open(&body)?).map_err(|_| invalid())?;
    let authenticated = {
        let mut s = lock(&shared.state);
        if s.stopped || !s.connections.contains_key(&id) {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        let (device_id, credential) = if let Some(token) = token {
            let (device, credential) = s.devices.pair(&token, &proof.label, now_ms())?;
            (device.device_id, Some(hex(&credential)))
        } else {
            let device_id = auth.device_id.ok_or_else(invalid)?;
            s.devices.seen(&device_id, now_ms())?;
            (device_id, None)
        };
        let conn = s.connections.get_mut(&id).ok_or_else(invalid)?;
        conn.device = Some(device_id.clone());
        Authenticated {
            device_id,
            credential,
        }
    };
    let payload = channel.seal(&serde_json::to_vec(&authenticated).map_err(io::Error::other)?)?;
    outbox.send_mux(MUX_BINARY, conn_id, &payload)?;
    shared.wake();

    let registry = registry.upgrade().ok_or_else(invalid)?;
    let server_stop = server.try_clone()?;
    let mut outgoing = local.try_clone()?;
    let cipher = Arc::new(Mutex::new(channel));
    let (c_out, out_clone, close_clone) = (Arc::clone(&cipher), outbox.clone(), close_tx.clone());
    let reader = thread::Builder::new()
        .name("mobile-relay-out".into())
        .stack_size(STACK)
        .spawn(move || {
            crate::alloc::mobile_thread().ok();
            let result = (|| -> io::Result<()> {
                while let Some((kind, body)) = ipc::read_frame(&mut outgoing)? {
                    let frame = ipc::encode_frame(kind, &body)?;
                    let sealed = lock(&c_out).seal(&frame)?;
                    out_clone.send_mux(MUX_BINARY, conn_id, &sealed)?;
                }
                Ok(())
            })();
            close_clone.close();
            result
        })?;
    let hello_owned = hello.clone();
    let worker = thread::Builder::new()
        .name("mobile-relay-ipc".into())
        .stack_size(RPC_STACK)
        .spawn(move || {
            crate::alloc::mobile_thread().ok();
            let done = server.try_clone();
            let result = registry.serve_mobile(server, &hello_owned);
            if let Ok(done) = done {
                done.shutdown(Shutdown::Both).ok();
            }
            result
        });
    let result = if worker.is_ok() {
        (|| -> io::Result<()> {
            loop {
                let (opcode, body) = recv_open_frame(rx)?;
                if opcode == 8 {
                    return Ok(());
                }
                if opcode != ws::BINARY {
                    return Err(invalid());
                }
                let frame = lock(&cipher).open(&body)?;
                let mut bytes = frame.as_slice();
                ipc::read_frame(&mut bytes)?.ok_or_else(invalid)?;
                if !bytes.is_empty() {
                    return Err(invalid());
                }
                local.write_all(&frame)?;
            }
        })()
    } else {
        Err(io::Error::other("mobile relay IPC worker unavailable"))
    };
    local.shutdown(Shutdown::Both).ok();
    server_stop.shutdown(Shutdown::Both).ok();
    reader.join().ok();
    if let Ok(worker) = worker {
        worker.join().ok();
    }
    match result {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tls_large_frames_drain_buffer_and_preserve_next_message() {
        // Public, test-only localhost certificate/key, with no production use.
        let cert = rustls_pki_types::CertificateDer::from(
            include_bytes!("../../tests/fixtures/mobile-relay-localhost.der").to_vec(),
        );
        let key = rustls_pki_types::PrivatePkcs8KeyDer::from(
            include_bytes!("../../tests/fixtures/mobile-relay-localhost-key.der").to_vec(),
        );
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key.into())
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let sizes = [128 * 1024, MAX_PLAINTEXT];
        let server = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let conn = rustls::ServerConnection::new(Arc::new(server_config)).unwrap();
            let mut stream = rustls::StreamOwned::new(conn, socket);
            for size in sizes {
                let (kind, payload) = ws::read(&mut stream).unwrap();
                assert_eq!(kind, ws::BINARY);
                assert_eq!(payload.len(), size);
                assert!(payload.iter().all(|&b| b == 0x5a));
            }
            let (kind, payload) = ws::read(&mut stream).unwrap();
            assert_eq!(
                (kind, payload.as_slice()),
                (ws::TEXT, b"still connected".as_slice())
            );
        });
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
        let conn = rustls::ClientConnection::new(Arc::new(config), name).unwrap();
        let socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut tls = rustls::StreamOwned::new(conn, socket);
        while tls.conn.is_handshaking() {
            tls.conn.complete_io(&mut tls.sock).unwrap();
        }
        let mut transport = Transport::Tls(Box::new(tls));
        for size in sizes {
            transport
                .write_masked(ws::BINARY, &vec![0x5a; size])
                .unwrap();
        }
        transport
            .write_masked(ws::TEXT, b"still connected")
            .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn mux_frame_roundtrip_and_tls_config_init() {
        let frame = encode_mux(MUX_OPEN, 0x1234_5678, &[9_u8; 32]);
        let (kind, conn_id, payload) = decode_mux(&frame).unwrap();
        assert_eq!(kind, MUX_OPEN);
        assert_eq!(conn_id, 0x1234_5678);
        assert_eq!(payload, &[9_u8; 32]);
        assert!(decode_mux(&[1, 2, 3, 4]).is_err());
        assert!(tls_config().is_ok());
        assert_eq!(
            parse_origin("https://krill-mobile.pages.dev").unwrap(),
            (
                true,
                "krill-mobile.pages.dev".into(),
                "krill-mobile.pages.dev".into(),
                443
            )
        );
        assert_eq!(
            parse_origin("http://127.0.0.1:8123").unwrap(),
            (false, "127.0.0.1:8123".into(), "127.0.0.1".into(), 8123)
        );
    }
}
