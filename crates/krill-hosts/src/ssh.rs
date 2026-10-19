//! MK-099 · MK-131: 원격 krilld + SSH Unix 소켓 포워딩(M6).
//!
//! `krill` CLI와 `krill-app`의 Connection Manager(`HostManager`)가 공유하는 SSH 터널 계층.
//! UI 계층에는 이 모듈의 타입을 직접 노출하지 않고 [`crate::HostTarget`]과 [`crate::HostManager`]만
//! 노출해 추후 MK-130(iroh QUIC)이 같은 타깃 추상화에 로컬 포워딩 소켓만 꽂을 수 있게 한다.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
#[cfg(unix)]
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::daemon::{self, Connection};
use krill_proto::ipc::ClientHello;
use krill_proto::os::fs::DirBuilderExt as _;
use krill_proto::os::net::{UnixListener, UnixStream};
use serde::{Deserialize, Serialize};

/// 포워더(ssh 또는 `KRILL_SSH_BIN`)가 local 소켓을 열 때까지 기다리는 상한.
#[cfg(unix)]
const READY_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const READY_POLL: Duration = Duration::from_millis(20);
/// 이보다 오래된 포워딩 소켓만 잔재로 본다. 다른 프로세스가 막 bind한(아직 listen 전) 소켓을 지우지 않게.
#[cfg(unix)]
const STALE_FORWARD_AGE: Duration = Duration::from_secs(60);
/// 터널 소켓 Hello 읽기·쓰기 상한(`daemon::HELLO_TIMEOUT`과 같은 값).
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// 파일명에 쓸 호스트명 상한(UDS 104바이트 제한 여유).
const HOST_SLUG_MAX: usize = 24;
const HOSTS_FILE: &str = "hosts.toml";
/// 터널 ssh의 원격 명령. stdin(부모가 쥔 파이프)이 닫힐 때까지 기다린다.
const TUNNEL_KEEPALIVE: &str = "cat >/dev/null";
/// [`remote_query`] 둘째 줄 표식: 원격에 krill이 없다.
const MISSING_MARK: &str = "__KRILL_MISSING__";
/// [`remote_query`] 둘째 줄 표식: 원격 `krill ls`가 실패했다(뒤에 오류 첫 줄).
const WAKE_FAILED_MARK: &str = "__KRILL_WAKE_FAILED__";

static TUNNEL_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// SSH 접속 옵션(목적지, 포트, 키 파일, 원격 소켓).
///
/// `Tunnel`·연결 테스트·접속 명령·원격 `$HOME` 조회가 같은 argv 빌더를 공유한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshOptions {
    pub destination: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub identity_file: Option<String>,
    #[serde(default)]
    pub remote_socket: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct HostsFile {
    #[serde(default)]
    hosts: Vec<HostEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct HostEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    label: String,
    destination: String,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    identity_file: Option<String>,
    #[serde(default)]
    remote_socket: Option<String>,
}

impl SshOptions {
    pub fn new(destination: impl Into<String>) -> Self {
        Self {
            destination: destination.into(),
            port: None,
            identity_file: None,
            remote_socket: None,
        }
    }

