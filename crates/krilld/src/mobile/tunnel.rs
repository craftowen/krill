//! 사용자가 지정한 Cloudflare Named Tunnel의 `cloudflared` 프로세스 수명. 키·pair token은 cloudflared 인자에 넣지 않는다.
use crate::session::lock;
use std::ffi::OsStr;
use std::io;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// 이 안에 공개 URL이 HTTP 200을 돌려주지 않으면 감시자가 다시 띄운다.
pub const READY_DEADLINE: Duration = Duration::from_secs(90);
const PROBE_INTERVAL: Duration = Duration::from_millis(400);
type Probe = fn(&str) -> bool;

/// 비밀은 Debug·프로세스 인자로 노출하지 않는다.
#[derive(Clone)]
pub(super) struct Publication {
    pub origin: String,
    pub rid: String,
    pub key: String,
    pub session: Option<super::relay_auth::Session>,
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
    url: String,
    ready: Arc<AtomicBool>,
    started: Instant,
    persist: Arc<AtomicBool>,
    stop: Arc<Stop>,
}
impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tunnel").finish_non_exhaustive()
    }
}
impl Tunnel {
    /// 즉시 반환한다. 준비 확인은 백그라운드 스레드가 하므로 게이트웨이 락을 잡고 기다리지 않는다.
    pub(super) fn start(binary: &OsStr, port: u16, named: &NamedTunnel) -> io::Result<Self> {
        Self::spawn_named(binary, port, probe, named)
    }
    /// 이전 `krilld`가 남긴 살아 있는 `cloudflared` 프로세스와 URL을 0초 만에 재입양한다.
    pub(super) fn adopt(pid: u32, url: String) -> Option<Self> {
        if pid <= 1 || !pid_alive(pid) || normalize_named_url(&url).as_deref() != Some(url.as_str())
        {
            return None;
        }
        Some(Self {
            child: None,
            adopted_pid: Some(pid),
            url,
            ready: Arc::new(AtomicBool::new(true)),
            started: Instant::now(),
            persist: Arc::new(AtomicBool::new(true)),
            stop: Arc::new(Stop::default()),
        })
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
        let (stderr_stdio, log_path) = detached_stderr(port)?;
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
            url: target_url.clone(),
            ready: Arc::new(AtomicBool::new(false)),
            started: Instant::now(),
            persist: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(Stop::default()),
        };
        let ready = Arc::clone(&tunnel.ready);
        let stop = Arc::clone(&tunnel.stop);
        thread::Builder::new()
            .name("mobile-tunnel-probe".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                if reachable(
                    &target_url,
                    probe,
                    &stop,
                    Instant::now() + READY_DEADLINE,
                    PROBE_INTERVAL,
                ) {
                    ready.store(true, Ordering::Release);
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
    /// Named Tunnel 공개 URL. 준비 전일 수 있다.
    pub fn url(&self) -> &str {
        &self.url
    }
    /// 공개 경로로 실제 HTTP 200을 받은 URL.
    pub fn ready_url(&self) -> Option<&str> {
        self.ready
            .load(Ordering::Acquire)
            .then_some(self.url.as_str())
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
            url: "https://fixture.krill.example".to_owned(),
            ready: Arc::new(AtomicBool::new(true)),
            started: Instant::now(),
            persist: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(Stop::default()),
        })
    }
}
#[cfg(test)]
#[cfg(unix)]
impl Tunnel {
    /// 준비 기한을 넘긴 터널.
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
            url: "https://fixture.krill.example".to_owned(),
            ready: Arc::new(AtomicBool::new(true)),
            started: Instant::now(),
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
    static PIDS: std::sync::OnceLock<Mutex<std::collections::HashSet<u32>>> =
        std::sync::OnceLock::new();
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
fn detached_stderr(port: u16) -> io::Result<(Stdio, std::path::PathBuf)> {
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
    Ok((Stdio::from(write_file), path))
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
/// 기한 안에 프로브가 한 번이라도 성공하면 참. 취소되면 거짓.
fn reachable(
    url: &str,
    mut probe: impl FnMut(&str) -> bool,
    stop: &Stop,
    deadline: Instant,
    interval: Duration,
) -> bool {
    while Instant::now() < deadline && !stop.cancelled() {
        if probe(url) {
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
    let manifest = format!(
        "{}/m/{}/manifest.json",
        publication.origin,
        super::pwa::asset_hash()
    );
    http_status(&manifest) == Some(200)
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
/// 공개 URL의 `manifest.json`이 HTTP 200인지 일반 DNS/TLS 경로로 확인한다.
fn probe(url: &str) -> bool {
    url.starts_with("https://") && http_status(&format!("{url}/manifest.json")) == Some(200)
}
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn reachable_retries_until_first_success_and_stops_on_deadline_or_cancel() {
        const URL: &str = "https://krill.example.com";
        let stop = Stop::default();
        let tick = Duration::from_millis(1);
        let mut calls = 0;
        let late = |_: &str| {
            calls += 1;
            calls > 3
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(reachable(URL, late, &stop, deadline, tick));
        assert_eq!(calls, 4);
        let deadline = Instant::now() + Duration::from_millis(50);
        assert!(!reachable(URL, |_| false, &stop, deadline, tick));
        assert!(Instant::now() >= deadline);
        stop.cancel();
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(!reachable(URL, |_| true, &stop, deadline, tick));
    }
    #[test]
    fn adopt_reuses_live_pid_and_terminate_kills_adopted_process() {
        let dir = std::env::temp_dir().join(format!("mk-tunnel-adopt-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("cloudflared");
        std::fs::write(&script, "#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tunnel = Tunnel::spawn_named(
            script.as_os_str(),
            4321,
            |_| true,
            &NamedTunnel {
                token: "eyJ-secret-token".into(),
                url: "adopted.krill.example".into(),
            },
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while tunnel.ready_url().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let pid = tunnel.pid().unwrap();
        let url = tunnel.ready_url().unwrap().to_owned();
        tunnel.set_persist(true);
        drop(tunnel);
        assert!(pid_alive(pid), "persisted tunnel process must survive Drop");

        let mut adopted = Tunnel::adopt(pid, url.clone()).unwrap();
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
    fn named_tunnel_passes_token_by_env_and_is_ready_on_first_200() {
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
        assert_eq!(tunnel.url(), "https://krill.example.com");
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
        assert!(Tunnel::adopt(pid, "https://adopted.krill.example".into()).is_none());
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
