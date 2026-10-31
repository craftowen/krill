//! MK-131: 원격 호스트 Connection Manager.
//!
//! - 원격 호스트당 활성 터널 1개만 유지한다(매 명령·폴링마다 새 SSH 핸드셰이크 금지).
//! - 터널 프로세스 종료 또는 원격 데몬 응답 불능 감지 시 지수 백오프로 재연결을 예약하고,
//!   백그라운드 실행자에서 재시도할 때 기존 패인 재연결 경로(`Known`)가 그대로 복구되게 한다.
//! - 호스트별 슬롯 잠금을 분리해 원격 호스트 하나의 지연·장애가 로컬 세션이나 다른 원격 호스트,
//!   UI 스레드를 절대 막지 않게 한다.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use krill_proto::daemon::Paths;

use crate::ssh::{self, SshOptions, Tunnel};
use crate::{HostId, HostStatus, HostTarget, RemoteConnector};

const INITIAL_BACKOFF: Duration = Duration::from_millis(200);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// 이만큼 잇달아 실패하면(약 30초) 꺼져 있는 기기로 보고 [`OFFLINE_BACKOFF`] 간격으로만 다시 시도한다.
const OFFLINE_AFTER_FAILURES: u32 = 10;
/// 꺼진 기기 재시도 간격. 사이드바 '다시 연결'([`HostManager::reconnect_now`])은 이 대기를 바로 푼다.
const OFFLINE_BACKOFF: Duration = Duration::from_secs(60);

/// 같은 접속 대상을 가리키는 관리자끼리 나누는 '꺼진 기기' 판정의 키.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct OfflineKey {
    ssh_bin: Option<std::ffi::OsString>,
    local_dir: Option<PathBuf>,
    destination: String,
    port: Option<u16>,
    identity_file: Option<String>,
    remote_socket: Option<String>,
}

/// 앱은 창마다 관리자를 따로 둔다. 판정을 나누지 않으면 꺼진 기기에 띄우는 ssh 재시도와 그에 따른
/// 상태 깜빡임(연결 중 → 끊김)이 창 수만큼, 창마다 다른 위상으로 [`OFFLINE_BACKOFF`]마다 반복된다.
/// 값은 다음 재시도 시각과 마지막 오류.
static OFFLINE: std::sync::LazyLock<Mutex<BTreeMap<OfflineKey, (Instant, String)>>> =
    std::sync::LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// 백그라운드에서 수집한 전송 상태. UI에서는 이 스냅샷만 읽는다.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostConnectionInfo {
    pub status: HostStatus,
    pub detail: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct ExternalHost {
    pub spec: RemoteHostSpec,
    pub socket: PathBuf,
    pub info: HostConnectionInfo,
    factory: Arc<dyn Fn() -> RemoteConnector + Send + Sync>,
}

impl std::fmt::Debug for ExternalHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalHost")
            .field("spec", &self.spec)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl ExternalHost {
    /// 이 등록으로 데몬에 붙는 호스트 타깃(연결 세대 추적 포함).
    pub fn target(&self) -> HostTarget {
        HostTarget::remote_with_connector(
            self.spec.id.clone(),
            self.socket.clone(),
            (self.factory)(),
        )
    }

    pub fn new(
        spec: RemoteHostSpec,
        socket: PathBuf,
        info: HostConnectionInfo,
        factory: impl Fn() -> RemoteConnector + Send + Sync + 'static,
    ) -> Self {
        Self {
            spec,
            socket,
            info,
            factory: Arc::new(factory),
        }
    }
}

/// UI·사이드바·설정 계층과 주고받는 전송 무관 원격 호스트 명세.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHostSpec {
    pub id: String,
    pub label: String,
    pub address: String,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub remote_socket: Option<String>,
}

impl RemoteHostSpec {
    pub fn display_name(&self) -> &str {
        let trimmed = self.label.trim();
        if trimmed.is_empty() {
            &self.address
        } else {
            trimmed
        }
    }

    fn to_ssh_options(&self) -> SshOptions {
        SshOptions {
            destination: self.address.clone(),
            port: self.port,
            identity_file: self.identity_file.clone(),
            remote_socket: self.remote_socket.clone(),
        }
    }
}

