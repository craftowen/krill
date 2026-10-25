//! 원격 호스트(`HostId::Remote`) 파일 탐색·열기·원자적 저장·빠른 열기·로컬호스트 URL 포트포워딩 클라이언트.
//!
//! 모든 요청은 기존 [`HostTarget`] 제어 소켓(`FEAT_CONTROL | FEAT_REMOTE_FS`) 위에서 동작하며
//! 별도의 SSH/SFTP 프로세스를 띄우지 않는다.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::hash::{BuildHasher as _, Hash as _, Hasher as _};
use std::io::{self, Write as _};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use krill_hosts::{HostId, HostTarget};
use krill_proto::control::{FEAT_CONTROL, FEAT_REMOTE_FS, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::os::ffi::{OsStrExt as _, OsStringExt as _};
use krill_proto::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use krill_proto::remote_fs::{
    MAX_REMOTE_CHUNK, RemoteFsReply, RemoteFsRequest, WireDirListing, WireExplorerOp,
    WireReadChunk, WireStamp, WireStat, WireWriteOutcome,
};
use krill_proto::screen::Msg as _;

use crate::editor::{self, Opened, SaveError, Stamp, Unopenable};
use crate::file_format::{self, ImageFormat, Kind, csv, raster};

const RPC_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REMOTE_CHUNK_U32: u32 = 512 * 1024;

/// 기본 앱으로 열기·Finder에서 보기 시 로컬 캐시로 내려받는 최대 크기(64 MiB).
pub const MAX_STAGE_BYTES: u64 = 64 * 1024 * 1024;

/// 원격 로컬호스트 URL 클릭으로 연 로컬 포트포워딩 리스너의 유휴 자동 해제 시간(10분).
const IDLE_FORWARD_TIMEOUT: Duration = Duration::from_mins(10);

fn path_to_wire(p: &Path) -> Vec<u8> {
    p.as_os_str().as_bytes().to_vec()
}

fn path_from_wire(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes))
}

fn stamp_from_opt_wire(w: Option<WireStamp>) -> Stamp {
    Stamp::from_wire(w.unwrap_or(WireStamp {
        modified_unix_ns: None,
        len: 0,
    }))
}

/// 원격 호스트 데몬에 `RemoteFsRequest` 1회를 보내고 응답을 받는다.
pub fn request(target: &HostTarget, req: RemoteFsRequest) -> io::Result<RemoteFsReply> {
    let hello = daemon::client_hello(ClientRole::App, "krill-app-remote-fs");
    let conn = target.connect(&hello).map_err(io::Error::other)?;
    let need = FEAT_CONTROL | FEAT_REMOTE_FS;
    if conn.negotiated.features & need != need {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "remote filesystem RPC unavailable (update remote krilld)",
        ));
    }
    let mut sock = conn.stream;
    sock.set_read_timeout(Some(RPC_TIMEOUT))?;
    sock.set_write_timeout(Some(RPC_TIMEOUT))?;
    if let Err(e) = sock.write_all(&Request::RemoteFs(req).encode()?) {
        target.record_outcome(false, Some(&e));
        return Err(e);
    }
    let frame = match ipc::read_frame(&mut sock) {
        Ok(Some(frame)) => frame,
        Ok(None) => {
            let e = io::Error::from(io::ErrorKind::UnexpectedEof);
            target.record_outcome(false, Some(&e));
            return Err(e);
        }
        Err(e) => {
            target.record_outcome(false, Some(&e));
            return Err(e);
        }
    };
    target.record_outcome(true, None);
    let (kind, bytes) = frame;
    match Reply::decode(kind, &bytes)? {
        Reply::RemoteFs(reply) => Ok(reply),
        Reply::Failed(f) => Err(io::Error::other(f.to_string())),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected remote_fs reply",
        )),
    }
}

/// 후보 경로들을 원격 호스트에서 한 번에 검사해 존재하는 파일 경로만 돌려준다.
#[must_use]
pub fn probe_paths(
    target: &HostTarget,
    cwd: Option<&Path>,
    candidates: &[PathBuf],
) -> Vec<Option<PathBuf>> {
    if candidates.is_empty() {
        return Vec::new();
    }
    match request(
        target,
        RemoteFsRequest::ProbePaths {
            cwd: cwd.map(path_to_wire),
            paths: candidates.iter().map(|p| path_to_wire(p)).collect(),
        },
    ) {
        Ok(RemoteFsReply::ProbedPaths(resolved)) => resolved
            .into_iter()
            .map(|opt| opt.map(path_from_wire))
            .collect(),
        _ => vec![None; candidates.len()],
    }
}