    /// 목적지·포트·키 파일·원격 소켓 값의 인젝션 및 형식 유효성을 검사한다.
    pub fn validate(&self) -> io::Result<()> {
        if !valid_host(&self.destination) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid SSH destination: {}", self.destination),
            ));
        }
        if self.port == Some(0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid SSH port: 0",
            ));
        }
        if let Some(id_file) = &self.identity_file {
            let trimmed = id_file.trim();
            if !trimmed.is_empty() && !valid_identity_file(trimmed) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid SSH identity_file: {trimmed}"),
                ));
            }
        }
        if let Some(sock) = &self.remote_socket {
            let trimmed = sock.trim();
            if !trimmed.is_empty() && !valid_remote_socket(trimmed) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid remote_socket: {trimmed}"),
                ));
            }
        }
        Ok(())
    }

    /// 안정성용 기본 옵션(`ServerAliveInterval=15`, `ServerAliveCountMax=3`, `ConnectTimeout`)과
    /// 포트(`-p`)·키 파일(`-i`)을 조립하는 공통 argv 빌더.
    pub fn common_args(&self, connect_timeout_secs: u16) -> Vec<String> {
        // 처음 보는 호스트 키는 기록하고 믿는다(연결 테스트와 같은 수준). 바뀐 키는 ssh가 그대로 거부한다.
        let mut args = vec![
            "-o".to_owned(),
            "StrictHostKeyChecking=accept-new".to_owned(),
            "-o".to_owned(),
            "ServerAliveInterval=15".to_owned(),
            "-o".to_owned(),
            "ServerAliveCountMax=3".to_owned(),
            "-o".to_owned(),
            format!("ConnectTimeout={connect_timeout_secs}"),
        ];
        if let Some(port) = self.port {
            args.push("-p".to_owned());
            args.push(port.to_string());
        }
        if let Some(id_file) = &self.identity_file {
            let trimmed = id_file.trim();
            if !trimmed.is_empty() {
                args.push("-i".to_owned());
                args.push(trimmed.to_owned());
            }
        }
        args
    }

    /// 소켓 포워딩 터널(`Tunnel::start`)용 ssh 인자 목록.
    pub fn tunnel_args(&self, local: &Path, remote: &Path) -> Vec<String> {
        let mut args = self.common_args(10);
        let forward = format!("{}:{}", local.display(), remote.display());
        args.extend([
            "-o".to_owned(),
            "ExitOnForwardFailure=yes".to_owned(),
            "-o".to_owned(),
            "StreamLocalBindUnlink=yes".to_owned(),
            "-T".to_owned(),
            "-L".to_owned(),
            forward,
            self.destination.clone(),
            // `-N` 대신 stdin을 읽는 원격 명령: 부모(앱·CLI)가 어떻게 죽든 stdin 파이프가 닫히면 ssh가 끝난다.
            // `-N`이면 ssh가 부모 사망을 알 길이 없어 고아로 남았다(ppid 1 터널 누적 실측).
            TUNNEL_KEEPALIVE.to_owned(),
        ]);
        args
    }

    /// 원격 `$HOME`·소켓 번들 조회([`remote_query`])용 ssh 인자 목록.
    pub fn remote_home_args(&self) -> Vec<String> {
        let mut args = self.common_args(5);
        args.extend([
            "-o".to_owned(),
            "BatchMode=yes".to_owned(),
            self.destination.clone(),
            remote_query(),
        ]);
        args
    }

    /// 관리형 원격 프로브(OS·아키·홈·버전·소켓 번들, OWE-897)용 ssh 인자 목록.
    pub fn probe_args(&self) -> Vec<String> {
        let mut args = self.common_args(5);
        args.extend([
            "-o".to_owned(),
            "BatchMode=yes".to_owned(),
            self.destination.clone(),
            crate::managed::probe_command(),
        ]);
        args
    }

    /// 연결 테스트용 ssh 인자 목록.
    #[cfg(test)]
    pub fn test_args(&self) -> Vec<String> {
        let mut args = self.common_args(4);
        args.extend([
            "-o".to_owned(),
            "BatchMode=yes".to_owned(),
            self.destination.clone(),
            "uname -sr && ([ -x \"$HOME/.local/bin/krill\" ] || command -v krill >/dev/null 2>&1) && echo 'krill: ready' || echo 'krill: not installed'".to_owned(),
        ]);
        args
    }

    /// 대화형 터미널 접속 명령용 ssh 인자 목록.
    #[cfg(test)]
    pub fn connect_args(&self) -> Vec<String> {
        let mut args = vec!["-t".to_owned()];
        args.extend(self.common_args(10));
        args.push(self.destination.clone());
        args
    }
}

