//! `krill-link serve`: iroh QUIC 서버(`ALPN_LINK` + `ALPN_PAIR`), 허용 목록 강제, 즉시 revoke, 제어 소켓(`AF_UNIX`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use iroh::address_lookup::{AddrFilter, MemoryLookup};
use iroh::endpoint::{
    AfterHandshakeOutcome, BindOpts, Connection, EndpointHooks, QuicTransportConfig, VarInt,
    presets,
};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{Mutex, oneshot, watch};

use super::ServeOptions;
use crate::config::{LinkConfig, RelaySetting};
use crate::identity::DeviceIdentity;
use crate::local::{Listener, Stream};
use crate::pair::{
    PairRateLimiter, PairWireRequest, PairWireResponse, encode_addr_ticket,
    generate_invite_for_addr, read_pair_frame, write_pair_frame,
};
use crate::store::{
    AllowedDevice, compact_endpoint_addr, load_allowed_node_set, now_unix_ms, now_unix_secs,
    remove_device, sanitize_id_slug, upsert_device, verify_and_consume_invite, write_atomic_0600,
};
use crate::{ALPN_LINK, ALPN_PAIR, Error};

/// 서버 바인드 포트 보존 파일 상대 경로 (`<state_dir>/link/serve.port`).
const SERVE_PORT_REL_PATH: &str = "link/serve.port";
/// 실행 중인 서버 주소 티켓 파일 상대 경로 (`<state_dir>/link/serve.ticket`).
const SERVE_TICKET_REL_PATH: &str = "link/serve.ticket";
/// 서버 로컬 제어 소켓 상대 경로 (`<state_dir>/run/link-ctl.sock`).
const SERVE_CTL_SOCK_REL_PATH: &str = "run/link-ctl.sock";

/// QUIC 경로 유휴 타임아웃(밀리초).
const QUIC_IDLE_TIMEOUT_MS: u32 = 3_000;
/// QUIC keep-alive 간격.
const QUIC_KEEP_ALIVE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone)]
struct ServerHooks {
    allowed: Arc<RwLock<BTreeSet<EndpointId>>>,
    rate_limiter: Arc<PairRateLimiter>,
}

impl EndpointHooks for ServerHooks {
    fn after_handshake(
        &self,
        conn: &Connection,
    ) -> impl std::future::Future<Output = AfterHandshakeOutcome> + Send {
        let alpn = conn.alpn();
        let outcome = if alpn == ALPN_LINK {
            let is_allowed = self
                .allowed
                .read()
                .is_ok_and(|set| set.contains(&conn.remote_id()));
            if is_allowed {
                AfterHandshakeOutcome::Accept
            } else {
                AfterHandshakeOutcome::Reject {
                    error_code: 403u32.into(),
                    reason: b"unauthorized device".to_vec(),
                }
            }
        } else if alpn == ALPN_PAIR {
            if self.rate_limiter.check_and_record_attempt(Instant::now()) {
                AfterHandshakeOutcome::Accept
            } else {
                AfterHandshakeOutcome::Reject {
                    error_code: 429u32.into(),
                    reason: b"too many pairing attempts".to_vec(),
                }
            }
        } else {
            AfterHandshakeOutcome::Reject {
                error_code: 400u32.into(),
                reason: b"unsupported alpn".to_vec(),
            }
        };
        std::future::ready(outcome)
    }
}

/// 프로세스 소프트 fd 한도를 올린다(CONVENTIONS §3). Windows는 fd 한도가 없어 아무것도 안 한다.
#[cfg(windows)]
pub fn raise_nofile_limit() {}

/// 프로세스 소프트 fd 한도를 올린다(CONVENTIONS §3).
#[cfg(unix)]
pub fn raise_nofile_limit() {
    use rustix::process::{Resource, getrlimit, setrlimit};
    const TARGET_NOFILE: u64 = 10_240;
    let mut lim = getrlimit(Resource::Nofile);
    let cap = lim.maximum.unwrap_or(TARGET_NOFILE).min(TARGET_NOFILE);
    if lim.current.is_none_or(|cur| cur < cap) {
        lim.current = Some(cap);
        setrlimit(Resource::Nofile, lim).ok();
    }
}

