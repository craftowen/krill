//! Cloudflare Quick Tunnel의 프로세스 수명. 키·pair token은 cloudflared 인자에 넣지 않는다.
use std::ffi::OsStr;
use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

/// URL이 찍힌 뒤에도 엣지 등록·DNS 전파 전에는 530/NXDOMAIN이 난다(실측 1.8~34초).
/// 프로브가 200을 받아야 `ready`이고, 이 시간 안에 준비되지 않으면 감시자가 다시 띄운다.
pub const READY_DEADLINE: Duration = Duration::from_secs(90);
type Probe = fn(&str) -> bool;

pub struct Tunnel {
    child: Option<Child>,
    url: Arc<OnceLock<String>>,
    ready: Arc<AtomicBool>,
    started: Instant,
}
impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tunnel").finish_non_exhaustive()
    }
}
impl Tunnel {
    /// 즉시 반환한다. URL 발행과 준비 확인은 백그라운드 스레드가 하므로 게이트웨이 락을 잡고 기다리지 않는다.
    pub fn start(port: u16) -> io::Result<Self> {
        let binary =
            std::env::var_os("KRILL_CLOUDFLARED_BIN").unwrap_or_else(|| "cloudflared".into());
        Self::spawn(&binary, port, probe)
    }
    fn spawn(binary: &OsStr, port: u16, probe: Probe) -> io::Result<Self> {
        let mut child = Command::new(binary)
            .args([
                "tunnel",
                "--no-autoupdate",
                "--url",
                &format!("http://127.0.0.1:{port}"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    "cloudflared unavailable; install with brew install cloudflared",
                )
            })?;
        let stderr = child.stderr.take();
        let tunnel = Self {
            child: Some(child),
            url: Arc::new(OnceLock::new()),
            ready: Arc::new(AtomicBool::new(false)),
            started: Instant::now(),
        };
        let mut stderr =
            stderr.ok_or_else(|| io::Error::other("cloudflared stderr unavailable"))?;
        let (url, ready) = (Arc::clone(&tunnel.url), Arc::clone(&tunnel.ready));
        thread::Builder::new()
            .name("mobile-tunnel-log".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let mut buffer = [0; 2048];
                let mut pending = String::new();
                while let Ok(n) = stderr.read(&mut buffer) {
                    if n == 0 {
                        break;
                    }
                    if let Some(bytes) = buffer.get(..n) {
                        pending.push_str(&String::from_utf8_lossy(bytes));
                    }
                    if url.get().is_none()
                        && let Some(found) = parse_url(&pending)
                    {
                        url.set(found.clone()).ok();
                        let ready = Arc::clone(&ready);
                        thread::Builder::new()
                            .name("mobile-tunnel-probe".into())
                            .stack_size(128 * 1024)
                            .spawn(move || {
                                let deadline = Instant::now() + READY_DEADLINE;
                                while Instant::now() < deadline {
                                    if probe(&found) {
                                        ready.store(true, Ordering::Release);
                                        return;
                                    }
                                    thread::sleep(Duration::from_millis(500));
                                }
                            })
                            .ok();
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
    /// cloudflared가 찍은 URL. 준비 전일 수 있다.
    pub fn url(&self) -> Option<&str> {
        self.url.get().map(String::as_str)
    }
    /// 공개 경로로 실제 HTTP 200을 받은 URL.
    pub fn ready_url(&self) -> Option<&str> {
        self.url().filter(|_| self.ready.load(Ordering::Acquire))
    }
    /// 프로세스가 살아 있고 준비 기한을 넘기지 않았다. 아니면 감시자가 교체한다.
    pub fn healthy(&mut self) -> bool {
        let alive = self
            .child
            .as_mut()
            .is_none_or(|c| c.try_wait().is_ok_and(|s| s.is_none()));
        alive && (self.ready.load(Ordering::Acquire) || self.started.elapsed() < READY_DEADLINE)
    }
    #[cfg(test)]
    pub(super) fn fixture(port: u16) -> io::Result<Self> {
        if port == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            child: None,
            url: Arc::new(OnceLock::from(
                "https://fixture-mobile.trycloudflare.com".to_owned(),
            )),
            ready: Arc::new(AtomicBool::new(true)),
            started: Instant::now(),
        })
    }
}
#[cfg(test)]
impl Tunnel {
    /// URL은 찍혔지만 준비 기한을 넘긴 터널.
    pub(super) fn stale(port: u16) -> io::Result<Self> {
        let mut tunnel = Self::fixture(port)?;
        tunnel.ready = Arc::new(AtomicBool::new(false));
        tunnel.started = Instant::now()
            .checked_sub(READY_DEADLINE + Duration::from_secs(1))
            .ok_or(io::ErrorKind::InvalidInput)?;
        Ok(tunnel)
    }
}
impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            child.kill().ok();
            child.wait().ok();
        }
    }
}
/// 로컬 리졸버의 NXDOMAIN 음성 캐시(60초)를 피하려고 apex로 연결하고 SNI·Host만 터널 이름을 쓴다.
/// curl이 없으면(일부 Linux) 확인을 건너뛰고 준비된 것으로 본다.
fn probe(url: &str) -> bool {
    let Some(host) = url.strip_prefix("https://") else {
        return false;
    };
    match Command::new("curl")
        .args([
            "-sS",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "--max-time",
            "5",
            "--connect-to",
            &format!("{host}:443:trycloudflare.com:443"),
            &format!("{url}/manifest.json"),
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(out) => out.stdout == b"200",
        Err(e) => e.kind() == io::ErrorKind::NotFound,
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
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
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
        let mut tunnel = Tunnel::spawn(script.as_os_str(), 1234, |_| true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
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
        let mut tunnel = Tunnel::spawn(script.as_os_str(), 1234, |_| false).unwrap();
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
}
