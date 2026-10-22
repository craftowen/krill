//! iroh 호스트 등록·상태 수집·수요 기반 connect 자식 소유. 모든 IO는 백그라운드 호출이다.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use crate::i18n::link_ui as text;
use krill_hosts::{ExternalHost, HostConnectionInfo, HostStatus, RemoteConnector, RemoteHostSpec};

type Registry = Mutex<BTreeMap<PathBuf, Weak<LinkHosts>>>;
static SERVICES: OnceLock<Registry> = OnceLock::new();

#[derive(Debug, Default)]
pub struct ConnectionsChanged(u64);
impl gpui::Global for ConnectionsChanged {}

pub fn notify_connections_changed(cx: &mut gpui::App) {
    let generation = cx.try_global::<ConnectionsChanged>().map_or(0, |v| v.0);
    cx.set_global(ConnectionsChanged(generation.wrapping_add(1)));
}

#[derive(Debug)]
pub struct LinkHosts {
    state_dir: PathBuf,
    executable: PathBuf,
    slots: Mutex<BTreeMap<String, Arc<Slot>>>,
    closed: Arc<AtomicBool>,
    repaired: Mutex<BTreeSet<String>>,
    /// 사용자가 사이드바에서 '연결 해제'를 눌러 수동으로 끊은 호스트.
    /// `paired`(재연결)로만 풀리며, 그 전까지 자식 기동·자동 재접속을 하지 않는다.
    suspended: Arc<Mutex<BTreeSet<String>>>,
}

#[derive(Debug)]
struct Slot {
    state_dir: PathBuf,
    executable: PathBuf,
    host: krill_link::RemoteHost,
    identity: Arc<()>,
    lease: Mutex<Weak<Lease>>,
    closed: Arc<AtomicBool>,
    removed: AtomicBool,
    revoked: AtomicBool,
    restart: AtomicBool,
    starting: AtomicBool,
    owned: Mutex<BTreeSet<u32>>,
    suspended: Arc<Mutex<BTreeSet<String>>>,
}

#[derive(Debug)]
struct Lease {
    slot: Arc<Slot>,
    child: Mutex<Option<Child>>,
}

impl LinkHosts {
    /// 같은 상태 디렉터리의 창들이 공유한다. 생성 시 IO나 자식 기동은 없다.
    pub fn shared(state_dir: PathBuf, executable: PathBuf) -> Arc<Self> {
        let registry = SERVICES.get_or_init(|| Mutex::new(BTreeMap::new()));
        if let Ok(mut map) = registry.lock() {
            if let Some(service) = map.get(&state_dir).and_then(Weak::upgrade) {
                return service;
            }
            let service = Arc::new(Self {
                state_dir: state_dir.clone(),
                executable,
                slots: Mutex::new(BTreeMap::new()),
                closed: Arc::new(AtomicBool::new(false)),
                repaired: Mutex::new(BTreeSet::new()),
                suspended: Arc::new(Mutex::new(BTreeSet::new())),
            });
            map.insert(state_dir, Arc::downgrade(&service));
            return service;
        }
        Arc::new(Self {
            state_dir,
            executable,
            slots: Mutex::new(BTreeMap::new()),
            closed: Arc::new(AtomicBool::new(false)),
            repaired: Mutex::new(BTreeSet::new()),
            suspended: Arc::new(Mutex::new(BTreeSet::new())),
        })
    }

