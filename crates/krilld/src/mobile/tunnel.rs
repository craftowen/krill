//! Cloudflare Quick Tunnel의 프로세스 수명. 키·pair token은 cloudflared 인자에 넣지 않는다.
use crate::session::lock;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
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
        Self::spawn(binary, port, probe, READY_SETTLE, Some(publication))
    }
    fn spawn(
        binary: &OsStr,
        port: u16,
        probe: Probe,
        settle: Duration,
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
                                        publication.as_ref().filter(|_| enabled),
                                        &ready,
                                        &fixed,
                                        &stop,
                                        publish,
                                    );
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
    #[cfg(unix)]
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
/// 판정이 끝난 뒤 ready를 공개한다. `publication`은 고정 출처가 확인됐을 때만 온다.
/// 게시 실패는 연결 준비를 막지 않고 취소 가능한 백오프로 재시도한다.
fn prepare(
    url: &str,
    publication: Option<&Publication>,
    ready: &AtomicBool,
    fixed: &AtomicBool,
    stop: &Stop,
    mut put: impl FnMut(&Publication, &str) -> bool,
) {
    let enabled = publication.is_some();
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
                enabled.then_some(&p),
                &ready,
                &fixed,
                &Stop::default(),
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
}
