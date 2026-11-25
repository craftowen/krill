//! 원격 호스트 파일 시스템(탐색·열기·원자적 저장·Quick Open) 및 루프백 포트 포워딩 서버 핸들러.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash as _, Hasher as _};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::child::{
    AsFd, PipeReader, kill_and_reap, own_group, spawn, terminate_gracefully, wait_readable,
};
use krill_proto::os::ffi::{OsStrExt as _, OsStringExt as _};
use krill_proto::os::fs::OpenOptionsExt as _;
use krill_proto::os::net::UnixStream;
use krill_proto::remote_fs::{
    MAX_DIR_ENTRIES, MAX_PROBE_PATHS, MAX_QUICK_OPEN_FILES, MAX_REMOTE_CHUNK, MAX_REMOTE_PATH,
    MAX_REMOTE_TEXT, RemoteFsReply, RemoteFsRequest, WireDirEntry, WireDirListing, WireExplorerOp,
    WireReadChunk, WireStamp, WireStat, WireWriteOutcome,
};

/// 프레임 크기 상한(`1MiB`)을 절대 넘지 않도록 디렉터리·목록 응답의 누적 경로 바이트를 제한한다.
const MAX_LISTING_WIRE_BYTES: usize = 384 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(3);
const LOOPBACK_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// 동시에 열어 둘 수 있는 다중 청크 스테이징 상한.
const MAX_ACTIVE_STAGES: usize = 64;
/// 완료·중단된 `(path, stage_id)` 핸들 재사용을 막기 위해 기억하는 상한.
const MAX_RETIRED_STAGES: usize = 4096;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);
static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

type StageKey = (PathBuf, u64);

struct ActiveStage {
    conn_id: u64,
    stage_path: PathBuf,
    written: u64,
}

#[derive(Default)]
struct StageRegistry {
    active: HashMap<StageKey, ActiveStage>,
    retired: HashSet<StageKey>,
    retired_order: VecDeque<StageKey>,
}

impl StageRegistry {
    fn retire(&mut self, key: StageKey) {
        if self.retired.insert(key.clone()) {
            self.retired_order.push_back(key);
            while self.retired_order.len() > MAX_RETIRED_STAGES {
                if let Some(old) = self.retired_order.pop_front() {
                    self.retired.remove(&old);
                }
            }
        }
    }

    fn is_retired(&self, key: &StageKey) -> bool {
        self.retired.contains(key)
    }

    fn owner_matches(owner: u64, conn_id: u64) -> bool {
        conn_id == 0 || owner == 0 || owner == conn_id
    }
}

static STAGES: LazyLock<Mutex<StageRegistry>> =
    LazyLock::new(|| Mutex::new(StageRegistry::default()));

/// 제어 연결이 시작될 때 고유 연결 ID를 발급한다. 연결 종료 시 [`cleanup_connection`]으로
/// 미완료 스테이징 임시 파일을 모두 정리한다.
#[must_use]
pub fn next_connection_id() -> u64 {
    NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed)
}

/// 연결이 끊기거나 끝났을 때 해당 연결이 열어 둔 미완료 다중 청크 스테이징 파일을 모두 지우고 핸들을 폐기한다.
pub fn cleanup_connection(conn_id: u64) {
    if conn_id == 0 {
        return;
    }
    let mut reg = STAGES.lock().unwrap_or_else(PoisonError::into_inner);
    let keys: Vec<StageKey> = reg
        .active
        .iter()
        .filter(|(_, stage)| stage.conn_id == conn_id)
        .map(|(key, _)| key.clone())
        .collect();
    let mut paths = Vec::with_capacity(keys.len());
    for key in keys {
        if let Some(stage) = reg.active.remove(&key) {
            paths.push(stage.stage_path);
        }
        reg.retire(key);
    }
    drop(reg);
    for path in paths {
        fs::remove_file(path).ok();
    }
}

pub fn serve(req: RemoteFsRequest) -> RemoteFsReply {
    serve_on_conn(0, req)
}

pub fn serve_on_conn(conn_id: u64, req: RemoteFsRequest) -> RemoteFsReply {
    match req {
        RemoteFsRequest::ProbePaths { cwd, paths } => {
            RemoteFsReply::ProbedPaths(probe_paths(cwd.as_deref(), &paths))
        }
        RemoteFsRequest::Stat { path } => {
            let p = PathBuf::from(OsStr::from_bytes(&path));
            RemoteFsReply::Stat(stat_path(&p).map_err(|e| truncate_text(e.to_string())))
        }
        RemoteFsRequest::ReadFile {
            path,
            offset,
            max_bytes,
        } => {
            let p = PathBuf::from(OsStr::from_bytes(&path));
            RemoteFsReply::FileChunk(
                read_file_chunk(&p, offset, max_bytes).map_err(|e| truncate_text(e.to_string())),
            )
        }
        RemoteFsRequest::WriteAtomic {
            path,
            bytes,
            expected,
            stage_id,
            offset,
            commit,
        } => {
            let p = PathBuf::from(OsStr::from_bytes(&path));
            RemoteFsReply::Written(write_atomic_on_conn(
                conn_id, &p, &bytes, expected, stage_id, offset, commit,
            ))
        }
        RemoteFsRequest::ReadDir { repo, dir } => {
            let repo_path = PathBuf::from(OsStr::from_bytes(&repo));
            let dir_path = PathBuf::from(OsStr::from_bytes(&dir));
            RemoteFsReply::DirEntries(
                read_dir_listing(&repo_path, &dir_path).map_err(|e| truncate_text(e.to_string())),
            )
        }
        RemoteFsRequest::ScanWorktree { repo, limit } => {
            let repo_path = PathBuf::from(OsStr::from_bytes(&repo));
            RemoteFsReply::DirEntries(
                scan_worktree_listing(&repo_path, limit).map_err(|e| truncate_text(e.to_string())),
            )
        }
        RemoteFsRequest::ExplorerOp { repo, op } => {
            let repo_path = PathBuf::from(OsStr::from_bytes(&repo));
            RemoteFsReply::ExplorerOpResult(
                execute_explorer_op(&repo_path, op)
                    .map(|p| p.into_os_string().into_vec())
                    .map_err(|e| truncate_text(e.to_string())),
            )
        }
        RemoteFsRequest::QuickOpenList { root } => {
            let root_path = PathBuf::from(OsStr::from_bytes(&root));
            RemoteFsReply::QuickOpenFiles(
                list_quick_open(&root_path).map_err(|e| truncate_text(e.to_string())),
            )
        }
        RemoteFsRequest::PortForward { .. } => {
            RemoteFsReply::PortForwarded(Err("port forward must be handled on stream".to_owned()))
        }
        RemoteFsRequest::AbortStage { path, stage_id } => {
            let p = PathBuf::from(OsStr::from_bytes(&path));
            RemoteFsReply::StageAborted(abort_stage_on_conn(conn_id, &p, stage_id))
        }
    }
}

pub fn connect_loopback_port(port: u16) -> Result<TcpStream, String> {
    if port == 0 {
        return Err("invalid port 0".to_owned());
    }
    let v4 = SocketAddr::from(([127, 0, 0, 1], port));
    match TcpStream::connect_timeout(&v4, LOOPBACK_CONNECT_TIMEOUT) {
        Ok(stream) => {
            stream.set_nodelay(true).ok();
            Ok(stream)
        }
        Err(err_v4) => {
            let v6 = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port));
            match TcpStream::connect_timeout(&v6, LOOPBACK_CONNECT_TIMEOUT) {
                Ok(stream) => {
                    stream.set_nodelay(true).ok();
                    Ok(stream)
                }
                Err(_) => Err(truncate_text(err_v4.to_string())),
            }
        }
    }
}