#[derive(Debug)]
struct RemoteSlot {
    /// 연결 작업끼리만 직렬화한다. UI가 읽는 상태 잠금은 네트워크 IO 동안 풀어 둔다.
    connection_gate: Arc<Mutex<()>>,
    spec: RemoteHostSpec,
    status: HostStatus,
    tunnel: Option<Tunnel>,
    placeholder_socket: PathBuf,
    failures: u32,
    next_retry_at: Option<Instant>,
    last_error: Option<String>,
    tunnel_starts: usize,
    generation: u64,
    registration: Arc<()>,
    /// 사용자가 사이드바에서 '연결 해제'를 눌러 수동으로 끊은 상태.
    /// `reconnect_now`로만 풀린다. 켜져 있으면 자동 재연결·폴링 재접속을 하지 않는다.
    suspended: bool,
}

impl RemoteSlot {
    fn new(spec: RemoteHostSpec) -> Self {
        let placeholder_socket = ssh::local_socket(&spec.address)
            .unwrap_or_else(|_| std::env::temp_dir().join(format!("mk-fwd-{}.sock", spec.id)));
        Self {
            connection_gate: Arc::new(Mutex::new(())),
            spec,
            status: HostStatus::Connecting,
            tunnel: None,
            placeholder_socket,
            failures: 0,
            next_retry_at: None,
            last_error: None,
            tunnel_starts: 0,
            generation: 0,
            registration: Arc::new(()),
            suspended: false,
        }
    }

    fn current_socket(&self) -> PathBuf {
        self.tunnel.as_ref().map_or_else(
            || self.placeholder_socket.clone(),
            |t| t.socket().to_path_buf(),
        )
    }
}

/// 다중 원격 호스트 연결 관리자.
#[derive(Debug, Clone, Default)]
pub struct HostManager {
    external: Arc<Mutex<BTreeMap<String, ExternalHost>>>,
    slots: Arc<Mutex<BTreeMap<String, Arc<Mutex<RemoteSlot>>>>>,
    order: Arc<Mutex<Vec<String>>>,
    /// 같은 기기를 가리키는 중복 등록 id → 대표 id. 중복 id로 저장된 칸도 대표 슬롯의 터널을 쓴다.
    aliases: Arc<Mutex<BTreeMap<String, String>>>,
    local_dir: Option<PathBuf>,
    ssh_bin: Option<std::ffi::OsString>,
}

impl HostManager {
    /// 외부 전송 목록 갱신은 SSH 슬롯을 제거하지 않는다. 팩토리는 타깃을 요청할 때만 호출한다.
    pub fn sync_external_hosts(&self, hosts: Vec<ExternalHost>) {
        if let Ok(mut map) = self.external.lock() {
            *map = hosts
                .into_iter()
                .map(|host| (host.spec.id.clone(), host))
                .collect();
        }
    }

    pub fn connection_info(&self, id: &str) -> Option<HostConnectionInfo> {
        self.external
            .lock()
            .ok()?
            .get(id)
            .map(|host| host.info.clone())
    }
    pub fn new() -> Self {
        Self::default()
    }

    /// 테스트·격리 환경에서 로컬 터널 소켓 디렉터리와 `ssh` 실행 파일을 주입해 생성한다.
    pub fn with_ssh_program(local_dir: PathBuf, ssh_bin: std::ffi::OsString) -> Self {
        Self {
            local_dir: Some(local_dir),
            ssh_bin: Some(ssh_bin),
            ..Self::default()
        }
    }

    /// 연속 실패 횟수에 따른 지수 백오프 지연(200ms → 400ms → 800ms → 1.6s → 3.2s → 최대 5s).
    /// [`OFFLINE_AFTER_FAILURES`]번을 넘기면 [`OFFLINE_BACKOFF`]로 늘려, 꺼진 기기에 ssh를 5초마다 띄우지 않는다.
    pub fn backoff_delay(failures: u32) -> Duration {
        if failures > OFFLINE_AFTER_FAILURES {
            return OFFLINE_BACKOFF;
        }
        let shift = failures.saturating_sub(1).min(5);
        let factor = 1u32.checked_shl(shift).unwrap_or(32);
        INITIAL_BACKOFF.saturating_mul(factor).min(MAX_BACKOFF)
    }

