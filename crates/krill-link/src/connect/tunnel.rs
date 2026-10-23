//! `krill-link connect <호스트 id>` 클라이언트 터널 및 상태 조회(`krill-link status --json`, lib API).
//!
//! - `<state_dir>/run/hosts/<id>.sock`를 리슨하고 각 UDS 연결마다 QUIC bi-stream을 연다.
//! - QUIC 연결이 끊기면 지수 백오프(50ms 시작, 상한 2,000ms)로 자동 재연결하며 로컬 소켓은 닫지 않고 유지한다.
//! - 앱(MK-131 호스트 계층)과 CLI가 소켓 경로 규칙 및 연결 상태(연결됨/연결 중/끊김, 직접/중계 경로, RTT)를
//!   조회할 수 있도록 함수와 상태 파일(`<state_dir>/run/hosts/<id>.status.json`)을 제공한다.

use std::fs::{self, DirBuilder};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::{
    ConnectOptions, HostTunnelStatus, LinkStatusReport, RouteKind, TunnelErrorKind, TunnelState,
    inspect_connection_route, reconnect_backoff_delay,
};
use iroh::endpoint::Connection;
use krill_proto::daemon::SUN_PATH_MAX;
use krill_proto::os::fs::DirBuilderExt as _;
use tokio::sync::{Notify, oneshot, watch};

use crate::config::{LinkConfig, RelaySetting};
use crate::identity::DeviceIdentity;
use crate::local::Listener;
use crate::serve::{build_iroh_endpoint, is_serve_running, raise_nofile_limit};
use crate::store::{
    acquire_instance_lock, find_host, load_devices, load_hosts, now_unix_ms, validate_host_id,
    write_status_0600,
};
use crate::{ALPN_LINK, Error};

/// 원격 호스트의 로컬 `AF_UNIX` 소켓 경로(`<state_dir>/run/hosts/<host_id>.sock`)를 계산하고
/// 플랫폼 `SUN_PATH_MAX`(macOS 104·Windows 108바이트) 제한을 검증한다(CONVENTIONS A6).
pub fn host_socket_path(state_dir: &Path, host_id: &str) -> Result<PathBuf, Error> {
    validate_host_id(host_id)?;
    let path = state_dir
        .join("run/hosts")
        .join(format!("{}.sock", host_id.trim()));
    if path.as_os_str().len() >= SUN_PATH_MAX {
        return Err(Error::PathTooLong(path));
    }
    Ok(path)
}

/// 원격 호스트의 상태 파일 경로(`<state_dir>/run/hosts/<host_id>.status.json`)를 계산한다.
pub fn host_status_path(state_dir: &Path, host_id: &str) -> Result<PathBuf, Error> {
    validate_host_id(host_id)?;
    Ok(state_dir
        .join("run/hosts")
        .join(format!("{}.status.json", host_id.trim())))
}

/// 특정 원격 호스트의 현재 터널 상태를 조회한다.
///
/// `connect` 프로세스가 살아 있지 않거나 상태 파일이 없으면 `TunnelState::Disconnected`를 돌려준다.
pub fn host_status(state_dir: &Path, host_id: &str) -> Result<HostTunnelStatus, Error> {
    let host = find_host(state_dir, host_id)?;
    let sock_path = host_socket_path(state_dir, &host.id)?;
    let status_file = host_status_path(state_dir, &host.id)?;

    if let Ok(text) = fs::read_to_string(&status_file)
        && let Ok(mut parsed) = serde_json::from_str::<HostTunnelStatus>(&text)
    {
        let alive = is_tunnel_alive(&parsed) && sock_path.exists();
        if alive {
            return Ok(parsed);
        }
        parsed.state = TunnelState::Disconnected;
        parsed.route = RouteKind::None;
        parsed.remote_addr = None;
        parsed.rtt_ms = None;
        parsed.pid = None;
        for stream in &mut parsed.streams {
            stream.state = crate::resume::StreamPhase::Closed;
            stream.close_reason = Some("tunnel process stopped".to_owned());
        }
        return Ok(parsed);
    }

    Ok(HostTunnelStatus {
        host_id: host.id,
        host_name: host.name,
        node_id: host.node_id,
        socket_path: sock_path,
        state: TunnelState::Disconnected,
        route: RouteKind::None,
        remote_addr: None,
        rtt_ms: None,
        reconnect_attempt: 0,
        updated_at_ms: now_unix_ms(),
        pid: None,
        last_error: None,
        last_error_kind: None,
        streams: Vec::new(),
    })
}

