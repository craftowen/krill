//! Cloudflare Quick Tunnel의 프로세스 수명. 키·pair token은 cloudflared 인자에 넣지 않는다.
use crate::session::lock;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

/// URL이 찍힌 뒤에도 엣지 등록·DNS 전파 전에는 530/NXDOMAIN이 난다. 첫 200 뒤에도 한동안 530이 섞인다
/// (2026-10-19 단독 퀵 터널 11회 실측: 10회는 URL 뒤 28.1~32.7초까지 530이 섞였고, 그 사이 200 연속이
/// 최대 약 20회였다. 1회는 첫 200 뒤 530이 없었다). 연속 성공 횟수만으로는 가를 수 없어서, URL 뒤
/// `READY_SETTLE`이 지났고 직전 `READY_STREAK`번이 연속 200이어야 `ready`다.
/// `READY_DEADLINE` 안에 준비되지 않으면 감시자가 다시 띄운다.
pub const READY_DEADLINE: Duration = Duration::from_secs(90);
/// 실측 마지막 530(URL 뒤 최대 32.7초) 직후.
const READY_SETTLE: Duration = Duration::from_secs(33);
/// 정착 시점 앞뒤로 약 4초(curl 포함, 추정) 동안 530이 없어야 한다.
const READY_STREAK: u32 = 8;
const PROBE_INTERVAL: Duration = Duration::from_millis(400);
type Probe = fn(&str) -> bool;

/// 비밀은 Debug·프로세스 인자로 노출하지 않는다.
#[derive(Clone)]
pub(super) struct Publication {
    pub origin: String,
    pub rid: String,
    pub key: String,
}
impl std::fmt::Debug for Publication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Publication").finish_non_exhaustive()
    }
}
/// 사용자가 지정한 Cloudflare Named Tunnel(토큰은 환경변수로만 전달해 `ps` 노출을 막는다).
#[derive(Clone)]
pub(super) struct NamedTunnel {
    pub token: String,
    pub url: String,
}
impl std::fmt::Debug for NamedTunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedTunnel")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
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