pub fn bridge_streams(mut uds: UnixStream, mut tcp: TcpStream) {
    uds.set_nonblocking(false).ok();
    uds.set_read_timeout(None).ok();
    uds.set_write_timeout(None).ok();
    tcp.set_nonblocking(false).ok();
    tcp.set_read_timeout(None).ok();
    tcp.set_write_timeout(None).ok();
    tcp.set_nodelay(true).ok();

    let (Ok(mut uds_reader), Ok(mut tcp_writer)) = (uds.try_clone(), tcp.try_clone()) else {
        return;
    };
    std::thread::scope(|s| {
        s.spawn(move || {
            io::copy(&mut uds_reader, &mut tcp_writer).ok();
            tcp_writer.shutdown(Shutdown::Write).ok();
        });
        io::copy(&mut tcp, &mut uds).ok();
        uds.shutdown(Shutdown::Write).ok();
    });
}

fn truncate_text(mut s: String) -> String {
    if s.len() <= MAX_REMOTE_TEXT {
        return s;
    }
    while s.len() > MAX_REMOTE_TEXT {
        s.pop();
    }
    s
}

fn clean_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir | Component::Prefix(_))
                ) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn probe_paths(cwd: Option<&[u8]>, paths: &[Vec<u8>]) -> Vec<Option<Vec<u8>>> {
    let cwd_path = cwd
        .filter(|b| !b.is_empty())
        .map(|b| PathBuf::from(OsStr::from_bytes(b)))
        .filter(|p| p.is_absolute());
    let home = crate::usage::accounts::default_home();
    paths
        .iter()
        .take(MAX_PROBE_PATHS)
        .map(|raw| resolve_existing_file(raw, cwd_path.as_deref(), &home))
        .collect()
}

fn resolve_existing_file(raw: &[u8], cwd: Option<&Path>, home: &Path) -> Option<Vec<u8>> {
    if raw.is_empty() || raw.len() > MAX_REMOTE_PATH {
        return None;
    }
    let candidate = if raw == b"~" {
        clean_path(home)
    } else if let Some(rest) = raw.strip_prefix(b"~/").or_else(|| raw.strip_prefix(b"~\\")) {
        clean_path(&home.join(OsStr::from_bytes(rest)))
    } else {
        let p = Path::new(OsStr::from_bytes(raw));
        if p.is_absolute() {
            clean_path(p)
        } else {
            let base = cwd?;
            let direct = clean_path(&base.join(p));
            if direct.is_file() {
                let bytes = direct.into_os_string().into_vec();
                return (bytes.len() <= MAX_REMOTE_PATH).then_some(bytes);
            }
            let stripped = raw
                .strip_prefix(b"a/")
                .or_else(|| raw.strip_prefix(b"b/"))?;
            clean_path(&base.join(OsStr::from_bytes(stripped)))
        }
    };
    if !candidate.is_file() {
        return None;
    }
    let bytes = candidate.into_os_string().into_vec();
    (bytes.len() <= MAX_REMOTE_PATH).then_some(bytes)
}

fn stamp_from_meta(meta: &fs::Metadata) -> WireStamp {
    let modified_unix_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    WireStamp {
        modified_unix_ns,
        len: meta.len(),
    }
}

fn stat_path(path: &Path) -> io::Result<WireStat> {
    let sym = fs::symlink_metadata(path);
    let is_symlink = sym.as_ref().is_ok_and(|m| m.file_type().is_symlink());
    match fs::metadata(path) {
        Ok(meta) => Ok(WireStat {
            exists: true,
            is_file: meta.is_file(),
            is_dir: meta.is_dir(),
            is_symlink,
            stamp: Some(stamp_from_meta(&meta)),
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(WireStat {
            exists: false,
            is_file: false,
            is_dir: false,
            is_symlink,
            stamp: None,
        }),
        Err(err) => Err(err),
    }
}

fn read_file_chunk(path: &Path, offset: u64, max_bytes: u32) -> io::Result<WireReadChunk> {
    let stat = stat_path(path)?;
    if !stat.exists || stat.is_dir || !stat.is_file {
        return Ok(WireReadChunk {
            stat,
            bytes: Vec::new(),
            eof: true,
        });
    }
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    let fresh_stat = WireStat {
        exists: true,
        is_file: meta.is_file(),
        is_dir: meta.is_dir(),
        is_symlink: stat.is_symlink,
        stamp: Some(stamp_from_meta(&meta)),
    };
    let cap = usize::try_from(max_bytes)
        .unwrap_or(MAX_REMOTE_CHUNK)
        .min(MAX_REMOTE_CHUNK);
    if offset >= meta.len() || cap == 0 {
        return Ok(WireReadChunk {
            stat: fresh_stat,
            bytes: Vec::new(),
            eof: true,
        });
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::with_capacity(cap.min(64 * 1024));
    let cap_u64 = u64::try_from(cap).unwrap_or(u64::MAX);
    file.take(cap_u64).read_to_end(&mut bytes)?;
    let next_offset = offset.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    let eof = bytes.len() < cap || next_offset >= meta.len();
    Ok(WireReadChunk {
        stat: fresh_stat,
        bytes,
        eof,
    })
}

fn current_stamp(path: &Path) -> Option<WireStamp> {
    fs::metadata(path).ok().map(|m| stamp_from_meta(&m))
}

fn staging_path(path: &Path, stage_id: u64) -> PathBuf {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hash);
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            ".krill-stage-{}-{}-{stage_id}.tmp",
            std::process::id(),
            hash.finish()
        ))
}

#[cfg(test)]
fn write_atomic(
    path: &Path,
    bytes: &[u8],
    expected: Option<WireStamp>,
    stage_id: u64,
    offset: u64,
    commit: bool,
) -> WireWriteOutcome {
    write_atomic_on_conn(0, path, bytes, expected, stage_id, offset, commit)
}

fn abort_stage_on_conn(conn_id: u64, path: &Path, stage_id: u64) -> bool {
    if stage_id == 0 {
        return false;
    }
    let key = (path.to_path_buf(), stage_id);
    let mut reg = STAGES.lock().unwrap_or_else(PoisonError::into_inner);
    let removed = match reg.active.get(&key) {
        Some(active) if StageRegistry::owner_matches(active.conn_id, conn_id) => {
            reg.active.remove(&key)
        }
        Some(_) => return false,
        None => None,
    };
    reg.retire(key);
    drop(reg);
    if let Some(stage) = removed {
        fs::remove_file(&stage.stage_path).ok();
        true
    } else {
        let fallback = staging_path(path, stage_id);
        fs::remove_file(&fallback).is_ok()
    }
}