/// `link/hosts.toml`에 등록된 모든 원격 호스트의 터널 상태를 조회한다.
pub fn all_host_statuses(state_dir: &Path) -> Result<Vec<HostTunnelStatus>, Error> {
    let hosts = load_hosts(state_dir)?;
    let mut out = Vec::with_capacity(hosts.len());
    for h in hosts {
        out.push(host_status(state_dir, &h.id)?);
    }
    Ok(out)
}

/// 로컬 기기 신원·설정·서버 상태·모든 원격 호스트 터널 상태를 종합한 보고서를 만든다.
pub fn status_report(state_dir: &Path) -> Result<LinkStatusReport, Error> {
    let identity = DeviceIdentity::load_or_create(state_dir)?;
    let cfg = LinkConfig::load(state_dir)?;
    let devices = load_devices(state_dir)?;
    let hosts = all_host_statuses(state_dir)?;
    Ok(LinkStatusReport {
        node_id: identity.node_id().to_string(),
        relay_mode: cfg.relay.as_str().to_owned(),
        serve_running: is_serve_running(state_dir),
        allowed_devices_count: devices.len(),
        hosts,
        server_streams: if is_serve_running(state_dir) {
            fs::read(state_dir.join("run/link-streams.status.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        },
    })
}

#[cfg(unix)]
fn is_tunnel_alive(status: &HostTunnelStatus) -> bool {
    status.pid.is_some_and(|pid| {
        let Ok(raw) = i32::try_from(pid) else {
            return false;
        };
        let Some(p) = rustix::process::Pid::from_raw(raw) else {
            return false;
        };
        rustix::process::test_kill_process(p).is_ok()
    })
}

/// Windows: PID 조회는 unsafe FFI라 쓰지 않고, 살아 있는 터널이 400ms마다 다시 쓰는 상태 파일의 신선도로 판단한다.
#[cfg(windows)]
fn is_tunnel_alive(status: &HostTunnelStatus) -> bool {
    // ponytail: 갱신 주기의 5배. 상태 파일 쓰기가 막히면 살아 있어도 끊김으로 보인다.
    const STALE_MS: u64 = 2_000;
    status.pid.is_some() && now_unix_ms().saturating_sub(status.updated_at_ms) <= STALE_MS
}

fn persist_status(status_path: &Path, status: &HostTunnelStatus) -> bool {
    serde_json::to_vec_pretty(status)
        .is_ok_and(|bytes| write_status_0600(status_path, &bytes).is_ok())
}

/// 실행 중인 `krill-link connect` 터널 인스턴스 핸들.
#[derive(Debug)]
pub struct TunnelHandle {
    socket_path: PathBuf,
    status_rx: watch::Receiver<HostTunnelStatus>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<tokio::task::JoinHandle<()>>,
    streams: StreamMap,
    dial_paused: Arc<std::sync::atomic::AtomicBool>,
    reconnect: Arc<Notify>,
}

impl TunnelHandle {
    /// 로컬 리슨 소켓 경로 (`<state_dir>/run/hosts/<id>.sock`).
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// 현재 터널 상태 스냅샷을 즉시 읽는다.
    pub fn status(&self) -> HostTunnelStatus {
        let mut status = self.status_rx.borrow().clone();
        status.streams = stream_snapshots(&self.streams);
        status
    }

    /// Fault-injection hook: keep every UDS while pausing transport redial.
    #[doc(hidden)]
    pub fn set_transport_offline(&self, offline: bool) {
        self.dial_paused
            .store(offline, std::sync::atomic::Ordering::Release);
        if offline {
            self.reconnect.notify_one();
        }
    }

    /// 터널을 정상 종료하고 소켓·상태 파일을 정리한다.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            tx.send(()).ok();
        }
        if let Some(jh) = self.join_handle.take() {
            jh.await.ok();
        }
    }
}

/// 백그라운드 태스크로 `krill-link connect <host_id>` 터널을 기동한다.
///
/// 로컬 소켓(`<state_dir>/run/hosts/<id>.sock`)은 즉시 바인드되어 열려 있으며,
/// QUIC 연결이 끊기더라도 로컬 소켓은 닫히지 않고 지수 백오프로 재연결한다.
pub async fn start_tunnel(opts: ConnectOptions) -> Result<TunnelHandle, Error> {
    raise_nofile_limit();
    let state_dir = opts.state_dir.clone();
    let host = find_host(&state_dir, &opts.host_id)?;
    let socket_path = host_socket_path(&state_dir, &host.id)?;
    let status_path = host_status_path(&state_dir, &host.id)?;
    let instance_lock =
        acquire_instance_lock(&socket_path.with_extension("lock")).map_err(Error::Io)?;
    let mut cfg = LinkConfig::load(&state_dir)?;
    cfg.validate()?;
    let remote_addr = host.to_endpoint_addr(cfg.relay != RelaySetting::Off)?;
    if remote_addr.relay_urls().next().is_none() && cfg.relay == RelaySetting::N0 {
        cfg.relay = RelaySetting::Off;
    }
    if let Some(parent) = socket_path.parent() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(Error::Io)?;
    }
    fs::remove_file(&socket_path).ok();
    let listener = Listener::bind(&socket_path).map_err(Error::Io)?;
    let identity = DeviceIdentity::load_or_create(&state_dir)?;
    let endpoint =
        build_iroh_endpoint(&state_dir, &identity, &cfg, false, Some(&remote_addr)).await?;
    let initial_conn = tokio::time::timeout(
        Duration::from_secs(2),
        endpoint.connect(remote_addr.clone(), ALPN_LINK),
    )
    .await
    .ok()
    .and_then(Result::ok);
    let initial_status = HostTunnelStatus {
        host_id: host.id,
        host_name: host.name,
        node_id: host.node_id,
        socket_path: socket_path.clone(),
        state: if initial_conn.is_some() {
            TunnelState::Connected
        } else {
            TunnelState::Connecting
        },
        route: RouteKind::None,
        remote_addr: None,
        rtt_ms: None,
        reconnect_attempt: 0,
        updated_at_ms: now_unix_ms(),
        pid: Some(std::process::id()),
        last_error: None,
        last_error_kind: None,
        streams: Vec::new(),
    };
    let (status_tx, status_rx) = watch::channel(initial_status);
    let (connection_tx, connection_rx) = watch::channel(ConnectionState {
        epoch: 0,
        connection: initial_conn,
    });
    let streams: StreamMap = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let dial_paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let rejected = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reconnect = Arc::new(Notify::new());
    let supervisor = {
        let endpoint = endpoint.clone();
        let connection_tx = connection_tx.clone();
        let paused = Arc::clone(&dial_paused);
        let reconnect = Arc::clone(&reconnect);
        let rejected = Arc::clone(&rejected);
        let streams = Arc::clone(&streams);
        let dial_status = status_tx.clone();
        tokio::spawn(async move {
            let mut failures = 0;
            let mut epoch = 0u64;
            loop {
                let current = connection_tx.borrow().connection.clone();
                if let Some(conn) = current {
                    tokio::select! { _=conn.closed()=>{}, ()=reconnect.notified()=>{ conn.close(0u32.into(),b"transport test outage"); } }
                    let reason = conn.close_reason();
                    let kind = reason
                        .as_ref()
                        .map_or(TunnelErrorKind::Network, |error| classify_error(error));
                    connection_tx.send_modify(|state| state.connection = None);
                    if record_failure(
                        &dial_status,
                        &streams,
                        &rejected,
                        0,
                        reason.map_or_else(
                            || "connection closed".to_owned(),
                            |error| error.to_string(),
                        ),
                        kind,
                    ) {
                        endpoint.close().await;
                        break;
                    }
                    failures = u32::from(kind == TunnelErrorKind::RateLimited);
                    // The first redial after a network loss has no backoff.
                }

                if paused.load(std::sync::atomic::Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                }
                if failures > 0 {
                    tokio::time::sleep(reconnect_backoff_delay(failures - 1)).await;
                }
                match tokio::time::timeout(
                    Duration::from_secs(3),
                    endpoint.connect(remote_addr.clone(), ALPN_LINK),
                )
                .await
                {
                    Ok(Ok(conn)) => {
                        rejected.store(false, std::sync::atomic::Ordering::Release);
                        dial_status.send_modify(|status| {
                            status.reconnect_attempt = 0;
                            status.last_error = None;
                            status.last_error_kind = None;
                        });
                        let Some(next_epoch) = epoch.checked_add(1) else {
                            close_streams(&streams, "connection epoch exhausted");
                            return;
                        };
                        epoch = next_epoch;
                        connection_tx.send_replace(ConnectionState {
                            epoch,
                            connection: Some(conn),
                        });
                        failures = 0;
                    }
                    Ok(Err(error)) => {
                        failures = failures.saturating_add(1);
                        let kind = classify_error(&error);
                        if record_failure(
                            &dial_status,
                            &streams,
                            &rejected,
                            failures,
                            error.to_string(),
                            kind,
                        ) {
                            endpoint.close().await;
                            break;
                        }
                    }
                    Err(_) => {
                        failures = failures.saturating_add(1);
                        record_failure(
                            &dial_status,
                            &streams,
                            &rejected,
                            failures,
                            "connect attempt timed out".to_owned(),
                            TunnelErrorKind::Network,
                        );
                    }
                }
            }
        })
    };
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let stream_map = Arc::clone(&streams);
    let cleanup_socket = socket_path.clone();
    let join_handle = tokio::spawn(async move {
        let _instance_lock = instance_lock;
        let mut tasks = tokio::task::JoinSet::new();
        let mut tick = tokio::time::interval(Duration::from_millis(400));
        let mut saved_status = None;
        loop {
            tokio::select! {
                _=&mut shutdown_rx=>break,
                _=tick.tick()=>{
                    let conn=connection_rx.borrow().connection.clone();
                    publish_status(&status_tx,&stream_map,conn.as_ref(),&status_path,&mut saved_status);
                }
                _=tasks.join_next(),if !tasks.is_empty()=>{},
                accepted=listener.accept()=>{
                    let Ok((unix,_))=accepted else { break };
                    if rejected.load(std::sync::atomic::Ordering::Acquire) { continue; }
                    let Ok(id)=crate::resume::StreamId::random() else { continue };
                    let grace=Duration::from_millis(cfg.resume_grace_ms);
                    let Ok((logical,actor))=crate::resume::Logical::spawn(unix,id,grace,cfg.replay_buffer_bytes) else { continue };
                    if let Ok(mut map)=stream_map.lock() { map.insert(id,Arc::clone(&logical)); }
                    let driver_logical=Arc::clone(&logical); let conn_rx=connection_rx.clone();
                    let driver=tokio::spawn(async move { drive_client_stream(driver_logical,conn_rx,grace).await; });
                    let map=Arc::clone(&stream_map);
                    tasks.spawn(async move {
                        actor.await.ok(); driver.await.ok();
                        if let Ok(mut map)=map.lock() { map.remove(&id); }
                    });
                }
            }
        }
        supervisor.abort();
        supervisor.await.ok();
        if let Some(conn) = connection_tx.borrow().connection.as_ref() {
            conn.close(0u32.into(), b"tunnel shutdown");
        }
        if let Ok(map) = stream_map.lock() {
            for logical in map.values() {
                logical.close("tunnel shutdown");
            }
        }
        while tasks.join_next().await.is_some() {}
        let mut final_status = status_tx.borrow().clone();
        final_status.state = TunnelState::Disconnected;
        final_status.route = RouteKind::None;
        final_status.remote_addr = None;
        final_status.rtt_ms = None;
        final_status.pid = None;
        final_status.streams = stream_snapshots(&stream_map);
        persist_status(&status_path, &final_status);
        status_tx.send_replace(final_status);
        fs::remove_file(&cleanup_socket).ok();
        endpoint.close().await;
    });
    Ok(TunnelHandle {
        socket_path,
        status_rx,
        shutdown_tx: Some(shutdown_tx),
        join_handle: Some(join_handle),
        streams,
        dial_paused,
        reconnect,
    })
}

