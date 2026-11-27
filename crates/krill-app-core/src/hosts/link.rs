//! iroh 호스트 등록·상태 수집·수요 기반 connect 자식 소유. 모든 IO는 백그라운드 호출이다.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    children: Arc<OwnedChildren>,
    closed: Arc<AtomicBool>,
    repaired: Mutex<BTreeSet<String>>,
    paired_at: Mutex<BTreeMap<String, u64>>,
    /// 사용자가 사이드바에서 '연결 해제'를 눌러 수동으로 끊은 호스트.
    /// `paired`(재연결)로만 풀리며, 그 전까지 자식 기동·자동 재접속을 하지 않는다.
    suspended: Arc<Mutex<BTreeSet<String>>>,
}

#[derive(Debug, Default)]
struct OwnedChildren {
    slots: Mutex<Vec<Weak<Slot>>>,
    pending: Mutex<usize>,
    changed: Condvar,
}

impl OwnedChildren {
    fn wait(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *pending != 0 {
            pending = self
                .changed
                .wait(pending)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

#[derive(Debug)]
struct Slot {
    state_dir: PathBuf,
    executable: PathBuf,
    host: krill_link_core::RemoteHost,
    identity: Arc<()>,
    lease: Mutex<Weak<Lease>>,
    closed: Arc<AtomicBool>,
    removed: AtomicBool,
    revoked: AtomicBool,
    restart: AtomicBool,
    starting: AtomicUsize,
    paired_at_ms: AtomicU64,
    owned: Mutex<BTreeSet<u32>>,
    children: Arc<OwnedChildren>,
    suspended: Arc<Mutex<BTreeSet<String>>>,
}

#[derive(Debug)]
struct Lease {
    slot: Arc<Slot>,
    child: Mutex<Option<Child>>,
    generation: AtomicU64,
    child_generation: AtomicU64,
    stop_pending: AtomicBool,
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
                children: Arc::new(OwnedChildren::default()),
                closed: Arc::new(AtomicBool::new(false)),
                repaired: Mutex::new(BTreeSet::new()),
                paired_at: Mutex::new(BTreeMap::new()),
                suspended: Arc::new(Mutex::new(BTreeSet::new())),
            });
            map.insert(state_dir, Arc::downgrade(&service));
            return service;
        }
        Arc::new(Self {
            state_dir,
            executable,
            slots: Mutex::new(BTreeMap::new()),
            children: Arc::new(OwnedChildren::default()),
            closed: Arc::new(AtomicBool::new(false)),
            repaired: Mutex::new(BTreeSet::new()),
            paired_at: Mutex::new(BTreeMap::new()),
            suspended: Arc::new(Mutex::new(BTreeSet::new())),
        })
    }

    /// 등록과 상태 조회만 수행한다. connect 프로세스를 시작하지 않는다.
    pub fn registrations(&self) -> io::Result<Vec<ExternalHost>> {
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| io::Error::other("link registry poisoned"))?;
        let hosts = krill_link_core::load_hosts(&self.state_dir).map_err(io::Error::other)?;
        slots.retain(|id, _| hosts.iter().any(|h| &h.id == id));
        let mut result = Vec::new();
        for host in hosts {
            let slot = slots
                .entry(host.id.clone())
                .or_insert_with(|| {
                    let paired_at = self
                        .paired_at
                        .lock()
                        .ok()
                        .and_then(|times| times.get(&host.id).copied())
                        .unwrap_or(0);
                    let slot = Arc::new(Slot {
                        state_dir: self.state_dir.clone(),
                        executable: self.executable.clone(),
                        host: host.clone(),
                        identity: Arc::new(()),
                        lease: Mutex::new(Weak::new()),
                        closed: self.closed.clone(),
                        removed: AtomicBool::new(false),
                        revoked: AtomicBool::new(false),
                        restart: AtomicBool::new(false),
                        starting: AtomicUsize::new(0),
                        paired_at_ms: AtomicU64::new(paired_at),
                        owned: Mutex::new(BTreeSet::new()),
                        children: self.children.clone(),
                        suspended: self.suspended.clone(),
                    });
                    let mut tracked = self
                        .children
                        .slots
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    tracked.retain(|slot| slot.strong_count() > 0);
                    tracked.push(Arc::downgrade(&slot));
                    slot
                })
                .clone();
            let socket = krill_link_core::host_socket_path(&self.state_dir, &host.id)
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
            let mut info = connection_info_since(
                &self.state_dir,
                &host.id,
                slot.paired_at_ms.load(Ordering::Acquire),
            )?;
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
                && slot.starting.load(Ordering::Acquire) == 0
            {
                slot.revoked.store(true, Ordering::Release);
                if let Some(lease) = slot.lease.lock().ok().and_then(|lease| lease.upgrade()) {
                    lease.stop();
                }
            }
            let starting = slot.starting.load(Ordering::Acquire) > 0;
            if repaired || slot.restart.load(Ordering::Acquire) || starting {
                info.error = None;
                if starting && info.status == HostStatus::Disconnected {
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
                    .flat_map(|s| {
                        s.owned
                            .lock()
                            .map(|pids| pids.iter().copied().collect::<Vec<_>>())
                            .unwrap_or_default()
                    })
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
        let paired_at = now_unix_ms();
        if let Ok(mut ids) = self.suspended.lock() {
            ids.remove(id);
        }
        if let Ok(mut times) = self.paired_at.lock() {
            times.insert(id.to_owned(), paired_at);
        }
        if let Ok(mut repaired) = self.repaired.lock() {
            repaired.insert(id.to_owned());
        }
        clear_status_file(&self.state_dir, id);
        let slot = self
            .slots
            .lock()
            .ok()
            .and_then(|slots| slots.get(id).cloned());
        if let Some(slot) = slot {
            slot.paired_at_ms.store(paired_at, Ordering::Release);
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
            match krill_link_core::remove_host(&self.state_dir, id) {
                Ok(_) | Err(krill_link_core::Error::HostNotFound(_)) => {}
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
            if let Ok(mut times) = self.paired_at.lock() {
                times.remove(id);
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

    fn take_owned_children_for_shutdown(&self, out: &mut Vec<(Arc<Slot>, Child)>) {
        self.closed.store(true, Ordering::Release);
        // A connector can keep a removed registration alive. Track weak slots
        // independently of the visible registry so every owned lease is stopped.
        let slots: Vec<_> = self
            .children
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for slot in slots {
            let lease = slot.lease.lock().ok().and_then(|lease| lease.upgrade());
            if let Some(lease) = lease {
                lease.generation.fetch_add(1, Ordering::AcqRel);
                if let Some(child) = lease
                    .child
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    out.push((Arc::clone(&lease.slot), child));
                }
            }
            // A Lease may already be dropping, or a detached stop worker may
            // have taken its Child. Ownership ends only after wait(), not take().
        }
    }

    /// 종료가 시작되었는지 돌려준다.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// 앱 종료 훅의 백그라운드 작업에서 호출한다.
    pub fn shutdown(&self) {
        let mut children = Vec::new();
        self.take_owned_children_for_shutdown(&mut children);
        terminate_tunnel_children(&mut children, SHUTDOWN_TERM_GRACE);
        for (slot, child) in children {
            slot.forget_child(child.id());
        }
        self.children.wait();
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

/// 여러 [`LinkHosts`] 인스턴스의 터널 자식을 한 번의 병렬 종료 단계로 정리한다.
/// GPUI `app_will_quit`의 200ms 예산 안에서도 직렬 300ms×N 대기로 타임아웃되지 않게 한다.
pub fn shutdown_services(services: &[Arc<LinkHosts>]) {
    let mut children = Vec::new();
    for service in services {
        service.take_owned_children_for_shutdown(&mut children);
    }
    terminate_tunnel_children(&mut children, SHUTDOWN_TERM_GRACE);
    for (slot, child) in children {
        slot.forget_child(child.id());
    }
    for service in services {
        service.children.wait();
    }
}

pub fn host_id(id: &str) -> String {
    format!("iroh:{id}")
}

/// 번들(`Contents/Helpers/krill-link`) 또는 개발 빌드(`target/.../krill-link`) 경로를 찾는다.
pub fn executable() -> PathBuf {
    krill_proto::daemon::sibling_exe("krill-link").unwrap_or_else(|_| PathBuf::from("krill-link"))
}

impl Slot {
    fn forget_child(&self, pid: u32) {
        self.owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&pid);
        let mut pending = self
            .children
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *pending = pending.saturating_sub(1);
        self.children.changed.notify_all();
    }

    fn connector(self: &Arc<Self>) -> RemoteConnector {
        let lease = if let Ok(mut cached) = self.lease.lock() {
            if let Some(lease) = cached.upgrade() {
                lease
            } else {
                let lease = Arc::new(Lease {
                    slot: self.clone(),
                    child: Mutex::new(None),
                    generation: AtomicU64::new(0),
                    child_generation: AtomicU64::new(0),
                    stop_pending: AtomicBool::new(false),
                });
                *cached = Arc::downgrade(&lease);
                lease
            }
        } else {
            Arc::new(Lease {
                slot: self.clone(),
                child: Mutex::new(None),
                generation: AtomicU64::new(0),
                child_generation: AtomicU64::new(0),
                stop_pending: AtomicBool::new(false),
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

    #[cfg(test)]
    fn stop_blocking(&self) {
        let child = self.child.lock().ok().and_then(|mut guard| guard.take());
        if let Some(mut child) = child {
            let pid = child.id();
            terminate_tunnel_child(&mut child);
            self.slot.forget_child(pid);
        }
    }

    fn user_error(&self, err: &io::Error) -> io::Error {
        if connection_info_since(
            &self.slot.state_dir,
            &self.slot.host.id,
            self.slot.paired_at_ms.load(Ordering::Acquire),
        )
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
        let generation = self.generation.load(Ordering::Acquire);
        if self.slot.closed.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
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
        let socket = krill_link_core::host_socket_path(&self.slot.state_dir, &self.slot.host.id)
            .map_err(io::Error::other)?;
        let _starting = Starting::enter(&self.slot.starting);
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
        if self.slot.closed.load(Ordering::Acquire)
            || self.generation.load(Ordering::Acquire) != generation
        {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if (restart || self.child_generation.load(Ordering::Acquire) != generation)
            && let Some(mut process) = child.take()
        {
            let pid = process.id();
            terminate_tunnel_child(&mut process);
            self.slot.forget_child(pid);
            clear_status_file(&self.slot.state_dir, &self.slot.host.id);
        }
        if let Some(process) = child.as_mut() {
            if process.try_wait()?.is_none() && socket.exists() {
                return Ok(socket);
            }
            if process.try_wait()?.is_some() {
                self.slot.forget_child(process.id());
                *child = None;
            }
        }
        let status = krill_link_core::host_status(&self.slot.state_dir, &self.slot.host.id)
            .map_err(io::Error::other)?;
        let paired_at = self.slot.paired_at_ms.load(Ordering::Acquire);
        // 이미 실행 중인 외부 CLI 연결은 같은/신규 빌드일 때만 빌려 쓴다.
        // 앱 업그레이드 뒤 고아로 남은 구버전(또는 build 필드가 없는 구버전) `krill-link connect`는
        // 안전하게 종료한 뒤 현재 번들의 바이너리로 교체 재시작한다.
        let owned = status
            .pid
            .is_some_and(|pid| self.slot.owned.lock().is_ok_and(|pids| pids.contains(&pid)));
        if child.is_none() && !owned && (paired_at == 0 || status.updated_at_ms >= paired_at) {
            if status.pid.is_some() {
                if krill_link_core::service::is_stale_build(&status.build) {
                    let _ = stop_stale_connect_process(
                        &self.slot.state_dir,
                        &self.slot.host.id,
                        &self.slot.executable,
                        &status,
                    );
                } else {
                    return Ok(socket);
                }
            } else if let Some(raw) = read_raw_host_status(&self.slot.state_dir, &self.slot.host.id)
                && raw.pid.is_some()
                && krill_link_core::service::is_stale_build(&raw.build)
            {
                let _ = stop_stale_connect_process(
                    &self.slot.state_dir,
                    &self.slot.host.id,
                    &self.slot.executable,
                    &raw,
                );
            }
        }
        if restart {
            clear_status_file(&self.slot.state_dir, &self.slot.host.id);
        }
        if child.is_none() {
            // Serialize admission with the shutdown barrier, including leases
            // whose slot was removed while preparation was running.
            let mut owners = self
                .slot
                .children
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.slot.closed.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let process = crate::platform::background_command(&self.slot.executable)
                .arg("--state-dir")
                .arg(&self.slot.state_dir)
                .args(["connect", &self.slot.host.id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|err| self.user_error(&err))?;
            self.slot
                .owned
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(process.id());
            *owners += 1;
            drop(owners);
            *child = Some(process);
            self.child_generation.store(generation, Ordering::Release);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.slot.closed.load(Ordering::Acquire)
                || self.generation.load(Ordering::Acquire) != generation
            {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if let Some(process) = child.as_mut()
                && process.try_wait()?.is_some()
            {
                self.slot.forget_child(process.id());
                *child = None;
                return Err(self.user_error(&io::Error::from(io::ErrorKind::ConnectionRefused)));
            }
            if socket.exists()
                && krill_link_core::host_status(&self.slot.state_dir, &self.slot.host.id)
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

    fn stop(self: &Arc<Self>) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if self.stop_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let lease = Arc::clone(self);
        if std::thread::Builder::new()
            .name("link-stop".into())
            .spawn(move || {
                let child = lease.child.lock().ok().and_then(|mut guard| {
                    lease.stop_pending.store(false, Ordering::SeqCst);
                    // A reconnect may already have installed a new child. An old
                    // stop request must never reap that generation.
                    (lease.child_generation.load(Ordering::Acquire)
                        != lease.generation.load(Ordering::SeqCst))
                    .then(|| guard.take())
                    .flatten()
                });
                if let Some(mut child) = child {
                    let pid = child.id();
                    terminate_tunnel_child(&mut child);
                    lease.slot.forget_child(pid);
                }
            })
            .is_err()
        {
            self.stop_pending.store(false, Ordering::SeqCst);
            tracing::warn!("could not start link child cleanup");
        }
    }
}

struct Starting<'a>(&'a AtomicUsize);
impl<'a> Starting<'a> {
    fn enter(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}
impl Drop for Starting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
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
        if let Some(child) = child {
            let process = Arc::new(Mutex::new(Some(child)));
            let pending = Arc::clone(&process);
            let slot = self.slot.clone();
            if std::thread::Builder::new()
                .name("link-drop".into())
                .spawn(move || {
                    reap_child(&slot, &pending);
                })
                .is_err()
            {
                // Exceptional OS thread exhaustion must not orphan an owned child.
                reap_child(&self.slot, &process);
            }
        }
    }
}

/// App shutdown grace before SIGKILL: stays under GPUI's 200ms `SHUTDOWN_TIMEOUT`
/// even when quit bypasses `quit_now` (e.g., OS Dock Quit).
const SHUTDOWN_TERM_GRACE: Duration = Duration::from_millis(100);

fn terminate_tunnel_children(children: &mut [(Arc<Slot>, Child)], grace: Duration) {
    if children.is_empty() {
        return;
    }
    #[cfg(unix)]
    {
        let mut live_pids = Vec::new();
        for (_, child) in children.iter_mut() {
            if child.try_wait().ok().flatten().is_none() {
                live_pids.push(child.id().to_string());
            }
        }
        if live_pids.is_empty() {
            return;
        }
        let mut cmd = crate::platform::background_command("/bin/kill");
        cmd.arg("-TERM");
        for pid in &live_pids {
            cmd.arg(pid);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if children
                .iter_mut()
                .all(|(_, child)| child.try_wait().ok().flatten().is_some())
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[cfg(not(unix))]
    let _ = grace;
    for (_, child) in children.iter_mut() {
        if child.try_wait().ok().flatten().is_none() {
            child.kill().ok();
            child.wait().ok();
        }
    }
}

fn terminate_tunnel_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    #[cfg(unix)]
    {
        crate::platform::background_command("/bin/kill")
            .args(["-TERM", &child.id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }
    child.kill().ok();
    child.wait().ok();
}

fn reap_child(slot: &Slot, pending: &Mutex<Option<Child>>) {
    if let Some(mut child) = pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        let pid = child.id();
        terminate_tunnel_child(&mut child);
        slot.forget_child(pid);
    }
}

fn now_unix_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    )
    .unwrap_or(u64::MAX)
}

fn clear_status_file(state_dir: &Path, id: &str) {
    if let Ok(path) = krill_link_core::host_status_path(state_dir, id) {
        std::fs::remove_file(path).ok();
    }
}

fn process_matches_krill_link_connect(
    process: &sysinfo::Process,
    host_id: &str,
    expected_executable: &Path,
) -> bool {
    let cmd: Vec<String> = process
        .cmd()
        .iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect();
    let has_connect_args =
        cmd.iter().any(|arg| arg == "connect") && cmd.iter().any(|arg| arg == host_id);
    if !has_connect_args {
        return false;
    }
    let exe_stem = process
        .exe()
        .and_then(Path::file_stem)
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if exe_stem.eq_ignore_ascii_case("krill-link") {
        return true;
    }
    let expected_stem = expected_executable
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if !expected_stem.is_empty() && exe_stem.eq_ignore_ascii_case(expected_stem) {
        return true;
    }
    let expected_name = expected_executable
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    !expected_name.is_empty()
        && cmd.iter().any(|arg| {
            Path::new(arg)
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(expected_name))
        })
}

fn is_pid_alive(pid: u32) -> bool {
    let spid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[spid]),
        sysinfo::ProcessRefreshKind::new(),
    );
    sys.process(spid).is_some()
}

fn stale_cleanup_lock(state_dir: &Path, host_id: &str) -> Arc<Mutex<()>> {
    type StaleLockMap = BTreeMap<(PathBuf, String), Arc<Mutex<()>>>;
    static LOCKS: OnceLock<Mutex<StaleLockMap>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let key = (state_dir.to_path_buf(), host_id.to_owned());
    let mut guard = map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(guard.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))))
}

fn clear_status_file_if_pid(state_dir: &Path, host_id: &str, expected_pid: u32) {
    if let Some(current) = read_raw_host_status(state_dir, host_id)
        && current.pid == Some(expected_pid)
    {
        clear_status_file(state_dir, host_id);
    }
}

/// 구버전(또는 `build` 필드가 없는) `krill-link connect <host_id>` 프로세스를
/// 실행 파일·인자 일치 검증 후 안전하게 종료해 새 바이너리로 교체할 수 있게 한다.
fn stop_stale_connect_process(
    state_dir: &Path,
    host_id: &str,
    executable: &Path,
    status: &krill_link_core::HostTunnelStatus,
) -> bool {
    let Some(pid) = status.pid else {
        return false;
    };
    if pid == std::process::id() || !krill_link_core::service::is_stale_build(&status.build) {
        return false;
    }
    let host_lock = stale_cleanup_lock(state_dir, host_id);
    let _guard = host_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Re-check the on-disk status after acquiring the per-host lock so concurrent callers
    // never double-signal a stale PID or delete a freshly spawned replacement's status file.
    let Some(current) = read_raw_host_status(state_dir, host_id) else {
        return false;
    };
    if current.pid != Some(pid) || !krill_link_core::service::is_stale_build(&current.build) {
        return false;
    }
    let spid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[spid]),
        sysinfo::ProcessRefreshKind::new()
            .with_exe(sysinfo::UpdateKind::Always)
            .with_cmd(sysinfo::UpdateKind::Always),
    );
    let Some(process) = sys.process(spid) else {
        clear_status_file_if_pid(state_dir, host_id, pid);
        return false;
    };
    if !process_matches_krill_link_connect(process, host_id, executable) {
        tracing::warn!(
            host_id,
            pid,
            "skipping stale krill-link connect cleanup because PID identity did not match"
        );
        return false;
    }
    tracing::info!(
        host_id,
        pid,
        running_build = %current.build,
        current_build = krill_proto::build_id(),
        "stopping stale krill-link connect process for upgrade replacement"
    );
    #[cfg(unix)]
    {
        crate::platform::background_command("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline && is_pid_alive(pid) {
            std::thread::sleep(Duration::from_millis(20));
        }
        if is_pid_alive(pid) {
            crate::platform::background_command("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok();
            let kill_deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < kill_deadline && is_pid_alive(pid) {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = process.kill();
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline && is_pid_alive(pid) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    clear_status_file_if_pid(state_dir, host_id, pid);
    true
}

fn read_raw_host_status(
    state_dir: &Path,
    host_id: &str,
) -> Option<krill_link_core::HostTunnelStatus> {
    let path = krill_link_core::host_status_path(state_dir, host_id).ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 등록된 모든 원격 호스트에 대해 구버전 `krill-link connect` 백그라운드 프로세스를 정리한다.
/// 앱 기동·업그레이드 직후 호출해 macOS/Windows 모두에서 구버전 바이너리 잔존 및 `.prev` 잠금을 막는다.
pub fn stop_stale_connect_processes(state_dir: &Path, executable: &Path) -> usize {
    let Ok(hosts) = krill_link_core::load_hosts(state_dir) else {
        return 0;
    };
    let mut stopped = 0usize;
    for host in hosts {
        if let Some(status) = read_raw_host_status(state_dir, &host.id)
            && status.pid.is_some()
            && krill_link_core::service::is_stale_build(&status.build)
            && stop_stale_connect_process(state_dir, &host.id, executable, &status)
        {
            stopped += 1;
        }
    }
    stopped
}

/// 공개 상태의 선택적 오류 분류를 읽고, 없는 버전에서는 명시적인 거부 사유만 대체 판별한다.
pub fn connection_info(state_dir: &Path, id: &str) -> io::Result<HostConnectionInfo> {
    connection_info_since(state_dir, id, 0)
}

fn connection_info_since(
    state_dir: &Path,
    id: &str,
    min_updated_at_ms: u64,
) -> io::Result<HostConnectionInfo> {
    let status = krill_link_core::host_status(state_dir, id).map_err(io::Error::other)?;
    if min_updated_at_ms > 0 && status.updated_at_ms < min_updated_at_ms {
        return Ok(HostConnectionInfo {
            status: HostStatus::Disconnected,
            detail: None,
            error: None,
        });
    }
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
        krill_link_core::RouteKind::Direct => Some(text::ROUTE_DIRECT.get()),
        krill_link_core::RouteKind::Relay => Some(text::ROUTE_RELAY.get()),
        krill_link_core::RouteKind::None => None,
    };
    // RTT(응답 xxms)를 표시하면 링크 폴링(1s)마다 detail이 바뀌어
    // 사이드바가 계속 다시 그려져 깜빡인다. 경로는 유지하고 속도는 표시하지 않는다.
    let detail = route.map(ToOwned::to_owned);
    Ok(HostConnectionInfo {
        status: if revoked {
            HostStatus::Disconnected
        } else {
            match status.state {
                krill_link_core::TunnelState::Connected => HostStatus::Connected,
                krill_link_core::TunnelState::Connecting => HostStatus::Connecting,
                krill_link_core::TunnelState::Disconnected => HostStatus::Disconnected,
            }
        },
        detail,
        error: revoked.then(|| text::ERR_REVOKED.get().to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease() -> Arc<Lease> {
        let slot = Arc::new(Slot {
            state_dir: PathBuf::new(),
            executable: PathBuf::new(),
            host: krill_link_core::RemoteHost {
                id: "fixture".into(),
                name: "Fixture".into(),
                node_id: String::new(),
                relay_url: None,
                addr_hints: Vec::new(),
                added_at: 0,
            },
            identity: Arc::new(()),
            removed: AtomicBool::new(false),
            lease: Mutex::new(Weak::new()),
            closed: Arc::new(AtomicBool::new(false)),
            revoked: AtomicBool::new(false),
            restart: AtomicBool::new(false),
            starting: AtomicUsize::new(0),
            paired_at_ms: AtomicU64::new(0),
            owned: Mutex::new(BTreeSet::new()),
            children: Arc::new(OwnedChildren::default()),
            suspended: Arc::new(Mutex::new(BTreeSet::new())),
        });
        slot.children
            .slots
            .lock()
            .unwrap()
            .push(Arc::downgrade(&slot));
        let lease = Arc::new(Lease {
            slot: Arc::clone(&slot),
            child: Mutex::new(None),
            generation: AtomicU64::new(0),
            child_generation: AtomicU64::new(0),
            stop_pending: AtomicBool::new(false),
        });
        *slot.lease.lock().unwrap() = Arc::downgrade(&lease);
        lease
    }

    #[test]
    fn stop_returns_while_connection_preparation_holds_child_lock() {
        let lease = lease();
        let held = lease.child.lock().unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        let request = Arc::clone(&lease);
        let caller = std::thread::spawn(move || {
            request.stop();
            sent.send(()).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_secs(1)).is_ok());
        assert_eq!(lease.generation.load(Ordering::Acquire), 1);
        drop(held);
        caller.join().unwrap();
        lease.stop_blocking();
    }

    #[test]
    fn shutdown_waits_for_detached_cleanup_even_after_slot_removal() {
        let lease = lease();
        let slot = Arc::clone(&lease.slot);
        slot.owned.lock().unwrap().insert(42);
        *slot.children.pending.lock().unwrap() = 1;
        let service = Arc::new(LinkHosts {
            state_dir: PathBuf::new(),
            executable: PathBuf::new(),
            slots: Mutex::new(BTreeMap::new()),
            children: Arc::clone(&slot.children),
            closed: Arc::clone(&slot.closed),
            repaired: Mutex::new(BTreeSet::new()),
            paired_at: Mutex::new(BTreeMap::new()),
            suspended: Arc::clone(&slot.suspended),
        });
        let (sent, received) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            service.shutdown();
            sent.send(()).unwrap();
        });
        let early = received.recv_timeout(Duration::from_millis(50));
        slot.forget_child(42);
        worker.join().unwrap();
        assert!(matches!(
            early,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        received.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_reaps_a_removed_registration_with_a_live_connector() {
        let lease = lease();
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        lease.slot.owned.lock().unwrap().insert(pid);
        *lease.slot.children.pending.lock().unwrap() = 1;
        *lease.child.lock().unwrap() = Some(child);
        let service = LinkHosts {
            state_dir: PathBuf::new(),
            executable: PathBuf::new(),
            slots: Mutex::new(BTreeMap::new()), // Registration was removed; connector survives.
            children: Arc::clone(&lease.slot.children),
            closed: Arc::clone(&lease.slot.closed),
            repaired: Mutex::new(BTreeSet::new()),
            paired_at: Mutex::new(BTreeMap::new()),
            suspended: Arc::clone(&lease.slot.suspended),
        };
        service.shutdown();
        assert!(lease.child.lock().unwrap().is_none());
        assert_eq!(*lease.slot.children.pending.lock().unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn delayed_stop_cannot_reap_a_new_connection_generation() {
        let lease = lease();
        let mut held = lease.child.lock().unwrap();
        lease.stop(); // The reaper waits for the old preparation to finish.
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        *held = Some(child);
        lease.child_generation.store(1, Ordering::Release);
        drop(held);
        let deadline = Instant::now() + Duration::from_secs(2);
        while lease.stop_pending.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let alive = lease
            .child
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|child| child.try_wait().unwrap().is_none());
        lease.stop_blocking();
        assert!(alive, "an old stop request killed the reconnected child");
    }

    #[cfg(unix)]
    #[test]
    fn stop_blocking_delivers_sigterm_before_falling_back_to_sigkill() {
        let dir = std::env::temp_dir().join(format!("krill-link-term-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let ready = dir.join("ready");
        let cleaned = dir.join("cleaned");
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "trap 'printf term > \"$2\"; exit 0' TERM; printf ok > \"$1\"; while :; do sleep 0.02; done",
                "sh",
            ])
            .arg(&ready)
            .arg(&cleaned)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let wait_ready = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < wait_ready {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "child shell must arm SIGTERM trap");

        let lease = lease();
        let pid = child.id();
        lease.slot.owned.lock().unwrap().insert(pid);
        *lease.slot.children.pending.lock().unwrap() = 1;
        *lease.child.lock().unwrap() = Some(child);

        lease.stop_blocking();
        assert_eq!(std::fs::read_to_string(&cleaned).unwrap(), "term");
        assert_eq!(*lease.slot.children.pending.lock().unwrap(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn paired_handoff_prevents_stale_revocation_and_ignores_old_status_files() {
        let dir =
            std::env::temp_dir().join(format!("krill-link-paired-handoff-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let node_id = krill_link_core::DeviceIdentity::load_or_create(&dir)
            .unwrap()
            .node_id()
            .to_string();
        let host = krill_link_core::RemoteHost {
            id: "peer-1".into(),
            name: "Peer".into(),
            node_id,
            relay_url: None,
            addr_hints: Vec::new(),
            added_at: 1,
        };
        krill_link_core::save_hosts(&dir, std::slice::from_ref(&host)).unwrap();
        let status_path = krill_link_core::host_status_path(&dir, &host.id).unwrap();
        let socket_path = krill_link_core::host_socket_path(&dir, &host.id).unwrap();
        std::fs::create_dir_all(status_path.parent().unwrap()).unwrap();

        let write_revoked = |updated_at_ms: u64| {
            let status = krill_link_core::HostTunnelStatus {
                host_id: host.id.clone(),
                host_name: host.name.clone(),
                node_id: host.node_id.clone(),
                socket_path: socket_path.clone(),
                state: krill_link_core::TunnelState::Disconnected,
                route: krill_link_core::RouteKind::None,
                remote_addr: None,
                rtt_ms: None,
                reconnect_attempt: 1,
                updated_at_ms,
                pid: None,
                build: String::new(),
                last_error: Some("unauthorized device".into()),
                last_error_kind: Some(krill_link_core::TunnelErrorKind::Unauthorized),
                streams: Vec::new(),
            };
            std::fs::write(&status_path, serde_json::to_vec(&status).unwrap()).unwrap();
        };

        // 1. Before re-pairing, an unauthorized status file marks the slot revoked.
        write_revoked(1_000);
        let service = Arc::new(LinkHosts {
            state_dir: dir.clone(),
            executable: PathBuf::new(),
            slots: Mutex::new(BTreeMap::new()),
            children: Arc::new(OwnedChildren::default()),
            closed: Arc::new(AtomicBool::new(false)),
            repaired: Mutex::new(BTreeSet::new()),
            paired_at: Mutex::new(BTreeMap::new()),
            suspended: Arc::new(Mutex::new(BTreeSet::new())),
        });
        let regs = service.registrations().unwrap();
        assert!(regs[0].info.error.is_some());
        let slot = service.slots.lock().unwrap().get("peer-1").unwrap().clone();
        assert!(slot.revoked.load(Ordering::Acquire));

        // 2. Re-pairing clears the stale status file and ignores any status predating paired().
        service.paired("peer-1");
        assert!(
            !status_path.exists(),
            "paired() must clear stale status file on disk"
        );
        // Consume the one-shot `repaired` flag while a pre-paired status file reappears.
        write_revoked(1_000);
        let regs = service.registrations().unwrap();
        assert!(regs[0].info.error.is_none());
        assert!(!slot.revoked.load(Ordering::Acquire));

        // 3. Even with a fresh unauthorized status file on disk, registrations() called during the
        //    restart -> child termination/spawn handoff window must NOT re-revoke or stop the lease.
        let lease = Arc::new(Lease {
            slot: Arc::clone(&slot),
            child: Mutex::new(None),
            generation: AtomicU64::new(0),
            child_generation: AtomicU64::new(0),
            stop_pending: AtomicBool::new(false),
        });
        *slot.lease.lock().unwrap() = Arc::downgrade(&lease);
        slot.restart.store(true, Ordering::Release);
        write_revoked(now_unix_ms().saturating_add(10_000));

        let held = lease.child.lock().unwrap();
        let ensure_lease = Arc::clone(&lease);
        let worker = std::thread::spawn(move || ensure_lease.ensure());

        let deadline = Instant::now() + Duration::from_secs(2);
        while slot.restart.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            !slot.restart.load(Ordering::Acquire),
            "ensure() should have swapped restart to false"
        );
        assert!(
            slot.starting.load(Ordering::Acquire) > 0,
            "starting guard must already be active before restart is swapped false"
        );

        let gen_before = lease.generation.load(Ordering::Acquire);
        let regs = service.registrations().unwrap();
        assert!(
            regs[0].info.error.is_none(),
            "registrations() must not surface revoked error during starting handoff"
        );
        assert_eq!(regs[0].info.status, HostStatus::Connecting);
        assert!(
            !slot.revoked.load(Ordering::Acquire),
            "registrations() must not re-revoke slot during starting handoff"
        );
        assert_eq!(
            lease.generation.load(Ordering::Acquire),
            gen_before,
            "registrations() must not stop the lease during starting handoff"
        );

        slot.closed.store(true, Ordering::Release);
        drop(held);
        assert!(worker.join().unwrap().is_err());
        assert_eq!(slot.starting.load(Ordering::Acquire), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_services_terminates_multiple_children_in_parallel() {
        let dir = std::env::temp_dir().join(format!(
            "krill-link-parallel-shutdown-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let service = Arc::new(LinkHosts {
            state_dir: dir.clone(),
            executable: PathBuf::new(),
            slots: Mutex::new(BTreeMap::new()),
            children: Arc::new(OwnedChildren::default()),
            closed: Arc::new(AtomicBool::new(false)),
            repaired: Mutex::new(BTreeSet::new()),
            paired_at: Mutex::new(BTreeMap::new()),
            suspended: Arc::new(Mutex::new(BTreeSet::new())),
        });

        // Two children that ignore SIGTERM so serial 300ms waits would take >= 600ms.
        let mut leases = Vec::new();
        for idx in 0..2 {
            let slot = Arc::new(Slot {
                host: krill_link_core::RemoteHost {
                    id: format!("peer-{idx}"),
                    name: format!("Peer {idx}"),
                    node_id: format!("node-{idx}"),
                    relay_url: None,
                    addr_hints: Vec::new(),
                    added_at: 1,
                },
                state_dir: dir.clone(),
                executable: PathBuf::new(),
                identity: Arc::new(()),
                lease: Mutex::new(Weak::new()),
                owned: Mutex::new(BTreeSet::new()),
                children: Arc::clone(&service.children),
                closed: Arc::clone(&service.closed),
                revoked: AtomicBool::new(false),
                removed: AtomicBool::new(false),
                restart: AtomicBool::new(false),
                starting: AtomicUsize::new(0),
                paired_at_ms: AtomicU64::new(0),
                suspended: Arc::clone(&service.suspended),
            });
            service
                .children
                .slots
                .lock()
                .unwrap()
                .push(Arc::downgrade(&slot));
            let child = std::process::Command::new("/bin/sh")
                .args(["-c", "trap '' TERM; while :; do sleep 0.05; done"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let pid = child.id();
            slot.owned.lock().unwrap().insert(pid);
            *service.children.pending.lock().unwrap() += 1;
            let lease = Arc::new(Lease {
                slot: Arc::clone(&slot),
                child: Mutex::new(Some(child)),
                generation: AtomicU64::new(0),
                child_generation: AtomicU64::new(0),
                stop_pending: AtomicBool::new(false),
            });
            *slot.lease.lock().unwrap() = Arc::downgrade(&lease);
            service
                .slots
                .lock()
                .unwrap()
                .insert(format!("peer-{idx}"), slot);
            leases.push(lease);
        }

        let started = Instant::now();
        shutdown_services(&[Arc::clone(&service)]);
        let elapsed = started.elapsed();

        assert!(service.closed.load(Ordering::Acquire));
        assert_eq!(*service.children.pending.lock().unwrap(), 0);
        assert!(
            elapsed < Duration::from_millis(500),
            "parallel shutdown of 2 SIGTERM-ignoring children took {elapsed:?} (serial 2x300ms would be >=600ms)"
        );
        for lease in &leases {
            assert!(lease.child.lock().unwrap().is_none());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn stop_stale_connect_processes_replaces_stale_and_preserves_current_and_unrelated() {
        use std::os::unix::fs::PermissionsExt as _;

        // Use short `/tmp` base so `run/hosts/<id>.sock` stays well under macOS's 104-byte sun_path limit.
        let dir = PathBuf::from(format!("/tmp/kl-sc-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let fake_bin = dir.join("krill-link-test-helper");
        std::fs::write(&fake_bin, "#!/bin/sh\nwhile :; do sleep 0.1; done\n").unwrap();
        std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let node_id = krill_link_core::DeviceIdentity::load_or_create(&dir)
            .unwrap()
            .node_id()
            .to_string();
        let hosts = vec![
            krill_link_core::RemoteHost {
                id: "stale-host".into(),
                name: "Stale".into(),
                node_id: node_id.clone(),
                relay_url: None,
                addr_hints: Vec::new(),
                added_at: 1,
            },
            krill_link_core::RemoteHost {
                id: "current-host".into(),
                name: "Current".into(),
                node_id: node_id.clone(),
                relay_url: None,
                addr_hints: Vec::new(),
                added_at: 2,
            },
            krill_link_core::RemoteHost {
                id: "unrelated-host".into(),
                name: "Unrelated".into(),
                node_id,
                relay_url: None,
                addr_hints: Vec::new(),
                added_at: 3,
            },
        ];
        krill_link_core::save_hosts(&dir, &hosts).unwrap();

        let mut stale_child = std::process::Command::new(&fake_bin)
            .args(["connect", "stale-host", "--state-dir"])
            .arg(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut current_child = std::process::Command::new(&fake_bin)
            .args(["connect", "current-host", "--state-dir"])
            .arg(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut unrelated_child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        let write_status = |host: &krill_link_core::RemoteHost, pid: u32, build: &str| {
            let status_path = krill_link_core::host_status_path(&dir, &host.id).unwrap();
            let socket_path = krill_link_core::host_socket_path(&dir, &host.id).unwrap();
            std::fs::create_dir_all(status_path.parent().unwrap()).unwrap();
            let status = krill_link_core::HostTunnelStatus {
                host_id: host.id.clone(),
                host_name: host.name.clone(),
                node_id: host.node_id.clone(),
                socket_path,
                state: krill_link_core::TunnelState::Connected,
                route: krill_link_core::RouteKind::Direct,
                remote_addr: None,
                rtt_ms: None,
                reconnect_attempt: 0,
                updated_at_ms: now_unix_ms(),
                pid: Some(pid),
                build: build.to_owned(),
                last_error: None,
                last_error_kind: None,
                streams: Vec::new(),
            };
            std::fs::write(status_path, serde_json::to_vec(&status).unwrap()).unwrap();
        };

        write_status(&hosts[0], stale_child.id(), "0.0.1");
        write_status(&hosts[1], current_child.id(), krill_proto::build_id());
        write_status(&hosts[2], unrelated_child.id(), "");

        // Wait briefly so execve completes and sysinfo sees the script args.
        std::thread::sleep(Duration::from_millis(80));

        let stopped = stop_stale_connect_processes(&dir, &fake_bin);
        assert_eq!(stopped, 1, "only stale krill-link connect must be stopped");

        // Reap stale child and verify it exited.
        let stale_exit = stale_child.wait().unwrap();
        assert!(!stale_exit.success() || stale_exit.code().is_some());
        assert!(
            !krill_link_core::host_status_path(&dir, "stale-host")
                .unwrap()
                .exists(),
            "stale host status file must be removed after stopping stale process"
        );

        // Current build child and unrelated PID must remain untouched.
        assert!(
            current_child.try_wait().unwrap().is_none(),
            "up-to-date krill-link connect must not be killed"
        );
        assert!(
            krill_link_core::host_status_path(&dir, "current-host")
                .unwrap()
                .exists()
        );
        assert!(
            unrelated_child.try_wait().unwrap().is_none(),
            "unrelated PID whose cmdline does not match must never be killed"
        );

        current_child.kill().ok();
        current_child.wait().ok();
        unrelated_child.kill().ok();
        unrelated_child.wait().ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clear_status_file_if_pid_preserves_replacement_child_status() {
        let dir = std::env::temp_dir().join(format!("rem04-pid-guard-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let host_id = "mac-mini";
        let status_path = krill_link_core::host_status_path(&dir, host_id).unwrap();
        std::fs::create_dir_all(status_path.parent().unwrap()).unwrap();
        let replacement = krill_link_core::HostTunnelStatus {
            host_id: host_id.into(),
            host_name: "Mac Mini".into(),
            node_id: "node-1".into(),
            socket_path: krill_link_core::host_socket_path(&dir, host_id).unwrap(),
            state: krill_link_core::TunnelState::Connected,
            route: krill_link_core::RouteKind::Direct,
            remote_addr: None,
            rtt_ms: None,
            reconnect_attempt: 0,
            updated_at_ms: now_unix_ms(),
            pid: Some(999_002),
            build: krill_proto::build_id().to_owned(),
            last_error: None,
            last_error_kind: None,
            streams: Vec::new(),
        };
        std::fs::write(&status_path, serde_json::to_vec(&replacement).unwrap()).unwrap();

        // Stale cleanup for an old PID must NOT delete the replacement child's status file.
        clear_status_file_if_pid(&dir, host_id, 999_001);
        assert!(
            status_path.exists(),
            "replacement child status file must survive stale PID cleanup"
        );

        // Matching PID removes the file.
        clear_status_file_if_pid(&dir, host_id, 999_002);
        assert!(!status_path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