/// `hosts.toml`에서 `destination`·`label`·`id`가 일치하는 항목을 찾아 포트·키·원격 소켓 옵션을 채운다.
pub fn resolve_host_options(host: &str, state_dir: Option<&Path>) -> io::Result<SshOptions> {
    let trimmed = host.trim();
    if let Some(dir) = state_dir
        && let Ok(text) = std::fs::read_to_string(dir.join(HOSTS_FILE))
        && let Ok(parsed) = toml::from_str::<HostsFile>(&text)
        && let Some(entry) = parsed.hosts.into_iter().find(|h| {
            h.destination == trimmed || h.label == trimmed || (!h.id.is_empty() && h.id == trimmed)
        })
    {
        let opts = SshOptions {
            destination: entry.destination,
            port: entry.port,
            identity_file: entry.identity_file.filter(|s| !s.trim().is_empty()),
            remote_socket: entry.remote_socket.filter(|s| !s.trim().is_empty()),
        };
        opts.validate()?;
        return Ok(opts);
    }
    let opts = SshOptions::new(trimmed);
    opts.validate()?;
    Ok(opts)
}

/// `ssh` 대신 쓸 프로그램. 테스트에서 가짜 포워더를 주입한다.
pub fn ssh_program() -> OsString {
    std::env::var_os("KRILL_SSH_BIN").unwrap_or_else(|| OsString::from("ssh"))
}

/// 원격 상태 디렉터리 후보(`$HOME` 기준, 탐색 순서): macOS 새 번들 → macOS 옛 번들 → Linux.
fn remote_state_dirs() -> [String; 3] {
    [
        format!("Library/Application Support/{}", daemon::BUNDLE_ID),
        format!("Library/Application Support/{}", daemon::LEGACY_BUNDLE_ID),
        format!("{}/{}", daemon::LINUX_STATE_PARENT, daemon::BUNDLE_ID),
    ]
}

/// 원격 `$HOME`과 krilld 소켓이 있는 상태 디렉터리를 한 번에 묻는 명령(MK-170). 후보는 [`remote_state_dirs`] 순서로
/// 찾는다(구버전 원격·이관 전 원격·Linux 호환). 로그인 셸이 fish·tcsh여도 같게 돌도록 `/bin/sh -c '…'`로 감싼다.
///
/// Linux 원격은 찾기 전에 항상 원격 `krill ls`(`~/.local/bin/krill` → `PATH`)를 한 번 돌린다. 데몬이 없으면 띄우고
/// (재부팅한 VM), holder만 죽었으면 새 holder로 재결합한다(ARCHITECTURE §4.1). krill이 없거나 실행이 실패하면
/// 둘째 줄에 표식을 남겨 원인을 알린다. macOS 원격은 깨우지 않는다 — ssh 세션 안에서 띄운 데몬은 GUI·키체인 밖이다.
pub fn remote_query() -> String {
    let dirs = remote_state_dirs().map(|d| format!("\"{d}\"")).join(" ");
    let [.., linux] = remote_state_dirs();
    format!(
        "/bin/sh -c 'printf \"%s\\n\" \"$HOME\"; \
         f() {{ for d in {dirs}; do if [ -S \"$HOME/$d/run/krilld.sock\" ]; then printf %s \"$d\"; exit 0; fi; done; }}; \
         if [ \"$(uname -s)\" != Linux ]; then f; exit 0; fi; \
         k=\"$HOME/.local/bin/krill\"; [ -x \"$k\" ] || k=$(command -v krill); \
         if [ -z \"$k\" ]; then f; printf %s {MISSING_MARK}; exit 0; fi; \
         if ! e=$(\"$k\" ls </dev/null 2>&1 >/dev/null); then f; printf \"%s %s\" {WAKE_FAILED_MARK} \"$(printf %s \"$e\" | head -n 1)\"; exit 0; fi; \
         f; printf %s \"{linux}\"'"
    )
}

