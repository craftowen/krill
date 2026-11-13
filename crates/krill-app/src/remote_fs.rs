//! 원격 호스트(`HostId::Remote`) 파일 탐색·열기·첨부 업로드·빠른 열기·로컬호스트 URL 포트포워딩 클라이언트.
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

use crate::editor::{self, Opened, Stamp, Unopenable};
use crate::file_format::{self, ImageFormat, Kind, csv, raster};

const RPC_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REMOTE_CHUNK_U32: u32 = 512 * 1024;

/// 이 PC에서 열기·내려받아 Finder에서 보기 시 단일 파일 스트리밍 최대 크기(4 GiB).
pub const MAX_STAGE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_CACHE_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MIN_FREE_DISK_BYTES: u64 = 256 * 1024 * 1024;
const STALE_STAGE_MAX_AGE: Duration = Duration::from_hours(1);
const HOST_SLUG_MAX: usize = 24;
const FILE_NAME_MAX_BYTES: usize = 120;

#[derive(Debug)]
struct StageSizeLimitError(u64);

impl std::fmt::Display for StageSizeLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "remote file exceeds staging size limit ({} B)", self.0)
    }
}

impl std::error::Error for StageSizeLimitError {}

const fn is_retryable_stream_close(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

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

struct RemoteSession {
    stream: Option<krill_proto::os::net::UnixStream>,
}

impl RemoteSession {
    const fn new() -> Self {
        Self { stream: None }
    }

    fn connect_fresh(target: &HostTarget) -> io::Result<krill_proto::os::net::UnixStream> {
        let hello = daemon::client_hello(ClientRole::App, "krill-app-remote-fs");
        let conn = target.connect(&hello).map_err(io::Error::other)?;
        let need = FEAT_CONTROL | FEAT_REMOTE_FS;
        if conn.negotiated.features & need != need {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "remote filesystem RPC unavailable (update remote krilld)",
            ));
        }
        conn.stream.set_read_timeout(Some(RPC_TIMEOUT))?;
        conn.stream.set_write_timeout(Some(RPC_TIMEOUT))?;
        Ok(conn.stream)
    }

    /// 단일 제어 스트림 위에서 `RemoteFsRequest`를 연속 호출한다.
    /// 읽기 전용 요청에서 이전 응답 직후 피어가 유휴 스트림을 닫았으면 1회 새 연결로 이어간다.
    fn request(&mut self, target: &HostTarget, req: RemoteFsRequest) -> io::Result<RemoteFsReply> {
        let can_retry = req.is_read_only() && self.stream.is_some();
        let encoded = Request::RemoteFs(req).encode()?;
        match self.send_encoded(target, &encoded, can_retry) {
            Ok(reply) => Ok(reply),
            Err(err) if can_retry && is_retryable_stream_close(err.kind()) => {
                self.stream = None;
                self.send_encoded(target, &encoded, false)
            }
            Err(err) => {
                self.stream = None;
                Err(err)
            }
        }
    }

    fn send_encoded(
        &mut self,
        target: &HostTarget,
        encoded: &[u8],
        allow_retry: bool,
    ) -> io::Result<RemoteFsReply> {
        if self.stream.is_none() {
            self.stream = Some(Self::connect_fresh(target)?);
        }
        let Some(sock) = self.stream.as_mut() else {
            return Err(io::Error::other("missing remote_fs stream"));
        };
        let record_err = |e: &io::Error| {
            if !(allow_retry && is_retryable_stream_close(e.kind())) {
                target.record_outcome(false, Some(e));
            }
        };
        if let Err(e) = sock.write_all(encoded) {
            record_err(&e);
            return Err(e);
        }
        let frame = match ipc::read_frame(sock) {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                let e = io::Error::from(io::ErrorKind::UnexpectedEof);
                record_err(&e);
                return Err(e);
            }
            Err(e) => {
                record_err(&e);
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
}

/// 원격 호스트 데몬에 `RemoteFsRequest` 1회를 보내고 응답을 받는다.
pub fn request(target: &HostTarget, req: RemoteFsRequest) -> io::Result<RemoteFsReply> {
    let mut session = RemoteSession::new();
    session.request(target, req)
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
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "remote file ended before its reported end",
            ));
        }
        bytes.extend_from_slice(&chunk.bytes);
        eof = chunk.eof;
    }
    Ok(bytes)
}

