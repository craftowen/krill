//! 상태 파일(`link/devices.toml`, `link/hosts.toml`, `link/invites.toml`) 원자적 저장소.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::Error;

/// 허용 기기 목록 파일 상대 경로 (`<state_dir>/link/devices.toml`).
pub const DEVICES_REL_PATH: &str = "link/devices.toml";
/// 원격 호스트 목록 파일 상대 경로 (`<state_dir>/link/hosts.toml`).
pub const HOSTS_REL_PATH: &str = "link/hosts.toml";
/// 1회용 초대 토큰 해시 목록 파일 상대 경로 (`<state_dir>/link/invites.toml`).
pub const INVITES_REL_PATH: &str = "link/invites.toml";
const STORE_LOCK_REL_PATH: &str = "link/.store.lock";

/// 호스트 ID 슬러그 최대 길이 (`run/hosts/<id>.sock`의 UDS 104바이트 제한 여유 확보).
pub const HOST_ID_MAX_LEN: usize = 24;
const DISPLAY_NAME_MAX_CHARS: usize = 64;

static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// `<state_dir>/link/.store.lock` 파일의 배타적 flock을 획득해 동시 프로세스 간 RMW 경합을 막는다.
pub(crate) fn acquire_store_lock(state_dir: &Path) -> Result<File, Error> {
    let lock_path = state_dir.join(STORE_LOCK_REL_PATH);
    if let Some(parent) = lock_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(Error::Io)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(Error::Io)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(Error::Timeout("timed out acquiring link store file lock"));
            }
            Err(TryLockError::Error(e)) => return Err(Error::Io(e)),
        }
    }
}

/// 표시 이름에서 제어문자(`\n`, `\t`, ANSI 이스케이프 등)를 제거하고 글자 수를 제한한다(CONVENTIONS P3).
pub fn sanitize_display_name(raw: &str, fallback: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(DISPLAY_NAME_MAX_CHARS)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// 현재 시각(Unix epoch 초).
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// 현재 시각(Unix epoch 밀리초).
pub fn now_unix_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// 부모 디렉터리를 0700으로 만들고, 임시 파일(0600)에 쓰고 `sync_all` 후 `rename`으로 원자적 교체한다.
pub fn write_atomic_0600(path: &Path, content: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let pid = std::process::id();
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut tmp_os = path.as_os_str().to_os_string();
    tmp_os.push(format!(".tmp.{pid}.{seq}"));
    let tmp_path = PathBuf::from(tmp_os);

    let write_res = (|| -> io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        f.write_all(content)?;
        f.sync_all()?;
        Ok(())
    })();

    if let Err(e) = write_res {
        fs::remove_file(&tmp_path).ok();
        return Err(e);
    }

    if let Err(e) = fs::rename(&tmp_path, path) {
        fs::remove_file(&tmp_path).ok();
        return Err(e);
    }
    Ok(())
}

/// 바이트 슬라이스를 소문자 16진수 문자열로 인코딩한다.
pub fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for &b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    out
}

/// 16진수 문자열을 바이트 벡터로 디코딩한다. 입력 원문은 오류 메시지에 포함하지 않는다.
pub fn decode_hex(s: &str) -> Result<Vec<u8>, &'static str> {
    if !s.len().is_multiple_of(2) {
        return Err("hex string must have even length");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_nibble(bytes[i])?;
        let lo = hex_nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8, &'static str> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err("invalid hex character"),
    }
}