    /// 등록과 상태 조회만 수행한다. connect 프로세스를 시작하지 않는다.
    pub fn registrations(&self) -> io::Result<Vec<ExternalHost>> {
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| io::Error::other("link registry poisoned"))?;
        let hosts = krill_link::load_hosts(&self.state_dir).map_err(io::Error::other)?;
        slots.retain(|id, _| hosts.iter().any(|h| &h.id == id));
        let mut result = Vec::new();
        for host in hosts {
            let slot = slots
                .entry(host.id.clone())
                .or_insert_with(|| {
                    Arc::new(Slot {
                        state_dir: self.state_dir.clone(),
                        executable: self.executable.clone(),
                        host: host.clone(),
                        identity: Arc::new(()),
                        lease: Mutex::new(Weak::new()),
                        closed: self.closed.clone(),
                        removed: AtomicBool::new(false),
                        revoked: AtomicBool::new(false),
                        restart: AtomicBool::new(false),
                        starting: AtomicBool::new(false),
                        owned: Mutex::new(BTreeSet::new()),
                        suspended: self.suspended.clone(),
                    })
                })
                .clone();
            let socket = krill_link::host_socket_path(&self.state_dir, &host.id)
                .map_err(io::Error::other)?;
            let suspended = self
                .suspended
                .lock()
                .is_ok_and(|ids| ids.contains(&host.id));
            if suspended
                && let Some(lease) = slot.lease.lock().ok().and_then(|lease| lease.upgrade())
            {
                lease.stop();
            }
            let mut info = connection_info(&self.state_dir, &host.id)?;
            if suspended {
                info.status = HostStatus::Disconnected;
                info.error = None;
            }
            let repaired = self
                .repaired
                .lock()
                .is_ok_and(|mut ids| ids.remove(&host.id));
            if repaired {
                slot.revoked.store(false, Ordering::Release);
                slot.restart.store(true, Ordering::Release);
            } else if info.error.is_some()
                && !slot.restart.load(Ordering::Acquire)
                && !slot.starting.load(Ordering::Acquire)
            {
                slot.revoked.store(true, Ordering::Release);
                if let Some(lease) = slot.lease.lock().ok().and_then(|lease| lease.upgrade()) {
                    lease.stop();
                }
            }
            if repaired
                || slot.restart.load(Ordering::Acquire)
                || slot.starting.load(Ordering::Acquire)
            {
                info.error = None;
                if slot.starting.load(Ordering::Acquire) && info.status == HostStatus::Disconnected
                {
                    info.status = HostStatus::Connecting;
                }
            }
            let spec = RemoteHostSpec {
                id: host_id(&host.id),
                label: host.name,
                address: host.node_id,
                port: None,
                identity_file: None,
                remote_socket: None,
            };
            result.push(ExternalHost::new(spec, socket, info, move || {
                slot.connector()
            }));
        }
        Ok(result)
    }

    pub fn owned_pids(&self) -> Vec<u32> {
        self.slots
            .lock()
            .map(|slots| {
                slots
                    .values()
                    .filter_map(|s| s.lease.lock().ok()?.upgrade())
                    .filter_map(|lease| lease.child.lock().ok()?.as_ref().map(Child::id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 사용자가 사이드바에서 '연결 해제'를 눌렀을 때 자식을 내리고 수동 끊김으로 고정한다.
    /// `paired`(재연결)로만 풀리며, 그 전까지 자식 기동·자동 재접속을 하지 않는다.
    pub fn disconnect(&self, id: &str) {
        if let Ok(mut ids) = self.suspended.lock() {
            ids.insert(id.to_owned());
        }
        let slot = self
            .slots
            .lock()
            .ok()
            .and_then(|slots| slots.get(id).cloned());
        if let Some(slot) = slot
            && let Some(lease) = slot.lease.lock().ok().and_then(|lease| lease.upgrade())
        {
            lease.stop();
        }
    }

    /// 새 초대로 페어링한 경우 이전 권한 해제 상태의 연결만 새로 시작한다.
    pub fn paired(&self, id: &str) {
        if let Ok(mut ids) = self.suspended.lock() {
            ids.remove(id);
        }
        if let Ok(mut repaired) = self.repaired.lock() {
            repaired.insert(id.to_owned());
        }
        let slot = self
            .slots
            .lock()
            .ok()
            .and_then(|slots| slots.get(id).cloned());
        if let Some(slot) = slot {
            slot.revoked.store(false, Ordering::Release);
            slot.restart.store(true, Ordering::Release);
            if let Some(lease) = slot.lease.lock().ok().and_then(|lease| lease.upgrade()) {
                lease.stop();
            }
        }
    }

    /// 저장된 원격 연결과 앱이 소유한 터널만 정리한다. 원격 세션에는 종료를 보내지 않는다.
    pub fn disconnect_host(&self, id: &str) -> io::Result<()> {
        let slot = {
            let mut slots = self
                .slots
                .lock()
                .map_err(|_| io::Error::other("link registry poisoned"))?;
            match krill_link::remove_host(&self.state_dir, id) {
                Ok(_) | Err(krill_link::Error::HostNotFound(_)) => {}
                Err(err) => return Err(io::Error::other(err)),
            }
            let slot = slots.remove(id);
            if let Some(slot) = &slot {
                // 이전 창/작업이 보유한 connector도 다시 연결을 시작하지 못한다.
                slot.removed.store(true, Ordering::Release);
            }
            if let Ok(mut repaired) = self.repaired.lock() {
                repaired.remove(id);
            }
            slot
        };
        if let Some(slot) = slot
            && let Some(lease) = slot.lease.lock().ok().and_then(|lease| lease.upgrade())
        {
            lease.stop();
        }
        Ok(())
    }

    /// 앱 종료 훅의 백그라운드 작업에서 호출한다.
    pub fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        if let Ok(slots) = self.slots.lock() {
            for slot in slots.values() {
                if let Some(lease) = slot.lease.lock().ok().and_then(|lease| lease.upgrade()) {
                    lease.stop();
                }
            }
        }
    }
}

/// 종료를 먼저 고정해 IO 태스크의 재기동을 막는다. 실제 자식 정리는 백그라운드에서 한다.
pub fn begin_shutdown() -> Vec<Arc<LinkHosts>> {
    let services: Vec<_> = SERVICES
        .get()
        .and_then(|map| map.lock().ok())
        .map(|map| map.values().filter_map(Weak::upgrade).collect())
        .unwrap_or_default();
    for service in &services {
        service.closed.store(true, Ordering::Release);
    }
    services
}

pub fn host_id(id: &str) -> String {
    format!("iroh:{id}")
}

/// 번들(`Contents/Helpers/krill-link`) 또는 개발 빌드(`target/.../krill-link`) 경로를 찾는다.
pub fn executable() -> PathBuf {
    krill_proto::daemon::sibling_exe("krill-link").unwrap_or_else(|_| PathBuf::from("krill-link"))
}

impl Slot {
    fn connector(self: &Arc<Self>) -> RemoteConnector {
        let lease = if let Ok(mut cached) = self.lease.lock() {
            if let Some(lease) = cached.upgrade() {
                lease
            } else {
                let lease = Arc::new(Lease {
                    slot: self.clone(),
                    child: Mutex::new(None),
                });
                *cached = Arc::downgrade(&lease);
                lease
            }
        } else {
            Arc::new(Lease {
                slot: self.clone(),
                child: Mutex::new(None),
            })
        };
        let connect = lease.clone();
        let errors = lease;
        let lifetime = self.clone();
        RemoteConnector::new(move || connect.ensure(), |_, _| {})
            .with_error_mapper(move |err| errors.user_error(&err))
            .with_lifetime(self.identity.clone(), move || {
                !lifetime.removed.load(Ordering::Acquire)
                    && !lifetime.closed.load(Ordering::Acquire)
            })
    }
}

impl Lease {
    fn check_available(&self) -> io::Result<()> {
        if self.slot.closed.load(Ordering::Acquire) || self.slot.removed.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(())
    }

    fn user_error(&self, err: &io::Error) -> io::Error {
        if connection_info(&self.slot.state_dir, &self.slot.host.id)
            .ok()
            .and_then(|s| s.error)
            .is_some()
        {
            return io::Error::new(io::ErrorKind::PermissionDenied, text::ERR_REVOKED.get());
        }
        io::Error::new(
            if err.kind() == io::ErrorKind::PermissionDenied {
                io::ErrorKind::ConnectionRefused
            } else {
                err.kind()
            },
            text::ERR_UNAVAILABLE.get(),
        )
    }

    fn ensure(&self) -> io::Result<PathBuf> {
        self.check_available()?;
        if self
            .slot
            .suspended
            .lock()
            .is_ok_and(|ids| ids.contains(&self.slot.host.id))
        {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                text::ERR_UNAVAILABLE.get(),
            ));
        }
        let socket = krill_link::host_socket_path(&self.slot.state_dir, &self.slot.host.id)
            .map_err(io::Error::other)?;
        let status = krill_link::host_status(&self.slot.state_dir, &self.slot.host.id)
            .map_err(io::Error::other)?;
        let restart = self.slot.restart.swap(false, Ordering::AcqRel);
        if !restart && self.slot.revoked.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                text::ERR_REVOKED.get(),
            ));
        }
        let mut child = self
            .child
            .lock()
            .map_err(|_| io::Error::other(text::ERR_UNAVAILABLE.get()))?;
        self.check_available()?;
        if restart && let Some(mut process) = child.take() {
            let pid = process.id();
            process.kill().ok();
            process.wait().ok();
            if let Ok(mut pids) = self.slot.owned.lock() {
                pids.remove(&pid);
            }
        }
        if let Some(process) = child.as_mut() {
            if process.try_wait()?.is_none() && socket.exists() {
                return Ok(socket);
            }
            if process.try_wait()?.is_some() {
                if let Ok(mut pids) = self.slot.owned.lock() {
                    pids.remove(&process.id());
                }
                *child = None;
            }
        }
        // 이미 실행 중인 외부 CLI 연결은 빌려 쓰되 소유하거나 종료하지 않는다.
        let owned = status
            .pid
            .is_some_and(|pid| self.slot.owned.lock().is_ok_and(|pids| pids.contains(&pid)));
        if child.is_none() && status.pid.is_some() && !owned {
            return Ok(socket);
        }
        if child.is_none() {
            let process = crate::platform::background_command(&self.slot.executable)
                .arg("--state-dir")
                .arg(&self.slot.state_dir)
                .args(["connect", &self.slot.host.id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|err| self.user_error(&err))?;
            if let Ok(mut pids) = self.slot.owned.lock() {
                pids.insert(process.id());
            }
            *child = Some(process);
        }
        self.slot.starting.store(true, Ordering::Release);
        let _starting = Starting(&self.slot.starting);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.check_available()?;
            if let Some(process) = child.as_mut()
                && process.try_wait()?.is_some()
            {
                if let Ok(mut pids) = self.slot.owned.lock() {
                    pids.remove(&process.id());
                }
                *child = None;
                return Err(self.user_error(&io::Error::from(io::ErrorKind::ConnectionRefused)));
            }
            if socket.exists()
                && krill_link::host_status(&self.slot.state_dir, &self.slot.host.id)
                    .is_ok_and(|status| status.pid == child.as_ref().map(Child::id))
            {
                return Ok(socket);
            }
            if Instant::now() >= deadline {
                return Err(self.user_error(&io::Error::from(io::ErrorKind::TimedOut)));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn stop(&self) {
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut child) = guard.take()
        {
            let pid = child.id();
            child.kill().ok();
            child.wait().ok();
            if let Ok(mut pids) = self.slot.owned.lock() {
                pids.remove(&pid);
            }
        }
    }
}