/// 설정과 신원에 맞춰 `iroh::Endpoint`를 생성·바인드한다.
///
/// - `is_server = true`일 때는 `link/config.toml`의 `bind_port` 또는 이전에 바인드했던 `link/serve.port`를
///   우선 재사용해, `serve`가 재시작돼도 로컬/LAN 직접 주소(`ip:port`)가 바뀌지 않게 한다.
/// - `known_peer`가 주어지면 `MemoryLookup`에 등록해 같은 LAN 내 직접 주소를 즉시 찾게 한다(요구사항 6).
pub(crate) async fn build_iroh_endpoint(
    state_dir: &Path,
    identity: &DeviceIdentity,
    cfg: &LinkConfig,
    is_server: bool,
    known_peer: Option<&EndpointAddr>,
) -> Result<Endpoint, Error> {
    let relay_mode = cfg.relay.to_iroh_relay_mode()?;
    cfg.validate()?;
    let transport_cfg = QuicTransportConfig::builder()
        .keep_alive_interval(QUIC_KEEP_ALIVE)
        .default_path_keep_alive_interval(QUIC_KEEP_ALIVE)
        .default_path_max_idle_timeout(Duration::from_millis(u64::from(QUIC_IDLE_TIMEOUT_MS)))
        .max_idle_timeout(Some(VarInt::from_u32(QUIC_IDLE_TIMEOUT_MS).into()))
        .max_concurrent_bidi_streams(VarInt::from_u32(32))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .stream_receive_window(VarInt::from_u32(32 * 1024))
        .receive_window(VarInt::from_u32(256 * 1024))
        .send_window(256 * 1024)
        .build();

    let preferred_port = if is_server {
        cfg.bind_port.or_else(|| read_saved_serve_port(state_dir))
    } else {
        None
    };

    if let Some(port) = preferred_port.filter(|&p| p > 0)
        && let Ok(ep) = bind_endpoint_once(
            identity,
            &cfg.relay,
            relay_mode.clone(),
            &transport_cfg,
            Some(port),
            known_peer,
        )
        .await
    {
        if is_server {
            save_bound_serve_port(state_dir, &ep.addr());
        }
        return Ok(ep);
    }

    let ep = bind_endpoint_once(
        identity,
        &cfg.relay,
        relay_mode,
        &transport_cfg,
        None,
        known_peer,
    )
    .await?;
    if is_server {
        save_bound_serve_port(state_dir, &ep.addr());
    }
    Ok(ep)
}

async fn bind_endpoint_once(
    identity: &DeviceIdentity,
    relay_setting: &RelaySetting,
    relay_mode: iroh::RelayMode,
    transport_cfg: &QuicTransportConfig,
    fixed_port: Option<u16>,
    known_peer: Option<&EndpointAddr>,
) -> Result<Endpoint, Error> {
    let mut builder = match relay_setting {
        RelaySetting::N0 => Endpoint::builder(presets::N0),
        RelaySetting::Custom(_) | RelaySetting::Off => Endpoint::builder(presets::Minimal),
    };
    builder = builder
        .secret_key(identity.secret_key().clone())
        .relay_mode(relay_mode)
        .transport_config(transport_cfg.clone());

    if *relay_setting == RelaySetting::Off {
        builder = builder.addr_filter(AddrFilter::unfiltered());
    }

    if let Some(peer) = known_peer {
        let mem_lookup = MemoryLookup::from_endpoint_info([peer.clone()]);
        builder = builder.address_lookup(mem_lookup);
    }

    if let Some(port) = fixed_port {
        builder = builder
            .clear_ip_transports()
            .bind_addr(("0.0.0.0", port))
            .map_err(|e| Error::Transport(format!("invalid ipv4 bind addr: {e}")))?
            .bind_addr_with_opts(("::", port), BindOpts::default().set_is_required(false))
            .map_err(|e| Error::Transport(format!("invalid ipv6 bind addr: {e}")))?;
    }

    builder
        .bind()
        .await
        .map_err(|e| Error::Transport(format!("iroh endpoint bind failed: {e}")))
}