/// 호스트·기기 식별자에 안전한 슬러그(`a-z0-9-`, 최대 `HOST_ID_MAX_LEN`바이트)를 만든다.
pub fn sanitize_id_slug(raw: &str, fallback_prefix: &str, node_id: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if (ch == '-' || ch == '_' || ch == '.' || ch.is_whitespace())
            && !slug.is_empty()
            && !last_dash
        {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= HOST_ID_MAX_LEN {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        let short: String = node_id
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(8)
            .collect();
        format!("{fallback_prefix}-{short}")
    } else {
        slug
    }
}

/// 호스트 ID가 파일 이름 및 UDS 소켓 경로 구성에 안전한지 검사한다.
pub fn validate_host_id(host_id: &str) -> Result<(), Error> {
    let trimmed = host_id.trim();
    if trimmed.is_empty()
        || trimmed.len() > HOST_ID_MAX_LEN
        || trimmed.starts_with('.')
        || trimmed.starts_with('-')
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(Error::InvalidHostId(host_id.to_owned()));
    }
    Ok(())
}

// ============================================================================
// 1. 허용 기기 목록 (`link/devices.toml`)
// ============================================================================

/// 서버가 연결을 허용한 클라이언트 기기 한 건 (`link/devices.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowedDevice {
    /// 짧은 관리 ID (예: `dev-1b892674`). 비어 있으면 `node_id` 앞 8자리로 채운다.
    #[serde(default)]
    pub id: String,
    /// 클라이언트 `EndpointId`(`NodeId`, 64자 hex).
    pub node_id: String,
    /// 사용자가 지정한 기기 이름.
    pub name: String,
    /// 허용 목록 추가 시각 (Unix epoch 초).
    pub added_at: u64,
}

impl AllowedDevice {
    /// 파싱된 `EndpointId`를 돌려준다.
    pub fn endpoint_id(&self) -> Result<EndpointId, Error> {
        self.node_id.trim().parse::<EndpointId>().map_err(|e| {
            Error::InvalidConfig(format!(
                "invalid node_id in devices.toml ({}): {e}",
                self.id
            ))
        })
    }

    fn normalize(mut self) -> Self {
        if self.id.trim().is_empty() {
            let short: String = self.node_id.chars().take(8).collect();
            self.id = format!("dev-{short}");
        }
        self
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DevicesFile {
    #[serde(default)]
    devices: Vec<AllowedDevice>,
}

/// `<state_dir>/link/devices.toml` 경로.
pub fn devices_path(state_dir: &Path) -> PathBuf {
    state_dir.join(DEVICES_REL_PATH)
}

/// 허용된 기기 목록을 읽는다. 파일이 없으면 빈 목록을 돌려준다.
pub fn load_devices(state_dir: &Path) -> Result<Vec<AllowedDevice>, Error> {
    let path = devices_path(state_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path).map_err(Error::Io)?;
    let parsed: DevicesFile = toml::from_str(&text)
        .map_err(|e| Error::InvalidConfig(format!("failed to parse devices.toml: {e}")))?;
    let mut out = Vec::with_capacity(parsed.devices.len());
    for dev in parsed.devices {
        let normalized = dev.normalize();
        // 저장된 NodeId 형식 검증
        let _ = normalized.endpoint_id()?;
        out.push(normalized);
    }
    Ok(out)
}

/// 허용된 기기 목록을 `<state_dir>/link/devices.toml`에 원자적으로(0600) 저장한다.
pub fn save_devices(state_dir: &Path, devices: &[AllowedDevice]) -> Result<(), Error> {
    let file = DevicesFile {
        devices: devices.to_vec(),
    };
    let text = toml::to_string_pretty(&file)
        .map_err(|e| Error::InvalidConfig(format!("failed to serialize devices.toml: {e}")))?;
    write_atomic_0600(&devices_path(state_dir), text.as_bytes()).map_err(Error::Io)
}

/// 허용된 기기의 `EndpointId` 집합을 읽는다.
pub fn load_allowed_node_set(state_dir: &Path) -> Result<BTreeSet<EndpointId>, Error> {
    let devices = load_devices(state_dir)?;
    let mut set = BTreeSet::new();
    for d in devices {
        set.insert(d.endpoint_id()?);
    }
    Ok(set)
}

/// 기기를 허용 목록에 추가(이미 같은 `node_id`가 있으면 이름·시각 갱신)하고 저장한다.
pub fn upsert_device(
    state_dir: &Path,
    node_id: EndpointId,
    name: &str,
    added_at: u64,
) -> Result<AllowedDevice, Error> {
    let _lock = acquire_store_lock(state_dir)?;
    let mut devices = load_devices(state_dir)?;
    let node_str = node_id.to_string();
    let short: String = node_str.chars().take(8).collect();
    let id = format!("dev-{short}");
    let clean_name = sanitize_display_name(name, &id);

    if let Some(existing) = devices.iter_mut().find(|d| d.node_id == node_str) {
        existing.name = clean_name;
        existing.added_at = added_at;
        if existing.id.trim().is_empty() {
            existing.id = id;
        }
        let updated = existing.clone();
        save_devices(state_dir, &devices)?;
        return Ok(updated);
    }

    let entry = AllowedDevice {
        id,
        node_id: node_str,
        name: clean_name,
        added_at,
    };
    devices.push(entry.clone());
    save_devices(state_dir, &devices)?;
    Ok(entry)
}

/// `id`, `node_id`(전체 또는 6자 이상 접두어), 또는 `name`이 일치하는 기기를 허용 목록에서 제거한다.
pub fn remove_device(state_dir: &Path, target: &str) -> Result<AllowedDevice, Error> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Err(Error::DeviceNotFound(target.to_owned()));
    }
    let _lock = acquire_store_lock(state_dir)?;
    let mut devices = load_devices(state_dir)?;
    let pos = devices
        .iter()
        .position(|d| {
            d.id == trimmed
                || d.node_id == trimmed
                || (trimmed.len() >= 6 && d.node_id.starts_with(trimmed))
                || d.name == trimmed
        })
        .ok_or_else(|| Error::DeviceNotFound(trimmed.to_owned()))?;
    let removed = devices.remove(pos);
    save_devices(state_dir, &devices)?;
    Ok(removed)
}