/// 원격 파일의 [`Stamp`]를 조회한다(파일이 없으면 `Ok(None)`).
pub fn stat(target: &HostTarget, path: &Path) -> io::Result<Option<Stamp>> {
    match request(
        target,
        RemoteFsRequest::Stat {
            path: path_to_wire(path),
        },
    )? {
        RemoteFsReply::Stat(Ok(s)) => {
            if s.exists {
                Ok(s.stamp.map(Stamp::from_wire))
            } else {
                Ok(None)
            }
        }
        RemoteFsReply::Stat(Err(e)) => Err(io::Error::other(e)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected stat reply",
        )),
    }
}

fn read_chunk(
    target: &HostTarget,
    path: &Path,
    offset: u64,
    max_bytes: u32,
) -> io::Result<WireReadChunk> {
    let chunk_cap = u32::try_from(MAX_REMOTE_CHUNK).unwrap_or(MAX_REMOTE_CHUNK_U32);
    match request(
        target,
        RemoteFsRequest::ReadFile {
            path: path_to_wire(path),
            offset,
            max_bytes: max_bytes.min(chunk_cap),
        },
    )? {
        RemoteFsReply::FileChunk(Ok(chunk)) => Ok(chunk),
        RemoteFsReply::FileChunk(Err(e)) => Err(io::Error::other(e)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected read chunk reply",
        )),
    }
}

fn read_rest_bounded(
    target: &HostTarget,
    path: &Path,
    mut bytes: Vec<u8>,
    mut eof: bool,
    max_total: u64,
) -> io::Result<Vec<u8>> {
    let chunk_cap = u32::try_from(MAX_REMOTE_CHUNK).unwrap_or(MAX_REMOTE_CHUNK_U32);
    while !eof && byte_len(&bytes) < max_total {
        let offset = byte_len(&bytes);
        let want =
            u32::try_from((max_total - offset).min(u64::from(chunk_cap))).unwrap_or(chunk_cap);
        if want == 0 {
            break;
        }
        let chunk = read_chunk(target, path, offset, want)?;
        if chunk.bytes.is_empty() {
            break;
        }
        bytes.extend_from_slice(&chunk.bytes);
        eof = chunk.eof;
    }
    Ok(bytes)
}

fn cache_file_path(host: &HostId, remote_path: &Path) -> io::Result<PathBuf> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    host.as_key().hash(&mut hasher);
    remote_path.hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());

    let host_slug: String = host
        .as_key()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let file_name = remote_path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty() && !s.contains('/') && !s.contains('\\'))
        .unwrap_or("remote-file");
    let dir = std::env::temp_dir()
        .join("krill-remote-files")
        .join(if host_slug.is_empty() {
            "remote"
        } else {
            &host_slug
        })
        .join(hash);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    Ok(dir.join(file_name))
}

fn write_cache_bytes(local_path: &Path, bytes: &[u8]) -> io::Result<()> {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let parent = local_path
        .parent()
        .ok_or_else(|| io::Error::other("missing cache parent dir"))?;
    let tmp = parent.join(format!(
        ".stage-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| -> io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        Ok(())
    })();
    if let Err(e) = written {
        fs::remove_file(&tmp).ok();
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, local_path) {
        fs::remove_file(&tmp).ok();
        return Err(e);
    }
    Ok(())
}