pub struct Tunnel {
    child: Option<Child>,
    adopted_pid: Option<u32>,
    url: Arc<OnceLock<String>>,
    ready: Arc<AtomicBool>,
    started: Instant,
    fixed: Arc<AtomicBool>,
    persist: Arc<AtomicBool>,
    stop: Arc<Stop>,
}
impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tunnel").finish_non_exhaustive()
    }
}
impl Tunnel {
    /// 즉시 반환한다. URL 발행과 준비 확인은 백그라운드 스레드가 하므로 게이트웨이 락을 잡고 기다리지 않는다.
    pub(super) fn start(
        binary: &OsStr,
        port: u16,
        publication: Publication,
        named: Option<NamedTunnel>,
    ) -> io::Result<Self> {
        if let Some(named) = named {
            Self::spawn_named(binary, port, probe, &named)
        } else {
            Self::spawn(binary, port, probe, READY_SETTLE, Some(publication))
        }
    }
    /// 이전 `krilld`가 남긴 살아 있는 `cloudflared` 프로세스와 URL을 0초 만에 재입양한다.
    pub(super) fn adopt(pid: u32, url: String, publication: Option<Publication>) -> Option<Self> {
        if pid <= 1 || !pid_alive(pid) {
            return None;
        }
        let is_quick = parse_url(&url).as_deref() == Some(url.as_str());
        if !is_quick && normalize_named_url(&url).as_deref() != Some(url.as_str()) {
            return None;
        }
        let tunnel = Self {
            child: None,
            adopted_pid: Some(pid),
            url: Arc::new(OnceLock::from(url.clone())),
            ready: Arc::new(AtomicBool::new(true)),
            started: Instant::now(),
            fixed: Arc::new(AtomicBool::new(false)),
            persist: Arc::new(AtomicBool::new(true)),
            stop: Arc::new(Stop::default()),
        };
        if is_quick && let Some(pub_cfg) = publication {
            let ready = Arc::clone(&tunnel.ready);
            let fixed = Arc::clone(&tunnel.fixed);
            let stop = Arc::clone(&tunnel.stop);
            thread::Builder::new()
                .name("mobile-tunnel-adopt".into())
                .stack_size(128 * 1024)
                .spawn(move || {
                    let enabled = fixed_available(Some(&pub_cfg), |u| http_status(u) == Some(200));
                    prepare(
                        &url,
                        Some(&pub_cfg),
                        enabled,
                        &ready,
                        &fixed,
                        &stop,
                        publish,
                    );
                })
                .ok();
        }
        Some(tunnel)
    }
    fn spawn_named(
        binary: &OsStr,
        port: u16,
        probe: Probe,
        named: &NamedTunnel,
    ) -> io::Result<Self> {
        let target_url = normalize_named_url(&named.url).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "유효하지 않은 Named Tunnel 호스트명",
            )
        })?;
        let (stderr_stdio, _log_reader, log_path) = detached_stderr(port)?;
        let mut command = Command::new(binary);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        command
            .env("TUNNEL_TOKEN", &named.token)
            .args([
                "tunnel",
                "--no-autoupdate",
                "--url",
                &format!("http://127.0.0.1:{port}"),
                "run",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr_stdio);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                std::fs::remove_file(&log_path).ok();
                return Err(io::Error::new(
                    e.kind(),
                    format!("cloudflared 실행 실패: {e}"),
                ));
            }
        };
        drop(command);
        register_pid_lock(&log_path, child.id());
        let tunnel = Self {
            child: Some(child),
            adopted_pid: None,
            url: Arc::new(OnceLock::from(target_url.clone())),
            ready: Arc::new(AtomicBool::new(false)),
            started: Instant::now(),
            fixed: Arc::new(AtomicBool::new(false)),
            persist: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(Stop::default()),
        };
        let ready = Arc::clone(&tunnel.ready);
        let stop = Arc::clone(&tunnel.stop);
        thread::Builder::new()
            .name("mobile-tunnel-probe".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let now = Instant::now();
                if stable(
                    &target_url,
                    probe,
                    &stop,
                    now,
                    now + READY_DEADLINE,
                    PROBE_INTERVAL,
                    1,
                ) {
                    ready.store(true, Ordering::Release);
                }
            })?;
        Ok(tunnel)
    }
    fn spawn(
        binary: &OsStr,
        port: u16,
        probe: Probe,
        settle: Duration,
        publication: Option<Publication>,
    ) -> io::Result<Self> {
        let (stderr_stdio, mut stderr, log_path) = detached_stderr(port)?;
        let mut command = Command::new(binary);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        command
            .args([
                "tunnel",
                "--no-autoupdate",
                "--url",
                &format!("http://127.0.0.1:{port}"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr_stdio);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                std::fs::remove_file(&log_path).ok();
                return Err(io::Error::new(
                    e.kind(),
                    format!("cloudflared 실행 실패: {e}"),
                ));
            }
        };
        drop(command);
        register_pid_lock(&log_path, child.id());
        let tunnel = Self {
            child: Some(child),
            adopted_pid: None,
            url: Arc::new(OnceLock::new()),
            ready: Arc::new(AtomicBool::new(false)),
            started: Instant::now(),
            fixed: Arc::new(AtomicBool::new(false)),
            persist: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(Stop::default()),
        };
        let (url, ready) = (Arc::clone(&tunnel.url), Arc::clone(&tunnel.ready));
        let fixed = Arc::clone(&tunnel.fixed);
        let stop = Arc::clone(&tunnel.stop);
        thread::Builder::new()
            .name("mobile-tunnel-log".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let mut buffer = [0; 2048];
                let mut pending = String::new();
                while !stop.cancelled() {
                    let Ok(n) = stderr.read(&mut buffer) else {
                        break;
                    };
                    if n == 0 {
                        if url.get().is_some() || stop.wait(Duration::from_millis(20)) {
                            break;
                        }
                        continue;
                    }
                    if let Some(bytes) = buffer.get(..n) {
                        pending.push_str(&String::from_utf8_lossy(bytes));
                    }
                    if url.get().is_none()
                        && let Some(found) = parse_url(&pending)
                    {
                        url.set(found.clone()).ok();
                        let ready = Arc::clone(&ready);
                        let fixed = Arc::clone(&fixed);
                        let stop = Arc::clone(&stop);
                        let publication = publication.clone();
                        thread::Builder::new()
                            .name("mobile-tunnel-probe".into())
                            .stack_size(128 * 1024)
                            .spawn(move || {
                                let start = Instant::now();
                                // 고정 출처 PWA는 터널 WS 530을 재시도로 견디므로 첫 200에서 준비로 본다.
                                // 터널 출처는 페이지 자체가 530이면 열리지 않으므로 정착을 기다린다.
                                let enabled = fixed_available(publication.as_ref(), |url| {
                                    http_status(url) == Some(200)
                                });
                                let (settle, streak) = if enabled {
                                    (start, 1)
                                } else {
                                    (start + settle, READY_STREAK)
                                };
                                let deadline = start + READY_DEADLINE;
                                if stable(
                                    &found,
                                    probe,
                                    &stop,
                                    settle,
                                    deadline,
                                    PROBE_INTERVAL,
                                    streak,
                                ) {
                                    prepare(
                                        &found,
                                        publication.as_ref(),
                                        enabled,
                                        &ready,
                                        &fixed,
                                        &stop,
                                        publish,
                                    );
                                }
                            })
                            .ok();
                        break;
                    }
                    if pending.len() > 4096 {
                        pending = pending
                            .chars()
                            .rev()
                            .take(512)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect();
                    }
                }
            })?;
        Ok(tunnel)
    }
    /// cloudflared 프로세스 PID(재입양 포함).
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id).or(self.adopted_pid)
    }
    /// `krilld` 재시작 시 `Drop`에서 자식 프로세스를 죽이지 않게 설정한다.
    pub fn set_persist(&self, persist: bool) {
        self.persist.store(persist, Ordering::Release);
    }
    /// 명시적 모드 전환·전체 기기 해제 시 프로세스를 종료한다.
    pub fn terminate(&mut self) {
        self.persist.store(false, Ordering::Release);
        self.stop.cancel();
        if let Some(mut child) = self.child.take() {
            let pid = child.id();
            child.kill().ok();
            child.wait().ok();
            std::fs::remove_file(pid_lock_path(pid)).ok();
        }
        if let Some(pid) = self.adopted_pid.take() {
            kill_pid(pid);
        }
    }
    /// cloudflared가 찍은 URL. 준비 전일 수 있다.
    pub fn url(&self) -> Option<&str> {
        self.url.get().map(String::as_str)
    }
    /// 공개 경로로 실제 HTTP 200을 받은 URL.
    pub fn ready_url(&self) -> Option<&str> {
        self.url().filter(|_| self.ready.load(Ordering::Acquire))
    }
    pub fn fixed(&self) -> bool {
        self.fixed.load(Ordering::Acquire)
    }
    /// 프로세스가 살아 있고 준비 기한을 넘기지 않았다. 아니면 감시자가 교체한다.
    pub fn healthy(&mut self) -> bool {
        let alive = if let Some(child) = self.child.as_mut() {
            child.try_wait().is_ok_and(|s| s.is_none())
        } else if let Some(pid) = self.adopted_pid {
            pid_alive(pid)
        } else {
            true
        };
        alive && (self.ready.load(Ordering::Acquire) || self.started.elapsed() < READY_DEADLINE)
    }
    #[cfg(test)]
    #[cfg(unix)]
    pub(super) fn fixture(port: u16) -> io::Result<Self> {
        if port == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            child: None,
            adopted_pid: None,
            url: Arc::new(OnceLock::from(
                "https://fixture-mobile.trycloudflare.com".to_owned(),
            )),
            ready: Arc::new(AtomicBool::new(true)),
            started: Instant::now(),
            fixed: Arc::new(AtomicBool::new(false)),
            persist: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(Stop::default()),
        })
    }
}
#[cfg(test)]
#[cfg(unix)]
impl Tunnel {
    pub(super) fn fixed_fixture(port: u16) -> io::Result<Self> {
        let tunnel = Self::fixture(port)?;
        tunnel.fixed.store(true, Ordering::Release);
        Ok(tunnel)
    }
    /// URL은 찍혔지만 준비 기한을 넘긴 터널.
    pub(super) fn stale(port: u16) -> io::Result<Self> {
        let mut tunnel = Self::fixture(port)?;
        tunnel.ready = Arc::new(AtomicBool::new(false));
        tunnel.started = Instant::now()
            .checked_sub(READY_DEADLINE + Duration::from_secs(1))
            .ok_or(io::ErrorKind::InvalidInput)?;
        Ok(tunnel)
    }
    /// 데몬 재시작 시 PID 재입양·종료를 검증하는 테스트용 터널(합성 PID로 병렬 테스트 간 fork FD 상속 경합 방지).
    pub(super) fn persistent_fixture(port: u16) -> io::Result<Self> {
        if port == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let pid = NEXT_SYNTHETIC_PID.fetch_add(1, Ordering::Relaxed);
        synthetic_pids()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(pid);
        Ok(Self {
            child: None,
            adopted_pid: Some(pid),
            url: Arc::new(OnceLock::from(
                "https://fixture-mobile.trycloudflare.com".to_owned(),
            )),
            ready: Arc::new(AtomicBool::new(true)),
            started: Instant::now(),
            fixed: Arc::new(AtomicBool::new(false)),
            persist: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(Stop::default()),
        })
    }
}
#[cfg(all(test, unix))]
const SYNTHETIC_PID_BASE: u32 = 900_000_000;
#[cfg(all(test, unix))]
static NEXT_SYNTHETIC_PID: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(SYNTHETIC_PID_BASE);
#[cfg(all(test, unix))]
fn synthetic_pids() -> &'static Mutex<std::collections::HashSet<u32>> {
    static PIDS: OnceLock<Mutex<std::collections::HashSet<u32>>> = OnceLock::new();
    PIDS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}
impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop.cancel();
        if !self.persist.load(Ordering::Acquire) {
            self.terminate();
        }
    }
}
fn pid_lock_path(pid: u32) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(".krill-cf-pid-{pid}.lock"))
}
fn register_pid_lock(temp_path: &std::path::Path, pid: u32) {
    let lock_path = pid_lock_path(pid);
    if std::fs::rename(temp_path, &lock_path).is_err() {
        std::fs::remove_file(temp_path).ok();
    }
}
/// `krilld`가 종료돼도 `cloudflared`가 `SIGPIPE`로 죽지 않게 임시 파일로 stderr를 받으며,
/// 자식 프로세스가 열린 파일 기술자의 배타적 `flock`을 상속해 PID 재사용(OS 재부팅 등) 시 무관한 프로세스 오인을 막는다.
fn detached_stderr(port: u16) -> io::Result<(Stdio, std::fs::File, std::path::PathBuf)> {
    let nonce = super::devices::hex(&super::e2ee::random()?);
    let path = std::env::temp_dir().join(format!(".krill-cf-{port}-{nonce}.log"));
    let mut opts = std::fs::OpenOptions::new();
    opts.create_new(true).write(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let write_file = opts.open(&path)?;
    #[cfg(unix)]
    if let Err(err) = rustix::fs::flock(
        &write_file,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    ) {
        std::fs::remove_file(&path).ok();
        return Err(io::Error::from(err));
    }
    let read_file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(err) => {
            std::fs::remove_file(&path).ok();
            return Err(err);
        }
    };
    Ok((Stdio::from(write_file), read_file, path))
}
pub(super) fn pid_alive(pid: u32) -> bool {
    #[cfg(all(test, unix))]
    if pid >= SYNTHETIC_PID_BASE {
        return synthetic_pids()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&pid);
    }
    #[cfg(unix)]
    {
        let Some(p) = rustix::process::Pid::from_raw(pid.cast_signed()) else {
            return false;
        };
        rustix::process::waitpid(Some(p), rustix::process::WaitOptions::NOHANG).ok();
        if rustix::process::test_kill_process(p).is_err() {
            std::fs::remove_file(pid_lock_path(pid)).ok();
            return false;
        }
        let lock_path = pid_lock_path(pid);
        let Ok(file) = std::fs::File::open(&lock_path) else {
            return false;
        };
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Err(rustix::io::Errno::WOULDBLOCK) => true,
            Ok(()) => {
                rustix::fs::flock(&file, rustix::fs::FlockOperation::Unlock).ok();
                std::fs::remove_file(&lock_path).ok();
                false
            }
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}
pub(super) fn kill_pid(pid: u32) {
    #[cfg(all(test, unix))]
    if pid >= SYNTHETIC_PID_BASE {
        synthetic_pids()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&pid);
        return;
    }
    #[cfg(unix)]
    {
        if !pid_alive(pid) {
            return;
        }
        if let Some(p) = rustix::process::Pid::from_raw(pid.cast_signed()) {
            rustix::process::kill_process_group(p, rustix::process::Signal::KILL).ok();
            rustix::process::kill_process(p, rustix::process::Signal::KILL).ok();
            rustix::process::waitpid(Some(p), rustix::process::WaitOptions::empty()).ok();
        }
        std::fs::remove_file(pid_lock_path(pid)).ok();
    }
    #[cfg(not(unix))]
    let _ = pid;
}
pub fn normalize_named_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let host = trimmed.strip_prefix("https://").unwrap_or(trimmed);
    if host.is_empty() || host.len() > 253 || !host.contains('.') {
        return None;
    }
    let valid_labels = host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    });
    valid_labels.then(|| format!("https://{host}"))
}
/// `settle` 뒤, 기한 안에 직전 `needed`번이 연속 성공이면 참. 실패 한 번이면 연속을 처음부터 센다.
/// 취소되면 거짓.
fn stable(
    url: &str,
    mut probe: impl FnMut(&str) -> bool,
    stop: &Stop,
    settle: Instant,
    deadline: Instant,
    interval: Duration,
    needed: u32,
) -> bool {
    let mut streak = 0;
    while Instant::now() < deadline && !stop.cancelled() {
        streak = if probe(url) { streak + 1 } else { 0 };
        if streak >= needed && Instant::now() >= settle {
            return true;
        }
        if stop.wait(interval) {
            return false;
        }
    }
    false
}
/// 현재 에셋 해시가 고정 출처에 배포돼 있는지. 네트워크 IO는 상태 락 밖에서 수행한다.
pub(super) fn publication_available(publication: &Publication) -> bool {
    fixed_available(Some(publication), |url| http_status(url) == Some(200))
}

