//! Lazy Exposure HTTP/WS 게이트웨이. 세션 상태는 보관하지 않고 기존 Registry로 1:1 전달한다.
use super::devices::{Devices, decode_hex, hex};
use super::e2ee::{Channel, Endpoint, invalid, random};
use super::{net, ws};
use crate::registry::Registry;
use crate::session::lock;
use krill_proto::ipc::{self, ServerHello};
use krill_proto::mobile::{ConnectMode, MobileReply, MobileRequest, NetworkInterface};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddrV4, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 미인증 연결 상한. 인증된 연결은 기기당 WS 2개(메인+터미널)라 별도 상한을 둔다.
const MAX_PENDING: usize = 4;
const MAX_CONNECTIONS: usize = 12;
const STACK: usize = 128 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);
/// Cloudflare는 양방향 무전송 WS를 끊는다(Quick Tunnel 실측 약 125초). 이 주기로 ping하고,
/// 응답 없는 주기가 `IDLE_PINGS`번 이어지면(LTE 전환 등으로 죽은 연결) 닫는다.
const PING: Duration = Duration::from_secs(30);
const IDLE_PINGS: u8 = 3;
/// 페어링 요청이 터널 준비를 기다리는 최대 시간. 모바일 IPC 클라이언트 읽기 타임아웃(20초)보다 짧다.
const MINT_WAIT: Duration = Duration::from_secs(15);
/// 사용자가 고른 연결 모드. 데몬을 재시작해도 Tunnel 기기가 다시 붙을 수 있게 남긴다.
const INTENT_FILE: &str = "mobile-gateway.json";
const MOBILE_ORIGIN: &str = "https://krill-mobile.pages.dev";
fn mobile_origin() -> String {
    std::env::var("KRILL_MOBILE_ORIGIN")
        .unwrap_or_else(|_| MOBILE_ORIGIN.into())
        .trim_end_matches('/')
        .to_owned()
}

#[derive(Debug)]
pub struct Gateway {
    shared: Arc<Shared>,
    backend: super::pwa_rpc::Backend,
}
#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}
#[derive(Debug)]
struct State {
    devices: Devices,
    mode: ConnectMode,
    custom: Option<String>,
    interfaces: Vec<NetworkInterface>,
    selected: Option<SocketAddrV4>,
    listeners: Vec<TcpListener>,
    connections: HashMap<u64, Connection>,
    next: u64,
    stopped: bool,
    tunnel: Option<super::tunnel::Tunnel>,
    tunnel_start: Option<fn(u16) -> io::Result<super::tunnel::Tunnel>>,
    resolver: super::cloudflared::Resolver,
    publication: super::tunnel::Publication,
    tunnel_failures: u32,
    tunnel_retry_at: Option<Instant>,
    tunnel_error: Option<io::Error>,
    dir: Option<PathBuf>,
    ping: Duration,
}
#[derive(Deserialize, Serialize)]
struct Intent {
    mode: ConnectMode,
    custom: Option<String>,
    #[serde(default)]
    rid: String,
    #[serde(default)]
    key: String,
}
impl Intent {
    fn load(dir: Option<&Path>) -> io::Result<Self> {
        let mut intent = dir
            .and_then(|d| {
                super::devices::read_private(&d.join(INTENT_FILE))
                    .ok()
                    .flatten()
            })
            .and_then(|b| serde_json::from_slice::<Self>(&b).ok())
            .unwrap_or_else(|| Self {
                mode: ConnectMode::Lan,
                custom: None,
                rid: String::new(),
                key: String::new(),
            });
        if decode_hex::<16>(&intent.rid).is_err() || decode_hex::<32>(&intent.key).is_err() {
            intent.rid = hex(&random()?.into_iter().take(16).collect::<Vec<_>>());
            intent.key = hex(&random()?);
        }
        if let Some(dir) = dir {
            intent.save(dir)?;
        }
        Ok(intent)
    }
    fn save(&self, dir: &Path) -> io::Result<()> {
        super::devices::write_private(
            &dir.join(INTENT_FILE),
            &serde_json::to_vec(self).map_err(io::Error::other)?,
        )
    }
}
#[derive(Debug)]
struct Connection {
    accepted: Instant,
    device: Option<String>,
    socket: TcpStream,
}