/// 원격 파일을 로컬 캐시 경로로 내려받아 저장한다(이미지 뷰어·기본 앱으로 열기·Finder에서 보기용).
pub fn stage_to_local_cache(
    target: &HostTarget,
    host: &HostId,
    remote_path: &Path,
    max_bytes: u64,
) -> io::Result<(PathBuf, Stamp)> {
    let chunk_cap = u32::try_from(MAX_REMOTE_CHUNK).unwrap_or(MAX_REMOTE_CHUNK_U32);
    let first = read_chunk(target, remote_path, 0, chunk_cap)?;
    let stamp = stamp_from_opt_wire(first.stat.stamp);
    if first.stat.is_dir || !first.stat.is_file {
        return Err(io::Error::other("remote path is not a regular file"));
    }
    let file_len = first.stat.stamp.map_or(0, |s| s.len);
    if file_len > max_bytes {
        return Err(io::Error::other("remote file exceeds staging size limit"));
    }
    let bytes = read_rest_bounded(target, remote_path, first.bytes, first.eof, max_bytes)?;
    let local_path = cache_file_path(host, remote_path)?;
    write_cache_bytes(&local_path, &bytes)?;
    Ok((local_path, stamp))
}

/// 원격 파일을 읽어 에디터/뷰어 [`Opened`]와 [`Stamp`], 그리고 래스터 이미지인 경우 로컬 캐시 경로를 돌려준다.
pub fn open(
    target: &HostTarget,
    host: &HostId,
    path: &Path,
) -> io::Result<(Opened, Stamp, Option<PathBuf>)> {
    let chunk_cap = u32::try_from(MAX_REMOTE_CHUNK).unwrap_or(MAX_REMOTE_CHUNK_U32);
    let first = read_chunk(target, path, 0, chunk_cap)?;
    let WireStat {
        stamp: wire_stamp,
        is_dir,
        is_file,
        ..
    } = first.stat;
    let stamp = stamp_from_opt_wire(wire_stamp);
    if is_dir {
        return Ok((Opened::Unopenable(Unopenable::Directory), stamp, None));
    }
    if !is_file {
        return Ok((Opened::Unopenable(Unopenable::NotFile), stamp, None));
    }
    let len = wire_stamp.map_or(byte_len(&first.bytes), |s| s.len);
    let kind = file_format::classify(path, &first.bytes, len);
    match kind {
        Kind::Image(ImageFormat::Raster) => {
            let dims = raster::check(&first.bytes);
            if dims.is_err() {
                return Ok((Opened::Image(dims), stamp, None));
            }
            let bytes = read_rest_bounded(
                target,
                path,
                first.bytes,
                first.eof,
                file_format::MAX_IMAGE_BYTES,
            )?;
            let local_path = cache_file_path(host, path)?;
            write_cache_bytes(&local_path, &bytes)?;
            Ok((Opened::Image(dims), stamp, Some(local_path)))
        }
        kind if kind.is_text_editable() => {
            let bytes =
                read_rest_bounded(target, path, first.bytes, first.eof, editor::MAX_BYTES + 1)?;
            let total = byte_len(&bytes);
            let opened = match kind {
                _ if total <= editor::MAX_BYTES => String::from_utf8(bytes)
                    .map_or(Opened::Unopenable(Unopenable::NotUtf8), |text| {
                        Opened::Text(kind, text)
                    }),
                Kind::Csv { delimiter } => {
                    let mut preview_bytes = bytes;
                    preview_bytes.truncate(csv::PREVIEW_BYTES);
                    Opened::Table(csv::preview(&preview_bytes, len.max(total), delimiter))
                }
                _ => Opened::Placeholder(Kind::TooLarge, len.max(total)),
            };
            Ok((opened, stamp, None))
        }
        kind => Ok((Opened::Placeholder(kind, len), stamp, None)),
    }
}

