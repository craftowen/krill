//! SSH 호스트 PC-to-PC 원격 관리(`hosts.toml`, `~/.ssh/config` 가져오기, Tailscale 피어 감지,
//! 연결 테스트, 첫 연결 키 등록 도우미, 원격 세션 명령, MK-112 / MK-132).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde::{Deserialize, Serialize};

const HOSTS_FILE: &str = "hosts.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshHost {
    pub id: String,
    pub label: String,
    /// `user@host` 또는 `~/.ssh/config`의 Host 별칭.
    pub destination: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub identity_file: Option<String>,
    #[serde(default)]
    pub remote_socket: Option<String>,
    #[serde(default)]
    pub imported: bool,
    /// 첫 연결 도우미(`ssh-copy-id`)로 새 터미널 탭을 열 때만 `true`로 세팅되는 런타임 플래그.
    #[serde(skip)]
    pub copy_id_first: bool,
}

impl SshHost {
    /// 새 터미널 탭에서 `ssh` 대신 `ssh-copy-id`를 실행하도록 표시한 사본을 돌려준다.
    #[must_use]
    pub fn with_copy_id(mut self) -> Self {
        self.copy_id_first = true;
        self
    }

    /// 안정성 기본 옵션(`ServerAliveInterval=15`, `ServerAliveCountMax=3`, `ConnectTimeout`)과
    /// 포트(`-p`)·키 파일(`-i`)을 조립하는 공통 argv 빌더.
    #[must_use]
    pub fn common_ssh_args(&self, connect_timeout_secs: u16) -> Vec<String> {
        let mut args = vec![
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

    /// 연결 테스트(`test_connection`)용 ssh 인자 목록.
    #[must_use]
    pub fn test_args(&self) -> Vec<String> {
        let mut args = self.common_ssh_args(4);
        args.extend([
            "-o".to_owned(),
            "BatchMode=yes".to_owned(),
            "-o".to_owned(),
            "StrictHostKeyChecking=accept-new".to_owned(),
            self.destination.clone(),
            "uname -sr && ([ -x \"$HOME/.local/bin/krill\" ] || command -v krill >/dev/null 2>&1) && echo 'krill: ready' || echo 'krill: not installed'".to_owned(),
        ]);
        args
    }

    /// 소켓 포워딩 터널용 ssh 인자 목록.
    #[must_use]
    pub fn tunnel_args(&self, local: &Path, remote: &Path) -> Vec<String> {
        let mut args = self.common_ssh_args(10);
        let forward = format!("{}:{}", local.display(), remote.display());
        args.extend([
            "-o".to_owned(),
            "ExitOnForwardFailure=yes".to_owned(),
            "-o".to_owned(),
            "StreamLocalBindUnlink=yes".to_owned(),
            "-N".to_owned(),
            "-L".to_owned(),
            forward,
            self.destination.clone(),
        ]);
        args
    }

    /// 원격 `$HOME`·소켓 번들 조회(`krill_hosts::ssh::remote_query`, 새 번들 → 옛 번들)용 ssh 인자 목록.
    #[must_use]
    pub fn remote_home_args(&self) -> Vec<String> {
        let mut args = self.common_ssh_args(5);
        args.extend([
            "-o".to_owned(),
            "BatchMode=yes".to_owned(),
            self.destination.clone(),
            krill_hosts::ssh::remote_query(),
        ]);
        args
    }

    /// `remote_socket` 설정이 있으면 우선 사용하고, 없으면 원격 `HOME` 기준 기본 소켓 경로를 계산한다.
    #[must_use]
    pub fn resolve_remote_socket_from_home(&self, remote_home: &str) -> Option<PathBuf> {
        if let Some(sock) = &self.remote_socket {
            let trimmed = sock.trim();
            if !trimmed.is_empty() && valid_remote_socket(trimmed) {
                return Some(PathBuf::from(trimmed));
            }
        }
        remote_socket_for_home(remote_home)
    }
}

/// 원격 질의 답(`$HOME`, 있으면 소켓을 찾은 번들 id)으로 원격 krilld 소켓 경로를 계산한다(MK-170, 새 → 옛).
#[must_use]
pub fn remote_socket_for_home(remote_home: &str) -> Option<PathBuf> {
    if remote_home.contains('\0') {
        return None;
    }
    krill_hosts::ssh::remote_socket_for_home(remote_home).ok()
}

/// 호스트 입력 폼 검증 오류 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostInputError {
    /// 주소(`user@host` 또는 별칭)가 비어 있음.
    EmptyAddress,
    /// 주소 형식이 잘못되었거나 허용되지 않는 문자·옵션 인젝션(`-` 시작 등) 포함.
    InvalidAddress,
    /// 포트가 1~65535 범위의 숫자가 아님.
    InvalidPort,
    /// 키 파일 경로에 허용되지 않는 문자나 `-` 시작 인젝션이 포함됨.
    InvalidIdentityFile,
}

/// 검증을 통과한 호스트 추가 입력.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedHostInput {
    pub label: String,
    pub destination: String,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
}

