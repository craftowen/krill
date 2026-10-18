//! Cloudflare Quick Tunnel의 프로세스 수명. 키·pair token은 cloudflared 인자에 넣지 않는다.
use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

pub struct Tunnel {
    child: Option<Child>,
    pub url: String,
}
impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tunnel").finish_non_exhaustive()
    }
}
impl Tunnel {
    pub fn start(port: u16) -> io::Result<Self> {
        let binary =
            std::env::var_os("KRILL_CLOUDFLARED_BIN").unwrap_or_else(|| "cloudflared".into());
        Self::spawn(&binary, port, Duration::from_secs(15))
    }
    fn spawn(binary: &std::ffi::OsStr, port: u16, timeout: Duration) -> io::Result<Self> {
        let child = Command::new(binary)
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
        let mut tunnel = Self {
            child: Some(child),
            url: String::new(),
        };
        let mut stderr = tunnel
            .child
            .as_mut()
            .and_then(|c| c.stderr.take())
            .ok_or_else(|| io::Error::other("cloudflared stderr unavailable"))?;
        let (send, receive) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("mobile-tunnel-log".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let mut buffer = [0; 2048];
                let mut pending = String::new();
                let mut published = false;
                while let Ok(n) = stderr.read(&mut buffer) {
                    if n == 0 {
                        break;
                    }
                    if let Some(bytes) = buffer.get(..n) {
                        pending.push_str(&String::from_utf8_lossy(bytes));
                    }
                    if !published && let Some(url) = parse_url(&pending) {
                        send.send(url).ok();
                        published = true;
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
        tunnel.url = receive.recv_timeout(timeout).map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "cloudflared did not publish a Quick Tunnel URL",
            )
        })?;
        Ok(tunnel)
    }
    pub fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .is_none_or(|c| c.try_wait().is_ok_and(|s| s.is_none()))
    }
    #[cfg(test)]
    #[cfg(unix)]
    pub(super) fn fixture(port: u16) -> io::Result<Self> {
        if port == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            child: None,
            url: "https://fixture-mobile.trycloudflare.com".into(),
        })
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
        let mut tunnel = Tunnel::spawn(script.as_os_str(), 1234, Duration::from_secs(2)).unwrap();
        assert!(tunnel.running());
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
}