fn truncate_utf8_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

fn is_dos_reserved(stem: &str) -> bool {
    let prefix = stem.split('.').next().unwrap_or(stem).trim_end_matches(' ');
    matches!(
        prefix.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn sanitize_stem(stem: &str, max_bytes: usize, fallback: &str) -> String {
    let trimmed = stem.trim_end_matches([' ', '.']);
    if trimmed.is_empty() {
        return fallback.to_owned();
    }
    let prefixed = if is_dos_reserved(trimmed) {
        format!("_{trimmed}")
    } else {
        trimmed.to_owned()
    };
    let clipped = truncate_utf8_boundary(&prefixed, max_bytes).trim_end_matches([' ', '.']);
    if clipped.is_empty() {
        fallback.to_owned()
    } else if is_dos_reserved(clipped) {
        let escaped = format!("_{clipped}");
        let reclipped = truncate_utf8_boundary(&escaped, max_bytes).trim_end_matches([' ', '.']);
        if reclipped.is_empty() || is_dos_reserved(reclipped) {
            fallback.to_owned()
        } else {
            reclipped.to_owned()
        }
    } else {
        clipped.to_owned()
    }
}

/// 원격 경로(POSIX `/`·Windows `\`)에서 마지막 파일명을 꺼내고, 로컬 OS 파일시스템 금지 문자와
/// 길이 한도를 정리하되 확장자는 보존한다(`open_default_command`의 실행 파일 판정 유지).
fn sanitize_cache_file_name(remote_path: &Path) -> String {
    let raw = remote_path.to_string_lossy();
    let base = raw
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or("remote-file");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if let Some((raw_stem, raw_ext)) = cleaned.rsplit_once('.') {
        let ext = raw_ext.trim_end_matches([' ', '.']);
        if !ext.is_empty() && ext.len() < FILE_NAME_MAX_BYTES.saturating_sub(2) {
            let max_stem = FILE_NAME_MAX_BYTES.saturating_sub(ext.len() + 1);
            let fallback = if max_stem >= "remote-file".len()
                && raw_stem.trim_end_matches([' ', '.']).is_empty()
            {
                "remote-file"
            } else {
                "f"
            };
            let safe_stem = sanitize_stem(raw_stem, max_stem, fallback);
            return format!("{safe_stem}.{ext}");
        }
    }
    sanitize_stem(&cleaned, FILE_NAME_MAX_BYTES, "remote-file")
}

fn default_cache_root() -> PathBuf {
    if daemon::state_dir_isolated()
        && let Some(state) = daemon::state_dir()
    {
        return state.join("remote-files");
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for var in ["HOME", "LOCALAPPDATA", "USER", "USERNAME"] {
        if let Some(val) = std::env::var_os(var) {
            val.hash(&mut hasher);
        }
    }
    std::env::temp_dir().join(format!("krill-remote-files-{:016x}", hasher.finish()))
}

fn ensure_private_real_dir(dir: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cache directory must not be a symlink",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if meta.mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

fn cache_file_path_in(cache_root: &Path, host: &HostId, remote_path: &Path) -> io::Result<PathBuf> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    host.as_key().hash(&mut hasher);
    remote_path.hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());

    let host_slug: String = host
        .as_key()
        .chars()
        .take(HOST_SLUG_MAX)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let file_name = sanitize_cache_file_name(remote_path);
    let host_dir = cache_root.join(if host_slug.is_empty() {
        "remote"
    } else {
        &host_slug
    });
    let dir = host_dir.join(hash);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    ensure_private_real_dir(cache_root)?;
    ensure_private_real_dir(&host_dir)?;
    ensure_private_real_dir(&dir)?;
    Ok(dir.join(file_name))
}

fn cache_file_path(host: &HostId, remote_path: &Path) -> io::Result<PathBuf> {
    cache_file_path_in(&default_cache_root(), host, remote_path)
}

fn stamp_sidecar_path(local_path: &Path) -> Option<PathBuf> {
    local_path.parent().map(|p| p.join(".stamp"))
}

fn read_cached_stamp(local_path: &Path) -> Option<WireStamp> {
    let sidecar = stamp_sidecar_path(local_path)?;
    let text = fs::read_to_string(sidecar).ok()?;
    let mut lines = text.lines();
    let len = lines.next()?.parse::<u64>().ok()?;
    let mtime_raw = lines.next()?;
    let modified_unix_ns = if mtime_raw == "none" {
        None
    } else {
        Some(mtime_raw.parse::<u64>().ok()?)
    };
    Some(WireStamp {
        modified_unix_ns,
        len,
    })
}

fn write_cached_stamp(local_path: &Path, stamp: WireStamp) {
    let Some(sidecar) = stamp_sidecar_path(local_path) else {
        return;
    };
    let mtime = stamp
        .modified_unix_ns
        .map_or_else(|| "none".to_owned(), |ns| ns.to_string());
    let body = format!("{}\n{mtime}\n", stamp.len);
    write_cache_bytes(&sidecar, body.as_bytes()).ok();
}

/// 캐시 루트 아래의 오래된 `.stage-*` 임시 파일을 청소하고, 총량이 `max_total_bytes`를 넘으면
/// 오래된 해시 디렉터리부터 비운다(`keep_dir`와 진행 중인 `.stage-*`가 있는 디렉터리는 제외).
fn evict_remote_cache_if_needed(
    cache_root: &Path,
    keep_dir: &Path,
    incoming_bytes: u64,
    max_total_bytes: u64,
) {
    let Ok(host_dirs) = fs::read_dir(cache_root) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut candidates: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let mut total_bytes: u64 = 0;

    for host_entry in host_dirs.flatten() {
        if !host_entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let host_path = host_entry.path();
        let Ok(hash_dirs) = fs::read_dir(&host_path) else {
            continue;
        };
        for hash_entry in hash_dirs.flatten() {
            if !hash_entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let dir_path = hash_entry.path();
            let Ok(files) = fs::read_dir(&dir_path) else {
                continue;
            };
            let mut dir_bytes: u64 = 0;
            let mut has_active_stage = false;
            let mut newest_mtime = std::time::UNIX_EPOCH;
            for file_entry in files.flatten() {
                if !file_entry.file_type().is_ok_and(|t| t.is_file()) {
                    continue;
                }
                let fp = file_entry.path();
                let Ok(meta) = file_entry.metadata() else {
                    continue;
                };
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                let is_stage = fp
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(".stage-"));
                if is_stage {
                    let stale =
                        now.duration_since(mtime).unwrap_or(Duration::ZERO) >= STALE_STAGE_MAX_AGE;
                    if stale {
                        fs::remove_file(&fp).ok();
                    } else {
                        has_active_stage = true;
                    }
                    continue;
                }
                if mtime > newest_mtime {
                    newest_mtime = mtime;
                }
                dir_bytes = dir_bytes.saturating_add(meta.len());
            }
            total_bytes = total_bytes.saturating_add(dir_bytes);
            if dir_path == keep_dir {
                continue;
            }
            if !has_active_stage {
                candidates.push((newest_mtime, dir_bytes, dir_path));
            }
        }
    }

    if total_bytes.saturating_add(incoming_bytes) <= max_total_bytes {
        return;
    }
    candidates.sort_by_key(|(mtime, _, _)| *mtime);
    for (_, dir_bytes, dir_path) in candidates {
        if total_bytes.saturating_add(incoming_bytes) <= max_total_bytes {
            break;
        }
        if fs::remove_dir_all(&dir_path).is_ok() {
            total_bytes = total_bytes.saturating_sub(dir_bytes);
        }
    }
}

fn next_stage_temp_path(parent: &Path) -> PathBuf {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    parent.join(format!(
        ".stage-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

fn write_cache_bytes(local_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = local_path
        .parent()
        .ok_or_else(|| io::Error::other("missing cache parent dir"))?;
    let tmp = next_stage_temp_path(parent);
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

/// 스테이징 실패를 사용자 안내 문구로 바꾼다. 사용자가 취소(`Interrupted`)했으면 `None`을 돌려준다.
#[must_use]
pub fn stage_error_message(error: &io::Error) -> Option<String> {
    use crate::i18n::catalog::file_format as ftext;
    if let Some(limit) = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<StageSizeLimitError>())
    {
        return Some(ftext::STAGE_TOO_LARGE.format(&[("limit", &crate::resources::mb(limit.0))]));
    }
    match error.kind() {
        io::ErrorKind::Interrupted => None,
        io::ErrorKind::StorageFull => Some(ftext::STAGE_NO_SPACE.get().to_owned()),
        _ => Some(ftext::STAGE_FAILED.format(&[("error", &error.to_string())])),
    }
}

/// 원격 파일을 로컬 캐시 경로로 내려받아 저장한다(이 PC에서 열기·Finder에서 보기용).
pub fn stage_to_local_cache(
    target: &HostTarget,
    host: &HostId,
    remote_path: &Path,
    max_bytes: u64,
) -> io::Result<(PathBuf, Stamp)> {
    stage_to_local_cache_with_progress(target, host, remote_path, max_bytes, None, |_, _| {})
}

/// 단일 제어 연결 위에서 원격 파일을 디스크에 바로 스트리밍하고 진행률과 취소를 처리한다.
pub fn stage_to_local_cache_with_progress(
    target: &HostTarget,
    host: &HostId,
    remote_path: &Path,
    max_bytes: u64,
    cancel: Option<&AtomicBool>,
    on_progress: impl FnMut(u64, u64),
) -> io::Result<(PathBuf, Stamp)> {
    stage_to_local_cache_in(
        &default_cache_root(),
        MAX_CACHE_TOTAL_BYTES,
        target,
        host,
        remote_path,
        max_bytes,
        cancel,
        on_progress,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "internal helper injects cache_root and budget for isolated unit tests"
)]
fn stage_to_local_cache_in(
    cache_root: &Path,
    max_cache_total_bytes: u64,
    target: &HostTarget,
    host: &HostId,
    remote_path: &Path,
    max_bytes: u64,
    cancel: Option<&AtomicBool>,
    mut on_progress: impl FnMut(u64, u64),
) -> io::Result<(PathBuf, Stamp)> {
    let check_cancel = || -> io::Result<()> {
        if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "remote file download cancelled",
            ))
        } else {
            Ok(())
        }
    };
    check_cancel()?;

    let mut session = RemoteSession::new();
    let wire_path = path_to_wire(remote_path);
    let initial_stat = match session.request(
        target,
        RemoteFsRequest::Stat {
            path: wire_path.clone(),
        },
    )? {
        RemoteFsReply::Stat(Ok(s)) => s,
        RemoteFsReply::Stat(Err(e)) => return Err(io::Error::other(e)),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected stat reply",
            ));
        }
    };
    if !initial_stat.exists || initial_stat.is_dir || !initial_stat.is_file {
        return Err(io::Error::other("remote path is not a regular file"));
    }
    let expected_stamp = initial_stat.stamp;
    let file_len = expected_stamp.map_or(0, |s| s.len);
    if file_len > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            StageSizeLimitError(max_bytes),
        ));
    }

    let local_path = cache_file_path_in(cache_root, host, remote_path)?;
    let parent = local_path
        .parent()
        .ok_or_else(|| io::Error::other("missing cache parent dir"))?;

    if let Some(ws) = expected_stamp
        && ws.modified_unix_ns.is_some()
        && read_cached_stamp(&local_path) == Some(ws)
        && fs::symlink_metadata(&local_path)
            .is_ok_and(|m| m.file_type().is_file() && m.len() == ws.len)
    {
        write_cached_stamp(&local_path, ws);
        on_progress(ws.len, ws.len);
        return Ok((local_path, Stamp::from_wire(ws)));
    }

    evict_remote_cache_if_needed(cache_root, parent, file_len, max_cache_total_bytes);
    if file_len > 0
        && let Ok(avail) = crate::platform::available_disk_bytes(parent)
        && avail < file_len.saturating_add(MIN_FREE_DISK_BYTES)
    {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "insufficient local disk space",
        ));
    }

    let chunk_cap = u32::try_from(MAX_REMOTE_CHUNK).unwrap_or(MAX_REMOTE_CHUNK_U32);
    let tmp = next_stage_temp_path(parent);
    let mut latest_stamp = expected_stamp;

    let streamed = (|| -> io::Result<()> {
        let f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        let mut writer = io::BufWriter::with_capacity(MAX_REMOTE_CHUNK, f);
        let mut offset: u64 = 0;
        on_progress(0, file_len);

        if file_len == 0 {
            writer.flush()?;
            return Ok(());
        }

        loop {
            check_cancel()?;
            let reply = session.request(
                target,
                RemoteFsRequest::ReadFile {
                    path: wire_path.clone(),
                    offset,
                    max_bytes: chunk_cap,
                },
            )?;
            let chunk = match reply {
                RemoteFsReply::FileChunk(Ok(c)) => c,
                RemoteFsReply::FileChunk(Err(e)) => return Err(io::Error::other(e)),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unexpected read chunk reply",
                    ));
                }
            };
            if !chunk.stat.exists || chunk.stat.is_dir || !chunk.stat.is_file {
                return Err(io::Error::other("remote path is no longer a regular file"));
            }
            if expected_stamp.is_some() && chunk.stat.stamp != expected_stamp {
                return Err(io::Error::other("remote file changed while downloading"));
            }
            latest_stamp = chunk.stat.stamp.or(latest_stamp);
            if chunk.bytes.is_empty() {
                if chunk.eof && offset >= file_len {
                    break;
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "remote file ended before its reported end",
                ));
            }
            let next_offset = offset.saturating_add(byte_len(&chunk.bytes));
            if next_offset > max_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    StageSizeLimitError(max_bytes),
                ));
            }
            writer.write_all(&chunk.bytes)?;
            offset = next_offset;
            let total = latest_stamp.map_or(file_len, |s| s.len).max(offset);
            on_progress(offset, total);
            if chunk.eof {
                if offset < file_len {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "remote file ended before its reported end",
                    ));
                }
                break;
            }
        }
        writer.flush()?;
        Ok(())
    })();

    if let Err(e) = streamed {
        fs::remove_file(&tmp).ok();
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, &local_path) {
        fs::remove_file(&tmp).ok();
        return Err(e);
    }
    if let Some(ws) = latest_stamp {
        write_cached_stamp(&local_path, ws);
    }
    Ok((local_path, stamp_from_opt_wire(latest_stamp)))
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
            if let Some(parent) = local_path.parent() {
                evict_remote_cache_if_needed(
                    &default_cache_root(),
                    parent,
                    byte_len(&bytes),
                    MAX_CACHE_TOTAL_BYTES,
                );
            }
            write_cache_bytes(&local_path, &bytes)?;
            if let Some(ws) = wire_stamp {
                write_cached_stamp(&local_path, ws);
            }
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