/// 원격 텍스트 파일을 원자적으로 저장한다(`expected` 지문이 다르면 [`SaveError::Conflict`]).
pub fn save_atomic(
    target: &HostTarget,
    path: &Path,
    text: &str,
    expected: Option<Stamp>,
) -> Result<Stamp, SaveError> {
    static STAGE_SEQ: AtomicU64 = AtomicU64::new(1);
    let data = text.as_bytes();
    let chunk_limit = MAX_REMOTE_CHUNK;
    let expected_wire = expected.map(Stamp::to_wire);

    if data.len() <= chunk_limit {
        let reply = request(
            target,
            RemoteFsRequest::WriteAtomic {
                path: path_to_wire(path),
                bytes: data.to_vec(),
                expected: expected_wire,
                stage_id: 0,
                offset: 0,
                commit: true,
            },
        )?;
        return match reply {
            RemoteFsReply::Written(WireWriteOutcome::Saved(w)) => Ok(Stamp::from_wire(w)),
            RemoteFsReply::Written(WireWriteOutcome::Conflict { .. }) => Err(SaveError::Conflict),
            RemoteFsReply::Written(WireWriteOutcome::StageAck) => {
                Err(io::Error::other("unexpected staged reply on single-chunk write").into())
            }
            RemoteFsReply::Written(WireWriteOutcome::Failed(e)) => Err(io::Error::other(e).into()),
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected write reply").into()),
        };
    }

    let stage_id = std::collections::hash_map::RandomState::new()
        .hash_one((
            std::process::id(),
            STAGE_SEQ.fetch_add(1, Ordering::Relaxed),
        ))
        .max(1);
    let mut offset = 0usize;
    while offset < data.len() {
        let end = (offset + chunk_limit).min(data.len());
        let commit = end == data.len();
        let reply = request(
            target,
            RemoteFsRequest::WriteAtomic {
                path: path_to_wire(path),
                bytes: data[offset..end].to_vec(),
                expected: if commit { expected_wire } else { None },
                stage_id,
                offset: u64::try_from(offset).unwrap_or(u64::MAX),
                commit,
            },
        )?;
        match reply {
            RemoteFsReply::Written(WireWriteOutcome::StageAck) if !commit => {
                offset = end;
            }
            RemoteFsReply::Written(WireWriteOutcome::Saved(w)) if commit => {
                return Ok(Stamp::from_wire(w));
            }
            RemoteFsReply::Written(WireWriteOutcome::Conflict { .. }) => {
                return Err(SaveError::Conflict);
            }
            RemoteFsReply::Written(WireWriteOutcome::Failed(e)) => {
                return Err(io::Error::other(e).into());
            }
            RemoteFsReply::Written(_) => {
                return Err(io::Error::other("unexpected multi-chunk write state").into());
            }
            _ => {
                return Err(
                    io::Error::new(io::ErrorKind::InvalidData, "unexpected write reply").into(),
                );
            }
        }
    }
    Err(io::Error::other("unreachable multi-chunk write end").into())
}

/// 원격 디렉터리 1단계를 읽는다(우측 파일 탐색기용).
pub fn read_dir(target: &HostTarget, repo: &Path, dir: &Path) -> io::Result<WireDirListing> {
    match request(
        target,
        RemoteFsRequest::ReadDir {
            repo: path_to_wire(repo),
            dir: path_to_wire(dir),
        },
    )? {
        RemoteFsReply::DirEntries(Ok(listing)) => Ok(listing),
        RemoteFsReply::DirEntries(Err(e)) => Err(io::Error::other(e)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected read_dir reply",
        )),
    }
}

/// 원격 워크트리의 전체 디렉터리·파일을 스캔한다(우측 파일 탐색기 필터용).
pub fn scan_worktree(target: &HostTarget, repo: &Path, limit: usize) -> io::Result<WireDirListing> {
    match request(
        target,
        RemoteFsRequest::ScanWorktree {
            repo: path_to_wire(repo),
            limit: u32::try_from(limit).unwrap_or(u32::MAX),
        },
    )? {
        RemoteFsReply::DirEntries(Ok(listing)) => Ok(listing),
        RemoteFsReply::DirEntries(Err(e)) => Err(io::Error::other(e)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected scan_worktree reply",
        )),
    }
}

/// 원격 파일 탐색기 파일/폴더 생성·이름 변경·삭제 연산을 수행한다.
pub fn explorer_op(target: &HostTarget, repo: &Path, op: WireExplorerOp) -> io::Result<PathBuf> {
    match request(
        target,
        RemoteFsRequest::ExplorerOp {
            repo: path_to_wire(repo),
            op,
        },
    )? {
        RemoteFsReply::ExplorerOpResult(Ok(path)) => Ok(path_from_wire(path)),
        RemoteFsReply::ExplorerOpResult(Err(e)) => Err(io::Error::other(e)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected explorer_op reply",
        )),
    }
}