/// 사용자 입력(이름·주소·포트·키 파일)을 검증하고 정규화한다.
pub fn validate_host_input(
    label: &str,
    address: &str,
    port_str: &str,
    identity_file_str: &str,
) -> Result<ValidatedHostInput, HostInputError> {
    let dest = address.trim();
    if dest.is_empty() {
        return Err(HostInputError::EmptyAddress);
    }
    if !valid_host(dest) {
        return Err(HostInputError::InvalidAddress);
    }
    let port_trimmed = port_str.trim();
    let port = if port_trimmed.is_empty() {
        None
    } else {
        match port_trimmed.parse::<u16>() {
            Ok(p) if p > 0 => Some(p),
            _ => return Err(HostInputError::InvalidPort),
        }
    };
    let id_trimmed = identity_file_str.trim();
    let identity_file = if id_trimmed.is_empty() {
        None
    } else if valid_identity_file(id_trimmed) {
        Some(id_trimmed.to_owned())
    } else {
        return Err(HostInputError::InvalidIdentityFile);
    };
    let clean_label = if label.trim().is_empty() {
        dest.to_owned()
    } else {
        label.trim().to_owned()
    };
    Ok(ValidatedHostInput {
        label: clean_label,
        destination: dest.to_owned(),
        port,
        identity_file,
    })
}

/// `[user@]host` 또는 `~/.ssh/config` 별칭만 허용한다. `-`로 시작하면 `ssh`가 옵션으로
/// 해석하므로(`-oProxyCommand=...`) 거부한다.
#[must_use]
pub fn valid_host(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && !s.starts_with('-')
        && !s.starts_with('@')
        && !s.ends_with('@')
        && s.chars().filter(|&c| c == '@').count() <= 1
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'@'))
}

/// 키 파일 경로 검증: 빈 값·`-` 시작(옵션 인젝션)·제어 문자·널 바이트를 거부한다.
#[must_use]
pub fn valid_identity_file(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 512
        && !s.starts_with('-')
        && !s.contains('\0')
        && !s.chars().any(char::is_control)
}

/// 원격 소켓 경로 검증: 절대 경로(`/` 시작)여야 하며 `:`(`-L` 구분자)와 제어 문자를 거부한다.
#[must_use]
pub fn valid_remote_socket(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 512
        && s.starts_with('/')
        && !s.contains(':')
        && !s.contains('\0')
        && !s.chars().any(char::is_control)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HostsFile {
    #[serde(default)]
    hosts: Vec<SshHost>,
}

pub fn config_path(state_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = state_dir {
        dir.join(HOSTS_FILE)
    } else if let Some(dir) = krill_proto::daemon::state_dir() {
        dir.join(HOSTS_FILE)
    } else {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        home.join(".config").join("minikrill").join(HOSTS_FILE)
    }
}

pub fn load_hosts(state_dir: Option<&Path>) -> Vec<SshHost> {
    let path = config_path(state_dir);
    if let Ok(s) = std::fs::read_to_string(&path)
        && let Ok(f) = toml::from_str::<HostsFile>(&s)
    {
        return f.hosts;
    }
    Vec::new()
}

fn save_hosts(state_dir: Option<&Path>, hosts: &[SshHost]) -> io::Result<()> {
    let file = HostsFile {
        hosts: hosts.to_vec(),
    };
    let path = config_path(state_dir);
    let text = krill_proto::config_document::save_toml(&path, &file)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, text.as_bytes())?;
    Ok(())
}