// ============================================================================
// 2. 원격 호스트 목록 (`link/hosts.toml`)
// ============================================================================

/// 클라이언트가 짝지은 원격 서버 호스트 한 건 (`link/hosts.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHost {
    /// 호스트 ID (`run/hosts/<id>.sock` 파일명에 사용).
    pub id: String,
    /// 서버 표시 이름.
    pub name: String,
    /// 서버 `EndpointId`(`NodeId`, 64자 hex).
    pub node_id: String,
    /// 서버 중계 URL 힌트 (있으면 저장).
    #[serde(default)]
    pub relay_url: Option<String>,
    /// 서버 직접 IP 주소(`ip:port`) 힌트 목록.
    #[serde(default)]
    pub addr_hints: Vec<String>,
    /// 짝짓기 시각 (Unix epoch 초).
    #[serde(default)]
    pub added_at: u64,
}

impl RemoteHost {
    /// 저장된 `node_id`·`relay_url`·`addr_hints`로부터 `iroh::EndpointAddr`를 조립한다.
    pub fn to_endpoint_addr(&self, include_relay: bool) -> Result<EndpointAddr, Error> {
        let id: EndpointId = self.node_id.trim().parse().map_err(|e| {
            Error::InvalidConfig(format!("invalid host node_id for {:?}: {e}", self.id))
        })?;
        let mut addr = EndpointAddr::new(id);
        if include_relay
            && let Some(r_str) = &self.relay_url
            && !r_str.trim().is_empty()
        {
            let relay_url: RelayUrl = r_str.trim().parse().map_err(|e| {
                Error::InvalidConfig(format!("invalid relay_url for host {:?}: {e}", self.id))
            })?;
            addr = addr.with_relay_url(relay_url);
        }
        for hint in &self.addr_hints {
            let trimmed = hint.trim();
            if trimmed.is_empty() {
                continue;
            }
            let sock_addr: SocketAddr = trimmed.parse().map_err(|e| {
                Error::InvalidConfig(format!(
                    "invalid addr_hint {trimmed:?} for host {:?}: {e}",
                    self.id
                ))
            })?;
            addr = addr.with_ip_addr(sock_addr);
        }
        Ok(addr)
    }

