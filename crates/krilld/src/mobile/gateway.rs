//! Lazy Exposure HTTP/WS 게이트웨이. 세션 상태는 보관하지 않고 기존 Registry로 1:1 전달한다.
use super::devices::{Devices, decode_hex, hex};
use super::e2ee::{Channel, Endpoint, invalid, random};
use super::{net, ws};
use crate::registry::Registry;
use crate::session::lock;
use krill_proto::ipc::{self, ServerHello};
use krill_proto::mobile::{ConnectMode, MobileReply, MobileRequest, NetworkInterface};
use krill_proto::os::net::UnixStream;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 미인증 연결 상한. 인증된 연결은 기기당 WS 2개(메인+터미널)라 별도 상한을 둔다.
pub(super) const MAX_PENDING: usize = 4;
pub(super) const MAX_CONNECTIONS: usize = 12;
const STACK: usize = 128 * 1024;
/// 스냅샷·계정 RPC를 수행하는 워커 스택. 수십 KB JSON을 중첩 빌더·직렬화로 조립하는데
/// 128KiB 스레드에서는 `mobile_pwa` E2E 첫 스냅샷에서 스택 오버플로로 데몬이 중단됐다.
/// 연결당 2개(`mobile-ipc` + `mobile-rpc`)에만 적용한다(수락·WS 루프·전달 스레드는 128KiB 유지).
pub(super) const RPC_STACK: usize = 512 * 1024;
pub(super) const TIMEOUT: Duration = Duration::from_secs(5);
/// 수락 스레드가 일이 없어도 깨어 reconcile(터널 감시·백오프)하는 최대 간격.
const RECONCILE: Duration = Duration::from_secs(1);
/// Cloudflare는 양방향 무전송 WS를 끊는다(Quick Tunnel 실측 약 125초). 이 주기로 ping하고,
/// 응답 없는 주기가 `IDLE_PINGS`번 이어지면(LTE 전환 등으로 죽은 연결) 닫는다.
const PING: Duration = Duration::from_secs(30);
const IDLE_PINGS: u8 = 3;
/// 페어링 요청이 터널 준비를 기다리는 최대 시간. 모바일 IPC 클라이언트 읽기 타임아웃(60초)보다 짧다.
/// 새 퀵 터널은 띄운 뒤 2.6~5.0초에 URL이 찍히고(2026-10-19 11회 실측) 그 뒤 `READY_SETTLE`(33초)
/// 이후에 준비되므로 약 36~42초(추정)가 걸린다.
const MINT_WAIT: Duration = Duration::from_secs(45);
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
pub(super) struct Shared {
    pub(super) state: Mutex<State>,
    changed: Condvar,
    waker: Arc<Waker>,
}
impl Shared {
    /// 상태 변경을 알린다. QR 대기(condvar)와 락 밖에서 poll 중인 수락 스레드를 함께 깨운다.
    pub(super) fn wake(&self) {
        self.changed.notify_all();
        self.waker.ring();
    }
}
struct RelayContext {
    shared: Weak<Shared>,
    registry: Weak<Registry>,
    hello: ServerHello,
}
impl std::fmt::Debug for RelayContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayContext").finish_non_exhaustive()
    }
}
/// 수락 스레드는 상태 락을 푼 채 리스너 복제 fd와 깨우기 소켓을 poll한다. 리스너를 닫는 쪽은
/// [`Waker::quiesce`]로 수락 스레드가 복제 fd를 놓을 때까지 기다려, 닫은 포트가 복제 fd로 계속
/// 열려 있지 않게 한다(Lazy Exposure). Windows는 condvar 50ms 대기를 유지한다.
#[derive(Debug)]
struct Waker {
    #[cfg(unix)]
    tx: std::os::unix::net::UnixStream,
    #[cfg(unix)]
    rx: std::os::unix::net::UnixStream,
    /// 수락 스레드가 리스너 복제 fd를 들고 poll 중이다.
    #[cfg(unix)]
    polling: Mutex<bool>,
    #[cfg(unix)]
    idle: Condvar,
}
impl Waker {
    #[cfg_attr(
        not(unix),
        expect(
            clippy::unnecessary_wraps,
            reason = "Windows는 깨우기 소켓 없이 condvar만 쓴다"
        )
    )]
    fn new() -> io::Result<Self> {
        #[cfg(unix)]
        {
            let (tx, rx) = std::os::unix::net::UnixStream::pair()?;
            tx.set_nonblocking(true)?;
            rx.set_nonblocking(true)?;
            Ok(Self {
                tx,
                rx,
                polling: Mutex::new(false),
                idle: Condvar::new(),
            })
        }
        #[cfg(not(unix))]
        Ok(Self {})
    }
    /// 버퍼가 차 있으면(WouldBlock) 이미 깨울 바이트가 남아 있으므로 버린다.
    #[cfg_attr(
        not(unix),
        expect(
            clippy::unused_self,
            reason = "Windows는 깨우기 소켓 없이 condvar만 쓴다"
        )
    )]
    fn ring(&self) {
        #[cfg(unix)]
        (&self.tx).write_all(&[1]).ok();
    }
    /// 상태 락을 쥔 채 리스너를 닫은 뒤 부른다. 수락 스레드는 poll을 끝낼 때 상태 락이 필요 없다.
    #[cfg_attr(
        not(unix),
        expect(
            clippy::unused_self,
            reason = "Windows는 깨우기 소켓 없이 condvar만 쓴다"
        )
    )]
    fn quiesce(&self) {
        #[cfg(unix)]
        {
            let mut polling = lock(&self.polling);
            if *polling {
                self.ring();
            }
            while *polling {
                polling = self
                    .idle
                    .wait(polling)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }
}
#[derive(Debug)]
pub(super) struct State {
    pub(super) devices: Devices,
    mode: ConnectMode,
    custom: Option<String>,
    interfaces: Vec<NetworkInterface>,
    selected: Option<SocketAddrV4>,
    listeners: Vec<TcpListener>,
    pub(super) connections: HashMap<u64, Connection>,
    pub(super) next: u64,
    pub(super) stopped: bool,
    pub(super) tunnel: Option<super::tunnel::Tunnel>,
    tunnel_start: Option<fn(u16) -> io::Result<super::tunnel::Tunnel>>,
    relay: Option<super::relay::RelayClient>,
    relay_enabled: bool,
    relay_ctx: Option<RelayContext>,
    saved_port: Option<u16>,
    saved_tunnel_pid: Option<u32>,
    saved_tunnel_url: Option<String>,
    tunnel_token: Option<String>,
    tunnel_hostname: Option<String>,
    resolver: super::cloudflared::Resolver,
    publication: super::tunnel::Publication,
    tunnel_failures: u32,
    tunnel_retry_at: Option<Instant>,
    tunnel_error: Option<io::Error>,
    dir: Option<PathBuf>,
    pub(super) ping: Duration,
    waker: Arc<Waker>,
}
#[derive(Deserialize, Serialize)]
struct Intent {
    mode: ConnectMode,
    custom: Option<String>,
    #[serde(default)]
    rid: String,
    #[serde(default)]
    key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tunnel_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tunnel_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tunnel_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tunnel_hostname: Option<String>,
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
                mode: ConnectMode::Tunnel,
                custom: None,
                rid: String::new(),
                key: String::new(),
                port: None,
                tunnel_pid: None,
                tunnel_url: None,
                tunnel_token: None,
                tunnel_hostname: None,
            });
        // Retire LAN exposure without deleting any paired-device credential or PTY.
        // Explicit loopback remains available to isolated QA fixtures only.
        let loopback = intent
            .custom
            .as_deref()
            .and_then(|value| value.parse::<std::net::SocketAddrV4>().ok())
            .is_some_and(|address| address.ip().is_loopback());
        if intent.mode == ConnectMode::Lan && !loopback {
            intent.mode = ConnectMode::Tunnel;
            intent.custom = None;
        }
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
fn save_intent(s: &State) -> io::Result<()> {
    let Some(dir) = &s.dir else {
        return Ok(());
    };
    let intent = Intent {
        mode: s.mode,
        custom: s.custom.clone(),
        rid: s.publication.rid.clone(),
        key: s.publication.key.clone(),
        port: s.saved_port,
        tunnel_pid: s.saved_tunnel_pid,
        tunnel_url: s.saved_tunnel_url.clone(),
        tunnel_token: s.tunnel_token.clone(),
        tunnel_hostname: s.tunnel_hostname.clone(),
    };
    intent.save(dir)
}
fn sync_tunnel_intent(s: &mut State) {
    if s.dir.is_none() || s.mode != ConnectMode::Tunnel {
        return;
    }
    let Some(port) = s.selected.map(|a| a.port()) else {
        return;
    };
    let Some(tunnel) = s.tunnel.as_ref() else {
        return;
    };
    let cur_pid = tunnel.pid();
    let cur_url = tunnel.url().map(str::to_owned);
    if cur_url.is_some() {
        tunnel.set_persist(true);
    }
    let next_url = cur_url.or_else(|| {
        (s.saved_tunnel_pid == cur_pid)
            .then(|| s.saved_tunnel_url.clone())
            .flatten()
    });
    if s.saved_port != Some(port) || s.saved_tunnel_pid != cur_pid || s.saved_tunnel_url != next_url
    {
        s.saved_port = Some(port);
        s.saved_tunnel_pid = cur_pid;
        s.saved_tunnel_url = next_url;
        save_intent(s).ok();
    }
}
#[derive(Debug)]
pub(super) enum ShutdownHandle {
    Tcp(TcpStream),
    Unix(UnixStream),
}
impl ShutdownHandle {
    pub(super) fn shutdown(&self) {
        match self {
            Self::Tcp(s) => {
                s.shutdown(Shutdown::Both).ok();
            }
            Self::Unix(s) => {
                s.shutdown(Shutdown::Both).ok();
            }
        }
    }
}
#[derive(Debug)]
pub(super) struct Connection {
    pub(super) accepted: Instant,
    pub(super) device: Option<String>,
    pub(super) socket: ShutdownHandle,
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
        let waker = Arc::new(Waker::new()?);
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
                relay: None,
                relay_enabled: false,
                relay_ctx: None,
                saved_port: intent.port,
                saved_tunnel_pid: intent.tunnel_pid,
                saved_tunnel_url: intent.tunnel_url,
                tunnel_token: intent.tunnel_token,
                tunnel_hostname: intent.tunnel_hostname,
                resolver: super::cloudflared::Resolver::default(),
                publication,
                tunnel_failures: 0,
                tunnel_retry_at: None,
                tunnel_error: None,
                dir: dir.map(Path::to_path_buf),
                ping: PING,
                waker: Arc::clone(&waker),
            }),
            changed: Condvar::new(),
            waker,
        });
        lock(&shared.state).relay_ctx = Some(RelayContext {
            shared: Arc::downgrade(&shared),
            registry: registry.clone(),
            hello: hello.clone(),
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
            if relay_pairing(&state) {
                fixed_base(&state)
            } else if let Some(tunnel) = state.tunnel.as_ref()
                && let Some(url) = tunnel.ready_url()
            {
                if tunnel.fixed() {
                    fixed_base(&state)
                } else {
                    url.to_owned()
                }
            } else {
                return None;
            }
        } else {
            format!("http://{address}")
        };
        Some(format!("{base}/?session={}", session.0))
    }
    pub fn request(&self, request: MobileRequest) -> MobileReply {
        if let MobileRequest::Workflow(json) = request {
            if serde_json::from_str::<serde_json::Value>(&json).ok()
                == Some(serde_json::json!({"action":"install_handoff"}))
            {
                return MobileReply::Workflow(self.install_handoff().to_string());
            }
            return MobileReply::Workflow(self.backend.request(&json));
        }
        let reply = match self.handle(request) {
            Ok(reply) => reply,
            Err(e) => MobileReply::Failed(e.to_string()),
        };
        self.shared.wake();
        reply
    }
    /// 홈 화면 앱(iOS는 Safari와 저장소가 분리) 넘김: 기존 5분 일회용 페어링 티켓의 URL 조각만 준다.
    /// 장기 기기 자격증명은 넘기지 않고, 앱은 이 조각으로 자기 기기로 따로 페어링한다(회수도 따로).
    fn install_handoff(&self) -> serde_json::Value {
        let fragment = (|| -> io::Result<String> {
            let mut state = lock(&self.shared.state);
            let now = now_ms();
            state.devices.mint(now, false)?;
            let MobileReply::PairingOffer { url, .. } = pairing_offer(&state, now)? else {
                return Err(invalid());
            };
            let (_, fragment) = url.split_once('#').ok_or_else(invalid)?;
            Ok(fragment.to_owned())
        })();
        self.shared.wake();
        match fragment {
            Ok(fragment) => serde_json::json!({"ok":true,"data":{"fragment":fragment}}),
            Err(e) => serde_json::json!({"ok":false,"error":e.to_string()}),
        }
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
                reject_retired_mode(mode, custom_address.as_deref())?;
                configure(&mut state, mode, custom_address, now)?;
                let ticket = state.devices.mint(now, rotate)?;
                state.tunnel_retry_at = None;
                reconcile(&mut state, now)?;
                if mode == ConnectMode::Tunnel {
                    // 설치·터널 준비·고정 출처 판정을 모두 락 밖에서 기다린다.
                    drop(state);
                    self.shared.wake();
                    let deadline = Instant::now() + MINT_WAIT;
                    loop {
                        let mut state = lock(&self.shared.state);
                        if state.stopped || state.mode != mode {
                            return Err(io::Error::other("모바일 연결 모드가 변경되었습니다"));
                        }
                        if state
                            .devices
                            .ticket(now_ms())
                            .is_none_or(|current| current.token != ticket.token)
                        {
                            return Err(io::Error::other(
                                "페어링 요청이 취소되었거나 만료되었습니다",
                            ));
                        }
                        // 릴레이가 연결되면 퀵 터널의 DNS/엣지 정착을 기다리지 않는다.
                        if relay_pairing(&state) {
                            return pairing_offer(&state, now_ms());
                        }
                        // 퀵 터널 예비 경로가 없으니 릴레이 첫 시도가 실패했거나 고정 출처에 이 버전 PWA가 없으면 바로 알린다.
                        // 릴레이는 뒤에서 계속 재시도한다.
                        if relay_only(&state)
                            && state
                                .relay
                                .as_ref()
                                .is_some_and(|r| r.settled() && !r.pairing())
                        {
                            return Err(tunnel_unavailable(&state));
                        }
                        // 기한이 지나도 준비되지 않으면 찍힌 URL로 낸다(터널 출처). 몇 초 뒤 새로고침하면 열린다.
                        let timed_out = Instant::now() >= deadline;
                        if state.tunnel.as_ref().is_some_and(|t| {
                            t.ready_url().is_some() || (timed_out && t.url().is_some())
                        }) {
                            sync_tunnel_intent(&mut state);
                            return pairing_offer(&state, now_ms());
                        }
                        let fatal_tunnel_err = state.tunnel.is_none()
                            && !state.resolver.downloading()
                            && state
                                .tunnel_error
                                .as_ref()
                                .is_some_and(|e| e.kind() != io::ErrorKind::WouldBlock);
                        let relay_failed = state
                            .relay
                            .as_ref()
                            .is_none_or(|r| r.settled() && !r.pairing());
                        if timed_out || (fatal_tunnel_err && relay_failed) {
                            return Err(tunnel_unavailable(&state));
                        }
                        // 상태 변경은 바로, 백그라운드 프로브의 준비 표시는 50ms 안에 본다.
                        drop(
                            self.shared
                                .changed
                                .wait_timeout(state, Duration::from_millis(50))
                                .unwrap_or_else(std::sync::PoisonError::into_inner),
                        );
                    }
                } else {
                    pairing_offer(&state, now)?
                }
            }
            MobileRequest::SetConnectMode {
                mode,
                custom_address,
            } => {
                reject_retired_mode(mode, custom_address.as_deref())?;
                configure(&mut state, mode, custom_address, now)?;
                reconcile(&mut state, now)?;
                status(&state, now)
            }
            MobileRequest::CancelPairingTicket => {
                state.devices.clear_ticket();
                reconcile(&mut state, now)?;
                MobileReply::Revoked { count: 0 }
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
                        c.socket.shutdown();
                    }
                }
                reconcile(&mut state, now)?;
                MobileReply::Revoked { count }
            }
        };
        self.shared.wake();
        Ok(reply)
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        let mut state = lock(&self.shared.state);
        state.stopped = true;
        let now = now_ms();
        let keep_tunnel = state.dir.is_some()
            && state.mode == ConnectMode::Tunnel
            && (state.devices.active_count() > 0 || state.devices.ticket(now).is_some());
        if keep_tunnel {
            sync_tunnel_intent(&mut state);
            if let Some(tunnel) = state.tunnel.as_ref() {
                tunnel.set_persist(true);
            }
            state.tunnel = None;
            state.relay = None;
            state.listeners.clear();
            state.waker.quiesce();
            for c in state.connections.values() {
                c.socket.shutdown();
            }
        } else {
            close(&mut state);
        }
        self.shared.wake();
    }
}
fn reject_retired_mode(mode: ConnectMode, custom: Option<&str>) -> io::Result<()> {
    let loopback = custom
        .and_then(|value| value.parse::<std::net::SocketAddrV4>().ok())
        .is_some_and(|address| address.ip().is_loopback());
    if mode == ConnectMode::Lan && !loopback {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "LAN pairing has been retired; choose Relay server or Tailscale",
        ));
    }
    Ok(())
}
// Lan discriminant is retained only for decoding and explicit loopback QA.
fn configure(s: &mut State, mode: ConnectMode, custom: Option<String>, now: u64) -> io::Result<()> {
    let mut next_custom = custom;
    let mut named_changed = false;
    if mode == ConnectMode::Tunnel
        && let Some(raw) = next_custom.as_deref()
    {
        if raw.trim().is_empty() {
            if s.tunnel_token.is_some() || s.tunnel_hostname.is_some() {
                s.tunnel_token = None;
                s.tunnel_hostname = None;
                named_changed = true;
            }
            next_custom = None;
        } else if let Some((token, hostname)) = raw.split_once('@') {
            let token = token.trim();
            let hostname = hostname.trim();
            if token.is_empty() || super::tunnel::normalize_named_url(hostname).is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "유효하지 않은 Named Tunnel 설정(token@hostname)",
                ));
            }
            let next_token = Some(token.to_owned());
            let next_host = Some(hostname.to_owned());
            if s.tunnel_token != next_token || s.tunnel_hostname != next_host {
                s.tunnel_token = next_token;
                s.tunnel_hostname = next_host;
                named_changed = true;
            }
            next_custom = None;
        }
    }
    let reissue = s.devices.ticket(now).is_some();
    let intent_changed = s.mode != mode || s.custom != next_custom || named_changed;
    if intent_changed {
        s.devices.clear_ticket();
        close(s);
        s.selected = None;
    }
    let interfaces = net::detect()?;
    let selected = net::select(&interfaces, mode, next_custom.as_deref())?;
    if intent_changed || s.selected.is_some_and(|a| a.ip() != selected.ip()) {
        s.devices.clear_ticket();
        close(s);
        s.selected = None;
        if reissue {
            s.devices.mint(now, true)?;
        }
    }
    s.mode = mode;
    s.custom = next_custom;
    s.interfaces = interfaces;
    if s.selected.is_none() {
        s.selected = Some(selected);
    }
    if intent_changed {
        save_intent(s)?;
    }
    Ok(())
}
fn pairing_offer(state: &State, now: u64) -> io::Result<MobileReply> {
    let ticket = state.devices.ticket(now).ok_or_else(invalid)?;
    let address = state.selected.ok_or_else(invalid)?;
    let host = address.ip().to_string();
    let port = address.port();
    let relay_only = state.mode == ConnectMode::Tunnel && relay_pairing(state);
    let base = if state.mode == ConnectMode::Tunnel {
        if relay_only {
            fixed_base(state)
        } else {
            let tunnel = state
                .tunnel
                .as_ref()
                .ok_or_else(|| tunnel_unavailable(state))?;
            let url = tunnel
                .ready_url()
                .or_else(|| tunnel.url())
                .ok_or_else(|| tunnel_unavailable(state))?;
            if tunnel.fixed() {
                fixed_base(state)
            } else {
                url.to_owned()
            }
        }
    } else {
        format!("http://{host}:{port}")
    };
    let mut url = format!(
        "{base}/#pair={}&key={}",
        hex(&ticket.token),
        ws::base64(&ticket.secret, true)
    );
    if state.mode == ConnectMode::Tunnel {
        if relay_only {
            write!(url, "&r={}", state.publication.rid).map_err(io::Error::other)?;
        } else if let Some(tunnel) = state.tunnel.as_ref().filter(|t| t.fixed()) {
            let host = tunnel
                .ready_url()
                .and_then(|url| url.strip_prefix("https://"))
                .ok_or_else(invalid)?;
            write!(url, "&t={host}&r={}", state.publication.rid).map_err(io::Error::other)?;
        }
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

fn relay_pairing(state: &State) -> bool {
    state
        .relay
        .as_ref()
        .is_some_and(super::relay::RelayClient::pairing)
}

fn fixed_base(state: &State) -> String {
    format!(
        "{}/m/{}",
        state.publication.origin,
        super::pwa::asset_hash()
    )
}
fn tunnel_unavailable(state: &State) -> io::Error {
    let message = if relay_only(state) {
        if state
            .relay
            .as_ref()
            .is_some_and(super::relay::RelayClient::connected)
        {
            "이 버전의 모바일 웹앱이 아직 배포되지 않아 새 기기를 연결할 수 없습니다".to_owned()
        } else {
            "모바일 릴레이에 연결하지 못했습니다 — 네트워크 확인 후 다시 시도".to_owned()
        }
    } else if state.resolver.downloading() {
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
    if let Some(mut tunnel) = s.tunnel.take() {
        tunnel.terminate();
    } else if let Some(pid) = s.saved_tunnel_pid.take() {
        super::tunnel::kill_pid(pid);
    }
    s.relay = None;
    s.tunnel_failures = 0;
    s.tunnel_retry_at = None;
    if s.saved_port.is_some() || s.saved_tunnel_pid.is_some() || s.saved_tunnel_url.is_some() {
        s.saved_port = None;
        s.saved_tunnel_pid = None;
        s.saved_tunnel_url = None;
        save_intent(s).ok();
    }
    s.listeners.clear();
    s.waker.quiesce();
    for c in s.connections.values() {
        c.socket.shutdown();
    }
}
fn reconcile(s: &mut State, now: u64) -> io::Result<()> {
    if s.devices.ticket(now).is_none() {
        s.devices.clear_ticket();
    }
    if s.stopped {
        return Ok(());
    }
    if s.devices.active_count() == 0 && s.devices.ticket(now).is_none() {
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
    let loopback = if s.mode == ConnectMode::Tunnel
        && selected.port() == 0
        && let Some(saved_port) = s.saved_port.filter(|p| *p > 0)
    {
        if let Ok(l) = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, saved_port)) {
            l
        } else {
            if let Some(pid) = s.saved_tunnel_pid.take() {
                super::tunnel::kill_pid(pid);
            }
            s.saved_port = None;
            s.saved_tunnel_url = None;
            save_intent(s).ok();
            TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?
        }
    } else {
        TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, selected.port()))?
    };
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
fn named_tunnel(s: &State) -> Option<super::tunnel::NamedTunnel> {
    if let (Some(token), Some(hostname)) = (&s.tunnel_token, &s.tunnel_hostname)
        && !token.is_empty()
        && !hostname.is_empty()
    {
        return Some(super::tunnel::NamedTunnel {
            token: token.clone(),
            url: hostname.clone(),
        });
    }
    if let Some((token, hostname)) = s.custom.as_deref().and_then(|c| c.split_once('@'))
        && !token.trim().is_empty()
        && !hostname.trim().is_empty()
    {
        return Some(super::tunnel::NamedTunnel {
            token: token.trim().to_owned(),
            url: hostname.trim().to_owned(),
        });
    }
    let token = std::env::var("KRILL_MOBILE_TUNNEL_TOKEN").ok()?;
    let hostname = std::env::var("KRILL_MOBILE_TUNNEL_HOSTNAME").ok()?;
    if token.trim().is_empty() || hostname.trim().is_empty() {
        return None;
    }
    Some(super::tunnel::NamedTunnel {
        token: token.trim().to_owned(),
        url: hostname.trim().to_owned(),
    })
}
/// 운영 빌드는 늘 릴레이를 쓴다. 테스트 고정 터널(`tunnel_start`)만 릴레이 없이 돈다.
fn relay_on(s: &State) -> bool {
    s.tunnel_start.is_none() || s.relay_enabled
}
/// 릴레이만으로 외부 접속한다(Named Tunnel 없음). 퀵 터널은 무보증·엣지 연결 1개·재시작마다 주소가 바뀌어
/// 예비 경로에서 뺐다.
fn relay_only(s: &State) -> bool {
    s.mode == ConnectMode::Tunnel && relay_on(s) && named_tunnel(s).is_none()
}
/// 외부 접속은 릴레이로 한다. 명시적 Named Tunnel만 터널을 띄우고, 터널 실패는 지수 백오프로 재시도한다.
fn supervise_tunnel(s: &mut State) {
    if s.mode != ConnectMode::Tunnel {
        if let Some(mut tunnel) = s.tunnel.take() {
            tunnel.terminate();
        }
        s.relay = None;
        return;
    }
    let named = named_tunnel(s);
    if named.is_none()
        && relay_on(s)
        && s.relay.is_none()
        && let Some(ctx) = s.relay_ctx.as_ref()
        && let Some(shared) = ctx.shared.upgrade()
    {
        s.relay = super::relay::RelayClient::start(
            s.publication.clone(),
            &shared,
            &ctx.registry,
            &ctx.hello,
        )
        .ok();
    }
    if named.is_none() && relay_on(s) {
        // 이전 버전이 띄워 둔 퀵 터널(입양 대기 PID 포함)을 정리한다.
        if let Some(mut tunnel) = s.tunnel.take() {
            tunnel.terminate();
        }
        let pid = s.saved_tunnel_pid.take();
        let url = s.saved_tunnel_url.take();
        if let Some(pid) = pid {
            super::tunnel::kill_pid(pid);
        }
        if pid.is_some() || url.is_some() {
            save_intent(s).ok();
        }
        s.tunnel_error = None;
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
            sync_tunnel_intent(s);
            return;
        }
        tracing::warn!("mobile tunnel exited or never became reachable; restarting");
        if let Some(mut old) = s.tunnel.take() {
            old.terminate();
        }
        s.saved_tunnel_pid = None;
        s.saved_tunnel_url = None;
        save_intent(s).ok();
        backoff(s);
    }
    if let Some(pid) = s.saved_tunnel_pid {
        if s.saved_port == Some(port)
            && let Some(url) = s.saved_tunnel_url.clone()
            && let Some(tunnel) =
                super::tunnel::Tunnel::adopt(pid, url, Some(s.publication.clone()))
        {
            if s.dir.is_some() {
                tunnel.set_persist(true);
            }
            s.tunnel = Some(tunnel);
            s.tunnel_failures = 0;
            s.tunnel_retry_at = None;
            s.tunnel_error = None;
            return;
        }
        super::tunnel::kill_pid(pid);
        s.saved_tunnel_pid = None;
        s.saved_tunnel_url = None;
        save_intent(s).ok();
    }
    if s.tunnel_retry_at.is_some_and(|at| Instant::now() < at) {
        return;
    }
    let result = if let Some(start) = s.tunnel_start {
        start(port)
    } else {
        s.resolver.resolve(s.dir.as_deref()).and_then(|binary| {
            super::tunnel::Tunnel::start(binary.as_os_str(), port, s.publication.clone(), named)
        })
    };
    match result {
        Ok(tunnel) => {
            s.tunnel = Some(tunnel);
            s.tunnel_retry_at = None;
            s.tunnel_error = None;
            sync_tunnel_intent(s);
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
        let now = Instant::now();
        let mut deadline: Option<Instant> = None;
        for connection in state.connections.values() {
            if connection.device.is_some() {
                continue;
            }
            let due = connection.accepted + TIMEOUT;
            if now >= due {
                connection.socket.shutdown();
            } else {
                deadline = Some(deadline.map_or(due, |d| d.min(due)));
            }
        }
        let pending = state
            .connections
            .values()
            .filter(|c| c.device.is_none())
            .count();
        let room = pending < MAX_PENDING && state.connections.len() < MAX_CONNECTIONS;
        let mut failed = false;
        let incoming = if room {
            state.listeners.iter().find_map(|l| match l.accept() {
                Ok((s, _)) => Some(s),
                Err(e) => {
                    failed |= e.kind() != io::ErrorKind::WouldBlock;
                    None
                }
            })
        } else {
            None
        };
        let Some(socket) = incoming else {
            let idle = state.listeners.is_empty()
                && state.devices.active_count() == 0
                && state.devices.ticket(now_ms()).is_none();
            // 수락 오류(EMFILE 등)는 리스너가 계속 읽기 가능이라 50ms 뒤 다시 시도한다.
            let timeout = if idle {
                None
            } else if failed {
                Some(Duration::from_millis(50))
            } else {
                Some(deadline.map_or(RECONCILE, |d| {
                    d.saturating_duration_since(now).min(RECONCILE)
                }))
            };
            wait(shared, state, room && !failed, timeout);
            continue;
        };
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
                socket: ShutdownHandle::Tcp(kept),
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
    }
}
/// 상태 락을 풀고 새 연결(`listen`이면)·깨우기·`timeout`(`None`이면 깨우기까지) 중 먼저 오는 것을 기다린다.
#[cfg(unix)]
fn wait(shared: &Shared, state: MutexGuard<'_, State>, listen: bool, timeout: Option<Duration>) {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let fds: Vec<TcpListener> = if listen {
        state
            .listeners
            .iter()
            .filter_map(|l| l.try_clone().ok())
            .collect()
    } else {
        Vec::new()
    };
    // 상태 락 안에서 표시해야 리스너를 닫는 쪽(quiesce)이 놓치지 않는다.
    *lock(&shared.waker.polling) = !fds.is_empty();
    drop(state);
    let mut polls: Vec<PollFd<'_>> = fds.iter().map(|l| PollFd::new(l, PollFlags::IN)).collect();
    polls.push(PollFd::new(&shared.waker.rx, PollFlags::IN));
    let timeout = timeout.and_then(|t| Timespec::try_from(t).ok());
    poll(&mut polls, timeout.as_ref()).ok();
    drop(polls);
    drop(fds);
    *lock(&shared.waker.polling) = false;
    shared.waker.idle.notify_all();
    let mut drain = [0; 64];
    while io::Read::read(&mut &shared.waker.rx, &mut drain).is_ok_and(|n| n > 0) {}
}
#[cfg(not(unix))]
fn wait(shared: &Shared, state: MutexGuard<'_, State>, _listen: bool, timeout: Option<Duration>) {
    drop(match timeout {
        None => shared
            .changed
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        Some(_) => {
            shared
                .changed
                .wait_timeout(state, Duration::from_millis(50))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0
        }
    });
}
struct Connected {
    shared: Arc<Shared>,
    id: u64,
}
impl Drop for Connected {
    fn drop(&mut self) {
        let mut s = lock(&self.shared.state);
        if let Some(c) = s.connections.remove(&self.id) {
            c.socket.shutdown();
        }
        reconcile(&mut s, now_ms()).ok();
        self.shared.wake();
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
    let channel = match authenticate(&mut socket, shared, id) {
        Ok(channel) => channel,
        Err(error) => {
            // RFC 6455 policy violation은 인증/프로토콜 거부만 나타낸다.
            // EOF·timeout·쓰기 오류는 이 신호를 보내지 않아 클라이언트가 전송 실패와 구별한다.
            if let Some(code) = auth_close_code(&error) {
                ws::write(&mut socket, 8, &code.to_be_bytes()).ok();
                socket.shutdown(Shutdown::Write).ok();
                socket
                    .set_read_timeout(Some(Duration::from_millis(50)))
                    .ok();
                // 클라이언트가 인증 뒤 프레임을 미리 보내므로(파이프라이닝) 닫기 전에 EOF까지 비운다.
                // 남은 바이트가 있으면 close가 RST가 되어 클라이언트가 1008 대신 1006을 본다.
                let mut drain = [0; 512];
                let until = Instant::now() + Duration::from_millis(250);
                while Instant::now() < until
                    && io::Read::read(&mut socket, &mut drain).is_ok_and(|n| n > 0)
                {
                }
            }
            return Err(error);
        }
    };
    shared.wake();
    let registry = registry.upgrade().ok_or_else(invalid)?;
    let ping = lock(&shared.state).ping;
    socket.set_read_timeout(Some(ping))?;
    bridge(socket, channel, registry, hello.clone())
}

pub(super) fn auth_close_code(error: &io::Error) -> Option<u16> {
    use super::devices::AuthRejection;
    if let Some(rejection) = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<AuthRejection>())
    {
        return Some(match rejection {
            AuthRejection::Capacity => 4009,
            AuthRejection::Cooldown => 1013,
        });
    }
    (error.kind() == io::ErrorKind::InvalidData).then_some(1008)
}

