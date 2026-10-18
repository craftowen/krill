//! Cloudflare Quick Tunnel의 프로세스 수명. 키·pair token은 cloudflared 인자에 넣지 않는다.
use crate::session::lock;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

/// URL이 찍힌 뒤에도 엣지 등록·DNS 전파 전에는 530/NXDOMAIN이 난다(실측 1.8~34초).
/// 프로브가 200을 받아야 `ready`이고, 이 시간 안에 준비되지 않으면 감시자가 다시 띄운다.
pub const READY_DEADLINE: Duration = Duration::from_secs(90);
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
    url: Arc<OnceLock<String>>,
    ready: Arc<AtomicBool>,
    started: Instant,
    fixed: Arc<AtomicBool>,
    stop: Arc<Stop>,
}
impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tunnel").finish_non_exhaustive()
    }
}
impl Tunnel {
    /// 즉시 반환한다. URL 발행과 준비 확인은 백그라운드 스레드가 하므로 게이트웨이 락을 잡고 기다리지 않는다.
    pub(super) fn start(binary: &OsStr, port: u16, publication: Publication) -> io::Result<Self> {
        Self::spawn(binary, port, probe, Some(publication))
    }
    fn spawn(
        binary: &OsStr,
        port: u16,
        probe: Probe,
        publication: Option<Publication>,
    ) -> io::Result<Self> {
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
            .map_err(|e| io::Error::new(e.kind(), format!("cloudflared 실행 실패: {e}")))?;
        let stderr = child.stderr.take();
        let tunnel = Self {
            child: Some(child),
            url: Arc::new(OnceLock::new()),
            ready: Arc::new(AtomicBool::new(false)),
            started: Instant::now(),
            fixed: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(Stop::default()),
        };
        let mut stderr =
            stderr.ok_or_else(|| io::Error::other("cloudflared stderr unavailable"))?;
        let (url, ready) = (Arc::clone(&tunnel.url), Arc::clone(&tunnel.ready));
        let fixed = Arc::clone(&tunnel.fixed);
        let stop = Arc::clone(&tunnel.stop);
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
                        let fixed = Arc::clone(&fixed);
                        let stop = Arc::clone(&stop);
                        let publication = publication.clone();
                        thread::Builder::new()
                            .name("mobile-tunnel-probe".into())
                            .stack_size(128 * 1024)
                            .spawn(move || {
                                let deadline = Instant::now() + READY_DEADLINE;
                                while Instant::now() < deadline && !stop.cancelled() {
                                    if probe(&found) {
                                        prepare(
                                            &found,
                                            publication.as_ref(),
                                            &ready,
                                            &fixed,
                                            &stop,
                                            |url| http_status(url) == Some(200),
                                            publish,
                                        );
                                        return;
                                    }
                                    if stop.wait(Duration::from_millis(500)) {
                                        return;
                                    }
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
    pub fn fixed(&self) -> bool {
        self.fixed.load(Ordering::Acquire)
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
            fixed: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(Stop::default()),
        })
    }
}
#[cfg(test)]
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
}
impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(child) = &mut self.child {
            child.kill().ok();
            child.wait().ok();
        }
    }
}
/// 판정이 끝난 뒤 ready를 공개한다. 게시 실패는 연결 준비를 막지 않고 취소 가능한 백오프로 재시도한다.
fn prepare(
    url: &str,
    publication: Option<&Publication>,
    ready: &AtomicBool,
    fixed: &AtomicBool,
    stop: &Stop,
    available: impl FnOnce(&str) -> bool,
    mut put: impl FnMut(&Publication, &str) -> bool,
) {
    let enabled = publication.is_some_and(|p| {
        available(&format!(
            "{}/m/{}/manifest.json",
            p.origin,
            super::pwa::asset_hash()
        ))
    });
    if stop.cancelled() {
        return;
    }
    fixed.store(enabled, Ordering::Release);
    ready.store(true, Ordering::Release);
    if let Some(p) = publication.filter(|_| enabled) {
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
    let out = Command::new("curl")
        .args([
            "-sS",
            "-o",
            "/dev/null",
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
    let Ok(mut child) = Command::new("curl")
        .args([
            "-sS",
            "-o",
            "/dev/null",
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
/// 로컬 리졸버의 NXDOMAIN 음성 캐시(60초)를 피하려고 apex로 연결하고 SNI·Host만 터널 이름을 쓴다.
/// curl 실행 실패·네트워크 오류는 준비 완료로 취급하지 않는다.
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
    fn availability_is_decided_before_ready_and_failure_falls_back() {
        for enabled in [true, false] {
            let ready = AtomicBool::new(false);
            let fixed = AtomicBool::new(false);
            let mut puts = 0;
            prepare(
                "https://fixture.trycloudflare.com",
                Some(&publication()),
                &ready,
                &fixed,
                &Stop::default(),
                |url| {
                    assert!(!ready.load(Ordering::Acquire));
                    assert_eq!(
                        url,
                        format!(
                            "https://stage.test/m/{}/manifest.json",
                            super::super::pwa::asset_hash()
                        )
                    );
                    enabled
                },
                |_, _| {
                    assert!(ready.load(Ordering::Acquire));
                    assert!(fixed.load(Ordering::Acquire));
                    puts += 1;
                    true
                },
            );
            assert!(ready.load(Ordering::Acquire));
            assert_eq!(fixed.load(Ordering::Acquire), enabled);
            assert_eq!(puts, usize::from(enabled));
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
                &AtomicBool::new(false),
                &AtomicBool::new(false),
                &stop,
                |_| true,
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
        let mut tunnel = Tunnel::spawn(script.as_os_str(), 1234, |_| true, None).unwrap();
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
        let mut tunnel = Tunnel::spawn(script.as_os_str(), 1234, |_| false, None).unwrap();
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