/// 원격 워크트리의 Quick Open(`⌘P`) 상대 경로 목록을 조회한다.
#[must_use]
pub fn quick_open_list(target: &HostTarget, root: &Path) -> Option<Vec<PathBuf>> {
    match request(
        target,
        RemoteFsRequest::QuickOpenList {
            root: path_to_wire(root),
        },
    ) {
        Ok(RemoteFsReply::QuickOpenFiles(Ok(files))) => {
            Some(files.into_iter().map(path_from_wire).collect())
        }
        _ => None,
    }
}

fn byte_len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------
// 원격 로컬호스트 URL 클릭 자동 포트포워딩 (유휴 시 자동 해제)
// ---------------------------------------------------------------------------

struct ActiveForward {
    local_port: u16,
    target: HostTarget,
    last_active_ms: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
}

static FORWARD_EPOCH: OnceLock<Instant> = OnceLock::new();
static FORWARDS: OnceLock<Mutex<HashMap<(HostId, u16), ActiveForward>>> = OnceLock::new();

fn now_ms() -> u64 {
    let epoch = FORWARD_EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// URL이 루프백(`localhost`, `127.0.0.1`, `0.0.0.0`, `[::1]`)과 명시적 포트를 가진 `http(s)` URL이면 포트를 돌려준다.
#[must_use]
pub fn parse_loopback_http_url(uri: &str) -> Option<u16> {
    let (_, host, port, _) = split_http_url(uri)?;
    if !is_loopback_host(host) {
        return None;
    }
    port
}

/// 루프백 `http(s)` URL의 포트를 로컬 포워딩 포트로 바꾼 URL을 돌려준다.
/// `0.0.0.0`·`[::1]`은 로컬 IPv4 리스너(`127.0.0.1`)에 맞게 `localhost` 또는 `127.0.0.1`로 정규화한다.
#[must_use]
pub fn rewrite_loopback_url_port(uri: &str, local_port: u16) -> String {
    let Some((scheme, host, Some(_), tail)) = split_http_url(uri) else {
        return uri.to_owned();
    };
    if !is_loopback_host(host) {
        return uri.to_owned();
    }
    let mapped_host = if host.eq_ignore_ascii_case("localhost") || host == "0.0.0.0" {
        "localhost"
    } else {
        "127.0.0.1"
    };
    format!("{scheme}://{mapped_host}:{local_port}{tail}")
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "0.0.0.0"
        || host == "[::1]"
}

fn split_http_url(uri: &str) -> Option<(&str, &str, Option<u16>, &str)> {
    let (scheme, rest) = if let Some(r) = uri.strip_prefix("http://") {
        ("http", r)
    } else {
        ("https", uri.strip_prefix("https://")?)
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at_checked(authority_end)?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port_str) = if authority.starts_with('[') {
        let close = authority.find(']')?;
        let (host, after) = authority.split_at_checked(close + 1)?;
        let port_str = after.strip_prefix(':');
        (host, port_str)
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        (h, Some(p))
    } else {
        (authority, None)
    };
    let port = match port_str {
        Some(p) => {
            let parsed = p.parse::<u16>().ok()?;
            if parsed == 0 {
                return None;
            }
            Some(parsed)
        }
        None => None,
    };
    Some((scheme, host, port, tail))
}

/// 원격 호스트의 `127.0.0.1:<remote_port>`로 연결되는 로컬 루프백 리스너를 보장하고 로컬 포트를 돌려준다.
///
/// - 가능하면 동일한 로컬 포트(`127.0.0.1:<remote_port>`)를 바인드하고, 이미 사용 중이면 임시 포트를 할당한다.
/// - 활성 연결이 없고 [`IDLE_FORWARD_TIMEOUT`] 동안 트래픽이 없으면 리스너를 자동으로 닫아 로컬 포트를 반납한다.
pub fn ensure_port_forward(
    host: &HostId,
    target: &HostTarget,
    remote_port: u16,
) -> io::Result<u16> {
    ensure_port_forward_with_idle(host, target, remote_port, IDLE_FORWARD_TIMEOUT)
}

fn ensure_port_forward_with_idle(
    host: &HostId,
    target: &HostTarget,
    remote_port: u16,
    idle_timeout: Duration,
) -> io::Result<u16> {
    let map = FORWARDS.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (host.clone(), remote_port);
    if let Ok(guard) = map.lock()
        && let Some(existing) = guard.get(&key)
        && existing.alive.load(Ordering::Acquire)
        && existing.target == *target
    {
        existing.last_active_ms.store(now_ms(), Ordering::Release);
        return Ok(existing.local_port);
    }

    let listener = TcpListener::bind(("127.0.0.1", remote_port))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))?;
    let local_port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;

    let last_active_ms = Arc::new(AtomicU64::new(now_ms()));
    let alive = Arc::new(AtomicBool::new(true));
    let active_conns = Arc::new(AtomicUsize::new(0));

    if let Ok(mut guard) = map.lock()
        && let Some(prev) = guard.insert(
            key.clone(),
            ActiveForward {
                local_port,
                target: target.clone(),
                last_active_ms: last_active_ms.clone(),
                alive: alive.clone(),
            },
        )
    {
        prev.alive.store(false, Ordering::Release);
    }

    let target_cloned = target.clone();
    let last_active_loop = last_active_ms.clone();
    let alive_loop = alive.clone();
    let idle_ms = u64::try_from(idle_timeout.as_millis()).unwrap_or(600_000);

    thread::Builder::new()
        .name(format!("krill-pf-{remote_port}-{local_port}"))
        .spawn(move || {
            loop {
                if !alive_loop.load(Ordering::Acquire) || !target_cloned.is_current() {
                    break;
                }
                match listener.accept() {
                    Ok((tcp, _)) => {
                        tcp.set_nonblocking(false).ok();
                        tcp.set_nodelay(true).ok();
                        last_active_loop.store(now_ms(), Ordering::Release);
                        active_conns.fetch_add(1, Ordering::AcqRel);
                        let conn_target = target_cloned.clone();
                        let conn_active = active_conns.clone();
                        let conn_last = last_active_loop.clone();
                        thread::spawn(move || {
                            let _guard = ConnGuard {
                                active_conns: conn_active,
                                last_active_ms: conn_last.clone(),
                            };
                            bridge_forwarded_conn(tcp, &conn_target, remote_port, &conn_last).ok();
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        let conns = active_conns.load(Ordering::Acquire);
                        let elapsed =
                            now_ms().saturating_sub(last_active_loop.load(Ordering::Acquire));
                        if conns == 0 && elapsed >= idle_ms {
                            break;
                        }
                        thread::sleep(Duration::from_millis(40));
                    }
                    Err(_) => {
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }
            alive_loop.store(false, Ordering::Release);
            if let Some(map) = FORWARDS.get()
                && let Ok(mut guard) = map.lock()
                && guard
                    .get(&key)
                    .is_some_and(|cur| Arc::ptr_eq(&cur.alive, &alive_loop))
            {
                guard.remove(&key);
            }
        })?;

    Ok(local_port)
}

struct ConnGuard {
    active_conns: Arc<AtomicUsize>,
    last_active_ms: Arc<AtomicU64>,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.last_active_ms.store(now_ms(), Ordering::Release);
        self.active_conns.fetch_sub(1, Ordering::AcqRel);
    }
}

fn bridge_forwarded_conn(
    mut tcp: TcpStream,
    target: &HostTarget,
    remote_port: u16,
    last_active_ms: &Arc<AtomicU64>,
) -> io::Result<()> {
    let hello = daemon::client_hello(ClientRole::App, "krill-app-port-forward");
    let conn = target.connect(&hello).map_err(io::Error::other)?;
    let need = FEAT_CONTROL | FEAT_REMOTE_FS;
    if conn.negotiated.features & need != need {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "remote port-forward unavailable",
        ));
    }
    let mut ipc_stream = conn.stream;
    ipc_stream.set_read_timeout(Some(RPC_TIMEOUT))?;
    ipc_stream.set_write_timeout(Some(RPC_TIMEOUT))?;
    ipc_stream.write_all(
        &Request::RemoteFs(RemoteFsRequest::PortForward { port: remote_port }).encode()?,
    )?;
    let (kind, bytes) = ipc::read_frame(&mut ipc_stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    match Reply::decode(kind, &bytes)? {
        Reply::RemoteFs(RemoteFsReply::PortForwarded(Ok(()))) => {}
        Reply::RemoteFs(RemoteFsReply::PortForwarded(Err(e))) => {
            return Err(io::Error::other(e));
        }
        Reply::Failed(f) => return Err(io::Error::other(f.to_string())),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected port-forward reply",
            ));
        }
    }
    ipc_stream.set_read_timeout(None)?;
    ipc_stream.set_write_timeout(None)?;

    let mut ipc_read = ipc_stream.try_clone()?;
    let mut tcp_write = tcp.try_clone()?;
    let touch_a = last_active_ms.clone();
    let t = thread::spawn(move || {
        copy_touching(&mut ipc_read, &mut tcp_write, &touch_a);
        tcp_write.shutdown(Shutdown::Write).ok();
    });
    copy_touching(&mut tcp, &mut ipc_stream, last_active_ms);
    ipc_stream.shutdown(Shutdown::Write).ok();
    t.join().ok();
    Ok(())
}