struct Starting<'a>(&'a AtomicBool);
impl Drop for Starting<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // 앱이 생성한 자식만 정리한다. 상태 파일의 PID는 프로세스 소유권 증거가 아니다.
        let child = self
            .child
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(mut child) = child {
            let pid = child.id();
            let slot = self.slot.clone();
            std::thread::spawn(move || {
                child.kill().ok();
                child.wait().ok();
                if let Ok(mut pids) = slot.owned.lock() {
                    pids.remove(&pid);
                }
            });
        }
    }
}

/// 공개 상태의 선택적 오류 분류를 읽고, 없는 버전에서는 명시적인 거부 사유만 대체 판별한다.
pub fn connection_info(state_dir: &Path, id: &str) -> io::Result<HostConnectionInfo> {
    let status = krill_link::host_status(state_dir, id).map_err(io::Error::other)?;
    let serialized = serde_json::to_value(&status).map_err(io::Error::other)?;
    let kind = serialized
        .get("last_error_kind")
        .and_then(serde_json::Value::as_str);
    let revoked = kind == Some("unauthorized")
        || (kind.is_none()
            && status
                .last_error
                .as_deref()
                .is_some_and(|s| s.contains("unauthorized device")));
    let route = match status.route {
        krill_link::RouteKind::Direct => Some(text::ROUTE_DIRECT.get()),
        krill_link::RouteKind::Relay => Some(text::ROUTE_RELAY.get()),
        krill_link::RouteKind::None => None,
    };
    // RTT(응답 xxms)를 표시하면 링크 폴링(1s)마다 detail이 바뀌어
    // 사이드바가 계속 다시 그려져 깜빡인다. 경로는 유지하고 속도는 표시하지 않는다.
    let detail = route.map(ToOwned::to_owned);
    Ok(HostConnectionInfo {
        status: if revoked {
            HostStatus::Disconnected
        } else {
            match status.state {
                krill_link::TunnelState::Connected => HostStatus::Connected,
                krill_link::TunnelState::Connecting => HostStatus::Connecting,
                krill_link::TunnelState::Disconnected => HostStatus::Disconnected,
            }
        },
        detail,
        error: revoked.then(|| text::ERR_REVOKED.get().to_owned()),
    })
}