fn authenticate(socket: &mut TcpStream, shared: &Arc<Shared>, id: u64) -> io::Result<Channel> {
    // 주소와 무관한 실패 예산. 미인증 연결은 최대 4개·5초로 제한한다.
    lock(&shared.state).devices.check_cooldown(now_ms())?;
    let result = (|| {
        let challenge = random()?;
        ws::write(
            socket,
            ws::TEXT,
            serde_json::to_vec(&serde_json::json!({"version":2,"challenge":hex(&challenge)}))
                .map_err(io::Error::other)?
                .as_slice(),
        )?;
        let (opcode, body) = ws::read_limited(socket, 4096)?;
        if opcode == 8 {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
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
        let (opcode, body) = ws::read_limited(socket, 512)?;
        if opcode == 8 {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        if opcode != ws::BINARY {
            return Err(invalid());
        }
        let proof: AuthProof =
            serde_json::from_slice(&channel.open(&body)?).map_err(|_| invalid())?;
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
        let payload =
            channel.seal(&serde_json::to_vec(&authenticated).map_err(io::Error::other)?)?;
        ws::write(socket, ws::BINARY, &payload)?;
        Ok(channel)
    })();
    if result
        .as_ref()
        .is_err_and(|error: &io::Error| error.kind() == io::ErrorKind::InvalidData)
    {
        lock(&shared.state).devices.attempt(now_ms()).ok();
    }
    result
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
        .stack_size(RPC_STACK)
        .spawn(move || {
            crate::alloc::mobile_thread().ok();
            // server_stop 복제가 fd를 붙잡고 있어 serve가 끝나도 local 쪽에 EOF가 오지 않는다.
            // 레지스트리가 먼저 끝내면(첫 요청 기한 초과·세션 종료) 여기서 닫아 WS도 닫히게 한다.
            let done = server.try_clone();
            let result = registry.serve_mobile(server, &hello);
            if let Ok(done) = done {
                done.shutdown(Shutdown::Both).ok();
            }
            result
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
#[cfg(unix)]
mod tests;