fn copy_touching(reader: &mut impl io::Read, writer: &mut impl io::Write, touch: &AtomicU64) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        touch.store(now_ms(), Ordering::Release);
        if writer.write_all(&buf[..n]).is_err() {
            break;
        }
        touch.store(now_ms(), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use krill_proto::ipc::{ClientHello, ServerHello};
    use krill_proto::os::net::UnixListener;
    use krill_proto::remote_fs::WireStamp;
    use std::io::Read as _;
    use std::sync::atomic::AtomicU32;

    use super::*;

    fn accept_test_handshake(stream: &mut impl io::Read, writer: &mut impl io::Write) {
        let server = ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features: FEAT_CONTROL | FEAT_REMOTE_FS,
            build: "test".to_owned(),
            instance_id: 1,
        };
        writer.write_all(&server.encode().unwrap()).unwrap();
        let (kind, body) = ipc::read_frame(stream).unwrap().unwrap();
        let _client = ClientHello::decode(kind, &body).unwrap();
    }

    #[test]
    fn loopback_url_parse_and_rewrite() {
        assert_eq!(
            parse_loopback_http_url("http://localhost:3000/app?x=1#top"),
            Some(3000)
        );
        assert_eq!(
            parse_loopback_http_url("https://127.0.0.1:8080/api"),
            Some(8080)
        );
        assert_eq!(parse_loopback_http_url("http://0.0.0.0:5173/"), Some(5173));
        assert_eq!(parse_loopback_http_url("http://[::1]:4321"), Some(4321));
        assert_eq!(parse_loopback_http_url("http://localhost/no-port"), None);
        assert_eq!(parse_loopback_http_url("https://example.com:3000/"), None);

        assert_eq!(
            rewrite_loopback_url_port("http://localhost:3000/app?x=1#top", 54321),
            "http://localhost:54321/app?x=1#top"
        );
        assert_eq!(
            rewrite_loopback_url_port("http://0.0.0.0:5173/index.html", 5173),
            "http://localhost:5173/index.html"
        );
        assert_eq!(
            rewrite_loopback_url_port("http://127.0.0.1:8080/health", 9090),
            "http://127.0.0.1:9090/health"
        );
        assert_eq!(
            rewrite_loopback_url_port("http://[::1]:8080/health", 9090),
            "http://127.0.0.1:9090/health"
        );
    }

    #[test]
    fn port_forward_bridges_and_auto_releases_on_idle() {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let sock_dir = std::env::temp_dir().join(format!(
            "krill-pf-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&sock_dir).unwrap();
        let sock_path = sock_dir.join("d.sock");
        let ipc_listener = UnixListener::bind(&sock_path).unwrap();

        let daemon_thread = thread::spawn(move || {
            let (mut stream, _) = ipc_listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            accept_test_handshake(&mut stream, &mut writer);
            let (kind, payload) = ipc::read_frame(&mut stream).unwrap().unwrap();
            let req = Request::decode(kind, &payload).unwrap();
            assert_eq!(
                req,
                Request::RemoteFs(RemoteFsRequest::PortForward { port: 18080 })
            );
            stream
                .write_all(
                    &Reply::RemoteFs(RemoteFsReply::PortForwarded(Ok(())))
                        .encode()
                        .unwrap(),
                )
                .unwrap();
            let mut req_bytes = [0u8; 4];
            stream.read_exact(&mut req_bytes).unwrap();
            assert_eq!(&req_bytes, b"GET ");
            stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
        });

        let host = HostId::Remote("box".into());
        let target = HostTarget::remote("box", sock_path);
        let local_port =
            ensure_port_forward_with_idle(&host, &target, 18080, Duration::from_millis(120))
                .unwrap();

        let mut client = TcpStream::connect(("127.0.0.1", local_port)).unwrap();
        client.write_all(b"GET ").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut resp = Vec::new();
        client.read_to_end(&mut resp).unwrap();
        assert_eq!(resp, b"HTTP/1.1 200 OK\r\n\r\n");
        drop(client);
        daemon_thread.join().unwrap();

        // Inspect the listener state: probing TCP would itself reset the idle timer.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut released = false;
        while Instant::now() < deadline {
            let still_alive = FORWARDS
                .get()
                .and_then(|m| m.lock().ok())
                .and_then(|g| {
                    g.get(&(host.clone(), 18080))
                        .map(|f| f.alive.load(Ordering::Acquire))
                })
                .unwrap_or(false);
            if !still_alive {
                released = TcpStream::connect(("127.0.0.1", local_port)).is_err();
                break;
            }
            thread::sleep(Duration::from_millis(40));
        }
        fs::remove_dir_all(&sock_dir).ok();
        assert!(
            released,
            "expected forwarded local port to auto-release after idle timeout"
        );
    }

    #[test]
    fn remote_open_and_save_atomic_over_ipc() {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let sock_dir = std::env::temp_dir().join(format!(
            "krill-rfs-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&sock_dir).unwrap();
        let sock_path = sock_dir.join("d.sock");
        let ipc_listener = UnixListener::bind(&sock_path).unwrap();

        let stamp1 = WireStamp {
            modified_unix_ns: Some(1_700_000_000_000_000_000),
            len: 6,
        };
        let stamp2 = WireStamp {
            modified_unix_ns: Some(1_700_000_001_000_000_000),
            len: 8,
        };

        let daemon_thread = thread::spawn(move || {
            for step in 0..2 {
                let (mut stream, _) = ipc_listener.accept().unwrap();
                let mut writer = stream.try_clone().unwrap();
                accept_test_handshake(&mut stream, &mut writer);
                let (kind, payload) = ipc::read_frame(&mut stream).unwrap().unwrap();
                let req = Request::decode(kind, &payload).unwrap();
                let reply = match (step, req) {
                    (0, Request::RemoteFs(RemoteFsRequest::ReadFile { .. })) => {
                        Reply::RemoteFs(RemoteFsReply::FileChunk(Ok(WireReadChunk {
                            stat: WireStat {
                                exists: true,
                                is_file: true,
                                is_dir: false,
                                is_symlink: false,
                                stamp: Some(stamp1),
                            },
                            bytes: b"hello\n".to_vec(),
                            eof: true,
                        })))
                    }
                    (
                        1,
                        Request::RemoteFs(RemoteFsRequest::WriteAtomic {
                            bytes,
                            expected,
                            commit: true,
                            ..
                        }),
                    ) => {
                        assert_eq!(bytes, b"updated\n");
                        assert_eq!(expected, Some(stamp1));
                        Reply::RemoteFs(RemoteFsReply::Written(WireWriteOutcome::Saved(stamp2)))
                    }
                    other => panic!("unexpected step/request: {other:?}"),
                };
                stream.write_all(&reply.encode().unwrap()).unwrap();
            }
        });

        let host = HostId::Remote("box".into());
        let target = HostTarget::remote("box", sock_path);
        let (opened, loaded_stamp, staged) =
            open(&target, &host, Path::new("/remote/repo/note.txt")).unwrap();
        assert!(staged.is_none());
        assert_eq!(loaded_stamp, Stamp::from_wire(stamp1));
        assert!(matches!(opened, Opened::Text(_, ref s) if s == "hello\n"));

        let saved_stamp = save_atomic(
            &target,
            Path::new("/remote/repo/note.txt"),
            "updated\n",
            Some(loaded_stamp),
        )
        .unwrap();
        assert_eq!(saved_stamp, Stamp::from_wire(stamp2));

        daemon_thread.join().unwrap();
        fs::remove_dir_all(&sock_dir).ok();
    }
}