fn write_atomic_on_conn(
    conn_id: u64,
    path: &Path,
    bytes: &[u8],
    expected: Option<WireStamp>,
    stage_id: u64,
    offset: u64,
    commit: bool,
) -> WireWriteOutcome {
    let Some(parent) = path.parent() else {
        return WireWriteOutcome::Failed("missing parent directory".to_owned());
    };
    let pid = std::process::id();
    let current = current_stamp(path);
    if expected.is_some() && current != expected {
        return WireWriteOutcome::Conflict { current };
    }

    if stage_id == 0 {
        if !commit || offset != 0 {
            return WireWriteOutcome::Failed("invalid single-chunk atomic write".to_owned());
        }
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = parent.join(format!(".krill-save-{pid}-{seq}.tmp"));
        let res = (|| -> io::Result<WireStamp> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp_path)?;
            if let Ok(existing) = fs::metadata(path) {
                fs::set_permissions(&tmp_path, existing.permissions()).ok();
            }
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp_path, path)?;
            let meta = fs::metadata(path)?;
            Ok(stamp_from_meta(&meta))
        })();
        match res {
            Ok(stamp) => WireWriteOutcome::Saved(stamp),
            Err(err) => {
                fs::remove_file(&tmp_path).ok();
                WireWriteOutcome::Failed(truncate_text(err.to_string()))
            }
        }
    } else {
        let key = (path.to_path_buf(), stage_id);
        let stage_path = staging_path(path, stage_id);
        if offset == 0 {
            let reg = STAGES.lock().unwrap_or_else(PoisonError::into_inner);
            if reg.is_retired(&key) {
                return WireWriteOutcome::Failed("staging handle already used".to_owned());
            }
            if reg.active.contains_key(&key) {
                return WireWriteOutcome::Failed("staging handle already active".to_owned());
            }
            if reg.active.len() >= MAX_ACTIVE_STAGES {
                return WireWriteOutcome::Failed("too many active staged writes".to_owned());
            }
            drop(reg);
            let res = (|| -> io::Result<WireWriteOutcome> {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&stage_path)?;
                file.write_all(bytes)?;
                if !commit {
                    let mut reg = STAGES.lock().unwrap_or_else(PoisonError::into_inner);
                    if reg.is_retired(&key)
                        || reg.active.contains_key(&key)
                        || reg.active.len() >= MAX_ACTIVE_STAGES
                    {
                        drop(reg);
                        drop(file);
                        fs::remove_file(&stage_path).ok();
                        return Ok(WireWriteOutcome::Failed(
                            "staging handle was aborted or exhausted".to_owned(),
                        ));
                    }
                    reg.active.insert(
                        key.clone(),
                        ActiveStage {
                            conn_id,
                            stage_path: stage_path.clone(),
                            written: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                        },
                    );
                    return Ok(WireWriteOutcome::StageAck);
                }
                if let Ok(existing) = fs::metadata(path) {
                    fs::set_permissions(&stage_path, existing.permissions()).ok();
                }
                file.sync_all()?;
                drop(file);
                let now_stamp = current_stamp(path);
                if expected.is_some() && now_stamp != expected {
                    fs::remove_file(&stage_path).ok();
                    STAGES
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .retire(key.clone());
                    return Ok(WireWriteOutcome::Conflict { current: now_stamp });
                }
                fs::rename(&stage_path, path)?;
                STAGES
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .retire(key.clone());
                let meta = fs::metadata(path)?;
                Ok(WireWriteOutcome::Saved(stamp_from_meta(&meta)))
            })();
            match res {
                Ok(outcome) => outcome,
                Err(err) => {
                    if err.kind() != io::ErrorKind::AlreadyExists {
                        fs::remove_file(&stage_path).ok();
                        STAGES
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .retire(key);
                    }
                    WireWriteOutcome::Failed(truncate_text(err.to_string()))
                }
            }
        } else {
            let active_stage_path = {
                let reg = STAGES.lock().unwrap_or_else(PoisonError::into_inner);
                if reg.is_retired(&key) {
                    return WireWriteOutcome::Failed(
                        "staging handle was aborted or completed".to_owned(),
                    );
                }
                match reg.active.get(&key) {
                    Some(entry) if !StageRegistry::owner_matches(entry.conn_id, conn_id) => {
                        return WireWriteOutcome::Failed(
                            "staging handle belongs to another connection".to_owned(),
                        );
                    }
                    Some(entry) if entry.written != offset => {
                        return WireWriteOutcome::Failed("staging file offset mismatch".to_owned());
                    }
                    Some(entry) => entry.stage_path.clone(),
                    None => {
                        return WireWriteOutcome::Failed("unknown staging handle".to_owned());
                    }
                }
            };
            let res = (|| -> io::Result<WireWriteOutcome> {
                let mut file = OpenOptions::new().write(true).open(&active_stage_path)?;
                if file.metadata()?.len() != offset {
                    return Ok(WireWriteOutcome::Failed(
                        "staging file offset mismatch".to_owned(),
                    ));
                }
                file.seek(SeekFrom::Start(offset))?;
                file.write_all(bytes)?;
                let next_offset =
                    offset.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                if !commit {
                    let mut reg = STAGES.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(entry) = reg.active.get_mut(&key)
                        && entry.stage_path == active_stage_path
                        && StageRegistry::owner_matches(entry.conn_id, conn_id)
                    {
                        entry.written = next_offset;
                        return Ok(WireWriteOutcome::StageAck);
                    }
                    drop(reg);
                    drop(file);
                    fs::remove_file(&active_stage_path).ok();
                    return Ok(WireWriteOutcome::Failed(
                        "staging handle was aborted".to_owned(),
                    ));
                }
                if let Ok(existing) = fs::metadata(path) {
                    fs::set_permissions(&active_stage_path, existing.permissions()).ok();
                }
                file.sync_all()?;
                drop(file);
                {
                    let mut reg = STAGES.lock().unwrap_or_else(PoisonError::into_inner);
                    let valid = reg.active.get(&key).is_some_and(|e| {
                        e.stage_path == active_stage_path
                            && StageRegistry::owner_matches(e.conn_id, conn_id)
                    });
                    if !valid {
                        drop(reg);
                        fs::remove_file(&active_stage_path).ok();
                        return Ok(WireWriteOutcome::Failed(
                            "staging handle was aborted".to_owned(),
                        ));
                    }
                    reg.active.remove(&key);
                    reg.retire(key.clone());
                }
                let now_stamp = current_stamp(path);
                if expected.is_some() && now_stamp != expected {
                    fs::remove_file(&active_stage_path).ok();
                    return Ok(WireWriteOutcome::Conflict { current: now_stamp });
                }
                fs::rename(&active_stage_path, path)?;
                let meta = fs::metadata(path)?;
                Ok(WireWriteOutcome::Saved(stamp_from_meta(&meta)))
            })();
            match res {
                Ok(outcome) => outcome,
                Err(err) => {
                    let mut reg = STAGES.lock().unwrap_or_else(PoisonError::into_inner);
                    if reg
                        .active
                        .get(&key)
                        .is_some_and(|e| e.stage_path == active_stage_path)
                    {
                        reg.active.remove(&key);
                    }
                    reg.retire(key);
                    drop(reg);
                    fs::remove_file(&active_stage_path).ok();
                    WireWriteOutcome::Failed(truncate_text(err.to_string()))
                }
            }
        }
    }
}

fn sort_wire_entries(entries: &mut [WireDirEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            })
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn is_forbidden_git_or_win32_segment(part: &OsStr) -> bool {
    let lossy = part.to_string_lossy();
    let trimmed = lossy.trim_end_matches(['.', ' ']);
    trimmed.eq_ignore_ascii_case(".git")
        || (cfg!(windows) && (lossy.contains(':') || lossy.ends_with(['.', ' '])))
}

fn is_allowed_canonical_target(canon_repo: &Path, canon_target: &Path) -> bool {
    let Ok(rel) = canon_target.strip_prefix(canon_repo) else {
        return false;
    };
    rel.components().all(
        |comp| matches!(comp, Component::Normal(part) if !is_forbidden_git_or_win32_segment(part)),
    )
}

fn read_raw_dir_entries(repo: &Path, dir: &Path) -> io::Result<Vec<WireDirEntry>> {
    let denied = || {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "directory is outside repository",
        )
    };
    if !dir.starts_with(repo)
        || dir
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(denied());
    }
    let dir_rel = dir.strip_prefix(repo).map_err(|_| denied())?;
    for comp in dir_rel.components() {
        let Component::Normal(part) = comp else {
            return Err(denied());
        };
        if is_forbidden_git_or_win32_segment(part) {
            return Err(denied());
        }
    }
    let canon_repo = fs::canonicalize(repo)?;
    let canon_dir = fs::canonicalize(dir)?;
    if !is_allowed_canonical_target(&canon_repo, &canon_dir) {
        return Err(denied());
    }
    let rd = fs::read_dir(dir)?;
    let mut out = Vec::new();
    let mut used_bytes = 0usize;
    for entry in rd.flatten() {
        if out.len() >= MAX_DIR_ENTRIES {
            break;
        }
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy().into_owned();
        if is_forbidden_git_or_win32_segment(&file_name) || name.len() > MAX_REMOTE_TEXT {
            continue;
        }
        let path = entry.path();
        let path_bytes = path.as_os_str().as_bytes().to_vec();
        if path_bytes.len() > MAX_REMOTE_PATH {
            continue;
        }
        let (is_dir, is_symlink) = entry.file_type().map_or((false, false), |ft| {
            let is_symlink = ft.is_symlink();
            let is_dir = ft.is_dir()
                || (is_symlink
                    && fs::canonicalize(&path)
                        .is_ok_and(|t| is_allowed_canonical_target(&canon_repo, &t) && t.is_dir()));
            (is_dir, is_symlink)
        });
        let relative_path = path
            .strip_prefix(repo)
            .ok()
            .map_or_else(|| name.clone(), |p| p.to_string_lossy().replace('\\', "/"));
        if relative_path.len() > MAX_REMOTE_TEXT {
            continue;
        }
        let entry_cost = name
            .len()
            .saturating_add(path_bytes.len())
            .saturating_add(relative_path.len())
            .saturating_add(16);
        if used_bytes.saturating_add(entry_cost) > MAX_LISTING_WIRE_BYTES {
            break;
        }
        used_bytes = used_bytes.saturating_add(entry_cost);
        out.push(WireDirEntry {
            name,
            path: path_bytes,
            relative_path,
            is_dir,
            is_symlink,
        });
    }
    sort_wire_entries(&mut out);
    Ok(out)
}