    /// 설정된 원격 호스트 목록을 동기화한다. 기존 호스트의 살아 있는 터널은 그대로 유지하고,
    /// 주소·포트·키가 바뀐 경우에만 기존 터널을 내린다.
    pub fn sync_specs(&self, specs: &[RemoteHostSpec]) {
        let Ok(mut map) = self.slots.lock() else {
            return;
        };
        let Ok(mut order) = self.order.lock() else {
            return;
        };
        order.clear();
        let mut keep = BTreeMap::new();
        for spec in specs {
            if spec.id.trim().is_empty() || spec.address.trim().is_empty() {
                continue;
            }
            order.push(spec.id.clone());
            if let Some(existing) = map.remove(&spec.id) {
                if let Ok(mut slot) = existing.lock() {
                    let transport_changed = slot.spec.address != spec.address
                        || slot.spec.port != spec.port
                        || slot.spec.identity_file != spec.identity_file
                        || slot.spec.remote_socket != spec.remote_socket;
                    slot.spec = spec.clone();
                    if transport_changed {
                        slot.registration = Arc::new(());
                        slot.generation = slot.generation.wrapping_add(1);
                        slot.tunnel = None;
                        slot.failures = 0;
                        slot.next_retry_at = None;
                        slot.status = HostStatus::Connecting;
                    }
                }
                keep.insert(spec.id.clone(), existing);
            } else {
                keep.insert(
                    spec.id.clone(),
                    Arc::new(Mutex::new(RemoteSlot::new(spec.clone()))),
                );
            }
        }
        *map = keep;
    }

    pub fn specs(&self) -> Vec<RemoteHostSpec> {
        let Ok(map) = self.slots.lock() else {
            return Vec::new();
        };
        let Ok(order) = self.order.lock() else {
            return Vec::new();
        };
        let mut specs: Vec<_> = order
            .iter()
            .filter_map(|id| map.get(id))
            .filter_map(|slot| slot.lock().ok().map(|s| s.spec.clone()))
            .collect();
        if let Ok(external) = self.external.lock() {
            specs.extend(external.values().map(|h| h.spec.clone()));
        }
        specs
    }

    /// 등록된 원격 호스트와 현재 연결 상태를 등록 순서대로 돌려준다.
    pub fn statuses(&self) -> Vec<(RemoteHostSpec, HostStatus)> {
        let Ok(map) = self.slots.lock() else {
            return Vec::new();
        };
        let Ok(order) = self.order.lock() else {
            return Vec::new();
        };
        let mut statuses: Vec<_> = order
            .iter()
            .filter_map(|id| map.get(id))
            .filter_map(|slot| {
                let mut s = slot.lock().ok()?;
                if let Some(t) = &mut s.tunnel
                    && !t.is_alive()
                {
                    s.tunnel = None;
                    s.status = HostStatus::Disconnected;
                }
                Some((s.spec.clone(), s.status))
            })
            .collect();
        if let Ok(external) = self.external.lock() {
            statuses.extend(external.values().map(|h| (h.spec.clone(), h.info.status)));
        }
        statuses
    }

    pub fn status(&self, id: &str) -> HostStatus {
        if let Some(info) = self.connection_info(id) {
            return info.status;
        }
        let Some(slot) = self.slot(id) else {
            return HostStatus::Disconnected;
        };
        let Ok(mut s) = slot.lock() else {
            return HostStatus::Disconnected;
        };
        if let Some(t) = &mut s.tunnel
            && !t.is_alive()
        {
            s.tunnel = None;
            s.status = HostStatus::Disconnected;
        }
        s.status
    }

    /// 터널 기동 횟수(단위·통합 테스트에서 호스트당 터널 재사용 검증용).
    pub fn tunnel_starts(&self, id: &str) -> usize {
        self.slot(id)
            .and_then(|slot| slot.lock().ok().map(|s| s.tunnel_starts))
            .unwrap_or(0)
    }