#[derive(Debug, Clone)]
struct ConnectionState {
    epoch: u64,
    connection: Option<Connection>,
}

type StreamMap = Arc<
    std::sync::Mutex<
        std::collections::BTreeMap<crate::resume::StreamId, Arc<crate::resume::Logical>>,
    >,
>;
fn publish_status(
    status_tx: &watch::Sender<HostTunnelStatus>,
    streams: &StreamMap,
    connection: Option<&Connection>,
    path: &Path,
    saved: &mut Option<HostTunnelStatus>,
) {
    status_tx.send_modify(|status| {
        status.streams = stream_snapshots(streams);
        status.updated_at_ms = now_unix_ms();
        if let Some(conn) = connection.filter(|c| c.close_reason().is_none()) {
            let (route, remote, rtt) = inspect_connection_route(conn);
            status.state = TunnelState::Connected;
            status.route = route;
            status.remote_addr = remote;
            status.rtt_ms = rtt;
        } else {
            status.state = if status.last_error_kind == Some(TunnelErrorKind::Unauthorized) {
                TunnelState::Disconnected
            } else {
                TunnelState::Connecting
            };
            status.route = RouteKind::None;
            status.remote_addr = None;
            status.rtt_ms = None;
        }
    });
    let snapshot = status_tx.borrow().clone();
    if status_changed_or_due(saved.as_ref(), &snapshot) && persist_status(path, &snapshot) {
        *saved = Some(snapshot);
    }
}