/// [`remote_query`] 답(첫 줄 `$HOME`, 있으면 둘째 줄에 상태 디렉터리)으로 원격 krilld 소켓 경로를 계산한다.
/// 둘째 줄이 없거나 후보가 아니면 macOS 새 번들 경로다.
pub fn remote_socket_for_home(remote_home: &str) -> io::Result<PathBuf> {
    let mut lines = remote_home.lines();
    let home = lines.next().unwrap_or_default().trim();
    if !valid_remote_socket(home) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid remote HOME path: {remote_home:?}"),
        ));
    }
    let dirs = remote_state_dirs();
    let found = lines.next().map(str::trim);
    if found == Some(MISSING_MARK) {
        return Err(io::Error::other(
            "krill is not installed on the remote host (expected ~/.local/bin/krill; see install-linux.sh)",
        ));
    }
    if let Some(detail) = found.and_then(|l| l.strip_prefix(WAKE_FAILED_MARK)) {
        return Err(io::Error::other(format!(
            "failed to start krill on the remote host: {}",
            detail.trim()
        )));
    }
    let [default, ..] = &dirs;
    let dir = dirs
        .iter()
        .find(|d| Some(d.as_str()) == found)
        .unwrap_or(default);
    // 원격 경로는 로컬 OS가 Windows여도 POSIX 구분자를 보존한다.
    Ok(PathBuf::from(format!("{home}/{dir}/run/krilld.sock")))
}

/// `ssh <opts> <host> <remote_query>`를 실행해 원격 맥의 `$HOME`과 소켓이 있는 번들 id를 조회한다.
pub fn query_remote_home(opts: &SshOptions) -> io::Result<String> {
    query_remote_home_with_program(opts, &ssh_program())
}

fn query_remote_home_with_program(
    opts: &SshOptions,
    ssh_bin: &std::ffi::OsStr,
) -> io::Result<String> {
    opts.validate()?;
    #[cfg(windows)]
    let out = windows::stdio::output(
        ssh_bin,
        &opts.remote_home_args(),
        None,
        Duration::from_secs(30),
    )?;
    #[cfg(unix)]
    let out = Command::new(ssh_bin)
        .args(opts.remote_home_args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = stderr.trim();
        return Err(io::Error::other(if detail.is_empty() {
            format!(
                "ssh remote HOME query to {} failed ({})",
                opts.destination, out.status
            )
        } else {
            format!(
                "ssh remote HOME query to {} failed: {detail}",
                opts.destination
            )
        }));
    }
    String::from_utf8(out.stdout).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// 원격 krilld 소켓 경로 결정:
/// 1) `KRILL_REMOTE_SOCK` 환경변수
/// 2) `SshOptions::remote_socket`(`hosts.toml`의 `remote_socket`)
/// 3) 원격 `HOME`·소켓 번들 질의([`remote_query`], 새 번들 → 옛 번들) 기반 계산
pub fn resolve_remote_socket(opts: &SshOptions) -> io::Result<PathBuf> {
    resolve_remote_socket_with_program(opts, &ssh_program())
}

/// HOME 조회와 터널 시작에 같은 SSH 실행 파일을 사용한다. 명시한 소켓은 HOME 조회 없이 반환한다.
pub fn resolve_remote_socket_with_program(
    opts: &SshOptions,
    ssh_bin: &std::ffi::OsStr,
) -> io::Result<PathBuf> {
    if let Some(sock) = std::env::var_os("KRILL_REMOTE_SOCK") {
        let Some(s) = sock.to_str().filter(|s| !s.is_empty()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "KRILL_REMOTE_SOCK is empty or non-UTF-8",
            ));
        };
        if !valid_remote_socket(s) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid KRILL_REMOTE_SOCK: {s}"),
            ));
        }
        return Ok(PathBuf::from(s));
    }
    if let Some(sock) = opts
        .remote_socket
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !valid_remote_socket(sock) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid remote_socket: {sock}"),
            ));
        }
        return Ok(PathBuf::from(sock));
    }
    let remote_home = query_remote_home_with_program(opts, ssh_bin)?;
    remote_socket_for_home(&remote_home)
}