    /// 원격 중계(`krill __bridge`, Windows 원격)로 붙은 호스트가 보고한 OS 이름(`windows` 등).
    /// 소켓 포워딩(POSIX 원격)이거나 터널이 없으면 `None`. 대상 OS에 맞는 명령을 고를 때 쓴다.
    pub fn remote_os(&self, id: &str) -> Option<String> {
        let slot = self.slot(id)?;
        let s = slot.lock().ok()?;
        s.tunnel.as_ref()?.remote_os().map(str::to_owned)
    }

    /// 현재 살아 있는 터널의 자식 PID(테스트에서 터널 강제 종료 검증용).
    pub fn tunnel_pid(&self, id: &str) -> Option<u32> {
        let slot = self.slot(id)?;
        let mut s = slot.lock().ok()?;
        let t = s.tunnel.as_mut()?;
        t.is_alive().then(|| t.pid())
    }

    /// 사용자가 사이드바에서 '다시 연결'을 눌렀을 때 백오프를 즉시 해제하고 재연결 상태로 만든다.
    pub fn reconnect_now(&self, id: &str) {
        if let Ok(mut external) = self.external.lock()
            && let Some(host) = external.get_mut(id)
        {
            host.info.status = HostStatus::Connecting;
            host.info.error = None;
            return;
        }
        let Some(slot) = self.slot(id) else {
            return;
        };
        let Ok(mut s) = slot.lock() else {
            return;
        };
        // 사용자가 직접 누른 재연결은 다른 창이 내린 '꺼진 기기' 판정도 바로 푼다.
        if let Ok(mut offline) = OFFLINE.lock() {
            offline.remove(&self.offline_key(&s.spec));
        }
        // 살아 있는 터널도 내린다: 원격 데몬 재탐색·깨우기는 터널을 새로 만들 때만 돈다(remote_query).
        s.generation = s.generation.wrapping_add(1);
        s.tunnel = None;
        s.next_retry_at = None;
        s.failures = 0;
        s.last_error = None;
        s.suspended = false;
        s.status = HostStatus::Connecting;
    }

    /// 사용자가 사이드바에서 '연결 해제'를 눌렀을 때 터널을 내리고 수동 끊김으로 고정한다.
    /// `reconnect_now`로만 풀리며, 그 전까지 자동 재연결·폴링 재접속을 하지 않는다.
    pub fn disconnect_now(&self, id: &str) {
        if let Ok(mut external) = self.external.lock()
            && let Some(host) = external.get_mut(id)
        {
            host.info.status = HostStatus::Disconnected;
            return;
        }
        let Some(slot) = self.slot(id) else {
            return;
        };
        let Ok(mut s) = slot.lock() else {
            return;
        };
        s.generation = s.generation.wrapping_add(1);
        s.tunnel = None;
        s.next_retry_at = None;
        s.suspended = true;
        s.status = HostStatus::Disconnected;
    }

    /// 지정한 [`HostId`]에 대한 전송 무관 [`HostTarget`]을 돌려준다.
    ///
    /// - `HostId::Local`: `allow_local_launch == true` (로컬 `krill-holder`/`krilld` 기동 허용).
    /// - `HostId::Remote(id)`: `allow_local_launch == false` (절대 로컬 데몬을 기동하지 않으며,
    ///   이 매니저가 관리하는 단일 공유 터널 소켓을 통해 통신).
    pub fn target_for(&self, host: &HostId, local_paths: Option<&Paths>) -> Option<HostTarget> {
        match host {
            HostId::Local => {
                let paths = local_paths.cloned().or_else(|| Paths::dev().ok())?;
                Some(HostTarget::local(paths))
            }
            HostId::Remote(id) => {
                if let Some(host) = self.external.lock().ok()?.get(id).cloned() {
                    return Some(HostTarget::remote_with_connector(
                        id.clone(),
                        host.socket,
                        (host.factory)(),
                    ));
                }
                let slot = self.slot(id)?;
                let (initial_socket, registration) = {
                    let slot = slot.lock().ok()?;
                    (slot.current_socket(), slot.registration.clone())
                };
                let manager = self.clone();
                let host_id = id.clone();
                let connector =
                    RemoteConnector::new(move || manager.ensure_tunnel_socket(&host_id), {
                        let manager = self.clone();
                        let host_id = id.clone();
                        move |ok, err| manager.record_ipc_outcome(&host_id, ok, err)
                    })
                    .with_lifetime(registration.clone(), {
                        let manager = self.clone();
                        let host_id = id.clone();
                        let expected = Arc::downgrade(&slot);
                        move || {
                            let (Some(current), Some(expected)) =
                                (manager.slot(&host_id), expected.upgrade())
                            else {
                                return false;
                            };
                            if !Arc::ptr_eq(&current, &expected) {
                                return false;
                            }
                            current.lock().is_ok_and(|slot| {
                                !slot.suspended && Arc::ptr_eq(&slot.registration, &registration)
                            })
                        }
                    });
                Some(HostTarget::remote_with_connector(
                    id.clone(),
                    initial_socket,
                    connector,
                ))
            }
        }
    }