fn status_changed_or_due(previous: Option<&HostTunnelStatus>, next: &HostTunnelStatus) -> bool {
    let Some(previous) = previous else {
        return true;
    };
    // Keep Windows' two-second liveness contract without making timestamp-only
    // changes force a new file on every 400ms observation tick.
    if next.updated_at_ms.saturating_sub(previous.updated_at_ms) >= 1_000 {
        return true;
    }
    let mut comparable = next.clone();
    comparable.updated_at_ms = previous.updated_at_ms;
    &comparable != previous
}

fn classify_error(error: &(dyn std::error::Error + 'static)) -> TunnelErrorKind {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(connection) = error.downcast_ref::<iroh::endpoint::ConnectionError>() {
            return match connection {
                iroh::endpoint::ConnectionError::ApplicationClosed(close)
                    if close.error_code == iroh::endpoint::VarInt::from_u32(403) =>
                {
                    TunnelErrorKind::Unauthorized
                }
                iroh::endpoint::ConnectionError::ApplicationClosed(close)
                    if close.error_code == iroh::endpoint::VarInt::from_u32(429) =>
                {
                    TunnelErrorKind::RateLimited
                }
                iroh::endpoint::ConnectionError::ApplicationClosed(_) => TunnelErrorKind::Other,
                _ => TunnelErrorKind::Network,
            };
        }
        current = error.source();
    }
    TunnelErrorKind::Other
}