pub fn add_host(
    state_dir: Option<&Path>,
    label: &str,
    destination: &str,
    port: Option<u16>,
    identity_file: Option<String>,
) -> io::Result<Vec<SshHost>> {
    let dest = destination.trim();
    if dest.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SSH destination cannot be empty",
        ));
    }
    if !valid_host(dest) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid SSH destination: {dest}"),
        ));
    }
    if port == Some(0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SSH port cannot be 0",
        ));
    }
    let clean_identity = match identity_file {
        Some(s) if !s.trim().is_empty() => {
            let trimmed = s.trim();
            if !valid_identity_file(trimmed) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid SSH identity file path",
                ));
            }
            Some(trimmed.to_owned())
        }
        _ => None,
    };
    let clean_label = if label.trim().is_empty() {
        dest
    } else {
        label.trim()
    };
    let id = format!("ssh-{}", slugify(clean_label));
    let mut hosts = load_hosts(state_dir);
    let entry = SshHost {
        id: id.clone(),
        label: clean_label.to_owned(),
        destination: dest.to_owned(),
        port,
        identity_file: clean_identity,
        remote_socket: None,
        imported: false,
        copy_id_first: false,
    };
    if let Some(ex) = hosts
        .iter_mut()
        .find(|h| h.id == id || h.destination == dest)
    {
        *ex = entry;
    } else {
        hosts.push(entry);
    }
    save_hosts(state_dir, &hosts)?;
    Ok(hosts)
}

pub fn remove_host(state_dir: Option<&Path>, id: &str) -> io::Result<Vec<SshHost>> {
    let mut hosts = load_hosts(state_dir);
    hosts.retain(|h| h.id != id);
    save_hosts(state_dir, &hosts)?;
    Ok(hosts)
}

/// `~/.ssh/config` 내용을 파싱해 와일드카드(`*`, `?`)가 아닌 `Host` 항목들을 추출한다.
pub fn parse_ssh_config(text: &str) -> Vec<SshHost> {
    let mut out = Vec::new();
    let mut current_aliases: Vec<String> = Vec::new();
    let mut hostname: Option<String> = None;
    let mut user: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut identity: Option<String> = None;

    let flush = |aliases: &[String],
                 hostname: &Option<String>,
                 user: &Option<String>,
                 port: Option<u16>,
                 identity: &Option<String>,
                 out: &mut Vec<SshHost>| {
        for alias in aliases {
            if alias.contains('*') || alias.contains('?') || alias.is_empty() {
                continue;
            }
            let target_host = hostname.as_deref().unwrap_or(alias.as_str());
            let destination = match user.as_deref() {
                Some(u) if !u.is_empty() => format!("{u}@{target_host}"),
                _ => alias.clone(),
            };
            if !valid_host(&destination) {
                continue;
            }
            out.push(SshHost {
                id: format!("ssh-{}", slugify(alias)),
                label: alias.clone(),
                destination,
                port: port.filter(|&p| p > 0),
                identity_file: identity.clone().filter(|s| valid_identity_file(s)),
                remote_socket: None,
                imported: true,
                copy_id_first: false,
            });
        }
    };

    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let rest: Vec<&str> = parts.collect();
        if rest.is_empty() {
            continue;
        }
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                flush(
                    &current_aliases,
                    &hostname,
                    &user,
                    port,
                    &identity,
                    &mut out,
                );
                current_aliases = rest.into_iter().map(str::to_owned).collect();
                hostname = None;
                user = None;
                port = None;
                identity = None;
            }
            "hostname" => hostname = Some(rest[0].to_owned()),
            "user" => user = Some(rest[0].to_owned()),
            "port" => port = rest[0].parse::<u16>().ok(),
            "identityfile" => identity = Some(rest.join(" ")),
            _ => {}
        }
    }
    flush(
        &current_aliases,
        &hostname,
        &user,
        port,
        &identity,
        &mut out,
    );
    out
}