fn read_saved_serve_port(state_dir: &Path) -> Option<u16> {
    let text = fs::read_to_string(state_dir.join(SERVE_PORT_REL_PATH)).ok()?;
    text.trim().parse::<u16>().ok().filter(|&p| p > 0)
}

fn save_bound_serve_port(state_dir: &Path, addr: &EndpointAddr) {
    if let Some(sock) = addr.ip_addrs().next() {
        let port = sock.port();
        if port > 0 {
            write_atomic_0600(
                &state_dir.join(SERVE_PORT_REL_PATH),
                format!("{port}\n").as_bytes(),
            )
            .ok();
        }
    }
}

/// 실행 중인 `krill-link serve`의 현재 `EndpointAddr`를 읽는다(없거나 프로세스가 죽었으면 `None`).
pub fn read_live_server_addr(state_dir: &Path) -> Option<EndpointAddr> {
    let ctl = state_dir.join(SERVE_CTL_SOCK_REL_PATH);
    if !ctl.exists() {
        return None;
    }
    let ticket = fs::read_to_string(state_dir.join(SERVE_TICKET_REL_PATH)).ok()?;
    crate::pair::decode_addr_ticket(ticket.trim()).ok()
}

#[derive(Debug, Clone)]
struct ActiveConnectionEntry {
    conn: Connection,
    revoke_tx: watch::Sender<bool>,
}

type ActivePeersMap = Arc<Mutex<BTreeMap<EndpointId, Vec<ActiveConnectionEntry>>>>;

/// 실행 중인 `krill-link serve` 인스턴스 핸들.
#[derive(Debug)]
pub struct ServerHandle {
    state_dir: PathBuf,
    addr: EndpointAddr,
    allowed: Arc<RwLock<BTreeSet<EndpointId>>>,
    active_peers: ActivePeersMap,
    streams: Arc<crate::resume::registry::Registry>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<tokio::task::JoinHandle<()>>,
}

impl ServerHandle {
    /// 서버의 현재 `EndpointAddr`(루프백 포함 보장).
    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.addr.clone()
    }

    /// 서버 `EndpointId`(`NodeId`).
    pub fn node_id(&self) -> EndpointId {
        self.addr.id
    }

    /// 실행 중인 서버에 1회용 초대 링크를 생성한다.
    pub fn create_invite(&self, ttl: Duration) -> Result<crate::pair::InviteOutput, Error> {
        let cfg = LinkConfig::load(&self.state_dir)?;
        let include_relay = cfg.relay != RelaySetting::Off;
        generate_invite_for_addr(&self.state_dir, &self.addr, include_relay, ttl)
    }

    /// 기기를 즉시 허용 목록에서 제거하고 해당 기기의 모든 활성 QUIC 연결과 스트림을 즉시 끊는다.
    pub async fn revoke_device(&self, target: &str) -> Result<AllowedDevice, Error> {
        let removed = remove_device(&self.state_dir, target)?;
        let node_id = removed.endpoint_id()?;
        if let Ok(mut set) = self.allowed.write() {
            set.remove(&node_id);
        }
        self.streams.revoke(node_id);
        disconnect_peer(&self.active_peers, node_id).await;
        Ok(removed)
    }

    /// Current logical stream states, including suspended connections.
    pub fn stream_statuses(&self) -> Vec<crate::resume::StreamStatus> {
        self.streams.statuses()
    }

    /// 서버를 정상 종료하고 모든 리소스를 정리한다.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            tx.send(()).ok();
        }
        if let Some(jh) = self.join_handle.take() {
            jh.await.ok();
        }
    }
}