    /// `EndpointAddr`로부터 `RemoteHost` 레코드를 생성한다.
    pub fn from_endpoint_addr(id: String, name: &str, addr: &EndpointAddr, added_at: u64) -> Self {
        let relay_url = addr.relay_urls().next().map(ToString::to_string);
        let addr_hints = addr.ip_addrs().map(ToString::to_string).collect();
        let clean_name = sanitize_display_name(name, &id);
        Self {
            id,
            name: clean_name,
            node_id: addr.id.to_string(),
            relay_url,
            addr_hints,
            added_at,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HostsFile {
    #[serde(default)]
    hosts: Vec<RemoteHost>,
}

/// `<state_dir>/link/hosts.toml` 경로.
pub fn hosts_path(state_dir: &Path) -> PathBuf {
    state_dir.join(HOSTS_REL_PATH)
}

/// 저장된 원격 호스트 목록을 읽는다.
pub fn load_hosts(state_dir: &Path) -> Result<Vec<RemoteHost>, Error> {
    let path = hosts_path(state_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path).map_err(Error::Io)?;
    let parsed: HostsFile = toml::from_str(&text)
        .map_err(|e| Error::InvalidConfig(format!("failed to parse link/hosts.toml: {e}")))?;
    for h in &parsed.hosts {
        validate_host_id(&h.id)?;
        let _ = h.to_endpoint_addr(true)?;
    }
    Ok(parsed.hosts)
}

/// 원격 호스트 목록을 `<state_dir>/link/hosts.toml`에 원자적으로(0600) 저장한다.
pub fn save_hosts(state_dir: &Path, hosts: &[RemoteHost]) -> Result<(), Error> {
    for h in hosts {
        validate_host_id(&h.id)?;
    }
    let file = HostsFile {
        hosts: hosts.to_vec(),
    };
    let text = toml::to_string_pretty(&file)
        .map_err(|e| Error::InvalidConfig(format!("failed to serialize link/hosts.toml: {e}")))?;
    write_atomic_0600(&hosts_path(state_dir), text.as_bytes()).map_err(Error::Io)
}

/// 원격 호스트를 추가 또는 갱신(같은 `node_id`면 갱신, 다른 `node_id`와 `id`가 겹치면 고유 접미어 부여)한다.
pub fn upsert_host(state_dir: &Path, mut host: RemoteHost) -> Result<RemoteHost, Error> {
    validate_host_id(&host.id)?;
    let _ = host.to_endpoint_addr(true)?;
    host.name = sanitize_display_name(&host.name, &host.id);
    let _lock = acquire_store_lock(state_dir)?;
    let mut hosts = load_hosts(state_dir)?;
    if let Some(existing) = hosts.iter_mut().find(|h| h.node_id == host.node_id) {
        host.id.clone_from(&existing.id);
        *existing = host.clone();
    } else {
        if hosts.iter().any(|h| h.id == host.id) {
            host.id = disambiguate_host_id(&host.id, &host.node_id, &hosts);
        }
        hosts.push(host.clone());
    }
    save_hosts(state_dir, &hosts)?;
    Ok(host)
}

fn disambiguate_host_id(base_id: &str, node_id: &str, existing: &[RemoteHost]) -> String {
    let short_node: String = node_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(6)
        .collect();
    let trim_prefix = |len: usize| {
        let mut p: String = base_id.chars().take(len).collect();
        while p.ends_with('-') {
            p.pop();
        }
        p
    };
    let p = trim_prefix(HOST_ID_MAX_LEN.saturating_sub(short_node.len().saturating_add(1)));
    let candidate = if p.is_empty() {
        format!("host-{short_node}")
    } else {
        format!("{p}-{short_node}")
    };
    if !existing.iter().any(|h| h.id == candidate) {
        return candidate;
    }
    for n in 2u32..1000 {
        let suffix = format!("-{n}");
        let next = format!(
            "{}{suffix}",
            trim_prefix(HOST_ID_MAX_LEN.saturating_sub(suffix.len()))
        );
        if !existing.iter().any(|h| h.id == next) {
            return next;
        }
    }
    candidate
}

/// `id`, `name`, 또는 `node_id`로 원격 호스트를 찾는다.
pub fn find_host(state_dir: &Path, query: &str) -> Result<RemoteHost, Error> {
    let trimmed = query.trim();
    let hosts = load_hosts(state_dir)?;
    hosts
        .into_iter()
        .find(|h| {
            h.id == trimmed
                || h.name == trimmed
                || h.node_id == trimmed
                || (trimmed.len() >= 6 && h.node_id.starts_with(trimmed))
        })
        .ok_or_else(|| Error::HostNotFound(trimmed.to_owned()))
}

// ============================================================================
// 3. 1회용 초대 토큰 (`link/invites.toml`)
// ============================================================================

/// 1회용 초대 토큰 기본 만료 시간 (10분).
pub const DEFAULT_INVITE_TTL: Duration = Duration::from_mins(10);

/// 메모리에만 잠시 유지되는 128비트(32 hex chars) 1회용 초대 토큰 원문.
///
/// `fmt::Debug`는 항상 `"<redacted>"`를 출력한다.
#[derive(Clone)]
pub struct InviteToken {
    raw: Zeroizing<String>,
}

impl fmt::Debug for InviteToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InviteToken(<redacted>)")
    }
}

impl InviteToken {
    /// 128비트(16바이트) CSPRNG 난수로 새 초대 토큰을 생성한다.
    pub fn generate() -> Result<Self, Error> {
        let mut bytes = Zeroizing::new([0u8; 16]);
        SystemRandom::new()
            .fill(bytes.as_mut())
            .map_err(|_| Error::Crypto("failed to generate 128-bit invite token"))?;
        Ok(Self {
            raw: Zeroizing::new(encode_hex(&*bytes)),
        })
    }