fn read_dir_listing(repo: &Path, dir: &Path) -> io::Result<WireDirListing> {
    let entries = read_raw_dir_entries(repo, dir)?;
    let rels: Vec<String> = entries.iter().map(|e| e.relative_path.clone()).collect();
    let ignored = query_git_ignored(repo, &rels);
    Ok(WireDirListing { entries, ignored })
}

fn scan_worktree_listing(repo: &Path, limit: u32) -> io::Result<WireDirListing> {
    let cap = usize::try_from(limit)
        .unwrap_or(MAX_DIR_ENTRIES)
        .min(MAX_DIR_ENTRIES);
    let mut entries = Vec::new();
    let mut used_bytes = 0usize;
    let mut queue = VecDeque::new();
    let mut visited = BTreeSet::new();
    let canon_root = repo.canonicalize()?;
    if !canon_root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "workspace is not a directory",
        ));
    }
    visited.insert(canon_root);
    queue.push_back(repo.to_path_buf());
    while let Some(dir) = queue.pop_front() {
        if entries.len() >= cap {
            break;
        }
        let Ok(children) = read_raw_dir_entries(repo, &dir) else {
            continue;
        };
        for entry in children {
            if entries.len() >= cap {
                break;
            }
            let entry_cost = entry
                .name
                .len()
                .saturating_add(entry.path.len())
                .saturating_add(entry.relative_path.len())
                .saturating_add(16);
            if used_bytes.saturating_add(entry_cost) > MAX_LISTING_WIRE_BYTES {
                break;
            }
            let entry_path = PathBuf::from(OsStr::from_bytes(&entry.path));
            let recurse = entry.is_dir
                && !entry.is_symlink
                && entry_path.file_name() != Some(OsStr::new("node_modules"))
                && entry_path
                    .canonicalize()
                    .is_ok_and(|canon| visited.insert(canon));
            used_bytes = used_bytes.saturating_add(entry_cost);
            entries.push(entry);
            if recurse {
                queue.push_back(entry_path);
            }
        }
    }
    let rels: Vec<String> = entries.iter().map(|e| e.relative_path.clone()).collect();
    let ignored = query_git_ignored(repo, &rels);
    Ok(WireDirListing { entries, ignored })
}

const GIT_TERMINATE_GRACE: Duration = Duration::from_millis(200);
const MAX_LS_FILES_BYTES: usize = 1024 * 1024;
const MAX_IGNORED_STDOUT_BYTES: usize = 256 * 1024;

fn drain_pipe_capped(
    pipe: &mut impl io::Read,
    dst: &mut Vec<u8>,
    max_bytes: usize,
    buf: &mut [u8],
) -> io::Result<(bool, bool)> {
    loop {
        match pipe.read(buf) {
            Ok(0) => return Ok((true, false)),
            Ok(n) => {
                let remaining = max_bytes.saturating_sub(dst.len());
                let take = n.min(remaining);
                if let Some(chunk) = buf.get(..take) {
                    dst.extend_from_slice(chunk);
                }
                if dst.len() >= max_bytes {
                    return Ok((false, true));
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok((false, false));
            }
            Err(e) => return Err(e),
        }
    }
}

const CLEARED_GIT_ENV: [&str; 5] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
];

fn git_command(program: &Path, repo: &Path) -> std::process::Command {
    let mut cmd = krill_proto::os::background_command(program);
    own_group(&mut cmd)
        .current_dir(krill_proto::os::shell_path(repo))
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    for var in CLEARED_GIT_ENV {
        cmd.env_remove(var);
    }
    cmd
}

fn query_git_ignored(repo: &Path, relative_paths: &[String]) -> Vec<String> {
    query_git_ignored_with(Path::new("git"), repo, relative_paths, GIT_TIMEOUT)
}

fn query_git_ignored_with(
    program: &Path,
    repo: &Path,
    relative_paths: &[String],
    timeout: Duration,
) -> Vec<String> {
    if relative_paths.is_empty() || !repo.join(".git").exists() {
        return Vec::new();
    }
    let mut cmd = git_command(program, repo);
    cmd.args([
        "-c",
        "core.quotePath=false",
        "check-ignore",
        "--stdin",
        "-z",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let Ok(mut child) = spawn(&mut cmd) else {
        return Vec::new();
    };
    let Some(stdout_raw) = child.stdout.take() else {
        kill_and_reap(&mut child);
        return Vec::new();
    };
    let Ok(mut stdout) = PipeReader::new(stdout_raw) else {
        kill_and_reap(&mut child);
        return Vec::new();
    };
    let stdin_handle = child.stdin.take().and_then(|mut stdin| {
        let mut payload = Vec::new();
        for rel in relative_paths {
            payload.extend_from_slice(rel.as_bytes());
            payload.push(0);
        }
        std::thread::Builder::new()
            .name("remote-fs-git-stdin".into())
            .spawn(move || {
                stdin.write_all(&payload).ok();
            })
            .ok()
    });
    let deadline = Instant::now() + timeout;
    let mut stdout_bytes = Vec::new();
    let mut buf = [0u8; 8192];
    let mut eof = false;
    loop {
        if !eof {
            if let Ok((hit_eof, hit_cap)) = drain_pipe_capped(
                &mut stdout,
                &mut stdout_bytes,
                MAX_IGNORED_STDOUT_BYTES,
                &mut buf,
            ) {
                eof |= hit_eof;
                if hit_cap {
                    terminate_gracefully(&mut child, GIT_TERMINATE_GRACE);
                    break;
                }
            } else {
                terminate_gracefully(&mut child, GIT_TERMINATE_GRACE);
                break;
            }
        }
        match child.try_wait() {
            Ok(Some(_)) => {
                if !eof {
                    if Instant::now() >= deadline {
                        terminate_gracefully(&mut child, GIT_TERMINATE_GRACE);
                        break;
                    }
                    wait_readable(&[Some(&stdout as &dyn AsFd)], Duration::from_millis(5));
                    continue;
                }
                kill_and_reap(&mut child);
                break;
            }
            Ok(None) if Instant::now() < deadline => {
                wait_readable(
                    &[(!eof).then_some(&stdout as &dyn AsFd)],
                    Duration::from_millis(5),
                );
            }
            Ok(None) | Err(_) => {
                terminate_gracefully(&mut child, GIT_TERMINATE_GRACE);
                break;
            }
        }
    }
    drop(stdout);
    drop(child);
    if let Some(handle) = stdin_handle {
        handle.join().ok();
    }
    let mut ignored = Vec::new();
    let mut used = 0usize;
    for part in stdout_bytes.split(|b| *b == 0) {
        if part.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(part)
            .trim_end_matches('/')
            .to_owned();
        if s.is_empty() || s.len() > MAX_REMOTE_TEXT {
            continue;
        }
        used = used.saturating_add(s.len().saturating_add(4));
        if used > 128 * 1024 || ignored.len() >= MAX_DIR_ENTRIES {
            break;
        }
        ignored.push(s);
    }
    ignored
}

fn validate_relative_input(raw: &str) -> io::Result<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "name must not be empty",
        ));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "name must be relative",
        ));
    }
    let mut clean = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(part) if !is_forbidden_git_or_win32_segment(part) => clean.push(part),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid path segment",
                ));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "name must not be empty",
        ));
    }
    Ok(clean)
}