async fn disconnect_peer(active_peers: &ActivePeersMap, node_id: EndpointId) {
    let entries = {
        let mut map = active_peers.lock().await;
        map.remove(&node_id).unwrap_or_default()
    };
    for entry in entries {
        entry.revoke_tx.send(true).ok();
        entry.conn.close(403u32.into(), b"device revoked");
    }
}

#[derive(Debug, Serialize, Deserialize)]
enum ControlRequest {
    Revoke { target: String },
    Reload,
    Status,
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
enum ControlResponse {
    Ok {
        message: String,
    },
    Status {
        node_id: String,
        allowed_count: usize,
        active_peers: usize,
    },
    Err {
        message: String,
    },
}

/// 백그라운드 태스크로 `krill-link serve`를 기동하고 `ServerHandle`을 돌려준다.
pub async fn start_server(opts: ServeOptions) -> Result<ServerHandle, Error> {
    raise_nofile_limit();
    let state_dir = opts.state_dir.clone();
    let krilld_sock = opts.effective_krilld_socket();
    let cfg = LinkConfig::load(&state_dir)?;
    let identity = DeviceIdentity::load_or_create(&state_dir)?;
    let initial_allowed = load_allowed_node_set(&state_dir)?;
    let allowed = Arc::new(RwLock::new(initial_allowed));
    let rate_limiter = opts
        .rate_limiter
        .unwrap_or_else(|| Arc::new(PairRateLimiter::default()));

    let hooks = ServerHooks {
        allowed: Arc::clone(&allowed),
        rate_limiter: Arc::clone(&rate_limiter),
    };

    let relay_mode = cfg.relay.to_iroh_relay_mode()?;
    cfg.validate()?;
    let transport_cfg = QuicTransportConfig::builder()
        .keep_alive_interval(QUIC_KEEP_ALIVE)
        .default_path_keep_alive_interval(QUIC_KEEP_ALIVE)
        .default_path_max_idle_timeout(Duration::from_millis(u64::from(QUIC_IDLE_TIMEOUT_MS)))
        .max_idle_timeout(Some(VarInt::from_u32(QUIC_IDLE_TIMEOUT_MS).into()))
        .max_concurrent_bidi_streams(VarInt::from_u32(32))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .stream_receive_window(VarInt::from_u32(32 * 1024))
        .receive_window(VarInt::from_u32(256 * 1024))
        .send_window(256 * 1024)
        .build();

    let preferred_port = cfg.bind_port.or_else(|| read_saved_serve_port(&state_dir));

    let endpoint = if let Some(port) = preferred_port.filter(|&p| p > 0)
        && let Ok(ep) = bind_server_endpoint(
            &identity,
            &cfg.relay,
            relay_mode.clone(),
            &transport_cfg,
            hooks.clone(),
            Some(port),
        )
        .await
    {
        ep
    } else {
        bind_server_endpoint(
            &identity,
            &cfg.relay,
            relay_mode,
            &transport_cfg,
            hooks,
            None,
        )
        .await?
    };

    if cfg.relay != RelaySetting::Off {
        tokio::time::timeout(Duration::from_secs(5), endpoint.online())
            .await
            .ok();
    }

    let mut addr = endpoint.addr();
    ensure_loopback_addr(&mut addr);
    save_bound_serve_port(&state_dir, &addr);

    let include_relay = cfg.relay != RelaySetting::Off;
    let ticket = encode_addr_ticket(&compact_endpoint_addr(&addr, include_relay))?;
    let ticket_path = state_dir.join(SERVE_TICKET_REL_PATH);
    write_atomic_0600(&ticket_path, ticket.as_bytes()).ok();

    let ctl_sock_path = state_dir.join(SERVE_CTL_SOCK_REL_PATH);
    let ctl_listener = bind_ctl_listener(&ctl_sock_path)?;

    let active_peers: ActivePeersMap = Arc::new(Mutex::new(BTreeMap::new()));
    let streams = Arc::new(crate::resume::registry::Registry::new(
        Duration::from_millis(cfg.resume_grace_ms),
        cfg.replay_buffer_bytes,
    ));
    let streams_loop = Arc::clone(&streams);
    let connection_slots = Arc::new(tokio::sync::Semaphore::new(8));
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    let allowed_loop = Arc::clone(&allowed);
    let peers_loop = Arc::clone(&active_peers);
    let state_loop = state_dir.clone();
    let krilld_arc = Arc::new(krilld_sock);

    let join_handle = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                _=tick.tick()=> {
                    streams_loop.sweep();
                    if let Ok(bytes)=serde_json::to_vec(&streams_loop.statuses()) { write_atomic_0600(&state_loop.join("run/link-streams.status.json"),&bytes).ok(); }
                }
                _=tasks.join_next(),if !tasks.is_empty()=>{},
                incoming_opt = endpoint.accept() => {
                    let Some(incoming) = incoming_opt else { break };
                    let Ok(permit)=Arc::clone(&connection_slots).try_acquire_owned() else { incoming.refuse(); continue; };
                    let registry=Arc::clone(&streams_loop);
                    let allowed_conn = Arc::clone(&allowed_loop);
                    let peers_conn = Arc::clone(&peers_loop);
                    let krilld_conn = Arc::clone(&krilld_arc);
                    let state_conn = state_loop.clone();
                    let rate_conn = Arc::clone(&rate_limiter);
                    tasks.spawn(async move {
                        let _permit=permit;
                        let Ok(accepting) = incoming.accept() else { return };
                        let Ok(Ok(conn)) = tokio::time::timeout(Duration::from_secs(5),accepting).await else { return };
                        let alpn = conn.alpn().to_vec();
                        if alpn == ALPN_LINK {
                            handle_link_connection(
                                conn,
                                krilld_conn,
                                allowed_conn,
                                peers_conn,
                                registry,
                            )
                            .await;
                        } else if alpn == ALPN_PAIR {
                            handle_pair_connection(
                                conn,
                                state_conn,
                                allowed_conn,
                                rate_conn,
                            )
                            .await;
                        } else {
                            conn.close(400u32.into(), b"unsupported alpn");
                        }
                    });
                }
                ctl_res = accept_ctl_optional(ctl_listener.as_ref()) => {
                    if let Some(stream) = ctl_res {
                        let allowed_ctl = Arc::clone(&allowed_loop);
                        let peers_ctl = Arc::clone(&peers_loop);
                        let state_ctl = state_loop.clone();
                        let node_str = endpoint.id().to_string();
                        let should_stop = handle_ctl_stream(
                            stream,
                            &state_ctl,
                            &node_str,
                            &allowed_ctl,
                            &peers_ctl,
                            &streams_loop,
                        )
                        .await;
                        if should_stop {
                            break;
                        }
                    }
                }
            }
        }

        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        streams_loop.shutdown().await;
        fs::remove_file(state_loop.join("run/link-streams.status.json")).ok();
        // 모든 활성 QUIC 연결 종료
        let all_entries: Vec<ActiveConnectionEntry> = {
            let mut map = peers_loop.lock().await;
            let mut v = Vec::new();
            for (_, list) in std::mem::take(&mut *map) {
                v.extend(list);
            }
            v
        };
        for entry in all_entries {
            entry.revoke_tx.send(true).ok();
            entry.conn.close(0u32.into(), b"server shutting down");
        }
        fs::remove_file(&ctl_sock_path).ok();
        fs::remove_file(&ticket_path).ok();
        endpoint.close().await;
    });

    Ok(ServerHandle {
        state_dir,
        addr,
        allowed,
        active_peers,
        streams,
        shutdown_tx: Some(shutdown_tx),
        join_handle: Some(join_handle),
    })
}