    /// 문자열로부터 토큰을 감싼다(형식 검사: 최소 32자 hex = 128비트 이상).
    pub fn from_raw(s: &str) -> Result<Self, Error> {
        let trimmed = s.trim();
        if trimmed.len() < 32 || !trimmed.len().is_multiple_of(2) {
            return Err(Error::InvalidPairLink(
                "invite token must be at least 128 bits (32 hex chars)",
            ));
        }
        let _ = decode_hex(trimmed)
            .map_err(|_| Error::InvalidPairLink("invite token contains non-hex characters"))?;
        Ok(Self {
            raw: Zeroizing::new(trimmed.to_ascii_lowercase()),
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.raw
    }

    /// 토큰의 SHA-256 해시(소문자 hex)를 계산한다. 디스크에는 해시만 저장한다.
    pub fn sha256_hex(&self) -> String {
        encode_hex(digest(&SHA256, self.raw.as_bytes()).as_ref())
    }
}

/// 디스크(`link/invites.toml`)에 보관되는 초대 토큰 메타데이터. 원문 대신 SHA-256만 저장한다.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRecord {
    pub id: String,
    pub token_sha256: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub used: bool,
}

impl fmt::Debug for InviteRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InviteRecord")
            .field("id", &self.id)
            .field("token_sha256", &"<redacted>")
            .field("created_at_ms", &self.created_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("used", &self.used)
            .finish()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct InvitesFile {
    #[serde(default)]
    invites: Vec<InviteRecord>,
}

/// 초대 토큰 검증 실패 사유(토큰 원문을 절대 포함하지 않는다).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteVerifyError {
    InvalidFormat,
    UnknownToken,
    AlreadyUsed,
    Expired,
}

impl fmt::Display for InviteVerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat => f.write_str("invalid invite token format"),
            Self::UnknownToken => f.write_str("unknown invite token"),
            Self::AlreadyUsed => f.write_str("invite token has already been used"),
            Self::Expired => f.write_str("invite token has expired"),
        }
    }
}