/// 호스트 문자열 검사. ssh 마지막 인자로 그대로 가므로 플래그·경로·공백을 거부한다.
pub fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && host
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'/' | b':' | b'@' | b'\\'))
        || valid_user_at_host(host)
}

/// 키 파일 경로 검사. 플래그 인젝션(`-`로 시작)과 제어문자를 거부한다.
pub fn valid_identity_file(path: &str) -> bool {
    let s = path.trim();
    !s.is_empty() && !s.starts_with('-') && !s.chars().any(char::is_control)
}

/// 원격 소켓(또는 원격 HOME) 경로 검사. 절대 경로(`/`)여야 하며 `-L` 구분자(`:`)와 제어문자를 거부한다.
pub fn valid_remote_socket(path: &str) -> bool {
    let s = path.trim();
    s.starts_with('/') && !s.contains(':') && !s.chars().any(char::is_control)
}

/// `user@host` 형태. `@`는 정확히 하나, user·host는 그래픽 문자.
fn valid_user_at_host(host: &str) -> bool {
    let mut parts = host.split('@');
    let (Some(user), Some(h), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    for s in [user, h] {
        if s.is_empty()
            || s.starts_with('-')
            || !s
                .bytes()
                .all(|b| b.is_ascii_graphic() && !matches!(b, b'/' | b':' | b'\\'))
        {
            return false;
        }
    }
    true
}

/// 파일명에 쓸 호스트 별명. `@`는 살리고 나머지는 영숫자·`.-`_ 로 바꾼다.
pub fn slug(host: &str) -> String {
    let mut out = String::with_capacity(host.len());
    for c in host.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.len() > HOST_SLUG_MAX {
        out.truncate(HOST_SLUG_MAX);
    }
    out
}

/// 이 터널 인스턴스 전용 로컬 포워딩 소켓(`<상태>/run/fwd-<slug>-<pid>-<seq>.sock`).
/// 동시 실행 중인 다른 `krill --host` 프로세스의 터널 소켓과 충돌하지 않는다.
pub fn local_socket(host: &str) -> io::Result<PathBuf> {
    let state = daemon::state_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    local_socket_in(&state, host)
}

/// 지정한 상태 디렉터리 아래에 이 터널 인스턴스 전용 로컬 포워딩 소켓 경로를 만든다.
pub fn local_socket_in(state: &Path, host: &str) -> io::Result<PathBuf> {
    let pid = std::process::id();
    let seq = TUNNEL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut path = state.join(format!("run/fwd-{}-{pid}-{seq}.sock", slug(host)));
    if path.as_os_str().len() >= daemon::SUN_PATH_MAX {
        path = state.join(format!("run/f-{pid}-{seq}.sock"));
    }
    if path.as_os_str().len() >= daemon::SUN_PATH_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("forward socket path too long for {}", path.display()),
        ));
    }
    Ok(path)
}

/// 인자에서 `--host <값>`을 꺼낸다. `--` 뒤(리터럴 프로그램·전송 텍스트)는 건드리지 않는다.
pub fn extract_host(args: &[OsString]) -> Result<(Option<String>, Vec<OsString>), String> {
    let mut host = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut it = args.iter();
    let state = daemon::state_dir();
    while let Some(arg) = it.next() {
        if arg == "--" {
            rest.push(arg.clone());
            rest.extend(it.cloned());
            break;
        }
        if arg == "--host" {
            if host.is_some() {
                return Err("duplicate --host".to_owned());
            }
            let value = it.next().ok_or("missing --host value")?;
            let value = value
                .to_str()
                .filter(|s| valid_host(s) || resolve_host_options(s, state.as_deref()).is_ok())
                .ok_or("invalid --host value (expected user@host or host)")?;
            host = Some(value.to_owned());
        } else {
            rest.push(arg.clone());
        }
    }
    Ok((host, rest))
}

