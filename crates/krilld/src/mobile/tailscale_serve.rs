//! Private HTTPS endpoint. The guard owns exactly one foreground Serve child.
use crate::session::lock;
use serde_json::Value;
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const READY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
enum Progress {
    Preparing,
    Ready(String),
    Failed(String, Instant),
}
#[derive(Debug)]
pub(super) struct Serve {
    port: u16,
    progress: Arc<Mutex<Progress>>,
    cancelled: Arc<AtomicBool>,
}
impl Drop for Serve {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
impl Serve {
    pub(super) fn start(port: u16, origin: String, dir: Option<PathBuf>) -> io::Result<Self> {
        let progress = Arc::new(Mutex::new(Progress::Preparing));
        let cancelled = Arc::new(AtomicBool::new(false));
        let state = Arc::clone(&progress);
        let stop = Arc::clone(&cancelled);
        thread::Builder::new()
            .name("mobile-tailscale".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                if let Err(error) = serve(port, &origin, dir.as_deref(), &state, &stop) {
                    *lock(&state) = Progress::Failed(format!(
                        "Tailscale HTTPS setup failed: {error}. Check that Tailscale is running and HTTPS certificates are enabled for your tailnet, then create a new QR code."
                    ), Instant::now());
                }
            })?;
        Ok(Self {
            port,
            progress,
            cancelled,
        })
    }
    pub(super) fn retry_due(&self) -> bool {
        matches!(&*lock(&self.progress), Progress::Failed(_, failed) if failed.elapsed() >= Duration::from_secs(30))
    }
    pub(super) fn matches_port(&self, port: u16) -> bool {
        self.port == port
    }
    #[cfg(test)]
    pub(super) fn fixture(port: u16, origin: String) -> Self {
        Self {
            port,
            progress: Arc::new(Mutex::new(Progress::Ready(origin))),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
    pub(super) fn ready(&self) -> Option<String> {
        match &*lock(&self.progress) {
            Progress::Ready(origin) => Some(origin.clone()),
            _ => None,
        }
    }
    pub(super) fn error(&self) -> Option<String> {
        match &*lock(&self.progress) {
            Progress::Failed(error, _) => Some(error.clone()),
            _ => None,
        }
    }
}
fn drain(reader: &mut impl Read, output: &mut Vec<u8>) -> io::Result<bool> {
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                output.extend_from_slice(buffer.get(..n).ok_or(io::ErrorKind::InvalidData)?);
                if output.len() > 4 * 1024 * 1024 {
                    return Err(io::Error::other("Tailscale command output exceeded 4 MiB"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}
fn command_output(command: &mut Command) -> io::Result<Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()?;
    let result = (|| {
        let mut stdout =
            crate::child::PipeReader::new(child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?)?;
        let mut stderr =
            crate::child::PipeReader::new(child.stderr.take().ok_or(io::ErrorKind::BrokenPipe)?)?;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let until = Instant::now() + COMMAND_TIMEOUT;
        loop {
            let stdout_eof = drain(&mut stdout, &mut out)?;
            let stderr_eof = drain(&mut stderr, &mut err)?;
            if let Some(status) = child.try_wait()?
                && stdout_eof
                && stderr_eof
            {
                return Ok(Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            if Instant::now() >= until {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Tailscale command timed out",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        child.kill().ok();
    }
    child.wait().ok();
    result
}
fn json_command(binary: &std::path::Path, args: &[&str]) -> io::Result<Value> {
    let output = command_output(Command::new(binary).args(args))?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(io::Error::other)
}
fn binary() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("KRILL_TAILSCALE_BIN") {
        return Ok(PathBuf::from(path));
    }
    if cfg!(test) || std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        return Err(io::Error::other(
            "Tailscale Serve is disabled for offline fixtures",
        ));
    }
    #[cfg(target_os = "macos")]
    for path in [
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/opt/homebrew/bin/tailscale",
        "/usr/local/bin/tailscale",
    ] {
        if std::path::Path::new(path).is_file() {
            return Ok(PathBuf::from(path));
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let name = if cfg!(windows) {
            "tailscale.exe"
        } else {
            "tailscale"
        };
        if let Some(path) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|path| path.is_file())
        }) {
            return Ok(path);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Tailscale CLI was not found",
    ))
}
fn occupied_ports(config: &Value, ports: &mut HashSet<u16>) {
    if let Some(tcp) = config.get("TCP").and_then(Value::as_object) {
        ports.extend(tcp.keys().filter_map(|port| port.parse::<u16>().ok()));
    }
    for key in ["Web", "AllowFunnel"] {
        if let Some(entries) = config.get(key).and_then(Value::as_object) {
            ports.extend(
                entries
                    .keys()
                    .filter_map(|authority| authority.rsplit_once(':')?.1.parse::<u16>().ok()),
            );
        }
    }
    if let Some(foreground) = config.get("Foreground").and_then(Value::as_object) {
        for child in foreground.values() {
            occupied_ports(child, ports);
        }
    }
}
fn choose_port(config: &Value, preferred: Option<u16>) -> io::Result<u16> {
    let mut occupied = HashSet::new();
    occupied_ports(config, &mut occupied);
    preferred
        .into_iter()
        .chain(18449..18549)
        .find(|port| *port > 1024 && !occupied.contains(port))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                "No unused private HTTPS port is available",
            )
        })
}
fn trusted_websocket(origin: &str, mobile_origin: &str) -> bool {
    let Ok(output) = command_output(Command::new("curl").args([
        "--silent",
        "--show-error",
        "--http1.1",
        "--include",
        "--no-buffer",
        "--connect-timeout",
        "2",
        "--max-time",
        "2",
        "--noproxy",
        "*",
        "-H",
        "Connection: Upgrade",
        "-H",
        "Upgrade: websocket",
        "-H",
        "Sec-WebSocket-Version: 13",
        "-H",
        "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==",
        "-H",
        &format!("Origin: {mobile_origin}"),
        &format!("{origin}/ws"),
    ])) else {
        return false;
    };
    let bytes = output.stdout;
    bytes.starts_with(b"HTTP/1.1 101")
        && bytes
            .windows(b"\"version\":2".len())
            .any(|window| window == b"\"version\":2")
}
fn serve(
    port: u16,
    mobile_origin: &str,
    dir: Option<&std::path::Path>,
    progress: &Mutex<Progress>,
    stop: &AtomicBool,
) -> io::Result<()> {
    let cli = binary()?;
    let status = json_command(&cli, &["status", "--json"])?;
    if status.get("BackendState").and_then(Value::as_str) != Some("Running") {
        return Err(io::Error::other("Tailscale is not connected"));
    }
    let domain = status
        .get("CertDomains")
        .and_then(Value::as_array)
        .and_then(|domains| domains.first())
        .and_then(Value::as_str)
        .filter(|domain| {
            domain.ends_with(".ts.net")
                && domain
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        })
        .ok_or_else(|| io::Error::other("Tailscale has no trusted HTTPS certificate domain"))?;
    let receipt_path = dir.map(|dir| dir.join("mobile-tailscale-serve.json"));
    let preferred = receipt_path
        .as_deref()
        .and_then(|path| super::devices::read_private(path).ok().flatten())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|receipt| {
            receipt
                .get("https_port")?
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
        });
    let config = json_command(&cli, &["serve", "status", "--json"])?;
    let https_port = choose_port(&config, preferred)?;
    if let Some(path) = receipt_path {
        super::devices::write_private(
            &path,
            &serde_json::to_vec(&serde_json::json!({
                "https_port": https_port, "loopback_port": port, "domain": domain,
            }))
            .map_err(io::Error::other)?,
        )?;
    }
    let origin = format!("https://{domain}:{https_port}");
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let executable = std::env::current_exe()?;
    if executable.file_stem().is_none_or(|name| name != "krilld") {
        return Err(io::Error::other(
            "The krilld Serve guard is unavailable in this process",
        ));
    }
    let mut guard = Command::new(executable)
        .arg("--mobile-tailscale-serve-guard")
        .arg(cli)
        .arg(https_port.to_string())
        .arg(port.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let owner = guard.stdin.take();
    let result = (|| {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            if let Some(status) = guard.try_wait()? {
                return Err(io::Error::other(format!(
                    "Tailscale Serve exited ({status})"
                )));
            }
            if trusted_websocket(&origin, mobile_origin) {
                *lock(progress) = Progress::Ready(origin);
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "The private HTTPS WebSocket did not become ready",
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
        while !stop.load(Ordering::Acquire) {
            if guard.try_wait()?.is_some() {
                return Err(io::Error::other("Tailscale Serve stopped"));
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    })();
    drop(owner);
    wait_guard(&mut guard);
    result
}
fn wait_guard(child: &mut Child) {
    let until = Instant::now() + COMMAND_TIMEOUT;
    while child.try_wait().ok().flatten().is_none() && Instant::now() < until {
        thread::sleep(Duration::from_millis(25));
    }
    if child.try_wait().ok().flatten().is_none() {
        child.kill().ok();
    }
    child.wait().ok();
}
/// Runs before daemon startup. EOF means its owning daemon exited, including SIGKILL.
pub fn run_guard(mut args: impl Iterator<Item = OsString>) -> ExitCode {
    let (Some(cli), Some(https), Some(loopback)) = (args.next(), args.next(), args.next()) else {
        return ExitCode::from(2);
    };
    let ports = https
        .to_str()
        .and_then(|port| port.parse::<u16>().ok())
        .zip(loopback.to_str().and_then(|port| port.parse::<u16>().ok()));
    let Some((https, loopback)) = ports.filter(|(https, loopback)| *https > 1024 && *loopback > 0)
    else {
        return ExitCode::from(2);
    };
    let Ok(child) = Command::new(cli)
        .arg("serve")
        .arg(format!("--https={https}"))
        .arg(format!("http://127.0.0.1:{loopback}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return ExitCode::FAILURE;
    };
    guard_child(child, io::stdin())
}
fn guard_child(mut child: Child, mut owner: impl Read + Send + 'static) -> ExitCode {
    let (tx, owner_closed) = std::sync::mpsc::channel();
    if thread::Builder::new()
        .name("serve-owner".into())
        .spawn(move || {
            let mut buffer = [0_u8; 32];
            while owner.read(&mut buffer).is_ok_and(|n| n > 0) {}
            tx.send(()).ok();
        })
        .is_err()
    {
        child.kill().ok();
        child.wait().ok();
        return ExitCode::FAILURE;
    }
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return ExitCode::FAILURE,
            Ok(None) => {}
            Err(_) => break,
        }
        match owner_closed.recv_timeout(Duration::from_millis(50)) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    child.kill().ok();
    child.wait().ok();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn guard_kills_and_reaps_its_child_when_owner_pipe_closes() {
        let child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let started = Instant::now();
        assert_eq!(
            guard_child(child, io::Cursor::new(Vec::<u8>::new())),
            ExitCode::SUCCESS
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    #[cfg(unix)]
    fn guard_exits_when_child_dies_with_owner_pipe_still_open() {
        let child = Command::new("/usr/bin/true").spawn().unwrap();
        let (owner, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let started = Instant::now();
        assert_eq!(guard_child(child, owner), ExitCode::FAILURE);
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(writer);
    }
    #[test]
    fn failed_endpoint_retries_after_bounded_backoff() {
        let serve = Serve::fixture(1234, "https://desktop.tail123.ts.net:18449".into());
        *lock(&serve.progress) = Progress::Failed("offline".into(), Instant::now());
        assert!(!serve.retry_due());
        *lock(&serve.progress) = Progress::Failed(
            "offline".into(),
            Instant::now().checked_sub(Duration::from_secs(31)).unwrap(),
        );
        assert!(serve.retry_due());
    }
    #[test]
    #[cfg(unix)]
    fn status_output_larger_than_the_pipe_capacity_is_drained_before_waiting() {
        let output = command_output(Command::new("python3").args([
            "-c",
            "import json,sys;sys.stderr.write('x'*200000);print(json.dumps({'Peer': 'x'*200000}))",
        ]))
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stderr.len(), 200_000);
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["Peer"]
                .as_str()
                .unwrap()
                .len(),
            200_000
        );
    }
    #[test]
    fn chooses_only_unused_private_ports_across_background_and_foreground() {
        let config = serde_json::json!({"TCP":{"443":{"HTTPS":true},"18449":{"HTTPS":true}},"AllowFunnel":{"desktop.ts.net:443":true,"desktop.ts.net:18452":true},"Foreground":{"other":{"TCP":{"18450":{"HTTPS":true}},"Web":{"desktop.ts.net:18451":{}}}}});
        let before = config.clone();
        assert_eq!(choose_port(&config, Some(443)).unwrap(), 18453);
        assert_eq!(config, before);
    }
}