fn ensure_worktree_create_path(
    repo: &Path,
    parent_dir: &Path,
    rel: &Path,
) -> io::Result<(PathBuf, PathBuf)> {
    let denied = || {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "target directory is outside worktree",
        )
    };
    if !parent_dir.starts_with(repo)
        || parent_dir
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(denied());
    }
    let parent_rel = parent_dir.strip_prefix(repo).map_err(|_| denied())?;
    let canon_repo = repo.canonicalize().map_err(|_| denied())?;
    let full_path = parent_dir.join(rel);
    let mut cur = repo.to_path_buf();
    for comp in parent_rel.components().chain(rel.components()) {
        let Component::Normal(part) = comp else {
            return Err(denied());
        };
        if is_forbidden_git_or_win32_segment(part) {
            return Err(denied());
        }
        cur.push(part);
        match fs::symlink_metadata(&cur) {
            Ok(_) => {
                let canon_cur = cur.canonicalize().map_err(|_| denied())?;
                if !is_allowed_canonical_target(&canon_repo, &canon_cur) {
                    return Err(denied());
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok((canon_repo, full_path))
}

fn execute_explorer_op(repo: &Path, op: WireExplorerOp) -> io::Result<PathBuf> {
    match op {
        WireExplorerOp::CreateFile { parent, name } => {
            let parent_dir = PathBuf::from(OsStr::from_bytes(&parent));
            let rel = validate_relative_input(&name)?;
            let (canon_repo, full_path) = ensure_worktree_create_path(repo, &parent_dir, &rel)?;
            if full_path.exists() || fs::symlink_metadata(&full_path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "file already exists",
                ));
            }
            if let Some(p) = full_path.parent() {
                fs::create_dir_all(p)?;
                let canon_parent = p.canonicalize().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "target directory is outside worktree",
                    )
                })?;
                if !is_allowed_canonical_target(&canon_repo, &canon_parent) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "target directory is outside worktree",
                    ));
                }
            }
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&full_path)?;
            Ok(full_path)
        }
        WireExplorerOp::CreateFolder { parent, name } => {
            let parent_dir = PathBuf::from(OsStr::from_bytes(&parent));
            let rel = validate_relative_input(&name)?;
            let (canon_repo, full_path) = ensure_worktree_create_path(repo, &parent_dir, &rel)?;
            if full_path.exists() || fs::symlink_metadata(&full_path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "folder already exists",
                ));
            }
            if let Some(p) = full_path.parent() {
                fs::create_dir_all(p)?;
                let canon_parent = p.canonicalize().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "target directory is outside worktree",
                    )
                })?;
                if !is_allowed_canonical_target(&canon_repo, &canon_parent) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "target directory is outside worktree",
                    ));
                }
            }
            fs::create_dir_all(&full_path)?;
            Ok(full_path)
        }
        WireExplorerOp::Rename { path, new_name } => {
            let target = PathBuf::from(OsStr::from_bytes(&path));
            let denied = || {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "cannot rename outside worktree root",
                )
            };
            if !target.starts_with(repo)
                || target == repo
                || target
                    .components()
                    .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
                || target
                    .file_name()
                    .is_some_and(is_forbidden_git_or_win32_segment)
            {
                return Err(denied());
            }
            let clean = new_name.trim();
            if clean.is_empty()
                || clean == "."
                || clean == ".."
                || clean.contains('/')
                || clean.contains('\\')
                || is_forbidden_git_or_win32_segment(OsStr::new(clean))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid file or folder name",
                ));
            }
            let parent = target
                .parent()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
            let canon_repo = repo.canonicalize().map_err(|_| denied())?;
            let parent_rel = parent.strip_prefix(repo).map_err(|_| denied())?;
            let mut cur = repo.to_path_buf();
            for comp in parent_rel.components() {
                let Component::Normal(part) = comp else {
                    return Err(denied());
                };
                if is_forbidden_git_or_win32_segment(part) {
                    return Err(denied());
                }
                cur.push(part);
                let canon_cur = cur.canonicalize().map_err(|_| denied())?;
                if !is_allowed_canonical_target(&canon_repo, &canon_cur) {
                    return Err(denied());
                }
            }
            let canon_parent = parent.canonicalize().map_err(|_| denied())?;
            if !is_allowed_canonical_target(&canon_repo, &canon_parent) {
                return Err(denied());
            }
            target.symlink_metadata()?;
            let new_path = parent.join(clean);
            if new_path == target {
                return Ok(new_path);
            }
            if new_path.exists() || fs::symlink_metadata(&new_path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "target name already exists",
                ));
            }
            fs::rename(&target, &new_path)?;
            Ok(new_path)
        }
        WireExplorerOp::Delete { path } => {
            let target = PathBuf::from(OsStr::from_bytes(&path));
            let denied = || {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "cannot delete worktree root, .git, or outside path",
                )
            };
            if !target.starts_with(repo)
                || target == repo
                || target
                    .components()
                    .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            {
                return Err(denied());
            }
            let rel = target.strip_prefix(repo).map_err(|_| denied())?;
            let canon_repo = repo.canonicalize().map_err(|_| denied())?;
            let mut cur = repo.to_path_buf();
            let mut comps = rel.components().peekable();
            if comps.peek().is_none() {
                return Err(denied());
            }
            while let Some(comp) = comps.next() {
                let Component::Normal(part) = comp else {
                    return Err(denied());
                };
                if is_forbidden_git_or_win32_segment(part) {
                    return Err(denied());
                }
                cur.push(part);
                if comps.peek().is_some() {
                    let canon_cur = cur.canonicalize().map_err(|_| denied())?;
                    if !is_allowed_canonical_target(&canon_repo, &canon_cur) {
                        return Err(denied());
                    }
                }
            }
            let home = crate::usage::accounts::default_home();
            krill_proto::usage::profiles::move_path_to_trash(&home, repo, &target)
        }
    }
}

fn list_quick_open(root: &Path) -> io::Result<Vec<Vec<u8>>> {
    if !root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "workspace directory not found",
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let paths = git_ls_files(root, deadline).unwrap_or_else(|| walk_quick_open(root, deadline));
    let mut out = Vec::new();
    let mut used = 0usize;
    for rel in paths {
        if out.len() >= MAX_QUICK_OPEN_FILES {
            break;
        }
        let bytes = rel.into_os_string().into_vec();
        // Quick-open paths are relative wire names, independent of the host separator.
        #[cfg(windows)]
        let bytes = bytes
            .into_iter()
            .map(|b| if b == b'\\' { b'/' } else { b })
            .collect::<Vec<_>>();
        if bytes.is_empty() || bytes.len() > MAX_REMOTE_PATH {
            continue;
        }
        used = used.saturating_add(bytes.len().saturating_add(4));
        if used > MAX_LISTING_WIRE_BYTES {
            break;
        }
        out.push(bytes);
    }
    Ok(out)
}

fn git_ls_files(root: &Path, deadline: Instant) -> Option<Vec<PathBuf>> {
    git_ls_files_with(Path::new("git"), root, deadline)
}