async fn bind_server_endpoint(
    identity: &DeviceIdentity,
    relay_setting: &RelaySetting,
    relay_mode: iroh::RelayMode,
    transport_cfg: &QuicTransportConfig,
    hooks: ServerHooks,
    fixed_port: Option<u16>,
) -> Result<Endpoint, Error> {
    let mut builder = match relay_setting {
        RelaySetting::N0 => Endpoint::builder(presets::N0),
        RelaySetting::Custom(_) | RelaySetting::Off => Endpoint::builder(presets::Minimal),
    };
    builder = builder
        .secret_key(identity.secret_key().clone())
        .relay_mode(relay_mode)
        .transport_config(transport_cfg.clone())
        .hooks(hooks)
        .alpns(vec![ALPN_LINK.to_vec(), ALPN_PAIR.to_vec()]);

    if *relay_setting == RelaySetting::Off {
        builder = builder.addr_filter(AddrFilter::unfiltered());
    }

    if let Some(port) = fixed_port {
        builder = builder
            .clear_ip_transports()
            .bind_addr(("0.0.0.0", port))
            .map_err(|e| Error::Transport(format!("invalid ipv4 bind addr: {e}")))?
            .bind_addr_with_opts(("::", port), BindOpts::default().set_is_required(false))
            .map_err(|e| Error::Transport(format!("invalid ipv6 bind addr: {e}")))?;
    }

    builder
        .bind()
        .await
        .map_err(|e| Error::Transport(format!("server endpoint bind failed: {e}")))
}