/// 공개 JSON 핸드셰이크. 비밀 대신 server challenge와 client nonce로 키를 유도한다.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthHello {
    pub token: Option<String>,
    pub device_id: Option<String>,
    /// 32B hex
    pub client_nonce: String,
}
impl std::fmt::Debug for AuthHello {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthHello").finish_non_exhaustive()
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthProof {
    pub label: String,
}
#[derive(Deserialize, Serialize)]
pub struct Authenticated {
    pub device_id: String,
    pub credential: Option<String>,
}
impl std::fmt::Debug for Authenticated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authenticated")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

impl Gateway {
    pub fn start(
        registry: Weak<Registry>,
        dir: Option<&Path>,
        hello: ServerHello,
    ) -> io::Result<Arc<Self>> {
        let intent = Intent::load(dir)?;
        let publication = super::tunnel::Publication {
            origin: mobile_origin(),
            rid: intent.rid,
            key: intent.key,
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                devices: Devices::open(dir)?,
                mode: intent.mode,
                custom: intent.custom,
                interfaces: net::detect()?,
                selected: None,
                listeners: Vec::new(),
                connections: HashMap::new(),
                next: 0,
                stopped: false,
                tunnel: None,
                tunnel_start: None,
                resolver: super::cloudflared::Resolver::default(),
                publication,
                tunnel_failures: 0,
                tunnel_retry_at: None,
                tunnel_error: None,
                dir: dir.map(Path::to_path_buf),
                ping: PING,
            }),
            changed: Condvar::new(),
        });
        let backend = super::pwa_rpc::Backend::new(registry.clone(), dir, hello.clone());
        let worker = Arc::clone(&shared);
        thread::Builder::new()
            .name("mobile-listen".into())
            .stack_size(STACK)
            .spawn(move || {
                crate::alloc::mobile_thread().ok();
                accept(&worker, &registry, &hello);
            })?;
        Ok(Arc::new(Self { shared, backend }))
    }
    /// 알림 링크에는 페어링 티켓·자격증명을 넣지 않는다.
    pub fn deep_link(&self, session: krill_proto::SessionId) -> Option<String> {
        let state = lock(&self.shared.state);
        if state.listeners.is_empty() || state.stopped {
            return None;
        }
        let address = state.selected?;
        let base = if state.mode == ConnectMode::Tunnel {
            let tunnel = state.tunnel.as_ref()?;
            let url = tunnel.ready_url()?;
            if tunnel.fixed() {
                fixed_base(&state)
            } else {
                url.to_owned()
            }
        } else {
            format!("http://{address}")
        };
        Some(format!("{base}/?session={}", session.0))
    }
    pub fn request(&self, request: MobileRequest) -> MobileReply {
        if let MobileRequest::Workflow(json) = request {
            return MobileReply::Workflow(self.backend.request(&json));
        }
        let reply = match self.handle(request) {
            Ok(reply) => reply,
            Err(e) => MobileReply::Failed(e.to_string()),
        };
        self.shared.changed.notify_all();
        reply
    }
    fn handle(&self, request: MobileRequest) -> io::Result<MobileReply> {
        let mut state = lock(&self.shared.state);
        let now = now_ms();
        let reply = match request {
            MobileRequest::Workflow(_) => return Err(invalid()),
            MobileRequest::MintPairingTicket {
                mode,
                custom_address,
                rotate,
            } => {
                configure(&mut state, mode, custom_address, now)?;
                state.devices.mint(now, rotate)?;
                state.tunnel_retry_at = None;
                reconcile(&mut state, now)?;
                if mode == ConnectMode::Tunnel {
                    // 설치·터널 준비·고정 출처 판정을 모두 락 밖에서 기다린다.
                    drop(state);
                    self.shared.changed.notify_all();
                    let deadline = Instant::now() + MINT_WAIT;
                    loop {
                        let state = lock(&self.shared.state);
                        if state.stopped || state.mode != mode {
                            return Err(io::Error::other("모바일 연결 모드가 변경되었습니다"));
                        }
                        if state
                            .tunnel
                            .as_ref()
                            .is_some_and(|t| t.ready_url().is_some())
                        {
                            return pairing_offer(&state, now_ms());
                        }
                        if Instant::now() >= deadline
                            || (state.tunnel.is_none()
                                && !state.resolver.downloading()
                                && state
                                    .tunnel_error
                                    .as_ref()
                                    .is_some_and(|e| e.kind() != io::ErrorKind::WouldBlock))
                        {
                            return Err(tunnel_unavailable(&state));
                        }
                        drop(state);
                        thread::sleep(Duration::from_millis(200));
                    }
                } else {
                    pairing_offer(&state, now)?
                }
            }
            MobileRequest::SetConnectMode {
                mode,
                custom_address,
            } => {
                configure(&mut state, mode, custom_address, now)?;
                reconcile(&mut state, now)?;
                status(&state, now)
            }
            MobileRequest::CurrentPairingOffer => pairing_offer(&state, now)?,
            MobileRequest::Status => {
                let (mode, custom) = (state.mode, state.custom.clone());
                configure(&mut state, mode, custom, now)?;
                reconcile(&mut state, now)?;
                status(&state, now)
            }
            MobileRequest::ConnectedDevices => MobileReply::ConnectedDevices(
                state
                    .connections
                    .values()
                    .filter_map(|c| c.device.clone())
                    .collect(),
            ),
            MobileRequest::ListDevices => MobileReply::Devices(state.devices.list()),
            MobileRequest::RevokeDevice { device_id, all } => {
                let count = state.devices.revoke(device_id.as_deref(), all, now)?;
                for c in state.connections.values() {
                    if all || c.device.as_deref() == device_id.as_deref() {
                        c.socket.shutdown(Shutdown::Both).ok();
                    }
                }
                reconcile(&mut state, now)?;
                MobileReply::Revoked { count }
            }
        };
        self.shared.changed.notify_all();
        Ok(reply)
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        let mut state = lock(&self.shared.state);
        state.stopped = true;
        close(&mut state);
        self.shared.changed.notify_all();
    }
}
fn configure(s: &mut State, mode: ConnectMode, custom: Option<String>, now: u64) -> io::Result<()> {
    let reissue = s.devices.ticket(now).is_some();
    let intent_changed = s.mode != mode || s.custom != custom;
    if intent_changed {
        s.devices.clear_ticket();
        close(s);
        s.selected = None;
    }
    let interfaces = net::detect()?;
    let selected = if mode != ConnectMode::Tunnel && custom.is_some() {
        net::select(&[], mode, custom.as_deref())?
    } else {
        net::select(&interfaces, mode, custom.as_deref())?
    };
    if intent_changed || s.selected.is_some_and(|a| a.ip() != selected.ip()) {
        s.devices.clear_ticket();
        close(s);
        s.selected = None;
        if reissue {
            s.devices.mint(now, true)?;
        }
    }
    if intent_changed && let Some(dir) = &s.dir {
        let intent = Intent {
            mode,
            custom: custom.clone(),
            rid: s.publication.rid.clone(),
            key: s.publication.key.clone(),
        };
        intent.save(dir)?;
    }
    s.mode = mode;
    s.custom = custom;
    s.interfaces = interfaces;
    if s.selected.is_none() {
        s.selected = Some(selected);
    }
    Ok(())
}
fn pairing_offer(state: &State, now: u64) -> io::Result<MobileReply> {
    let ticket = state.devices.ticket(now).ok_or_else(invalid)?;
    let address = state.selected.ok_or_else(invalid)?;
    let host = address.ip().to_string();
    let port = address.port();
    let base = if state.mode == ConnectMode::Tunnel {
        let tunnel = state
            .tunnel
            .as_ref()
            .ok_or_else(|| tunnel_unavailable(state))?;
        let url = tunnel
            .ready_url()
            .ok_or_else(|| tunnel_unavailable(state))?;
        if tunnel.fixed() {
            fixed_base(state)
        } else {
            url.to_owned()
        }
    } else {
        format!("http://{host}:{port}")
    };
    let mut url = format!(
        "{base}/#pair={}&key={}",
        hex(&ticket.token),
        ws::base64(&ticket.secret, true)
    );
    if let Some(tunnel) = state.tunnel.as_ref().filter(|t| t.fixed())
        && state.mode == ConnectMode::Tunnel
    {
        let host = tunnel
            .ready_url()
            .and_then(|url| url.strip_prefix("https://"))
            .ok_or_else(invalid)?;
        write!(url, "&t={host}&r={}", state.publication.rid).map_err(io::Error::other)?;
    }
    Ok(MobileReply::PairingOffer {
        url,
        host,
        port,
        expires_at_ms: ticket.expires_at_ms,
        mode: state.mode,
        interfaces: state.interfaces.clone(),
    })
}