fn git_ls_files_with(program: &Path, root: &Path, deadline: Instant) -> Option<Vec<PathBuf>> {
    if !root.join(".git").exists() {
        return None;
    }
    let mut cmd = git_command(program, root);
    cmd.args([
        "-c",
        "core.quotePath=false",
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let mut child = spawn(&mut cmd).ok()?;
    let stdout_raw = child.stdout.take()?;
    let mut stdout = PipeReader::new(stdout_raw).ok()?;
    let mut output = Vec::new();
    let mut buf = [0u8; 8192];
    let mut eof = false;
    let mut truncated = false;
    let ok = loop {
        if !eof {
            if let Ok((hit_eof, hit_cap)) =
                drain_pipe_capped(&mut stdout, &mut output, MAX_LS_FILES_BYTES, &mut buf)
            {
                eof |= hit_eof;
                if hit_cap {
                    truncated = true;
                    terminate_gracefully(&mut child, GIT_TERMINATE_GRACE);
                    break true;
                }
            } else {
                terminate_gracefully(&mut child, GIT_TERMINATE_GRACE);
                break false;
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !eof {
                    if Instant::now() >= deadline {
                        terminate_gracefully(&mut child, GIT_TERMINATE_GRACE);
                        break false;
                    }
                    wait_readable(&[Some(&stdout as &dyn AsFd)], Duration::from_millis(5));
                    continue;
                }
                kill_and_reap(&mut child);
                break status.success();
            }
            Ok(None) if Instant::now() < deadline => {
                wait_readable(
                    &[(!eof).then_some(&stdout as &dyn AsFd)],
                    Duration::from_millis(5),
                );
            }
            Ok(None) | Err(_) => {
                terminate_gracefully(&mut child, GIT_TERMINATE_GRACE);
                break false;
            }
        }
    };
    drop(stdout);
    drop(child);
    if !ok {
        return None;
    }
    let slice = if truncated {
        let end = output.iter().rposition(|&b| b == 0)?;
        output.get(..end).unwrap_or_default()
    } else {
        output.as_slice()
    };
    let mut files: Vec<PathBuf> = slice
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .take(MAX_QUICK_OPEN_FILES)
        .map(|p| PathBuf::from(OsStr::from_bytes(p)))
        .collect();
    files.dedup();
    files.retain(|p| Instant::now() < deadline && root.join(p).is_file());
    Some(files)
}

fn walk_quick_open(root: &Path, deadline: Instant) -> Vec<PathBuf> {
    const SKIP: [&str; 3] = ["node_modules", "target", "dist"];
    let mut files = Vec::new();
    let mut queue = VecDeque::from([PathBuf::new()]);
    while let Some(rel) = queue.pop_front() {
        if Instant::now() >= deadline {
            break;
        }
        let Ok(entries) = fs::read_dir(root.join(&rel)) else {
            continue;
        };
        for entry in entries.flatten() {
            if Instant::now() >= deadline {
                break;
            }
            let name: OsString = entry.file_name();
            if name.as_os_str().as_bytes().starts_with(b".") || SKIP.iter().any(|s| name == *s) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = rel.join(&name);
            if kind.is_dir() && queue.len() < MAX_QUICK_OPEN_FILES {
                queue.push_back(path);
            } else if kind.is_file() {
                files.push(path);
                if files.len() >= MAX_QUICK_OPEN_FILES {
                    return files;
                }
            }
        }
    }
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::thread;

    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            static SEQ: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "krilld-remote-fs-{label}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn read_write_and_probe_roundtrip() {
        let dir = TestDir::new("rw");
        let file = dir.path().join("hello.txt");
        let write_reply = serve(RemoteFsRequest::WriteAtomic {
            path: file.as_os_str().as_bytes().to_vec(),
            bytes: b"initial\n".to_vec(),
            expected: None,
            stage_id: 0,
            offset: 0,
            commit: true,
        });
        let RemoteFsReply::Written(WireWriteOutcome::Saved(stamp1)) = write_reply else {
            panic!("expected Saved, got {write_reply:?}");
        };

        let probed = probe_paths(
            Some(dir.path().as_os_str().as_bytes()),
            &[b"hello.txt".to_vec(), b"missing.txt".to_vec()],
        );
        assert_eq!(
            probed,
            vec![Some(file.as_os_str().as_bytes().to_vec()), None]
        );

        let chunk = read_file_chunk(&file, 0, 1024).unwrap();
        assert_eq!(chunk.bytes, b"initial\n");
        assert!(chunk.eof);
        assert_eq!(chunk.stat.stamp, Some(stamp1));

        // Conflict when expected stamp differs:
        let conflict = write_atomic(
            &file,
            b"bad".as_slice(),
            Some(WireStamp {
                modified_unix_ns: Some(1),
                len: 99,
            }),
            0,
            0,
            true,
        );
        assert!(matches!(conflict, WireWriteOutcome::Conflict { .. }));
        assert_eq!(fs::read(&file).unwrap(), b"initial\n");
        assert_eq!(
            write_atomic(&file, b"staged-", Some(stamp1), 7, 0, false),
            WireWriteOutcome::StageAck
        );
        assert_eq!(fs::read(&file).unwrap(), b"initial\n");
        assert!(matches!(
            write_atomic(&file, b"commit", Some(stamp1), 7, 7, true),
            WireWriteOutcome::Saved(_)
        ));
        assert_eq!(fs::read(&file).unwrap(), b"staged-commit");
        assert!(scan_worktree_listing(&file, 20).is_err());
        assert!(scan_worktree_listing(&dir.path().join("missing"), 20).is_err());
    }

    #[test]
    fn chunked_stage_abort_connection_cleanup_and_handle_non_reuse() {
        let dir = TestDir::new("stage-lifecycle");
        let target = dir.path().join("screenshot.png");
        let target_wire = target.as_os_str().as_bytes().to_vec();
        let conn1 = next_connection_id();
        let conn2 = next_connection_id();

        // 1. Multi-chunk transfer on conn1 -> atomic publish -> retired handle cannot be reused.
        let r1 = serve_on_conn(
            conn1,
            RemoteFsRequest::WriteAtomic {
                path: target_wire.clone(),
                bytes: b"part-one-".to_vec(),
                expected: None,
                stage_id: 101,
                offset: 0,
                commit: false,
            },
        );
        assert_eq!(r1, RemoteFsReply::Written(WireWriteOutcome::StageAck));
        assert!(
            !target.exists(),
            "target must not exist before final atomic commit"
        );
        // Another connection must not write into conn1's active stage.
        let wrong_conn = serve_on_conn(
            conn2,
            RemoteFsRequest::WriteAtomic {
                path: target_wire.clone(),
                bytes: b"hijack".to_vec(),
                expected: None,
                stage_id: 101,
                offset: 9,
                commit: true,
            },
        );
        assert!(matches!(
            wrong_conn,
            RemoteFsReply::Written(WireWriteOutcome::Failed(_))
        ));
        let r2 = serve_on_conn(
            conn1,
            RemoteFsRequest::WriteAtomic {
                path: target_wire.clone(),
                bytes: b"part-two".to_vec(),
                expected: None,
                stage_id: 101,
                offset: 9,
                commit: true,
            },
        );
        assert!(matches!(
            r2,
            RemoteFsReply::Written(WireWriteOutcome::Saved(stamp)) if stamp.len == 17
        ));
        assert_eq!(fs::read(&target).unwrap(), b"part-one-part-two");
        assert!(!staging_path(&target, 101).exists());

        // Reusing retired stage_id=101 for same path must be rejected!
        let reused = serve_on_conn(
            conn1,
            RemoteFsRequest::WriteAtomic {
                path: target_wire.clone(),
                bytes: b"stale".to_vec(),
                expected: None,
                stage_id: 101,
                offset: 0,
                commit: true,
            },
        );
        assert!(matches!(
            reused,
            RemoteFsReply::Written(WireWriteOutcome::Failed(_))
        ));
        assert_eq!(fs::read(&target).unwrap(), b"part-one-part-two");

        // 2. Explicit AbortStage removes staging file and prevents handle reuse or late chunks.
        let abort_target = dir.path().join("aborted.png");
        let abort_wire = abort_target.as_os_str().as_bytes().to_vec();
        assert_eq!(
            serve_on_conn(
                conn1,
                RemoteFsRequest::WriteAtomic {
                    path: abort_wire.clone(),
                    bytes: b"partial-data".to_vec(),
                    expected: None,
                    stage_id: 202,
                    offset: 0,
                    commit: false,
                }
            ),
            RemoteFsReply::Written(WireWriteOutcome::StageAck)
        );
        assert!(staging_path(&abort_target, 202).exists());
        assert_eq!(
            serve_on_conn(
                conn1,
                RemoteFsRequest::AbortStage {
                    path: abort_wire.clone(),
                    stage_id: 202,
                }
            ),
            RemoteFsReply::StageAborted(true)
        );
        assert!(!staging_path(&abort_target, 202).exists());
        assert!(!abort_target.exists());
        // Late chunk on aborted handle fails and does not recreate staging file.
        assert!(matches!(
            serve_on_conn(
                conn1,
                RemoteFsRequest::WriteAtomic {
                    path: abort_wire.clone(),
                    bytes: b"late".to_vec(),
                    expected: None,
                    stage_id: 202,
                    offset: 12,
                    commit: true,
                }
            ),
            RemoteFsReply::Written(WireWriteOutcome::Failed(_))
        ));
        assert!(!staging_path(&abort_target, 202).exists());

        // 3. Connection drop cleans up orphaned staging files automatically and retires handles.
        let orphan_target = dir.path().join("orphan.png");
        let orphan_wire = orphan_target.as_os_str().as_bytes().to_vec();
        assert_eq!(
            serve_on_conn(
                conn2,
                RemoteFsRequest::WriteAtomic {
                    path: orphan_wire.clone(),
                    bytes: b"inflight".to_vec(),
                    expected: None,
                    stage_id: 303,
                    offset: 0,
                    commit: false,
                }
            ),
            RemoteFsReply::Written(WireWriteOutcome::StageAck)
        );
        assert!(staging_path(&orphan_target, 303).exists());
        cleanup_connection(conn2);
        assert!(!staging_path(&orphan_target, 303).exists());
        assert!(!orphan_target.exists());
        assert!(matches!(
            serve_on_conn(
                conn2,
                RemoteFsRequest::WriteAtomic {
                    path: orphan_wire,
                    bytes: b"reopen".to_vec(),
                    expected: None,
                    stage_id: 303,
                    offset: 0,
                    commit: true,
                }
            ),
            RemoteFsReply::Written(WireWriteOutcome::Failed(_))
        ));
    }

    #[test]
    fn explorer_and_quick_open_operations() {
        let dir = TestDir::new("explorer");
        let repo = dir.path().canonicalize().unwrap();
        let repo_wire = repo.as_os_str().as_bytes().to_vec();

        let created_folder = match serve(RemoteFsRequest::ExplorerOp {
            repo: repo_wire.clone(),
            op: WireExplorerOp::CreateFolder {
                parent: repo_wire.clone(),
                name: "src".to_owned(),
            },
        }) {
            RemoteFsReply::ExplorerOpResult(Ok(p)) => PathBuf::from(OsString::from_vec(p)),
            other => panic!("unexpected create folder reply: {other:?}"),
        };
        assert!(created_folder.is_dir());

        let created_file = match serve(RemoteFsRequest::ExplorerOp {
            repo: repo_wire.clone(),
            op: WireExplorerOp::CreateFile {
                parent: created_folder.as_os_str().as_bytes().to_vec(),
                name: "lib.rs".to_owned(),
            },
        }) {
            RemoteFsReply::ExplorerOpResult(Ok(p)) => PathBuf::from(OsString::from_vec(p)),
            other => panic!("unexpected create file reply: {other:?}"),
        };
        assert!(created_file.is_file());

        let renamed = match serve(RemoteFsRequest::ExplorerOp {
            repo: repo_wire.clone(),
            op: WireExplorerOp::Rename {
                path: created_file.as_os_str().as_bytes().to_vec(),
                new_name: "main.rs".to_owned(),
            },
        }) {
            RemoteFsReply::ExplorerOpResult(Ok(p)) => PathBuf::from(OsString::from_vec(p)),
            other => panic!("unexpected rename reply: {other:?}"),
        };
        assert!(renamed.is_file());

        let RemoteFsReply::QuickOpenFiles(Ok(quick)) = serve(RemoteFsRequest::QuickOpenList {
            root: repo_wire.clone(),
        }) else {
            panic!("expected QuickOpenFiles");
        };
        assert_eq!(quick, vec![b"src/main.rs".to_vec()]);
    }

    #[test]
    fn loopback_port_bridge_transfers_bidirectional_bytes() {
        let tcp_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = tcp_listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut conn, _) = tcp_listener.accept().unwrap();
            let mut buf = [0u8; 4];
            conn.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"ping");
            conn.write_all(b"pong").unwrap();
        });

        let tcp_client = connect_loopback_port(port).unwrap();
        let (mut ipc_client, ipc_server) = UnixStream::pair().unwrap();
        let bridge = thread::spawn(move || {
            bridge_streams(ipc_server, tcp_client);
        });

        ipc_client.write_all(b"ping").unwrap();
        ipc_client.shutdown(Shutdown::Write).unwrap();
        let mut out = Vec::new();
        ipc_client.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"pong");

        server.join().unwrap();
        bridge.join().unwrap();
    }
    #[test]
    fn staged_writes_isolate_destinations_and_preserve_existing_stage_files() {
        let dir = TestDir::new("stage-isolation");
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        assert_eq!(
            write_atomic(&a, b"aaa", None, 9, 0, false),
            WireWriteOutcome::StageAck
        );
        assert_eq!(
            write_atomic(&b, b"bbb", None, 9, 0, false),
            WireWriteOutcome::StageAck
        );
        assert!(matches!(
            write_atomic(&a, b"A", None, 9, 3, true),
            WireWriteOutcome::Saved(_)
        ));
        assert!(matches!(
            write_atomic(&b, b"B", None, 9, 3, true),
            WireWriteOutcome::Saved(_)
        ));
        assert_eq!(fs::read(&a).unwrap(), b"aaaA");
        assert_eq!(fs::read(&b).unwrap(), b"bbbB");
        let stage = staging_path(&a, 11);
        fs::write(&stage, b"existing transfer").unwrap();
        assert!(matches!(
            write_atomic(&a, b"new", None, 11, 0, false),
            WireWriteOutcome::Failed(_)
        ));
        assert_eq!(fs::read(stage).unwrap(), b"existing transfer");
    }

    #[test]
    fn read_dir_and_explorer_ops_reject_traversal_escape_and_git_aliases() {
        let outer = TestDir::new("escape");
        let repo = outer.path().join("repo");
        let outside = outer.path().join("outside");
        fs::create_dir_all(repo.join(".git/objects")).unwrap();
        fs::write(repo.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        fs::write(repo.join("ok.txt"), b"ok\n").unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"secret\n").unwrap();

        let repo = repo.canonicalize().unwrap();
        let outside = outside.canonicalize().unwrap();
        let repo_wire = repo.as_os_str().as_bytes().to_vec();

        let traversal = repo.join("sub").join("..").join("..");
        assert!(read_dir_listing(&repo, &traversal).is_err());
        assert!(read_dir_listing(&repo, &outside).is_err());
        assert!(read_dir_listing(&repo, &repo.join(".git")).is_err());

        #[cfg(unix)]
        {
            let link = repo.join("out-link");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(read_dir_listing(&repo, &link).is_err());
            let listing = read_dir_listing(&repo, &repo).unwrap();
            let entry = listing
                .entries
                .iter()
                .find(|e| e.name == "out-link")
                .unwrap();
            assert!(entry.is_symlink);
            assert!(!entry.is_dir);
        }

        for bad in [".git", ".GIT", ".git.", ".GIT. "] {
            assert!(validate_relative_input(bad).is_err(), "{bad:?}");
            let create_res = execute_explorer_op(
                &repo,
                WireExplorerOp::CreateFolder {
                    parent: repo_wire.clone(),
                    name: bad.to_owned(),
                },
            );
            assert!(create_res.is_err(), "CreateFolder {bad:?}");
            let rename_res = execute_explorer_op(
                &repo,
                WireExplorerOp::Rename {
                    path: repo.join("ok.txt").as_os_str().as_bytes().to_vec(),
                    new_name: bad.to_owned(),
                },
            );
            assert!(rename_res.is_err(), "Rename {bad:?}");
        }

        for protected in [repo.join(".git"), repo.join(".git/HEAD"), repo.join(".GIT")] {
            let del = execute_explorer_op(
                &repo,
                WireExplorerOp::Delete {
                    path: protected.as_os_str().as_bytes().to_vec(),
                },
            );
            assert!(del.is_err(), "Delete {protected:?}");
        }
        assert!(repo.join(".git/HEAD").is_file());

        let home_dir = TestDir::new("tilde-home");
        let home_note = home_dir.path().join("note.txt");
        fs::write(&home_note, b"hi").unwrap();
        assert_eq!(
            resolve_existing_file(b"~\\note.txt", None, home_dir.path()),
            Some(home_note.as_os_str().as_bytes().to_vec())
        );
    }

    #[test]
    fn offset_conflict_or_mismatch_does_not_unlink_existing_stage_file() {
        let dir = TestDir::new("stage-preserve");
        let target = dir.path().join("target.txt");
        fs::write(&target, b"initial").unwrap();
        let initial_stamp = current_stamp(&target).unwrap();

        assert_eq!(
            write_atomic(&target, b"0123456789", None, 42, 0, false),
            WireWriteOutcome::StageAck
        );
        let stage = staging_path(&target, 42);
        assert_eq!(fs::read(&stage).unwrap(), b"0123456789");

        // Stale expected stamp at offset == 0 must report Conflict without deleting active stage.
        let stale = WireStamp {
            modified_unix_ns: Some(
                initial_stamp
                    .modified_unix_ns
                    .unwrap_or(0)
                    .saturating_add(1),
            ),
            len: 0,
        };
        assert!(matches!(
            write_atomic(&target, b"dup", Some(stale), 42, 0, false),
            WireWriteOutcome::Conflict { .. }
        ));
        assert_eq!(fs::read(&stage).unwrap(), b"0123456789");

        // Wrong offset on an existing stage file must fail without deleting it.
        assert!(matches!(
            write_atomic(&target, b"bad", None, 42, 999, false),
            WireWriteOutcome::Failed(_)
        ));
        assert_eq!(fs::read(&stage).unwrap(), b"0123456789");
    }

    #[cfg(unix)]
    #[test]
    fn git_ls_files_survives_output_over_cap_and_kills_lingering_descendant_groups() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = TestDir::new("git-ls-cap-and-group");
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        let kept = repo.join("kept-first.rs");
        fs::write(&kept, b"fn main() {}\n").unwrap();

        // 1. Over-1-MiB NUL output: previously closed stdout triggered SIGPIPE and returned None.
        let large_git = dir.path().join("large-git.sh");
        fs::write(
            &large_git,
            "#!/bin/sh\nprintf 'kept-first.rs\\0'\nawk 'BEGIN { pad = sprintf(\"%0200d\", 0); for (i = 0; i < 6000; i++) printf \"dir/%s-%d.txt\\0\", pad, i }'\n",
        )
        .unwrap();
        fs::set_permissions(&large_git, fs::Permissions::from_mode(0o700)).unwrap();
        let files = git_ls_files_with(&large_git, &repo, Instant::now() + Duration::from_secs(5))
            .expect("capped git ls-files output must not return None on cap truncation");
        assert_eq!(files, vec![PathBuf::from("kept-first.rs")]);

        // 2. Child exits after spawning a background sleep holding stdout open: both helpers must
        //    terminate the process group cleanly after the deadline instead of hanging forever.
        let pid_file_ls = dir.path().join("ls.pid");
        let lingering_ls = dir.path().join("lingering-ls.sh");
        fs::write(
            &lingering_ls,
            format!(
                "#!/bin/sh\nsleep 30 &\necho $! > '{}'\nprintf 'kept-first.rs\\0'\nexit 0\n",
                pid_file_ls.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&lingering_ls, fs::Permissions::from_mode(0o700)).unwrap();
        let started = Instant::now();
        assert!(
            git_ls_files_with(
                &lingering_ls,
                &repo,
                Instant::now() + Duration::from_millis(120)
            )
            .is_none()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let assert_reaped = |pid: i32| {
            // SIGKILL to a process group tears down reparented grandchildren asynchronously;
            // allow a brief kernel reap window before asserting process_name fails.
            let deadline = Instant::now() + Duration::from_secs(1);
            while crate::sys::process_name(pid.unsigned_abs()).is_ok() {
                assert!(
                    Instant::now() < deadline,
                    "lingering descendant pid {pid} still alive after process-group kill"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        if let Ok(pid_text) = fs::read_to_string(&pid_file_ls)
            && let Ok(pid) = pid_text.trim().parse::<i32>()
        {
            assert_reaped(pid);
        }

        let pid_file_ignored = dir.path().join("ignored.pid");
        let lingering_ignored = dir.path().join("lingering-ignored.sh");
        fs::write(
            &lingering_ignored,
            format!(
                "#!/bin/sh\ncat >/dev/null &\nsleep 30 &\necho $! > '{}'\nexit 0\n",
                pid_file_ignored.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&lingering_ignored, fs::Permissions::from_mode(0o700)).unwrap();
        let started = Instant::now();
        let ignored = query_git_ignored_with(
            &lingering_ignored,
            &repo,
            &["kept-first.rs".to_owned()],
            Duration::from_millis(120),
        );
        assert!(ignored.is_empty());
        assert!(started.elapsed() < Duration::from_secs(2));
        if let Ok(pid_text) = fs::read_to_string(&pid_file_ignored)
            && let Ok(pid) = pid_text.trim().parse::<i32>()
        {
            assert_reaped(pid);
        }
    }

    #[cfg(unix)]
    #[test]
    fn in_repo_symlink_to_git_cannot_bypass_git_protection() {
        let outer = TestDir::new("git-symlink");
        let repo = outer.path().join("repo");
        fs::create_dir_all(repo.join(".git/objects")).unwrap();
        fs::write(repo.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        let repo = repo.canonicalize().unwrap();

        let link = repo.join("git-link");
        std::os::unix::fs::symlink(repo.join(".git"), &link).unwrap();

        assert!(read_dir_listing(&repo, &link).is_err());
        let listing = read_dir_listing(&repo, &repo).unwrap();
        let entry = listing
            .entries
            .iter()
            .find(|e| e.name == "git-link")
            .unwrap();
        assert!(entry.is_symlink);
        assert!(!entry.is_dir);

        let link_wire = link.as_os_str().as_bytes().to_vec();
        let repo_wire = repo.as_os_str().as_bytes().to_vec();
        for op in [
            WireExplorerOp::CreateFile {
                parent: link_wire.clone(),
                name: "injected".to_owned(),
            },
            WireExplorerOp::CreateFile {
                parent: repo_wire.clone(),
                name: "git-link/injected".to_owned(),
            },
            WireExplorerOp::CreateFolder {
                parent: link_wire,
                name: "injected-dir".to_owned(),
            },
            WireExplorerOp::CreateFolder {
                parent: repo_wire,
                name: "git-link/injected-dir".to_owned(),
            },
            WireExplorerOp::Rename {
                path: link.join("HEAD").as_os_str().as_bytes().to_vec(),
                new_name: "HEAD2".to_owned(),
            },
            WireExplorerOp::Delete {
                path: link.join("HEAD").as_os_str().as_bytes().to_vec(),
            },
        ] {
            let err = execute_explorer_op(&repo, op).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        }
        assert!(repo.join(".git/HEAD").is_file());
        assert!(!repo.join(".git/injected").exists());
        assert!(!repo.join(".git/injected-dir").exists());
    }

    #[test]
    fn git_command_clears_inherited_git_repository_env_vars() {
        let cmd = git_command(Path::new("git"), Path::new("."));
        let envs: Vec<_> = cmd.get_envs().collect();
        for var in CLEARED_GIT_ENV {
            assert!(
                envs.iter()
                    .any(|(k, v)| *k == OsStr::new(var) && v.is_none()),
                "missing env_remove for {var}"
            );
        }
    }
}