/// 현재 에셋 해시가 고정 출처에 배포돼 있는지.
fn fixed_available(
    publication: Option<&Publication>,
    available: impl FnOnce(&str) -> bool,
) -> bool {
    publication.is_some_and(|p| {
        available(&format!(
            "{}/m/{}/manifest.json",
            p.origin,
            super::pwa::asset_hash()
        ))
    })
}
/// 판정이 끝난 뒤 ready를 공개한다. `enabled`는 고정 출처에 현재 에셋 해시가 배포됐는지다(QR 출처 선택).
/// `/r/<rid>` 게시는 배포 여부와 무관하게 한다: 이미 짝지은 PWA가 새 터널을 찾아야 하고,
/// 미배포 해시는 PWA 업데이트 준비(`updates.js` prepare)가 404로 거른다.
/// 게시 실패는 연결 준비를 막지 않고 취소 가능한 백오프로 재시도한다.
fn prepare(
    url: &str,
    publication: Option<&Publication>,
    enabled: bool,
    ready: &AtomicBool,
    fixed: &AtomicBool,
    stop: &Stop,
    mut put: impl FnMut(&Publication, &str) -> bool,
) {
    if stop.cancelled() {
        return;
    }
    fixed.store(enabled, Ordering::Release);
    ready.store(true, Ordering::Release);
    if let Some(p) = publication {
        let mut delay = Duration::from_secs(2);
        while !stop.cancelled() {
            if put(p, url) {
                return;
            }
            if stop.wait(delay) {
                return;
            }
            delay = (delay * 2).min(Duration::from_secs(60));
        }
    }
}
fn http_status(url: &str) -> Option<u16> {
    let mut command = Command::new("curl");
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let out = command
        .args([
            "-sS",
            "-o",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
            "-w",
            "%{http_code}",
            "--max-time",
            "5",
            url,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    std::str::from_utf8(&out.stdout).ok()?.parse().ok()
}
fn publish(p: &Publication, url: &str) -> bool {
    // 헤더 파일을 stdin으로 전달해 ps에 랑데부 키가 남지 않게 한다.
    let body = serde_json::json!({"url":url, "hash":super::pwa::asset_hash()}).to_string();
    let mut command = Command::new("curl");
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let Ok(mut child) = command
        .args([
            "-sS",
            "-o",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
            "-w",
            "%{http_code}",
            "--max-time",
            "5",
            "-X",
            "PUT",
            "-H",
            "@-",
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            &body,
            &format!("{}/r/{}", p.origin, p.rid),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let sent = child
        .stdin
        .take()
        .is_some_and(|mut stdin| writeln!(stdin, "Authorization: Bearer {}", p.key).is_ok());
    if !sent {
        child.kill().ok();
    }
    child
        .wait_with_output()
        .is_ok_and(|out| sent && out.status.success() && out.stdout == b"204")
}
/// 로컬 리졸버의 NXDOMAIN 음성 캐시(60초)를 피하려고 Quick Tunnel은 apex로 연결하고 SNI·Host만 터널 이름을 쓴다.
/// Named Tunnel(사용자 도메인)은 일반 DNS/TLS 경로로 프로브한다.
fn probe(url: &str) -> bool {
    let Some(host) = url.strip_prefix("https://") else {
        return false;
    };
    let mut cmd = Command::new("curl");
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd.args([
        "-sS",
        "-o",
        if cfg!(windows) { "NUL" } else { "/dev/null" },
        "-w",
        "%{http_code}",
        "--max-time",
        "5",
    ]);
    if host.ends_with(".trycloudflare.com") {
        cmd.args(["--connect-to", &format!("{host}:443:trycloudflare.com:443")]);
    }
    match cmd
        .arg(format!("{url}/manifest.json"))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(out) => out.status.success() && out.stdout == b"200",
        Err(_) => false,
    }
}
pub fn parse_url(text: &str) -> Option<String> {
    for tail in text.split("https://").skip(1) {
        let host: String = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.')
            .collect();
        let Some(label) = host.strip_suffix(".trycloudflare.com") else {
            continue;
        };
        if !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Some(format!("https://{host}"));
        }
    }
    None
}
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn publication() -> Publication {
        Publication {
            origin: "https://stage.test".into(),
            rid: "a".repeat(32),
            key: "b".repeat(64),
        }
    }
    #[test]
    fn alternating_probe_is_never_stable_until_consecutive_streak() {
        const URL: &str = "https://a.trycloudflare.com";
        let stop = Stop::default();
        let tick = Duration::from_millis(1);
        let now = Instant::now();
        let mut calls = 0;
        let alternating = |_: &str| {
            calls += 1;
            calls % 2 == 0
        };
        let deadline = now + Duration::from_millis(300);
        assert!(!stable(
            URL,
            alternating,
            &stop,
            now,
            deadline,
            tick,
            READY_STREAK
        ));
        assert!(calls > 2 * READY_STREAK);
        // 실패 몇 번 뒤 연속 성공이 쌓이면 READY_STREAK번째에 준비된다.
        let mut calls = 0;
        let late = |_: &str| {
            calls += 1;
            calls > 3
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(stable(URL, late, &stop, now, deadline, tick, READY_STREAK));
        assert_eq!(calls, 3 + READY_STREAK);
        // 정착 시점 전에는 계속 성공해도 준비가 아니다.
        let settle = Instant::now() + Duration::from_millis(100);
        assert!(stable(
            URL,
            |_| true,
            &stop,
            settle,
            deadline,
            tick,
            READY_STREAK
        ));
        assert!(Instant::now() >= settle);
        let early = Instant::now() + Duration::from_millis(50);
        assert!(!stable(
            URL,
            |_| true,
            &stop,
            deadline,
            early,
            tick,
            READY_STREAK
        ));
        stop.cancel();
        assert!(!stable(
            URL,
            |_| true,
            &stop,
            now,
            deadline,
            tick,
            READY_STREAK
        ));
    }
    #[test]
    fn availability_is_decided_before_ready_and_failure_falls_back() {
        let manifest = format!(
            "https://stage.test/m/{}/manifest.json",
            super::super::pwa::asset_hash()
        );
        assert!(!fixed_available(None, |_| true));
        for enabled in [true, false] {
            assert_eq!(
                fixed_available(Some(&publication()), |url| {
                    assert_eq!(url, manifest);
                    enabled
                }),
                enabled
            );
            let ready = AtomicBool::new(false);
            let fixed = AtomicBool::new(false);
            let mut puts = 0;
            let p = publication();
            prepare(
                "https://fixture.trycloudflare.com",
                Some(&p),
                enabled,
                &ready,
                &fixed,
                &Stop::default(),
                |_, _| {
                    assert!(ready.load(Ordering::Acquire));
                    assert_eq!(fixed.load(Ordering::Acquire), enabled);
                    puts += 1;
                    true
                },
            );
            assert!(ready.load(Ordering::Acquire));
            assert_eq!(fixed.load(Ordering::Acquire), enabled);
            // 미배포여도 `/r/<rid>`는 게시한다(짝지은 PWA가 새 터널을 찾는다).
            assert_eq!(puts, 1);
        }
    }
    #[test]
    fn failed_publication_retries_and_drop_cancels_backoff() {
        let tunnel = Tunnel::fixture(1234).unwrap();
        let stop = Arc::clone(&tunnel.stop);
        let (send, receive) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let mut attempts = 0;
            prepare(
                "https://fixture.trycloudflare.com",
                Some(&publication()),
                true,
                &AtomicBool::new(false),
                &AtomicBool::new(false),
                &stop,
                |_, _| {
                    attempts += 1;
                    send.send(attempts).unwrap();
                    false
                },
            );
            attempts
        });
        assert_eq!(receive.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        assert_eq!(receive.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
        drop(tunnel);
        assert_eq!(worker.join().unwrap(), 2);
        assert!(receive.try_recv().is_err());
    }
    #[test]
    fn publication_request_keeps_key_in_stdin_and_requires_204() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for status in ["403 Forbidden", "204 No Content"] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.extend(byte);
                }
                let headers = String::from_utf8(request).unwrap();
                assert!(headers.starts_with(&format!("PUT /r/{} HTTP/1.1", "a".repeat(32))));
                assert!(headers.contains(&format!("Authorization: Bearer {}", "b".repeat(64))));
                let length: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!(length <= 512);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["url"], "https://fixture.trycloudflare.com");
                assert_eq!(body["hash"], super::super::pwa::asset_hash());
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            }
        });
        let mut p = publication();
        p.origin = origin;
        assert!(!publish(&p, "https://fixture.trycloudflare.com"));
        assert!(publish(&p, "https://fixture.trycloudflare.com"));
        server.join().unwrap();
    }
    #[test]
    fn strict_url_parser() {
        assert_eq!(
            parse_url("INF | https://happy-krill.trycloudflare.com |"),
            Some("https://happy-krill.trycloudflare.com".into())
        );
        for bad in [
            "http://a.trycloudflare.com",
            "https://a.trycloudflare.com.evil.test",
            "https://a.b.trycloudflare.com",
            "https://-a.trycloudflare.com",
        ] {
            assert!(parse_url(bad).is_none());
        }
    }
    #[test]
    fn process_url_and_drop_cleanup() {
        let dir = std::env::temp_dir().join(format!("mk-tunnel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("cloudflared");
        std::fs::write(&script,"#!/bin/sh\nprintf '%s\\n' \"$*\" >&2\nprintf 'https://test-mobile.trycloudflare.com\\n' >&2\nexec /bin/sleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut tunnel =
            Tunnel::spawn(script.as_os_str(), 1234, |_| true, Duration::ZERO, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while tunnel.ready_url().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            tunnel.ready_url(),
            Some("https://test-mobile.trycloudflare.com")
        );
        assert!(tunnel.healthy());
        let pid = tunnel.child.as_ref().unwrap().id();
        drop(tunnel);
        let alive = Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn unready_url_is_published_but_not_ready() {
        let dir = std::env::temp_dir().join(format!("mk-tunnel-nr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("cloudflared");
        std::fs::write(&script,"#!/bin/sh\nprintf 'https://slow-mobile.trycloudflare.com\\n' >&2\nexec /bin/sleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut tunnel =
            Tunnel::spawn(script.as_os_str(), 1234, |_| false, Duration::ZERO, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while tunnel.url().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(tunnel.url(), Some("https://slow-mobile.trycloudflare.com"));
        assert!(tunnel.ready_url().is_none());
        assert!(tunnel.healthy());
        drop(tunnel);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn adopt_reuses_live_pid_and_terminate_kills_adopted_process() {
        let dir = std::env::temp_dir().join(format!("mk-tunnel-adopt-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("cloudflared");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf 'https://adopted-mobile.trycloudflare.com\\n' >&2\nexec /bin/sleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tunnel =
            Tunnel::spawn(script.as_os_str(), 4321, |_| true, Duration::ZERO, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while tunnel.ready_url().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let pid = tunnel.pid().unwrap();
        let url = tunnel.ready_url().unwrap().to_owned();
        tunnel.set_persist(true);
        drop(tunnel);
        assert!(pid_alive(pid), "persisted tunnel process must survive Drop");

        let mut adopted = Tunnel::adopt(pid, url.clone(), None).unwrap();
        assert_eq!(adopted.pid(), Some(pid));
        assert_eq!(adopted.ready_url(), Some(url.as_str()));
        assert!(adopted.healthy());
        adopted.terminate();
        let wait_end = Instant::now() + Duration::from_secs(2);
        while pid_alive(pid) && Instant::now() < wait_end {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!pid_alive(pid));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn named_tunnel_sets_token_env_and_skips_33s_settle() {
        let dir = std::env::temp_dir().join(format!("mk-tunnel-named-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let env_out = dir.join("env.txt");
        let script = dir.join("cloudflared");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s|%s\\n' \"$TUNNEL_TOKEN\" \"$*\" > '{}'\nexec /bin/sleep 30\n",
                env_out.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let started = Instant::now();
        let mut tunnel = Tunnel::spawn_named(
            script.as_os_str(),
            8765,
            |u| u == "https://krill.example.com",
            &NamedTunnel {
                token: "eyJ-secret-token".into(),
                url: "krill.example.com".into(),
            },
        )
        .unwrap();
        assert_eq!(tunnel.url(), Some("https://krill.example.com"));
        let deadline = Instant::now() + Duration::from_secs(3);
        while (tunnel.ready_url().is_none() || !env_out.exists()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(tunnel.ready_url(), Some("https://krill.example.com"));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(tunnel.healthy());
        let recorded = std::fs::read_to_string(&env_out).unwrap();
        assert_eq!(
            recorded.trim(),
            "eyJ-secret-token|tunnel --no-autoupdate --url http://127.0.0.1:8765 run"
        );
        let pid = tunnel.pid().unwrap();
        drop(tunnel);
        assert!(!pid_alive(pid));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn unrelated_process_is_not_adopted_or_killed_after_pid_recycle() {
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new("/bin/sleep");
        cmd.arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut innocent = cmd.spawn().unwrap();
        let pid = innocent.id();
        // OS 재부팅 등으로 무관한 프로세스가 동일한 PID를 배정받고 이전 락 파일만 남은 상황
        let lock_path = pid_lock_path(pid);
        std::fs::write(&lock_path, b"stale").unwrap();
        assert!(!pid_alive(pid));
        assert!(
            Tunnel::adopt(pid, "https://adopted-mobile.trycloudflare.com".into(), None).is_none()
        );
        kill_pid(pid);
        assert!(
            innocent.try_wait().unwrap().is_none(),
            "kill_pid must not terminate an unrelated process"
        );
        innocent.kill().ok();
        innocent.wait().ok();
        std::fs::remove_file(&lock_path).ok();
    }
}