    /// 같은 기기 중복 등록(`id` → 대표 id)을 알린다. [`Self::sync_specs`]에는 대표 명세만 넘긴다.
    pub fn set_aliases(&self, aliases: BTreeMap<String, String>) {
        if let Ok(mut map) = self.aliases.lock() {
            *map = aliases;
        }
    }

    fn slot(&self, id: &str) -> Option<Arc<Mutex<RemoteSlot>>> {
        let id = self
            .aliases
            .lock()
            .ok()
            .and_then(|a| a.get(id).cloned())
            .unwrap_or_else(|| id.to_owned());
        self.slots.lock().ok()?.get(&id).cloned()
    }

    fn offline_key(&self, spec: &RemoteHostSpec) -> OfflineKey {
        OfflineKey {
            ssh_bin: self.ssh_bin.clone(),
            local_dir: self.local_dir.clone(),
            destination: spec.address.clone(),
            port: spec.port,
            identity_file: spec.identity_file.clone(),
            remote_socket: spec.remote_socket.clone(),
        }
    }

    fn ensure_tunnel_socket(&self, id: &str) -> io::Result<PathBuf> {
        let slot = self.slot(id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::ConnectionRefused, "unknown remote host")
        })?;
        let gate = slot
            .lock()
            .map_err(|_| io::Error::other("remote host slot poisoned"))?
            .connection_gate
            .clone();
        let _connecting = gate
            .lock()
            .map_err(|_| io::Error::other("remote connection gate poisoned"))?;
        let mut s = slot
            .lock()
            .map_err(|_| io::Error::other("remote host slot poisoned"))?;

        if s.suspended {
            s.status = HostStatus::Disconnected;
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "remote host disconnected",
            ));
        }

        if let Some(t) = &mut s.tunnel
            && !t.is_alive()
        {
            s.tunnel = None;
        }

        if let Some(t) = &s.tunnel {
            return Ok(t.socket().to_path_buf());
        }

        if let Some(until) = s.next_retry_at {
            if Instant::now() < until {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    s.last_error
                        .clone()
                        .unwrap_or_else(|| "remote host backoff active".to_owned()),
                ));
            }
            s.next_retry_at = None;
            s.status = HostStatus::Connecting;
        }

        let key = self.offline_key(&s.spec);
        let tried = s.failures > 0;
        if let Some((until, error)) = OFFLINE
            .lock()
            .ok()
            .and_then(|offline| offline.get(&key).cloned())
            .filter(|(until, _)| tried && Instant::now() < *until)
        {
            // 다른 창의 관리자가 방금 꺼진 기기로 판정했다. 이 관리자도 한 번은 직접 시도해 봤으면(새 창은 첫 시도를 한다:
            // 그사이 사용자가 기기를 켰을 수 있다) ssh를 다시 띄우지 않고 그 판정을 따른다.
            s.failures = s.failures.max(OFFLINE_AFTER_FAILURES.saturating_add(1));
            s.next_retry_at = Some(until);
            s.last_error = Some(error.clone());
            s.status = HostStatus::Disconnected;
            return Err(io::Error::new(io::ErrorKind::WouldBlock, error));
        }

        s.status = HostStatus::Connecting;
        let opts = s.spec.to_ssh_options();
        let generation = s.generation;
        drop(s);
        let started = if let (Some(local_dir), Some(ssh_bin)) = (&self.local_dir, &self.ssh_bin) {
            opts.validate()
                .and_then(|()| ssh::resolve_remote_endpoint_with_program(&opts, ssh_bin))
                .and_then(|remote| {
                    let local = ssh::local_socket_in(local_dir, &opts.destination)?;
                    Tunnel::start_endpoint_with_program(&opts, local, &remote, ssh_bin)
                })
        } else {
            Tunnel::start_with_options(&opts)
        };
        let mut s = slot
            .lock()
            .map_err(|_| io::Error::other("remote host slot poisoned"))?;
        if s.generation != generation {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "remote host settings changed while connecting",
            ));
        }
        match started {
            Ok(tunnel) => {
                if let Ok(mut offline) = OFFLINE.lock() {
                    offline.remove(&key);
                }
                let sock = tunnel.socket().to_path_buf();
                s.placeholder_socket.clone_from(&sock);
                s.tunnel = Some(tunnel);
                s.tunnel_starts = s.tunnel_starts.saturating_add(1);
                Ok(sock)
            }
            Err(err) => {
                s.failures = s.failures.saturating_add(1);
                let delay = Self::backoff_delay(s.failures);
                let until = Instant::now() + delay;
                s.next_retry_at = Some(until);
                s.last_error = Some(err.to_string());
                s.status = HostStatus::Disconnected;
                if delay == OFFLINE_BACKOFF
                    && let Ok(mut offline) = OFFLINE.lock()
                {
                    offline.insert(key, (until, err.to_string()));
                }
                // Normalize NotFound so callers (e.g. shell::is_permanent_error) treat remote
                // tunnel setup failure as transient and keep retrying with backoff.
                let kind = if err.kind() == io::ErrorKind::NotFound {
                    io::ErrorKind::ConnectionRefused
                } else {
                    err.kind()
                };
                Err(io::Error::new(kind, err.to_string()))
            }
        }
    }

    /// 활성 터널을 강제 종료하고 상태를 `Disconnected`(백오프 포함)로 전환한다.
    pub fn kill_tunnel(&self, id: &str) {
        let Some(slot) = self.slot(id) else {
            return;
        };
        let Ok(mut s) = slot.lock() else {
            return;
        };
        s.generation = s.generation.wrapping_add(1);
        s.tunnel = None;
        s.failures = s.failures.saturating_add(1);
        let delay = Self::backoff_delay(s.failures);
        s.next_retry_at = Some(Instant::now() + delay);
        s.status = HostStatus::Disconnected;
    }

    fn record_ipc_outcome(&self, id: &str, ok: bool, err: Option<&io::Error>) {
        let Some(slot) = self.slot(id) else {
            return;
        };
        let Ok(mut s) = slot.lock() else {
            return;
        };
        if s.suspended {
            s.status = HostStatus::Disconnected;
            return;
        }
        if ok {
            s.failures = 0;
            s.next_retry_at = None;
            s.last_error = None;
            s.status = HostStatus::Connected;
            return;
        }
        // 이미 ensure_tunnel_socket에서 백오프가 걸린 상태(WouldBlock)라면 실패 횟수를 중복 증가시키지 않는다.
        if err.is_some_and(|e| e.kind() == io::ErrorKind::WouldBlock) {
            s.status = HostStatus::Disconnected;
            return;
        }
        if let Some(t) = &mut s.tunnel
            && !t.is_alive()
        {
            s.tunnel = None;
        }
        // 터널은 살았는데 원격 krilld에 닿지 못했다(원격 소켓 없음·데몬 죽음 → ssh가 채널을 닫음). 터널을 내려야
        // 다음 시도에서 원격 경로를 다시 찾고 데몬을 깨운다. 그대로 두면 앱을 재시작할 때까지 계속 실패했다.
        if err.is_some_and(|e| {
            matches!(
                e.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
            )
        }) {
            s.tunnel = None;
        }
        s.failures = s.failures.saturating_add(1);
        if s.tunnel.is_none() {
            let delay = Self::backoff_delay(s.failures);
            s.next_retry_at = Some(Instant::now() + delay);
        }
        if let Some(e) = err {
            s.last_error = Some(e.to_string());
        }
        s.status = HostStatus::Disconnected;
    }
}

#[cfg(test)]
mod tests;
