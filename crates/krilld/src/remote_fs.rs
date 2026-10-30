//! 원격 호스트 파일 시스템(탐색·열기·원자적 저장·Quick Open) 및 루프백 포트 포워딩 서버 핸들러.

use std::collections::{BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash as _, Hasher as _};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use krill_proto::os::ffi::{OsStrExt as _, OsStringExt as _};
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

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn serve(req: RemoteFsRequest) -> RemoteFsReply {
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
            RemoteFsReply::Written(write_atomic(&p, &bytes, expected, stage_id, offset, commit))
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
    } else if let Some(rest) = raw.strip_prefix(b"~/") {
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

fn write_atomic(
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
    let stage_path = staging_path(path, stage_id);
    let current = current_stamp(path);
    if expected.is_some() && current != expected {
        if stage_id != 0 {
            fs::remove_file(&stage_path).ok();
        }
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
        let mut owned_stage = offset != 0;
        let res = (|| -> io::Result<WireWriteOutcome> {
            let mut file = if offset == 0 {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&stage_path)?
            } else {
                let f = OpenOptions::new().write(true).open(&stage_path)?;
                if f.metadata()?.len() != offset {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "staging file offset mismatch",
                    ));
                }
                f
            };
            owned_stage = true;
            if offset > 0 {
                file.seek(SeekFrom::Start(offset))?;
            }
            file.write_all(bytes)?;
            if !commit {
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
                return Ok(WireWriteOutcome::Conflict { current: now_stamp });
            }
            fs::rename(&stage_path, path)?;
            let meta = fs::metadata(path)?;
            Ok(WireWriteOutcome::Saved(stamp_from_meta(&meta)))
        })();
        match res {
            Ok(outcome) => outcome,
            Err(err) => {
                if owned_stage {
                    fs::remove_file(&stage_path).ok();
                }
                WireWriteOutcome::Failed(truncate_text(err.to_string()))
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

fn read_raw_dir_entries(repo: &Path, dir: &Path) -> io::Result<Vec<WireDirEntry>> {
    if !dir.starts_with(repo) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "directory is outside repository",
        ));
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
        if name == ".git" || name.len() > MAX_REMOTE_TEXT {
            continue;
        }
        let path = entry.path();
        let path_bytes = path.as_os_str().as_bytes().to_vec();
        if path_bytes.len() > MAX_REMOTE_PATH {
            continue;
        }
        let (is_dir, is_symlink) = entry.file_type().map_or((false, false), |ft| {
            let is_symlink = ft.is_symlink();
            (ft.is_dir() || (is_symlink && path.is_dir()), is_symlink)
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

fn query_git_ignored(repo: &Path, relative_paths: &[String]) -> Vec<String> {
    if relative_paths.is_empty() || !repo.join(".git").exists() {
        return Vec::new();
    }
    let Ok(mut child) = Command::new("git")
        .current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "-c",
            "core.quotePath=false",
            "check-ignore",
            "--stdin",
            "-z",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    if let Some(mut stdin) = child.stdin.take() {
        let mut payload = Vec::new();
        for rel in relative_paths {
            payload.extend_from_slice(rel.as_bytes());
            payload.push(0);
        }
        std::thread::spawn(move || {
            stdin.write_all(&payload).ok();
        });
    }
    let Some(stdout) = child.stdout.take() else {
        child.kill().ok();
        child.wait().ok();
        return Vec::new();
    };
    let reader_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        stdout.take(256 * 1024).read_to_end(&mut buf).ok();
        buf
    });
    let deadline = Instant::now() + GIT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) | Err(_) => {
                child.kill().ok();
                child.wait().ok();
                break;
            }
        }
    }
    let Ok(stdout_bytes) = reader_handle.join() else {
        return Vec::new();
    };
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
            Component::Normal(part) if part != OsStr::new(".git") => clean.push(part),
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
        if part == OsStr::new(".git") {
            return Err(denied());
        }
        cur.push(part);
        match fs::symlink_metadata(&cur) {
            Ok(_) => {
                let canon_cur = cur.canonicalize().map_err(|_| denied())?;
                if !canon_cur.starts_with(&canon_repo) {
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
                if !canon_parent.starts_with(&canon_repo) {
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
                if !canon_parent.starts_with(&canon_repo) {
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
            {
                return Err(denied());
            }
            let clean = new_name.trim();
            if clean.is_empty()
                || clean == "."
                || clean == ".."
                || clean == ".git"
                || clean.contains('/')
                || clean.contains('\\')
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
                if part == OsStr::new(".git") {
                    return Err(denied());
                }
                cur.push(part);
                let canon_cur = cur.canonicalize().map_err(|_| denied())?;
                if !canon_cur.starts_with(&canon_repo) {
                    return Err(denied());
                }
            }
            let canon_parent = parent.canonicalize().map_err(|_| denied())?;
            if !canon_parent.starts_with(&canon_repo) {
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
            if !target.starts_with(repo) || target == repo {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "cannot delete worktree root or outside path",
                ));
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
    if !root.join(".git").exists() {
        return None;
    }
    let mut child = Command::new("git")
        .current_dir(krill_proto::os::shell_path(root))
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
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
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        stdout.take(1024 * 1024).read_to_end(&mut buf).ok();
        buf
    });
    let ok = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) | Err(_) => {
                child.kill().ok();
                child.wait().ok();
                break false;
            }
        }
    };
    if !ok {
        return None;
    }
    let output = reader.join().ok()?;
    let mut files: Vec<PathBuf> = output
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
}