/// 같은 머신이나 오프라인 환경에서도 즉시 붙을 수 있도록 바인드된 포트의 `127.0.0.1:<port>`를 주소 목록에 보장한다.
fn ensure_loopback_addr(addr: &mut EndpointAddr) {
    let port = addr.ip_addrs().next().map(SocketAddr::port);
    if let Some(p) = port.filter(|&p| p > 0) {
        let loopback = SocketAddr::from(([127, 0, 0, 1], p));
        addr.addrs.insert(iroh::TransportAddr::Ip(loopback));
    }
}

fn bind_ctl_listener(ctl_path: &Path) -> Result<Option<Listener>, Error> {
    if ctl_path.as_os_str().len() >= krill_proto::daemon::SUN_PATH_MAX {
        return Ok(None);
    }
    if let Some(parent) = ctl_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        use krill_proto::os::fs::DirBuilderExt as _;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(Error::Io)?;
    }
    fs::remove_file(ctl_path).ok();
    let listener = Listener::bind(ctl_path).map_err(Error::Io)?;
    Ok(Some(listener))
}

async fn accept_ctl_optional(listener: Option<&Listener>) -> Option<Stream> {
    match listener {
        Some(l) => l.accept().await.ok().map(|(stream, _)| stream),
        None => std::future::pending().await,
    }
}