/// `~/.ssh/config`(또는 지정 경로)에서 호스트를 가져와 `hosts.toml`에 병합한다.
pub fn import_from_ssh_config(
    state_dir: Option<&Path>,
    ssh_config_path: Option<&Path>,
) -> io::Result<Vec<SshHost>> {
    let default_path = std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
        .join(".ssh")
        .join("config");
    let path = ssh_config_path.unwrap_or(&default_path);
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = parse_ssh_config(&text);
    let mut hosts = load_hosts(state_dir);
    for item in parsed {
        if !hosts
            .iter()
            .any(|h| h.id == item.id || h.destination == item.destination)
        {
            hosts.push(item);
        }
    }
    save_hosts(state_dir, &hosts)?;
    Ok(hosts)
}

/// Tailscale `status --json`에서 감지된 같은 tailnet의 온라인 macOS 기기.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscalePeer {
    /// 기기 표시 이름(`HostName`, 비줄바꿈 공백 정규화).
    pub hostname: String,
    /// `MagicDNS` 이름(끝의 `.` 제거). 없으면 빈 문자열.
    pub dns_name: String,
    /// 첫 번째 IPv4 주소(있을 때).
    pub ipv4: Option<String>,
    /// SSH 접속에 쓸 추천 주소(`dns_name` 우선, 없으면 `ipv4` 또는 `hostname`).
    pub address: String,
    /// Linux 피어(원격 호스트용 Krill). 접속 사용자를 알 수 없어 바로 추가하지 않고 입력 폼을 채운다.
    pub linux: bool,
}

#[derive(Deserialize)]
struct RawTailscaleStatus {
    #[serde(rename = "BackendState", default)]
    backend_state: Option<String>,
    #[serde(rename = "Self", default)]
    self_node: Option<RawTailscaleSelf>,
    #[serde(rename = "Peer", default)]
    peer: BTreeMap<String, RawTailscaleNode>,
}

#[derive(Deserialize)]
struct RawTailscaleSelf {
    #[serde(rename = "ID", default)]
    id: Option<String>,
    #[serde(rename = "PublicKey", default)]
    public_key: Option<String>,
}

#[derive(Deserialize)]
struct RawTailscaleNode {
    #[serde(rename = "ID", default)]
    id: Option<String>,
    #[serde(rename = "PublicKey", default)]
    public_key: Option<String>,
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "OS", default)]
    os: String,
    #[serde(rename = "Online", default)]
    online: bool,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
}

/// `tailscale status --json` 출력을 파싱해 온라인 상태인 macOS·Linux 피어(자기 자신 제외)만 돌려준다.
#[must_use]
pub fn parse_tailscale_status(json: &str) -> Vec<TailscalePeer> {
    let Ok(status) = serde_json::from_str::<RawTailscaleStatus>(json) else {
        return Vec::new();
    };
    if let Some(state) = &status.backend_state
        && state != "Running"
    {
        return Vec::new();
    }
    let self_id = status.self_node.as_ref().and_then(|s| s.id.as_deref());
    let self_pk = status
        .self_node
        .as_ref()
        .and_then(|s| s.public_key.as_deref());

    let mut peers = Vec::new();
    for (node_key, node) in status.peer {
        let linux = node.os.eq_ignore_ascii_case("linux");
        if !node.online || !(linux || node.os.eq_ignore_ascii_case("macos")) {
            continue;
        }
        if let (Some(sid), Some(nid)) = (self_id, node.id.as_deref())
            && sid == nid
        {
            continue;
        }
        if let (Some(spk), Some(npk)) = (self_pk, node.public_key.as_deref())
            && spk == npk
        {
            continue;
        }
        if self_pk.is_some_and(|spk| spk == node_key) {
            continue;
        }
        let hostname = node.host_name.replace('\u{a0}', " ").trim().to_owned();
        if hostname.is_empty() {
            continue;
        }
        let dns_name = node.dns_name.trim().trim_end_matches('.').to_owned();
        let ipv4 = node
            .tailscale_ips
            .iter()
            .find(|ip| !ip.contains(':') && valid_host(ip))
            .cloned();
        let address = if !dns_name.is_empty() && valid_host(&dns_name) {
            dns_name.clone()
        } else if let Some(ip) = &ipv4 {
            ip.clone()
        } else if valid_host(&hostname) {
            hostname.clone()
        } else {
            continue;
        };
        peers.push(TailscalePeer {
            hostname,
            dns_name,
            ipv4,
            address,
            linux,
        });
    }
    peers.sort_by(|a, b| a.hostname.cmp(&b.hostname).then(a.address.cmp(&b.address)));
    peers.dedup_by(|a, b| a.address == b.address);
    peers
}