fn record_failure(
    status: &watch::Sender<HostTunnelStatus>,
    streams: &StreamMap,
    rejected: &std::sync::atomic::AtomicBool,
    attempt: u32,
    message: String,
    kind: TunnelErrorKind,
) -> bool {
    let unauthorized = kind == TunnelErrorKind::Unauthorized;
    status.send_modify(|status| {
        status.reconnect_attempt = attempt;
        status.last_error = Some(message);
        status.last_error_kind = Some(kind);
        status.updated_at_ms = now_unix_ms();
        if unauthorized {
            status.state = TunnelState::Disconnected;
            status.route = RouteKind::None;
            status.remote_addr = None;
            status.rtt_ms = None;
        }
    });
    if unauthorized {
        rejected.store(true, std::sync::atomic::Ordering::Release);
        close_streams(streams, "device authorization revoked");
    }
    unauthorized
}
fn close_streams(streams: &StreamMap, reason: &str) {
    if let Ok(map) = streams.lock() {
        for logical in map.values() {
            logical.close(reason);
        }
    }
}
fn stream_snapshots(streams: &StreamMap) -> Vec<crate::resume::StreamStatus> {
    streams
        .lock()
        .map(|map| map.values().map(|l| l.snapshot()).collect())
        .unwrap_or_default()
}