/// 바이트를 원격 파일로 원자적으로 올린다(클립보드 이미지 등 원격 첨부 업로드).
pub fn save_atomic_bytes(target: &HostTarget, path: &Path, data: &[u8]) -> io::Result<()> {
    static STAGE_SEQ: AtomicU64 = AtomicU64::new(1);
    let chunk_limit = MAX_REMOTE_CHUNK;

    if data.len() <= chunk_limit {
        let reply = request(
            target,
            RemoteFsRequest::WriteAtomic {
                path: path_to_wire(path),
                bytes: data.to_vec(),
                expected: None,
                stage_id: 0,
                offset: 0,
                commit: true,
            },
        )?;
        return match reply {
            RemoteFsReply::Written(WireWriteOutcome::Saved(_)) => Ok(()),
            RemoteFsReply::Written(WireWriteOutcome::Failed(e)) => Err(io::Error::other(e)),
            RemoteFsReply::Written(_) => {
                Err(io::Error::other("unexpected single-chunk write reply"))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected write reply",
            )),
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
                expected: None,
                stage_id,
                offset: u64::try_from(offset).unwrap_or(u64::MAX),
                commit,
            },
        )?;
        match reply {
            RemoteFsReply::Written(WireWriteOutcome::StageAck) if !commit => {
                offset = end;
            }
            RemoteFsReply::Written(WireWriteOutcome::Saved(_)) if commit => return Ok(()),
            RemoteFsReply::Written(WireWriteOutcome::Failed(e)) => {
                return Err(io::Error::other(e));
            }
            RemoteFsReply::Written(_) => {
                return Err(io::Error::other("unexpected multi-chunk write state"));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected write reply",
                ));
            }
        }
    }
    Err(io::Error::other("unreachable multi-chunk write end"))
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
    fn remote_open_rejects_a_truncated_read() {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let sock_dir = std::env::temp_dir().join(format!(
            "krill-rfs-trunc-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&sock_dir).unwrap();
        let sock_path = sock_dir.join("d.sock");
        let ipc_listener = UnixListener::bind(&sock_path).unwrap();
        let stat = WireStat {
            exists: true,
            is_file: true,
            is_dir: false,
            is_symlink: false,
            stamp: Some(WireStamp {
                modified_unix_ns: Some(1),
                len: 100,
            }),
        };
        let daemon_thread = thread::spawn(move || {
            for step in 0..2 {
                let (mut stream, _) = ipc_listener.accept().unwrap();
                let mut writer = stream.try_clone().unwrap();
                accept_test_handshake(&mut stream, &mut writer);
                ipc::read_frame(&mut stream).unwrap().unwrap();
                let bytes = if step == 0 {
                    b"hello\n".to_vec()
                } else {
                    Vec::new()
                };
                let reply = Reply::RemoteFs(RemoteFsReply::FileChunk(Ok(WireReadChunk {
                    stat,
                    bytes,
                    eof: false,
                })));
                stream.write_all(&reply.encode().unwrap()).unwrap();
            }
        });
        let host = HostId::Remote("box".into());
        let target = HostTarget::remote("box", sock_path);
        let got = open(&target, &host, Path::new("/remote/note.txt"));
        daemon_thread.join().unwrap();
        fs::remove_dir_all(&sock_dir).ok();
        assert!(
            got.is_err(),
            "a short read must not open as a complete file"
        );
    }

    #[test]
    fn remote_open_over_ipc() {
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
        let daemon_thread = thread::spawn(move || {
            let (mut stream, _) = ipc_listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            accept_test_handshake(&mut stream, &mut writer);
            let (kind, payload) = ipc::read_frame(&mut stream).unwrap().unwrap();
            let req = Request::decode(kind, &payload).unwrap();
            assert!(matches!(
                req,
                Request::RemoteFs(RemoteFsRequest::ReadFile { .. })
            ));
            let reply = Reply::RemoteFs(RemoteFsReply::FileChunk(Ok(WireReadChunk {
                stat: WireStat {
                    exists: true,
                    is_file: true,
                    is_dir: false,
                    is_symlink: false,
                    stamp: Some(stamp1),
                },
                bytes: b"hello\n".to_vec(),
                eof: true,
            })));
            stream.write_all(&reply.encode().unwrap()).unwrap();
        });

        let host = HostId::Remote("box".into());
        let target = HostTarget::remote("box", sock_path);
        let (opened, loaded_stamp, staged) =
            open(&target, &host, Path::new("/remote/repo/note.txt")).unwrap();
        assert!(staged.is_none());
        assert_eq!(loaded_stamp, Stamp::from_wire(stamp1));
        assert!(matches!(opened, Opened::Text(_, ref s) if s == "hello\n"));

        daemon_thread.join().unwrap();
        fs::remove_dir_all(&sock_dir).ok();
    }

    #[test]
    fn sanitize_cache_file_name_preserves_extensions_and_strips_forbidden_chars() {
        assert_eq!(
            sanitize_cache_file_name(Path::new(r"C:\Users\macmini\output\demo.mp4")),
            "demo.mp4"
        );
        assert_eq!(
            sanitize_cache_file_name(Path::new("/tmp/recording_12:30:00\"a?.mp4")),
            "recording_12_30_00_a_.mp4"
        );
        assert_eq!(
            sanitize_cache_file_name(Path::new("/tmp/CON.mp4")),
            "_CON.mp4"
        );
        assert_eq!(
            sanitize_cache_file_name(Path::new("/tmp/CON.tar.gz")),
            "_CON.tar.gz"
        );
        let clipped_dos = format!("/tmp/CON{}.mp4", " ".repeat(FILE_NAME_MAX_BYTES));
        assert_eq!(
            sanitize_cache_file_name(Path::new(&clipped_dos)),
            "_CON.mp4"
        );
        assert_eq!(
            sanitize_cache_file_name(Path::new(r"C:\repo\installer.pkg")),
            "installer.pkg"
        );
        assert_eq!(
            sanitize_cache_file_name(Path::new("/tmp/.mp4")),
            "remote-file.mp4"
        );
        let long = format!("/tmp/{}.mp4", "가".repeat(80));
        let sanitized = sanitize_cache_file_name(Path::new(&long));
        assert!(sanitized.len() <= FILE_NAME_MAX_BYTES);
        assert!(
            Path::new(&sanitized)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
        );
    }

    #[test]
    fn stage_streams_over_single_connection_reuses_stamp_cache_and_cancels_cleanly() {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let base_dir = std::env::temp_dir().join(format!(
            "krill-rfs-stage-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let cache_root = base_dir.join("cache");
        fs::create_dir_all(&cache_root).unwrap();
        let sock_path = base_dir.join("d.sock");
        let ipc_listener = UnixListener::bind(&sock_path).unwrap();

        let payload_len = MAX_REMOTE_CHUNK * 2 + 12345;
        let payload: Vec<u8> = (0..payload_len)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let expected_payload = payload.clone();
        let accepts = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let generation = Arc::new(AtomicU64::new(10));
        let stop = Arc::new(AtomicBool::new(false));

        let accepts_bg = accepts.clone();
        let reads_bg = reads.clone();
        let gen_bg = generation.clone();
        let stop_bg = stop.clone();
        let daemon_thread = thread::spawn(move || {
            while !stop_bg.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = ipc_listener.accept() else {
                    break;
                };
                if stop_bg.load(Ordering::Acquire) {
                    break;
                }
                accepts_bg.fetch_add(1, Ordering::AcqRel);
                let mut writer = stream.try_clone().unwrap();
                accept_test_handshake(&mut stream, &mut writer);
                while let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) {
                    let req = Request::decode(kind, &body).unwrap();
                    let cur_gen = gen_bg.load(Ordering::Acquire);
                    let wire_stat = WireStat {
                        exists: true,
                        is_file: true,
                        is_dir: false,
                        is_symlink: false,
                        stamp: Some(WireStamp {
                            modified_unix_ns: Some(cur_gen),
                            len: u64::try_from(payload.len()).unwrap(),
                        }),
                    };
                    let reply = match req {
                        Request::RemoteFs(RemoteFsRequest::Stat { .. }) => {
                            Reply::RemoteFs(RemoteFsReply::Stat(Ok(wire_stat)))
                        }
                        Request::RemoteFs(RemoteFsRequest::ReadFile {
                            offset, max_bytes, ..
                        }) => {
                            reads_bg.fetch_add(1, Ordering::AcqRel);
                            let start = usize::try_from(offset).unwrap().min(payload.len());
                            let cap = usize::try_from(max_bytes).unwrap().min(MAX_REMOTE_CHUNK);
                            let end = (start + cap).min(payload.len());
                            let eof = end >= payload.len();
                            Reply::RemoteFs(RemoteFsReply::FileChunk(Ok(WireReadChunk {
                                stat: wire_stat,
                                bytes: payload[start..end].to_vec(),
                                eof,
                            })))
                        }
                        _ => panic!("unexpected request"),
                    };
                    if writer.write_all(&reply.encode().unwrap()).is_err() {
                        break;
                    }
                }
            }
        });

        let host = HostId::Remote("macmini".into());
        let target = HostTarget::remote("macmini", sock_path.clone());
        let remote_path = Path::new("/Users/macmini/video-test/output/clip.mp4");

        // 1. 첫 다운로드: 단일 연결 1회로 Stat + 3개 청크를 모두 스트리밍한다.
        let mut progress_steps = Vec::new();
        let (local, stamp) = stage_to_local_cache_in(
            &cache_root,
            MAX_CACHE_TOTAL_BYTES,
            &target,
            &host,
            remote_path,
            MAX_STAGE_BYTES,
            None,
            |done, total| progress_steps.push((done, total)),
        )
        .unwrap();
        assert_eq!(accepts.load(Ordering::Acquire), 1);
        assert_eq!(reads.load(Ordering::Acquire), 3);
        assert_eq!(fs::read(&local).unwrap(), expected_payload);
        assert_eq!(
            stamp,
            Stamp::from_wire(WireStamp {
                modified_unix_ns: Some(10),
                len: u64::try_from(payload_len).unwrap(),
            })
        );
        assert_eq!(progress_steps.first().copied().map(|p| p.0), Some(0));
        assert_eq!(
            progress_steps.last().copied(),
            Some((
                u64::try_from(payload_len).unwrap(),
                u64::try_from(payload_len).unwrap()
            ))
        );

        // 2. 같은 Stamp 재요청: ReadFile을 한 번도 호출하지 않고 즉시 캐시를 돌려준다.
        let (cached_path, _) = stage_to_local_cache_in(
            &cache_root,
            MAX_CACHE_TOTAL_BYTES,
            &target,
            &host,
            remote_path,
            MAX_STAGE_BYTES,
            None,
            |_, _| {},
        )
        .unwrap();
        assert_eq!(cached_path, local);
        assert_eq!(reads.load(Ordering::Acquire), 3);

        // 3. 원격 Stamp가 바뀌고 진행 중 취소하면 Interrupted를 반환하며 .stage-* 임시 파일을 남기지 않는다.
        generation.store(20, Ordering::Release);
        let cancel = AtomicBool::new(false);
        let err = stage_to_local_cache_in(
            &cache_root,
            MAX_CACHE_TOTAL_BYTES,
            &target,
            &host,
            remote_path,
            MAX_STAGE_BYTES,
            Some(&cancel),
            |done, _| {
                if done > 0 {
                    cancel.store(true, Ordering::Release);
                }
            },
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert!(stage_error_message(&err).is_none());
        let leftovers: Vec<_> = fs::read_dir(local.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".stage-"))
            .collect();
        assert!(leftovers.is_empty());

        stop.store(true, Ordering::Release);
        krill_proto::os::net::UnixStream::connect(&sock_path).ok();
        daemon_thread.join().unwrap();
        fs::remove_dir_all(&base_dir).ok();
    }

    #[test]
    fn cache_eviction_error_mapping_and_symlink_guards_behave_honestly() {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let base_dir = std::env::temp_dir().join(format!(
            "krill-rfs-evict-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let cache_root = base_dir.join("cache");
        fs::create_dir_all(&cache_root).unwrap();

        let host = HostId::Remote("macmini".into());
        let old_file = cache_file_path_in(&cache_root, &host, Path::new("/repo/old.mp4")).unwrap();
        let old_dir = old_file.parent().unwrap().to_path_buf();
        fs::write(&old_file, vec![b'a'; 600]).unwrap();
        thread::sleep(Duration::from_millis(15));

        let active_file =
            cache_file_path_in(&cache_root, &host, Path::new("/repo/active.mp4")).unwrap();
        let active_dir = active_file.parent().unwrap().to_path_buf();
        fs::write(active_dir.join(".stage-999-0"), vec![b's'; 200]).unwrap();

        let keep_file =
            cache_file_path_in(&cache_root, &host, Path::new("/repo/keep.mp4")).unwrap();
        let keep_dir = keep_file.parent().unwrap().to_path_buf();
        fs::write(&keep_file, vec![b'k'; 400]).unwrap();

        // keep_dir(400 B) + old_dir(600 B) + incoming(500 B) > 1200 B -> old_dir만 제거되고 active_dir/keep_dir은 유지된다.
        evict_remote_cache_if_needed(&cache_root, &keep_dir, 500, 1200);
        assert!(!old_dir.exists());
        assert!(active_dir.exists());
        assert!(keep_dir.exists());

        // 크기 한도·디스크 부족 에러 문구가 정확히 매핑된다.
        let too_large = io::Error::new(io::ErrorKind::InvalidInput, StageSizeLimitError(10 << 20));
        assert!(stage_error_message(&too_large).is_some_and(|m| m.contains("10.0 MB")));
        let disk_full = io::Error::new(io::ErrorKind::StorageFull, "disk full");
        assert_eq!(
            stage_error_message(&disk_full).as_deref(),
            Some(crate::i18n::catalog::file_format::STAGE_NO_SPACE.get())
        );

        #[cfg(unix)]
        {
            let sym_root = base_dir.join("sym-cache");
            std::os::unix::fs::symlink(&cache_root, &sym_root).unwrap();
            assert!(cache_file_path_in(&sym_root, &host, Path::new("/repo/x.mp4")).is_err());
        }

        fs::remove_dir_all(&base_dir).ok();
    }
}