/// 로컬 맥에서 `tailscale status --json`을 실행해 온라인 macOS·Linux 피어를 조회한다.
/// Tailscale이 없거나 꺼져 있으면 빈 목록을 돌려준다(오류 아님).
/// 반드시 백그라운드 실행기에서 호출해야 한다(CONVENTIONS T7).
#[must_use]
pub fn discover_tailscale_peers() -> Vec<TailscalePeer> {
    let mut candidates = Vec::new();
    if let Some(custom) = std::env::var_os("KRILL_TAILSCALE_BIN") {
        candidates.push(PathBuf::from(custom));
    }
    candidates.push(PathBuf::from("tailscale"));
    candidates.push(PathBuf::from(
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ));
    for bin in candidates {
        let Ok(out) = Command::new(&bin)
            .args(["status", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
        else {
            continue;
        };
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            return parse_tailscale_status(&text);
        }
    }
    Vec::new()
}

/// SSH 연결 테스트 실패 원인 분류(첫 연결 도우미 안내용).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshTestFailureKind {
    /// SSH 키 미등록·인증 거부(`Permission denied` 등) → "이 기기 등록하기" 노출.
    AuthFailed,
    /// 원격 로그인 꺼짐(`Connection refused`) → 시스템 설정 > 공유 > 원격 로그인 안내.
    ConnectionRefused,
    /// 그 외 네트워크·호스트 오류.
    Other,
}

/// `ssh` 표준 오류 문자열에서 실패 원인을 분류한다.
#[must_use]
pub fn classify_ssh_error(stderr: &str) -> SshTestFailureKind {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("connection refused") {
        SshTestFailureKind::ConnectionRefused
    } else if lower.contains("permission denied")
        || lower.contains("publickey")
        || lower.contains("authentication failed")
    {
        SshTestFailureKind::AuthFailed
    } else {
        SshTestFailureKind::Other
    }
}

/// 기본 Ed25519 개인키 경로(`~/.ssh/id_ed25519`)를 돌려준다.
#[must_use]
pub fn default_ssh_key_path(home: Option<&Path>) -> PathBuf {
    let base = home.map_or_else(
        || std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from),
        Path::to_path_buf,
    );
    base.join(".ssh").join("id_ed25519")
}

/// 기본 SSH 키(`~/.ssh/id_ed25519`)가 이미 존재하는지 확인한다.
#[must_use]
pub fn has_default_ssh_key(home: Option<&Path>) -> bool {
    default_ssh_key_path(home).is_file()
}

/// `~/.ssh/id_ed25519` 키가 없으면 빈 패스프레이즈로 새로 생성한다.
pub fn generate_default_ssh_key(home: Option<&Path>) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;

    let key_path = default_ssh_key_path(home);
    if key_path.is_file() {
        return Ok(key_path);
    }
    if let Some(ssh_dir) = key_path.parent() {
        std::fs::create_dir_all(ssh_dir)?;
        std::fs::set_permissions(ssh_dir, std::fs::Permissions::from_mode(0o700)).ok();
    }
    let keygen_bin = std::env::var_os("KRILL_SSH_KEYGEN_BIN")
        .map_or_else(|| PathBuf::from("ssh-keygen"), PathBuf::from);
    let out = Command::new(keygen_bin)
        .args(["-t", "ed25519", "-N", "", "-f"])
        .arg(&key_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(io::Error::other(if err.is_empty() {
            format!("ssh-keygen exited with {}", out.status)
        } else {
            err
        }));
    }
    Ok(key_path)
}