async fn drive_client_stream(
    logical: Arc<crate::resume::Logical>,
    mut connections: watch::Receiver<ConnectionState>,
    grace: Duration,
) {
    let mut previous = None;
    let mut generation = 0u64;
    loop {
        if logical.is_closed() {
            return;
        }
        let current = connections.borrow().clone();
        if let Some(conn) = current.connection
            && conn.close_reason().is_none()
            && previous != Some(current.epoch)
        {
            previous = Some(current.epoch);
            generation = generation.saturating_add(1);
            let Ok(hello) = logical.hello(generation, generation > 1, grace) else {
                logical.close("invalid local state");
                return;
            };
            let attempt = tokio::select! {
                result=async {
                let (mut send, mut recv) = conn
                    .open_bi()
                    .await
                    .map_err(|e| Error::Transport(e.to_string()))?;
                crate::resume::wire::write_hello(&mut send, hello).await?;
                let mut code = [0];
                recv.read_exact(&mut code)
                    .await
                    .map_err(|e| Error::Transport(e.to_string()))?;
                if code[0] != 0 {
                    return Err(Error::Protocol("remote stream unavailable"));
                }
                let reply = crate::resume::wire::read_hello(&mut recv).await?;
                if reply.id != logical.id || reply.generation != generation {
                    return Err(Error::Protocol("invalid ready identity"));
                }
                let (ready, rx) = oneshot::channel();
                logical
                    .attach(crate::resume::Attachment {
                        conn: conn.clone(),
                        send,
                        recv,
                        hello: reply,
                        reply: false,
                        ready,
                    })
                    .await?;
                rx.await
                    .map_err(|_| Error::Protocol("local stream closed"))?
                    .map_err(Error::Protocol)
            }=>result,
                _=conn.closed()=>Err(Error::Transport("connection closed during attachment".to_owned())),
                ()=logical.closed()=>return,
            };
            if matches!(attempt, Err(Error::Protocol(_))) {
                logical.close("remote stream unavailable");
                return;
            }
            // An ambiguous Open is never repeated: the next connection uses Resume.
            if attempt.is_err() && conn.close_reason().is_none() {
                previous = None;
            }
        }
        tokio::select! { result=connections.changed()=>if result.is_err() { return; }, ()=tokio::time::sleep(Duration::from_millis(20))=>{} }
    }
}