/// `<state_dir>/link/invites.toml` 경로.
pub fn invites_path(state_dir: &Path) -> PathBuf {
    state_dir.join(INVITES_REL_PATH)
}

fn load_invites(state_dir: &Path) -> Result<Vec<InviteRecord>, Error> {
    let path = invites_path(state_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path).map_err(Error::Io)?;
    let parsed: InvitesFile = toml::from_str(&text)
        .map_err(|e| Error::InvalidConfig(format!("failed to parse invites.toml: {e}")))?;
    Ok(parsed.invites)
}

fn save_invites(state_dir: &Path, invites: &[InviteRecord]) -> Result<(), Error> {
    let file = InvitesFile {
        invites: invites.to_vec(),
    };
    let text = toml::to_string_pretty(&file)
        .map_err(|e| Error::InvalidConfig(format!("failed to serialize invites.toml: {e}")))?;
    write_atomic_0600(&invites_path(state_dir), text.as_bytes()).map_err(Error::Io)
}

/// 새 1회용 초대 토큰을 생성하고 `link/invites.toml`에 등록한다.
pub fn create_invite(
    state_dir: &Path,
    ttl: Duration,
) -> Result<(InviteToken, InviteRecord), Error> {
    let token = InviteToken::generate()?;
    let now_ms = now_unix_ms();
    let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
    let expires_at_ms = now_ms.saturating_add(ttl_ms);
    let sha = token.sha256_hex();
    let short_id: String = sha.chars().take(8).collect();
    let record = InviteRecord {
        id: format!("inv-{short_id}"),
        token_sha256: sha,
        created_at_ms: now_ms,
        expires_at_ms,
        used: false,
    };
    let _lock = acquire_store_lock(state_dir)?;
    let mut invites = load_invites(state_dir)?;
    // 오래전에 만료되고 사용된 레코드는 최대 64개로 유지한다.
    if invites.len() >= 64 {
        invites.drain(..invites.len().saturating_sub(63));
    }
    invites.push(record.clone());
    save_invites(state_dir, &invites)?;
    Ok((token, record))
}