/// 새 터미널 탭에서 `ssh-copy-id`(비밀번호 1회 입력)로 공개키를 원격 맥에 등록하는 셸 명령줄.
#[must_use]
pub fn build_copy_id_command(host: &SshHost, default_key: Option<&Path>) -> String {
    let mut parts = vec!["ssh-copy-id".to_owned()];
    if let Some(port) = host.port {
        parts.push("-p".to_owned());
        parts.push(port.to_string());
    }
    let key = host
        .identity_file
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| default_key.map(|p| p.display().to_string()));
    if let Some(k) = key {
        parts.push("-i".to_owned());
        parts.push(shell_quote(&k));
    }
    parts.push(shell_quote(&host.destination));
    format!("{}\n", parts.join(" "))
}

/// SSH 호스트 연결 및 원격 `krill` 상태를 점검한다.
pub fn test_connection(host: &SshHost) -> Result<String, String> {
    if !valid_host(&host.destination) {
        return Err(format!("invalid SSH destination: {}", host.destination));
    }
    let start = Instant::now();
    let ssh_bin =
        std::env::var_os("KRILL_SSH_BIN").map_or_else(|| PathBuf::from("ssh"), PathBuf::from);
    let mut cmd = Command::new(ssh_bin);
    cmd.args(host.test_args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let out = cmd.output().map_err(|e| format!("ssh spawn failed: {e}"))?;
    let elapsed_ms = start.elapsed().as_millis();
    if out.status.success() {
        let info = String::from_utf8_lossy(&out.stdout)
            .lines()
            .collect::<Vec<_>>()
            .join(" · ");
        Ok(format!("OK ({elapsed_ms}ms) — {info}"))
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        Err(if err.is_empty() {
            format!("SSH exited with {}", out.status)
        } else {
            err
        })
    }
}

/// 새 터미널 탭에서 해당 원격 PC-to-PC 호스트에 접속(또는 `copy_id_first`면 키 등록)하는 셸 명령줄을 생성한다.
#[must_use]
pub fn build_connect_command(host: &SshHost) -> String {
    if host.copy_id_first {
        return build_copy_id_command(host, None);
    }
    let mut parts = vec!["ssh".to_owned(), "-t".to_owned()];
    if let Some(port) = host.port {
        parts.push("-p".to_owned());
        parts.push(port.to_string());
    }
    if let Some(id_file) = &host.identity_file
        && !id_file.trim().is_empty()
    {
        parts.push("-i".to_owned());
        parts.push(shell_quote(id_file.trim()));
    }
    parts.push(shell_quote(&host.destination));
    format!("{}\n", parts.join(" "))
}

/// `krill --host <dest> ls` 원격 데몬 세션 조회 명령줄을 생성한다.
#[must_use]
pub fn build_mk_host_command(host: &SshHost, subcmd: &str) -> String {
    format!("krill --host {} {subcmd}\n", shell_quote(&host.destination))
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '/' | ':'))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let t = out.trim_matches('-');
    if t.is_empty() {
        "host".to_owned()
    } else {
        t.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_config_and_skips_wildcards() {
        let cfg = r"
# comment
Host *
    ServerAliveInterval 30

Host mac-studio
    HostName 192.168.1.50
    User owen
    Port 2222
    IdentityFile ~/.ssh/id_ed25519

Host dev-box
    HostName dev.example.com
";
        let hosts = parse_ssh_config(cfg);
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].label, "mac-studio");
        assert_eq!(hosts[0].destination, "owen@192.168.1.50");
        assert_eq!(hosts[0].port, Some(2222));
        assert_eq!(hosts[0].identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
        assert_eq!(hosts[1].label, "dev-box");
        assert_eq!(hosts[1].destination, "dev-box");
    }

    #[test]
    fn add_remove_and_connect_command_roundtrip() {
        let dir = std::env::temp_dir().join(format!("mk112-ssh-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let list = add_host(
            Some(&dir),
            "Office Mac",
            "owen@10.0.0.8",
            Some(2200),
            Some("~/.ssh/office_key".into()),
        )
        .unwrap();
        assert_eq!(list.len(), 1);
        let cmd = build_connect_command(&list[0]);
        assert_eq!(cmd, "ssh -t -p 2200 -i '~/.ssh/office_key' owen@10.0.0.8\n");

        let copy_cmd = build_connect_command(&list[0].clone().with_copy_id());
        assert_eq!(
            copy_cmd,
            "ssh-copy-id -p 2200 -i '~/.ssh/office_key' owen@10.0.0.8\n"
        );

        let after = remove_host(Some(&dir), &list[0].id).unwrap();
        assert!(after.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn host_input_validation_checks_address_port_and_key() {
        assert_eq!(
            validate_host_input("", "  ", "", ""),
            Err(HostInputError::EmptyAddress)
        );
        assert_eq!(
            validate_host_input("Bad", "-oProxyCommand=evil", "", ""),
            Err(HostInputError::InvalidAddress)
        );
        assert_eq!(
            validate_host_input("Bad", "user@host with spaces", "", ""),
            Err(HostInputError::InvalidAddress)
        );
        assert_eq!(
            validate_host_input("Bad", "owen@10.0.0.1", "0", ""),
            Err(HostInputError::InvalidPort)
        );
        assert_eq!(
            validate_host_input("Bad", "owen@10.0.0.1", "abc", ""),
            Err(HostInputError::InvalidPort)
        );
        assert_eq!(
            validate_host_input("Bad", "owen@10.0.0.1", "22", "-oEvil"),
            Err(HostInputError::InvalidIdentityFile)
        );

        let ok = validate_host_input(
            "  집 맥미니 ",
            " owen@mac-mini.tailnet.ts.net ",
            " 2222 ",
            " ~/.ssh/id_ed25519 ",
        )
        .unwrap();
        assert_eq!(ok.label, "집 맥미니");
        assert_eq!(ok.destination, "owen@mac-mini.tailnet.ts.net");
        assert_eq!(ok.port, Some(2222));
        assert_eq!(ok.identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
    }

    #[test]
    fn tailscale_status_fixture_filters_online_macos_and_linux_peers_excluding_self() {
        let fixture = r#"{
            "BackendState": "Running",
            "Self": {
                "ID": "n1",
                "PublicKey": "nodekey:self111",
                "HostName": "My-Local-Mac",
                "DNSName": "my-local-mac.tail123.ts.net.",
                "OS": "macOS",
                "Online": true,
                "TailscaleIPs": ["100.100.1.1"]
            },
            "Peer": {
                "nodekey:self111": {
                    "ID": "n1",
                    "PublicKey": "nodekey:self111",
                    "HostName": "My-Local-Mac",
                    "DNSName": "my-local-mac.tail123.ts.net.",
                    "OS": "macOS",
                    "Online": true,
                    "TailscaleIPs": ["100.100.1.1"]
                },
                "nodekey:mac222": {
                    "ID": "n2",
                    "PublicKey": "nodekey:mac222",
                    "HostName": "오웬의 Mac\u00a0Studio",
                    "DNSName": "owen-studio.tail123.ts.net.",
                    "OS": "macOS",
                    "Online": true,
                    "TailscaleIPs": ["100.100.1.2", "fd7a:115c:a1e0::2"]
                },
                "nodekey:linux333": {
                    "ID": "n3",
                    "PublicKey": "nodekey:linux333",
                    "HostName": "ubuntu-server",
                    "DNSName": "ubuntu.tail123.ts.net.",
                    "OS": "linux",
                    "Online": true,
                    "TailscaleIPs": ["100.100.1.3"]
                },
                "nodekey:win555": {
                    "ID": "n5",
                    "PublicKey": "nodekey:win555",
                    "HostName": "gaming-pc",
                    "DNSName": "gaming-pc.tail123.ts.net.",
                    "OS": "windows",
                    "Online": true,
                    "TailscaleIPs": ["100.100.1.5"]
                },
                "nodekey:mac444": {
                    "ID": "n4",
                    "PublicKey": "nodekey:mac444",
                    "HostName": "Sleeping-MacBook",
                    "DNSName": "macbook.tail123.ts.net.",
                    "OS": "macOS",
                    "Online": false,
                    "TailscaleIPs": ["100.100.1.4"]
                }
            }
        }"#;
        let peers = parse_tailscale_status(fixture);
        // 자기 자신·오프라인·Windows는 빠지고, Linux는 원격 호스트 후보로 남는다(이름순).
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].hostname, "ubuntu-server");
        assert_eq!(peers[0].address, "ubuntu.tail123.ts.net");
        assert!(peers[0].linux);
        assert_eq!(peers[1].hostname, "오웬의 Mac Studio");
        assert_eq!(peers[1].dns_name, "owen-studio.tail123.ts.net");
        assert_eq!(peers[1].ipv4.as_deref(), Some("100.100.1.2"));
        assert_eq!(peers[1].address, "owen-studio.tail123.ts.net");
        assert!(!peers[1].linux);

        // Stopped backend returns empty list quietly.
        let stopped = r#"{"BackendState": "Stopped", "Peer": {}}"#;
        assert!(parse_tailscale_status(stopped).is_empty());
    }

    #[test]
    fn shared_argv_builder_and_remote_home_socket_resolution() {
        let host = SshHost {
            id: "ssh-studio".into(),
            label: "Studio".into(),
            destination: "owen@studio.local".into(),
            port: Some(2201),
            identity_file: Some("~/.ssh/id_ed25519".into()),
            remote_socket: None,
            imported: false,
            copy_id_first: false,
        };
        let remote_sock = host
            .resolve_remote_socket_from_home("/Users/remoteowen\n")
            .unwrap();
        assert_eq!(
            remote_sock,
            PathBuf::from(
                "/Users/remoteowen/Library/Application Support/dev.krill.app/run/krilld.sock"
            )
        );
        let tunnel = host.tunnel_args(Path::new("/tmp/local.sock"), &remote_sock);
        assert!(tunnel.windows(2).any(|w| w == ["-p", "2201"]));
        assert!(tunnel.windows(2).any(|w| w == ["-i", "~/.ssh/id_ed25519"]));
        assert!(tunnel.contains(&"ServerAliveInterval=15".to_owned()));
        assert!(tunnel.contains(&"ServerAliveCountMax=3".to_owned()));
        assert!(tunnel.contains(&"ExitOnForwardFailure=yes".to_owned()));
        assert!(tunnel.contains(&"StreamLocalBindUnlink=yes".to_owned()));

        let home_args = host.remote_home_args();
        assert!(home_args.windows(2).any(|w| w == ["-p", "2201"]));
        assert_eq!(home_args.last().unwrap(), &krill_hosts::ssh::remote_query());
        assert_eq!(
            host.resolve_remote_socket_from_home(
                "/Users/remoteowen\nLibrary/Application Support/dev.minikrill.app\n",
            )
            .unwrap(),
            PathBuf::from(
                "/Users/remoteowen/Library/Application Support/dev.minikrill.app/run/krilld.sock"
            )
        );
    }

    #[test]
    fn classifies_ssh_errors_for_onboarding_guidance() {
        assert_eq!(
            classify_ssh_error("owen@10.0.0.2: Permission denied (publickey,password)."),
            SshTestFailureKind::AuthFailed
        );
        assert_eq!(
            classify_ssh_error("ssh: connect to host 10.0.0.2 port 22: Connection refused"),
            SshTestFailureKind::ConnectionRefused
        );
        assert_eq!(
            classify_ssh_error("ssh: Could not resolve hostname unknown.local"),
            SshTestFailureKind::Other
        );
    }
}

#[cfg(test)]
#[test]
fn as12_hosts_save_preserves_future_fields() {
    let dir = std::env::temp_dir().join(format!("as12-hosts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = config_path(Some(&dir));
    std::fs::write(&path, "# keep\nfuture = true\n[[hosts]]\nid = 'one'\nlabel = 'old'\ndestination = 'host'\nfuture = 'nested'\n").unwrap();
    let mut hosts = load_hosts(Some(&dir));
    hosts[0].label = "edited".into();
    save_hosts(Some(&dir), &hosts).unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    let value: toml::Value = toml::from_str(&body).unwrap();
    assert!(body.contains("# keep"));
    assert_eq!(value["future"].as_bool(), Some(true));
    assert_eq!(value["hosts"][0]["future"].as_str(), Some("nested"));
    std::fs::remove_dir_all(dir).unwrap();
}