#[cfg(test)]
mod tests {
    use super::super::BACKOFF_MAX;
    use super::*;

    #[test]
    fn status_heartbeat_skips_timestamp_only_ticks_but_reports_changes() {
        let mut status: HostTunnelStatus = serde_json::from_value(serde_json::json!({
            "host_id":"test", "host_name":"Test", "node_id":"node",
            "socket_path":"/tmp/test.sock", "state":"connecting", "route":"none",
            "remote_addr":null, "rtt_ms":null, "reconnect_attempt":0,
            "updated_at_ms":1000, "pid":null, "last_error":null
        }))
        .unwrap();
        assert!(status_changed_or_due(None, &status));
        let previous = status.clone();
        status.updated_at_ms = 1400;
        assert!(!status_changed_or_due(Some(&previous), &status));
        status.last_error = Some("changed".into());
        assert!(status_changed_or_due(Some(&previous), &status));
        status.last_error = None;
        status.updated_at_ms = 2000;
        assert!(status_changed_or_due(Some(&previous), &status));
    }

    #[test]
    fn rejection_codes_are_machine_readable_and_network_errors_are_separate() {
        for (code, kind) in [
            (403, TunnelErrorKind::Unauthorized),
            (429, TunnelErrorKind::RateLimited),
            (400, TunnelErrorKind::Other),
        ] {
            let error = iroh::endpoint::ConnectionError::ApplicationClosed(
                iroh::endpoint::ApplicationClose {
                    error_code: iroh::endpoint::VarInt::from_u32(code),
                    reason: b"server rejection".to_vec().into(),
                },
            );
            assert_eq!(classify_error(&error), kind);
        }
        assert_eq!(
            classify_error(&iroh::endpoint::ConnectionError::TimedOut),
            TunnelErrorKind::Network
        );
        assert_eq!(
            classify_error(&std::io::Error::other("configuration")),
            TunnelErrorKind::Other
        );
    }

    #[test]
    fn old_status_json_defaults_new_fields_and_error_values_are_stable() {
        let old = serde_json::json!({"host_id":"home","host_name":"Home","node_id":"node","socket_path":"/tmp/home.sock","state":"disconnected","route":"none","remote_addr":null,"rtt_ms":null,"reconnect_attempt":0,"updated_at_ms":0,"pid":null,"last_error":null});
        let parsed: HostTunnelStatus = serde_json::from_value(old).unwrap();
        assert_eq!(parsed.last_error_kind, None);
        assert!(parsed.streams.is_empty());
        for (kind, text) in [
            (TunnelErrorKind::Unauthorized, "unauthorized"),
            (TunnelErrorKind::RateLimited, "rate_limited"),
            (TunnelErrorKind::Network, "network"),
            (TunnelErrorKind::Other, "other"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), text);
        }
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(reconnect_backoff_delay(0), Duration::from_millis(50));
        assert_eq!(reconnect_backoff_delay(1), Duration::from_millis(100));
        assert_eq!(reconnect_backoff_delay(2), Duration::from_millis(200));
        assert_eq!(reconnect_backoff_delay(10), BACKOFF_MAX);
        assert_eq!(reconnect_backoff_delay(100), BACKOFF_MAX);
    }

    #[test]
    fn host_socket_path_rejects_traversal_and_long_paths() {
        let base = Path::new("/tmp/mk-state");
        assert_eq!(
            host_socket_path(base, "home-mac").unwrap(),
            PathBuf::from("/tmp/mk-state/run/hosts/home-mac.sock")
        );
        assert!(host_socket_path(base, "../evil").is_err());
        assert!(host_socket_path(base, "").is_err());
        assert!(host_socket_path(base, "-flag").is_err());

        let long_base = PathBuf::from(format!("/tmp/{}", "a".repeat(100)));
        assert!(matches!(
            host_socket_path(&long_base, "home-mac"),
            Err(Error::PathTooLong(_))
        ));
    }
}