/// 제시된 초대 토큰을 상수 시간 해시 비교로 검증하고 즉시 `used = true`로 소진시킨다.
pub fn verify_and_consume_invite(
    state_dir: &Path,
    raw_token: &str,
    now_ms: u64,
) -> Result<InviteRecord, Result<InviteVerifyError, Error>> {
    let Ok(token) = InviteToken::from_raw(raw_token) else {
        return Err(Ok(InviteVerifyError::InvalidFormat));
    };
    let candidate_sha = token.sha256_hex();
    let _lock = acquire_store_lock(state_dir).map_err(Err)?;
    let mut invites = load_invites(state_dir).map_err(Err)?;

    let mut matched_idx = None;
    for (idx, rec) in invites.iter().enumerate() {
        if constant_time_eq(rec.token_sha256.as_bytes(), candidate_sha.as_bytes()) {
            matched_idx = Some(idx);
        }
    }

    let Some(idx) = matched_idx else {
        return Err(Ok(InviteVerifyError::UnknownToken));
    };
    if invites[idx].used {
        return Err(Ok(InviteVerifyError::AlreadyUsed));
    }
    if now_ms > invites[idx].expires_at_ms {
        return Err(Ok(InviteVerifyError::Expired));
    }

    invites[idx].used = true;
    let consumed = invites[idx].clone();
    save_invites(state_dir, &invites).map_err(Err)?;
    Ok(consumed)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `EndpointAddr`의 직접 IP 주소와 선택적 중계 URL만 추려 콤팩트한 `EndpointAddr`를 만든다.
pub fn compact_endpoint_addr(addr: &EndpointAddr, include_relay: bool) -> EndpointAddr {
    let mut out = EndpointAddr::new(addr.id);
    if include_relay && let Some(r) = addr.relay_urls().next() {
        out = out.with_relay_url(r.clone());
    }
    for ip in addr.ip_addrs() {
        out.addrs.insert(TransportAddr::Ip(*ip));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    #[test]
    fn devices_and_hosts_atomic_roundtrip() {
        let dir = std::env::temp_dir().join(format!("mklink-store-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();

        let sk = SecretKey::generate();
        let node = sk.public();
        let dev = upsert_device(&dir, node, "오웬\n아이패드\t", 1_700_000_000).unwrap();
        assert_eq!(dev.name, "오웬아이패드");

        let list = load_devices(&dir).unwrap();
        assert_eq!(list.len(), 1);
        assert!(load_allowed_node_set(&dir).unwrap().contains(&node));

        let removed = remove_device(&dir, &dev.id).unwrap();
        assert_eq!(removed.node_id, node.to_string());
        assert!(load_devices(&dir).unwrap().is_empty());

        let addr = EndpointAddr::new(node).with_ip_addr("127.0.0.1:4433".parse().unwrap());
        let host = RemoteHost::from_endpoint_addr(
            "home-mac".to_owned(),
            "집 맥미니",
            &addr,
            1_700_000_000,
        );
        upsert_host(&dir, host).unwrap();
        let found = find_host(&dir, "home-mac").unwrap();
        assert_eq!(found.name, "집 맥미니");
        assert_eq!(found.to_endpoint_addr(false).unwrap(), addr);

        // 다른 node_id가 같은 host.id("home-mac")로 들어오면 기존 호스트를 덮어쓰지 않고 구분된 ID를 부여한다.
        let sk2 = SecretKey::generate();
        let addr2 = EndpointAddr::new(sk2.public()).with_ip_addr("127.0.0.1:4434".parse().unwrap());
        let host2 = RemoteHost::from_endpoint_addr(
            "home-mac".to_owned(),
            "두번째 맥미니",
            &addr2,
            1_700_000_001,
        );
        let saved2 = upsert_host(&dir, host2).unwrap();
        assert_ne!(saved2.id, "home-mac");
        assert_eq!(load_hosts(&dir).unwrap().len(), 2);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invite_token_single_use_and_expiry_and_redaction() {
        let dir = std::env::temp_dir().join(format!("mklink-inv-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();

        let (tok, rec) = create_invite(&dir, Duration::from_secs(60)).unwrap();
        // Debug 출력과 디스크 파일 모두 토큰 원문을 담지 않아야 한다.
        assert!(!format!("{tok:?}").contains(tok.as_str()));
        let on_disk = fs::read_to_string(invites_path(&dir)).unwrap();
        assert!(!on_disk.contains(tok.as_str()));

        // 만료 시각 이후에는 Expired 거부
        let expired_err =
            verify_and_consume_invite(&dir, tok.as_str(), rec.expires_at_ms + 1).unwrap_err();
        assert!(matches!(expired_err, Ok(InviteVerifyError::Expired)));

        // 유효 시각 내 1회 사용 성공
        let consumed =
            verify_and_consume_invite(&dir, tok.as_str(), rec.created_at_ms + 100).unwrap();
        assert!(consumed.used);

        // 재사용 시 AlreadyUsed 거부
        let reuse_err =
            verify_and_consume_invite(&dir, tok.as_str(), rec.created_at_ms + 200).unwrap_err();
        assert!(matches!(reuse_err, Ok(InviteVerifyError::AlreadyUsed)));

        fs::remove_dir_all(&dir).ok();
    }
}