async fn handle_ctl_stream(
    mut stream: Stream,
    state_dir: &Path,
    node_id: &str,
    allowed: &Arc<RwLock<BTreeSet<EndpointId>>>,
    active_peers: &ActivePeersMap,
    streams: &crate::resume::registry::Registry,
) -> bool {
    let mut buf = Vec::with_capacity(256);
    if !tokio::time::timeout(
        Duration::from_millis(250),
        (&mut stream).take(4097).read_to_end(&mut buf),
    )
    .await
    .is_ok_and(|r| r.is_ok())
        || buf.len() > 4096
    {
        return false;
    }
    let Ok(req) = serde_json::from_slice::<ControlRequest>(&buf) else {
        return false;
    };
    let mut should_stop = false;
    let resp = match req {
        ControlRequest::Revoke { target } => {
            if let Ok(peer_id) = target.parse::<EndpointId>() {
                if let Ok(mut set) = allowed.write() {
                    set.remove(&peer_id);
                }
                streams.revoke(peer_id);
                disconnect_peer(active_peers, peer_id).await;
                ControlResponse::Ok {
                    message: format!("revoked {peer_id}"),
                }
            } else {
                if let Ok(fresh) = load_allowed_node_set(state_dir) {
                    let removed_ids: Vec<EndpointId> = allowed
                        .read()
                        .map(|old| old.difference(&fresh).copied().collect())
                        .unwrap_or_default();
                    if let Ok(mut set) = allowed.write() {
                        *set = fresh;
                    }
                    for id in removed_ids {
                        streams.revoke(id);
                        disconnect_peer(active_peers, id).await;
                    }
                }
                ControlResponse::Ok {
                    message: "reloaded allowlist".to_owned(),
                }
            }
        }
        ControlRequest::Reload => match load_allowed_node_set(state_dir) {
            Ok(fresh) => {
                let removed_ids: Vec<EndpointId> = allowed
                    .read()
                    .map(|old| old.difference(&fresh).copied().collect())
                    .unwrap_or_default();
                if let Ok(mut set) = allowed.write() {
                    *set = fresh;
                }
                for id in removed_ids {
                    streams.revoke(id);
                    disconnect_peer(active_peers, id).await;
                }
                ControlResponse::Ok {
                    message: "reloaded".to_owned(),
                }
            }
            Err(e) => ControlResponse::Err {
                message: e.to_string(),
            },
        },
        ControlRequest::Status => {
            let allowed_count = allowed.read().map_or(0, |s| s.len());
            let active_count = active_peers.lock().await.len();
            ControlResponse::Status {
                node_id: node_id.to_owned(),
                allowed_count,
                active_peers: active_count,
            }
        }
        ControlRequest::Shutdown => {
            should_stop = true;
            ControlResponse::Ok {
                message: "shutting down".to_owned(),
            }
        }
    };
    if let Ok(json) = serde_json::to_vec(&resp) {
        stream.write_all(&json).await.ok();
        stream.shutdown().await.ok();
    }
    should_stop
}