/// 살아 있는 SSH(또는 모의) 포워더. Drop 시 자식을 죽이고 소켓 파일을 치운다.
///
/// Windows는 아직 미지원이다(`windows.rs`). 공개 API는 같다.
#[cfg(unix)]
#[derive(Debug)]
pub struct Tunnel {
    child: Child,
    socket: PathBuf,
}

impl Tunnel {
    pub fn start(host: &str) -> io::Result<Self> {
        let state = daemon::state_dir();
        let opts = resolve_host_options(host, state.as_deref())?;
        Self::start_with_options(&opts)
    }

    pub fn start_with_options(opts: &SshOptions) -> io::Result<Self> {
        opts.validate()?;
        let remote = resolve_remote_socket(opts)?;
        let local = local_socket(&opts.destination)?;
        Self::start_at(opts, local, &remote)
    }

    pub fn start_at(opts: &SshOptions, local: PathBuf, remote: &Path) -> io::Result<Self> {
        let prog = ssh_program();
        Self::start_at_with_program(opts, local, remote, &prog)
    }
}

#[cfg(unix)]
impl Tunnel {
    pub fn start_at_with_program(
        opts: &SshOptions,
        local: PathBuf,
        remote: &Path,
        ssh_bin: &std::ffi::OsStr,
    ) -> io::Result<Self> {
        opts.validate()?;
        if let Some(dir) = local.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
            sweep_stale_forwards(dir, STALE_FORWARD_AGE);
        }
        std::fs::remove_file(&local).ok();
        let host = &opts.destination;
        // stdin 파이프의 쓰기 쪽은 `child`가 쥔다. 이 프로세스가 죽으면 닫혀 원격 `cat`과 ssh가 끝난다.
        let mut child = Command::new(ssh_bin)
            .args(opts.tunnel_args(&local, remote))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if local.exists() {
                if let Some(status) = child.try_wait()? {
                    std::fs::remove_file(&local).ok();
                    return Err(io::Error::other(format!(
                        "ssh forwarding to {host} failed ({status}); check the host and KRILL_REMOTE_SOCK"
                    )));
                }
                return Ok(Self {
                    child,
                    socket: local,
                });
            }
            if let Some(status) = child.try_wait()? {
                std::fs::remove_file(&local).ok();
                return Err(io::Error::other(format!(
                    "ssh forwarding to {host} failed ({status}); check the host and KRILL_REMOTE_SOCK"
                )));
            }
            if Instant::now() >= deadline {
                child.kill().ok();
                child.wait().ok();
                std::fs::remove_file(&local).ok();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("ssh forwarding to {host} timed out"),
                ));
            }
            thread::sleep(READY_POLL);
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// 자식 포워더 프로세스가 아직 살아 있는지 확인한다.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

#[cfg(unix)]
impl Drop for Tunnel {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
        std::fs::remove_file(&self.socket).ok();
    }
}

/// 소유 프로세스가 SIGKILL로 죽어 `Drop`이 못 지운 포워딩 소켓(`fwd-*`·`f-*`)을 지운다.
/// 듣는 쪽이 없어 연결이 거부되는 것만 지우므로 살아 있는 다른 터널은 남는다.
#[cfg(unix)]
fn sweep_stale_forwards(dir: &Path, min_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("fwd-") || name.starts_with("f-")) || !name.ends_with(".sock") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age >= min_age));
        let path = entry.path();
        if old
            && UnixStream::connect(&path)
                .is_err_and(|e| e.kind() == io::ErrorKind::ConnectionRefused)
        {
            std::fs::remove_file(&path).ok();
        }
    }
}

/// 로컬/원격 접속 대상. 원격은 호출당 터널 하나를 들고 모든 연결이 그 소켓으로 간다.
/// 로컬 데몬 자동 기동은 로컬 대상에서만 일어난다(원격 실패가 로컬 세션을 건드리지 않는다).
#[derive(Debug)]
pub struct Target {
    host: Option<String>,
    tunnel: Option<Tunnel>,
}