fn fixed_base(state: &State) -> String {
    format!(
        "{}/m/{}",
        state.publication.origin,
        super::pwa::asset_hash()
    )
}
fn tunnel_unavailable(state: &State) -> io::Error {
    let message = if state.resolver.downloading() {
        "cloudflared 내려받는 중 — 잠시 후 다시 시도".to_owned()
    } else {
        state.tunnel_error.as_ref().map_or_else(
            || "모바일 터널 준비 중 — 잠시 후 다시 시도".into(),
            ToString::to_string,
        )
    };
    io::Error::new(io::ErrorKind::WouldBlock, message)
}

fn status(s: &State, now: u64) -> MobileReply {
    MobileReply::GatewayStatus {
        listening: !s.listeners.is_empty(),
        bind_addrs: s
            .listeners
            .iter()
            .filter_map(|l| l.local_addr().ok().map(|a| a.to_string()))
            .collect(),
        mode: s.mode,
        tunnel_url: s.tunnel.as_ref().and_then(|t| t.url()).map(str::to_owned),
        active_offer: s.devices.ticket(now).is_some(),
        devices: s.devices.list(),
    }
}
fn close(s: &mut State) {
    s.tunnel = None;
    s.tunnel_failures = 0;
    s.tunnel_retry_at = None;
    s.listeners.clear();
    for c in s.connections.values() {
        c.socket.shutdown(Shutdown::Both).ok();
    }
}
fn reconcile(s: &mut State, now: u64) -> io::Result<()> {
    if s.stopped || (s.devices.active_count() == 0 && s.devices.ticket(now).is_none()) {
        close(s);
        s.selected = None;
        return Ok(());
    }
    if !s.listeners.is_empty() {
        supervise_tunnel(s);
        return Ok(());
    }
    let selected = match s.selected {
        Some(a) => a,
        None => net::select(&s.interfaces, s.mode, s.custom.as_deref())?,
    };
    let loopback = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, selected.port()))?;
    let port = loopback.local_addr()?.port();
    loopback.set_nonblocking(true)?;
    let mut listeners = vec![loopback];
    if s.mode != ConnectMode::Tunnel && *selected.ip() != Ipv4Addr::LOCALHOST {
        let external = TcpListener::bind(SocketAddrV4::new(*selected.ip(), port))?;
        external.set_nonblocking(true)?;
        listeners.push(external);
    }
    s.selected = Some(SocketAddrV4::new(*selected.ip(), port));
    s.listeners = listeners;
    supervise_tunnel(s);
    Ok(())
}
/// 리스너가 열려 있는 Tunnel 모드에서는 cloudflared가 항상 떠 있게 한다.
/// 죽었거나 준비 기한을 넘기면 지수 백오프(2초~1분)로 다시 띄운다. 고정 출처 기기는 랑데부에서 새 URL을 찾는다.
fn supervise_tunnel(s: &mut State) {
    if s.mode != ConnectMode::Tunnel {
        s.tunnel = None;
        return;
    }
    let Some(port) = s.selected.map(|a| a.port()) else {
        return;
    };
    if let Some(tunnel) = s.tunnel.as_mut() {
        if tunnel.ready_url().is_some() {
            s.tunnel_failures = 0;
        }
        if tunnel.healthy() {
            return;
        }
        tracing::warn!("mobile tunnel exited or never became reachable; restarting");
        s.tunnel = None;
        backoff(s);
    }
    if s.tunnel_retry_at.is_some_and(|at| Instant::now() < at) {
        return;
    }
    let result = if let Some(start) = s.tunnel_start {
        start(port)
    } else {
        s.resolver.resolve(s.dir.as_deref()).and_then(|binary| {
            super::tunnel::Tunnel::start(binary.as_os_str(), port, s.publication.clone())
        })
    };
    match result {
        Ok(tunnel) => {
            s.tunnel = Some(tunnel);
            s.tunnel_retry_at = None;
            s.tunnel_error = None;
        }
        Err(error) => {
            tracing::warn!(kind=?error.kind(), "mobile tunnel start failed");
            let pending = error.kind() == io::ErrorKind::WouldBlock;
            s.tunnel_error = Some(error);
            if !pending {
                backoff(s);
            }
        }
    }
}
fn backoff(s: &mut State) {
    s.tunnel_failures = s.tunnel_failures.saturating_add(1);
    let delay = Duration::from_secs(1 << s.tunnel_failures.min(6)).min(Duration::from_secs(60));
    s.tunnel_retry_at = Some(Instant::now() + delay);
}
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn accept(shared: &Arc<Shared>, registry: &Weak<Registry>, hello: &ServerHello) {
    loop {
        let mut state = lock(&shared.state);
        if state.stopped {
            return;
        }
        reconcile(&mut state, now_ms()).ok();
        for connection in state.connections.values() {
            if connection.device.is_none() && connection.accepted.elapsed() >= TIMEOUT {
                connection.socket.shutdown(Shutdown::Both).ok();
            }
        }
        let pending = state
            .connections
            .values()
            .filter(|c| c.device.is_none())
            .count();
        let incoming = if pending < MAX_PENDING && state.connections.len() < MAX_CONNECTIONS {
            state
                .listeners
                .iter()
                .find_map(|l| l.accept().ok().map(|(s, _)| s))
        } else {
            None
        };
        if let Some(socket) = incoming {
            let Some(id) = state.next.checked_add(1) else {
                return;
            };
            state.next = id;
            let Ok(kept) = socket.try_clone() else {
                continue;
            };
            state.connections.insert(
                id,
                Connection {
                    accepted: Instant::now(),
                    device: None,
                    socket: kept,
                },
            );
            drop(state);
            let (s, r, h) = (Arc::clone(shared), registry.clone(), hello.clone());
            if thread::Builder::new()
                .name("mobile-ws".into())
                .stack_size(STACK)
                .spawn(move || {
                    crate::alloc::mobile_thread().ok();
                    let guard = Connected {
                        shared: Arc::clone(&s),
                        id,
                    };
                    if let Err(error) = serve(socket, &s, id, &r, &h) {
                        tracing::debug!(kind=?error.kind(), "mobile connection ended");
                    }
                    drop(guard);
                })
                .is_err()
            {
                lock(&shared.state).connections.remove(&id);
            }
        } else if state.listeners.is_empty()
            && state.devices.active_count() == 0
            && state.devices.ticket(now_ms()).is_none()
        {
            drop(
                shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        } else {
            drop(
                shared
                    .changed
                    .wait_timeout(state, Duration::from_millis(50))
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
    }
}
struct Connected {
    shared: Arc<Shared>,
    id: u64,
}
impl Drop for Connected {
    fn drop(&mut self) {
        let mut s = lock(&self.shared.state);
        if let Some(c) = s.connections.remove(&self.id) {
            c.socket.shutdown(Shutdown::Both).ok();
        }
        reconcile(&mut s, now_ms()).ok();
        self.shared.changed.notify_all();
    }
}
fn serve(
    mut socket: TcpStream,
    shared: &Arc<Shared>,
    id: u64,
    registry: &Weak<Registry>,
    hello: &ServerHello,
) -> io::Result<()> {
    // macOS에서 nonblocking listener의 accept 결과도 nonblocking이므로 명시적으로 되돌린다.
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(TIMEOUT))?;
    socket.set_write_timeout(Some(TIMEOUT))?;
    socket.set_nodelay(true)?;
    let origin = {
        let state = lock(&shared.state);
        (state.mode == ConnectMode::Tunnel).then(|| state.publication.origin.clone())
    };
    if !ws::upgrade(&mut socket, origin.as_deref())? {
        return Ok(());
    }
    // 주소와 무관한 실패 예산. 미인증 연결은 최대 4개·5초로 제한한다.
    lock(&shared.state).devices.attempt(now_ms())?;
    let challenge = random()?;
    ws::write(
        &mut socket,
        ws::TEXT,
        serde_json::to_vec(&serde_json::json!({"version":2,"challenge":hex(&challenge)}))
            .map_err(io::Error::other)?
            .as_slice(),
    )?;
    let (opcode, body) = ws::read_limited(&mut socket, 4096)?;
    if opcode != ws::TEXT {
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
    let mut salt = [0; 64];
    for (a, b) in salt
        .iter_mut()
        .zip(challenge.into_iter().chain(client_nonce))
    {
        *a = b;
    }
    let mut channel = Channel::new(&secret, &salt, Endpoint::Daemon)?;
    let (opcode, body) = ws::read_limited(&mut socket, 512)?;
    if opcode != ws::BINARY {
        return Err(invalid());
    }
    let proof: AuthProof = serde_json::from_slice(&channel.open(&body)?).map_err(|_| invalid())?;
    let authenticated = {
        let mut s = lock(&shared.state);
        if s.stopped || !s.connections.contains_key(&id) {
            return Err(invalid());
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
    ws::write(&mut socket, ws::BINARY, &payload)?;
    shared.changed.notify_all();
    let registry = registry.upgrade().ok_or_else(invalid)?;
    let ping = lock(&shared.state).ping;
    socket.set_read_timeout(Some(ping))?;
    bridge(socket, channel, registry, hello.clone())
}

/// 암호화 페이로드 하나는 IPC 프레임 하나다. 연결이 끊기면 이 pair만 닫는다.
fn bridge(
    mut socket: TcpStream,
    channel: Channel,
    registry: Arc<Registry>,
    hello: ServerHello,
) -> io::Result<()> {
    let (mut local, server) = UnixStream::pair()?;
    let server_stop = server.try_clone()?;
    let mut outgoing = local.try_clone()?;
    let output = Arc::new(Mutex::new(socket.try_clone()?));
    let cipher = Arc::new(Mutex::new(channel));
    let (c, w) = (Arc::clone(&cipher), Arc::clone(&output));
    let reader = thread::Builder::new()
        .name("mobile-out".into())
        .stack_size(STACK)
        .spawn(move || {
            crate::alloc::mobile_thread().ok();
            let result = (|| -> io::Result<()> {
                while let Some((kind, body)) = ipc::read_frame(&mut outgoing)? {
                    let frame = ipc::encode_frame(kind, &body)?;
                    let sealed = lock(&c).seal(&frame)?;
                    ws::write(&mut *lock(&w), ws::BINARY, &sealed)?;
                }
                Ok(())
            })();
            lock(&w).shutdown(Shutdown::Both).ok();
            result
        })?;
    let worker = thread::Builder::new()
        .name("mobile-ipc".into())
        .stack_size(STACK)
        .spawn(move || {
            crate::alloc::mobile_thread().ok();
            registry.serve_mobile(server, &hello)
        });
    let result = if worker.is_ok() {
        (|| -> io::Result<()> {
            let mut idle = 0u8;
            loop {
                // 프레임 경계에서만 유휴를 판정한다. 프레임 중간 타임아웃은 손상이므로 치명으로 둔다.
                match socket.peek(&mut [0]) {
                    Ok(0) => return Ok(()),
                    Ok(_) => idle = 0,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        idle = idle.saturating_add(1);
                        if idle >= IDLE_PINGS {
                            return Err(io::ErrorKind::TimedOut.into());
                        }
                        ws::write(&mut *lock(&output), 9, &[])?;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
                let (opcode, body) = ws::read(&mut socket)?;
                match opcode {
                    ws::BINARY => {
                        let frame = lock(&cipher).open(&body)?;
                        let mut bytes = frame.as_slice();
                        ipc::read_frame(&mut bytes)?.ok_or_else(invalid)?;
                        if !bytes.is_empty() {
                            return Err(invalid());
                        }
                        local.write_all(&frame)?;
                    }
                    9 => ws::write(&mut *lock(&output), 10, &body)?,
                    10 => {}
                    8 => return Ok(()),
                    _ => return Err(invalid()),
                }
            }
        })()
    } else {
        Err(io::Error::other("mobile IPC worker unavailable"))
    };
    local.shutdown(Shutdown::Both).ok();
    server_stop.shutdown(Shutdown::Both).ok();
    socket.shutdown(Shutdown::Both).ok();
    reader.join().ok();
    if let Ok(worker) = worker {
        worker.join().ok();
    }
    result
}

#[cfg(test)]
mod tests;