async fn handle_link_connection(
    conn: Connection,
    krilld_sock: Arc<PathBuf>,
    allowed: Arc<RwLock<BTreeSet<EndpointId>>>,
    active_peers: ActivePeersMap,
    streams: Arc<crate::resume::registry::Registry>,
) {
    let remote_id = conn.remote_id();
    let (revoke_tx, mut revoke_rx) = watch::channel(false);
    {
        let mut map = active_peers.lock().await;
        if !allowed.read().is_ok_and(|set| set.contains(&remote_id)) {
            conn.close(403u32.into(), b"unauthorized device");
            return;
        }
        map.entry(remote_id)
            .or_default()
            .push(ActiveConnectionEntry {
                conn: conn.clone(),
                revoke_tx,
            });
    }
    let mut tasks = tokio::task::JoinSet::new();
    let handshake_slots = Arc::new(tokio::sync::Semaphore::new(32));
    loop {
        tokio::select! {
            _=revoke_rx.changed()=> { conn.close(403u32.into(),b"device revoked"); break; }
            _=tasks.join_next(),if !tasks.is_empty()=>{},
            bi=conn.accept_bi()=> {
                let Ok((send,recv))=bi else { break };
                let Ok(permit)=Arc::clone(&handshake_slots).try_acquire_owned() else { drop((send,recv)); continue; };
                let registry=Arc::clone(&streams); let socket=Arc::clone(&krilld_sock); let allowed=Arc::clone(&allowed); let conn=conn.clone();
                tasks.spawn(async move { let _permit=permit; registry.accept(conn,send,recv,&socket,&allowed).await; });
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    let mut map = active_peers.lock().await;
    if let Some(entries) = map.get_mut(&remote_id) {
        entries.retain(|entry| entry.conn.stable_id() != conn.stable_id());
        if entries.is_empty() {
            map.remove(&remote_id);
        }
    }
}

async fn handle_pair_connection(
    conn: Connection,
    state_dir: PathBuf,
    allowed: Arc<RwLock<BTreeSet<EndpointId>>>,
    rate_limiter: Arc<PairRateLimiter>,
) {
    let remote_id = conn.remote_id();
    let Ok(Ok((mut send, mut recv))) =
        tokio::time::timeout(Duration::from_secs(5), conn.accept_bi()).await
    else {
        conn.close(400u32.into(), b"pair stream timeout");
        return;
    };

    let Ok(Ok(req)) = tokio::time::timeout(
        Duration::from_secs(5),
        read_pair_frame::<PairWireRequest>(&mut recv),
    )
    .await
    else {
        rate_limiter.record_failure(Instant::now());
        conn.close(400u32.into(), b"invalid pair request");
        return;
    };

    let cfg = LinkConfig::load(&state_dir).unwrap_or_default();
    let server_name = cfg.effective_server_name();
    let host_id = sanitize_id_slug(&server_name, "host", &conn.alpn().len().to_string());

    let verify_res = verify_and_consume_invite(&state_dir, &req.token, now_unix_ms());
    let resp = match verify_res {
        Ok(_record) => {
            match upsert_device(&state_dir, remote_id, &req.device_name, now_unix_secs()) {
                Ok(_) => {
                    if let Ok(mut set) = allowed.write() {
                        set.insert(remote_id);
                    }
                    PairWireResponse {
                        accepted: true,
                        server_name,
                        host_id,
                        reason: None,
                    }
                }
                Err(e) => PairWireResponse {
                    accepted: false,
                    server_name: String::new(),
                    host_id: String::new(),
                    reason: Some(format!("failed to persist device: {e}")),
                },
            }
        }
        Err(Ok(verify_err)) => {
            rate_limiter.record_failure(Instant::now());
            PairWireResponse {
                accepted: false,
                server_name: String::new(),
                host_id: String::new(),
                reason: Some(verify_err.to_string()),
            }
        }
        Err(Err(io_err)) => PairWireResponse {
            accepted: false,
            server_name: String::new(),
            host_id: String::new(),
            reason: Some(format!("server invite store error: {io_err}")),
        },
    };

    write_pair_frame(&mut send, &resp).await.ok();
    send.finish().ok();
    // 클라이언트가 응답 프레임을 읽을 시간을 준다.
    tokio::time::timeout(Duration::from_secs(2), conn.closed())
        .await
        .ok();
}

/// `krill-link revoke <id>`: 허용 목록(`link/devices.toml`)에서 기기를 제거하고,
/// 실행 중인 `krill-link serve`가 있으면 제어 소켓으로 통보해 열린 QUIC 연결을 즉시 끊는다.
pub async fn revoke_device_command(state_dir: &Path, target: &str) -> Result<AllowedDevice, Error> {
    let removed = remove_device(state_dir, target)?;
    let ctl_path = state_dir.join(SERVE_CTL_SOCK_REL_PATH);
    if ctl_path.exists()
        && let Ok(mut stream) = Stream::connect(&ctl_path).await
    {
        let req = ControlRequest::Revoke {
            target: removed.node_id.clone(),
        };
        if let Ok(bytes) = serde_json::to_vec(&req) {
            stream.write_all(&bytes).await.ok();
            stream.shutdown().await.ok();
            let mut reply = Vec::new();
            tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut reply))
                .await
                .ok();
        }
    }
    Ok(removed)
}

/// 실행 중인 `krill-link serve`가 살아 있는지 제어 소켓으로 확인한다.
pub fn is_serve_running(state_dir: &Path) -> bool {
    let ctl_path = state_dir.join(SERVE_CTL_SOCK_REL_PATH);
    krill_proto::os::net::UnixStream::connect(ctl_path).is_ok()
}