impl Target {
    /// `None`이면 로컬, `Some`이면 터널을 맺고 원격.
    ///
    /// # Errors
    /// 원격 터널 기동 실패(ssh 오류·타임아웃·소켓 경로 오류).
    pub fn establish(host: Option<String>) -> io::Result<Self> {
        let tunnel = host.as_deref().map(Tunnel::start).transpose()?;
        Ok(Self { host, tunnel })
    }

    pub fn is_remote(&self) -> bool {
        self.host.is_some()
    }

    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// 이미 떠 있는 데몬에 붙는다(어느 쪽도 새로 띄우지 않는다).
    ///
    /// # Errors
    /// 소켓 연결·Hello 협상 실패.
    pub fn dial(&self, hello: &ClientHello, timeout: Duration) -> io::Result<Connection> {
        if let Some(t) = &self.tunnel {
            return dial_tunnel(t.socket(), hello, timeout);
        }
        let paths = daemon::Paths::dev()?;
        daemon::handshake(&paths.socket, hello, timeout)
    }

    /// 붙는다. 로컬은 없으면 holder를 띄우고, 원격은 터널 소켓에만 붙는다.
    ///
    /// # Errors
    /// 로컬 기동 실패 또는 원격 Hello 협상 실패.
    pub fn connect(&self, hello: &ClientHello) -> Result<Connection, daemon::Error> {
        if let Some(t) = &self.tunnel {
            return dial_tunnel(t.socket(), hello, HELLO_TIMEOUT).map_err(daemon::Error::Io);
        }
        let paths = daemon::Paths::dev().map_err(daemon::Error::Io)?;
        daemon::connect(&paths, hello)
    }
}

pub fn dial_tunnel(
    socket: &Path,
    hello: &ClientHello,
    timeout: Duration,
) -> io::Result<Connection> {
    let deadline = Instant::now() + Duration::from_millis(200);
    loop {
        match daemon::handshake(socket, hello, timeout) {
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            other => return other,
        }
    }
}

/// `krill __forward <local> <remote>`: 한 소켓에서 받아 다른 소켓으로 그대로 흘린다.
/// 실제 ssh 대신 테스트(`KRILL_SSH_BIN`)에서 쓰는 숨은 하위 명령. 영원히 돌다 죽는다.
pub fn forward_main(args: &[OsString]) -> ExitCode {
    let paths: Vec<PathBuf> = args.iter().skip(1).map(PathBuf::from).collect();
    let [local, remote] = paths.as_slice() else {
        eprintln!("usage: krill __forward <local-sock> <remote-sock>");
        return ExitCode::from(2);
    };
    match serve(local, remote) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("__forward: {e}");
            ExitCode::FAILURE
        }
    }
}

fn serve(local: &Path, remote: &Path) -> io::Result<()> {
    if let Some(dir) = local.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    std::fs::remove_file(local).ok();
    let listener = UnixListener::bind(local)?;
    for down in listener.incoming() {
        let Ok(down) = down else { break };
        let Ok(up) = UnixStream::connect(remote) else {
            continue;
        };
        thread::spawn(move || proxy(down, up));
    }
    Ok(())
}

fn proxy(down: UnixStream, up: UnixStream) {
    let Ok(down_back) = down.try_clone() else {
        return;
    };
    let Ok(up_back) = up.try_clone() else { return };
    let one = thread::spawn(move || {
        let mut from = down;
        let mut to = up_back;
        io::copy(&mut from, &mut to).ok();
        to.shutdown(std::net::Shutdown::Write).ok();
    });
    let mut from = up;
    let mut to = down_back;
    io::copy(&mut from, &mut to).ok();
    to.shutdown(std::net::Shutdown::Write).ok();
    one.join().ok();
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::Tunnel;
#[cfg(windows)]
pub(crate) use windows::stdio::output as windows_output;

#[cfg(test)]
mod tests;
