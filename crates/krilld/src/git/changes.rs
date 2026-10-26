//! MK-053: 한 큐 작업 안에서 status와 staged/unstaged numstat을 스트림으로 읽는다.
//! 개수·바이트 상한, 취소, 제한시간, 파싱 실패의 모든 경로에서 자식을 kill + wait한다.

use krill_proto::os::ffi::OsStrExt;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use krill_proto::control::Reply;
use krill_proto::git::{ChangedFile, Changes, MAX_FILES, MAX_PATH_BYTES, Numstat};

use crate::child::{AsFd, PipeReader};

use super::GitQueue;

const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RECORD: usize = 64 * 1024;
const MAX_STDERR: usize = 8 * 1024;

/// Cancelled queries skip spawning, including while queued. Set cancellation before hiding,
/// then receive the result to ensure running children have been reaped.
pub fn query(
    q: &GitQueue,
    repo: PathBuf,
    cancel: Arc<AtomicBool>,
) -> Receiver<Result<Changes, String>> {
    submit(q, repo, cancel, |(changes, _)| changes)
}

/// [`query`]에 status `--branch` 헤더의 현재 브랜치를 더한다. 분리 HEAD는 빈 문자열이다
/// (`git branch --show-current`와 같다). 와이어 [`Changes`]는 바꾸지 않는 krilld 내부용.
pub(crate) fn query_with_branch(
    q: &GitQueue,
    repo: PathBuf,
    cancel: Arc<AtomicBool>,
) -> Receiver<Result<(Changes, String), String>> {
    submit(q, repo, cancel, |result| result)
}

fn submit<T: Send + 'static>(
    q: &GitQueue,
    repo: PathBuf,
    cancel: Arc<AtomicBool>,
    map: fn((Changes, String)) -> T,
) -> Receiver<Result<T, String>> {
    let (tx, rx) = mpsc::channel();
    q.enqueue(
        repo,
        0, // only fixed-size channel/cancellation handles; repo path is accounted by enqueue
        Box::new(move |program, repo| {
            // K-16: 숨김 처리로 취소된 조회는 큐 차례가 와도 git 자식을 띄우지 않는다.
            let result = if cancel.load(Ordering::Acquire) {
                Err("git query cancelled".to_owned())
            } else {
                run(program, repo, &cancel).map(map)
            };
            tx.send(result).ok();
        }),
    );
    rx
}

/// One-shot CLI query, independent of the watcher lifecycle.
pub fn serve(repo: &[u8]) -> Reply {
    let result = super::queue().and_then(|q| {
        query(
            q,
            PathBuf::from(OsStr::from_bytes(repo)),
            Arc::new(AtomicBool::new(false)),
        )
        .recv()
        .map_err(|e| e.to_string())?
    });
    match result {
        Ok(changes) => Reply::GitChanges(changes),
        Err(e) => Reply::GitChangesFailed(e),
    }
}

fn run(program: &Path, repo: &Path, cancel: &AtomicBool) -> Result<(Changes, String), String> {
    let deadline = Instant::now() + TIMEOUT;
    let mut parser = StatusParser::default();
    stream(
        program,
        repo,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
        ],
        cancel,
        deadline,
        |record| parser.record(record),
    )?;
    if parser.rename_pending {
        return Err("truncated status rename record".to_owned());
    }
    let branch = parser.branch;
    let mut changes = parser.changes;
    let paths: HashMap<Vec<u8>, usize> = changes
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.clone(), i))
        .collect();
    if changes.files.is_empty() {
        return Ok((changes, branch));
    }
    for staged in [true, false] {
        let mut args = vec![
            "-c",
            "diff.autoRefreshIndex=false",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "-z",
            "--numstat",
            "-M",
        ];
        if staged {
            args.push("--cached");
        }
        // `--cached` also returns index additions with an unborn HEAD.
        let mut stats = StatsParser::default();
        stream(program, repo, &args, cancel, deadline, |record| {
            let Some((path, value)) = stats.record(record)? else {
                return Ok(false);
            };
            if let Some(file) = paths.get(&path).and_then(|i| changes.files.get_mut(*i)) {
                if staged {
                    file.staged = Some(value);
                } else {
                    file.unstaged = Some(value);
                }
            }
            if stats.count >= MAX_FILES {
                changes.limited = true;
                return Ok(true);
            }
            Ok(false)
        })?;
        if stats.pending.is_some() {
            return Err("truncated numstat rename record".to_owned());
        }
    }
    Ok((changes, branch))
}

#[derive(Default)]
struct StatusParser {
    changes: Changes,
    branch: String,
    path_bytes: usize,
    rename_pending: bool,
}

impl StatusParser {
    fn record(&mut self, record: &[u8]) -> Result<bool, String> {
        if self.rename_pending {
            self.rename_pending = false;
            self.path_bytes += record.len();
            if self.path_bytes > MAX_PATH_BYTES {
                self.changes.files.pop();
                self.changes.limited = true;
                return Ok(true);
            }
            if let Some(file) = self.changes.files.last_mut() {
                file.old_path = Some(record.to_vec());
            }
            return Ok(self.cap());
        }
        let (fields, rename) = match record.first() {
            Some(b'#') => {
                if let Some(head) = record.strip_prefix(b"# branch.head ")
                    && head != b"(detached)"
                {
                    self.branch = String::from_utf8_lossy(head).into_owned();
                }
                return Ok(false);
            }
            Some(b'?') => (2, false),
            Some(b'1') => (9, false),
            Some(b'2') => (10, true),
            Some(b'u') => (11, false),
            _ => return Err("invalid porcelain v2 record".to_owned()),
        };
        let parts: Vec<_> = record.splitn(fields, |b| *b == b' ').collect();
        if parts.len() != fields {
            return Err("incomplete porcelain v2 record".to_owned());
        }
        let path = parts.last().copied().unwrap_or_default();
        if path.is_empty() {
            return Err("empty git path".to_owned());
        }
        let status = if fields == 2 {
            *b"??"
        } else {
            parts
                .get(1)
                .and_then(|s| <[u8; 2]>::try_from(*s).ok())
                .ok_or("invalid porcelain v2 status")?
        };
        self.path_bytes += path.len();
        if self.path_bytes > MAX_PATH_BYTES {
            self.changes.limited = true;
            return Ok(true);
        }
        self.changes.files.push(ChangedFile {
            path: path.to_vec(),
            old_path: None,
            status,
            staged: None,
            unstaged: None,
        });
        self.rename_pending = rename;
        Ok(!rename && self.cap())
    }

    fn cap(&mut self) -> bool {
        if self.changes.files.len() >= MAX_FILES {
            self.changes.limited = true;
        }
        self.changes.limited
    }
}

#[derive(Default)]
struct StatsParser {
    /// Rename: 0 waits for the old path, 1 waits for the new path.
    pending: Option<(Numstat, u8)>,
    count: usize,
}

impl StatsParser {
    fn record(&mut self, record: &[u8]) -> Result<Option<(Vec<u8>, Numstat)>, String> {
        if let Some((value, step)) = self.pending {
            if record.is_empty() {
                return Err("empty numstat rename path".to_owned());
            }
            if step == 0 {
                self.pending = Some((value, 1));
                return Ok(None);
            }
            self.pending = None;
            self.count += 1;
            return Ok(Some((record.to_vec(), value)));
        }
        let mut parts = record.splitn(3, |b| *b == b'\t');
        let added = number(parts.next().ok_or("missing numstat added")?)?;
        let deleted = number(parts.next().ok_or("missing numstat deleted")?)?;
        let path = parts.next().ok_or("missing numstat path")?;
        let value = Numstat { added, deleted };
        if added.is_none() != deleted.is_none() {
            return Err("invalid binary numstat".to_owned());
        }
        if path.is_empty() {
            self.pending = Some((value, 0));
            return Ok(None);
        }
        self.count += 1;
        Ok(Some((path.to_vec(), value)))
    }
}

fn number(bytes: &[u8]) -> Result<Option<u64>, String> {
    if bytes == b"-" {
        return Ok(None);
    }
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Some)
        .ok_or_else(|| "invalid numstat count".to_owned())
}

fn interrupted(cancel: &AtomicBool, deadline: Instant) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        return Err("git query cancelled".to_owned());
    }
    if Instant::now() >= deadline {
        return Err("git query timed out".to_owned());
    }
    Ok(())
}

fn stream(
    program: &Path,
    repo: &Path,
    args: &[&str],
    cancel: &AtomicBool,
    deadline: Instant,
    mut record: impl FnMut(&[u8]) -> Result<bool, String>,
) -> Result<(), String> {
    interrupted(cancel, deadline)?;
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut child = crate::child::spawn(
        super::command(program, repo, &args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .map_err(|e| e.to_string())?;
    let mut stdout = PipeReader::new(child.stdout.take().ok_or("missing git stdout")?)
        .map_err(|e| e.to_string())?;
    let mut stderr = PipeReader::new(child.stderr.take().ok_or("missing git stderr")?)
        .map_err(|e| e.to_string())?;
    let mut pending = Vec::new();
    let mut errors = Vec::new();
    let mut buf = [0u8; 8192];
    let mut eof = false;
    let mut stderr_eof = false;
    loop {
        interrupted(cancel, deadline)?;
        match stderr.read(&mut buf) {
            Ok(0) => stderr_eof = true,
            Ok(n) => errors.extend(
                buf.iter()
                    .take(n)
                    .take(MAX_STDERR.saturating_sub(errors.len())),
            ),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.to_string()),
        }
        if !eof {
            match stdout.read(&mut buf) {
                Ok(0) => eof = true,
                Ok(n) => {
                    for &byte in buf.iter().take(n) {
                        if byte == 0 {
                            if record(&pending)? {
                                return Ok(());
                            }
                            pending.clear();
                        } else {
                            if pending.len() >= MAX_RECORD {
                                return Err("git record exceeds byte limit".to_owned());
                            }
                            pending.push(byte);
                        }
                    }
                    continue;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        if eof && let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            // Drain final stderr while respecting the retained byte limit.
            drain_final_stderr(&mut stderr, &mut errors, &mut buf, cancel, deadline)?;
            if !status.success() {
                return Err(format!(
                    "git failed: {}",
                    String::from_utf8_lossy(&errors).trim()
                ));
            }
            if !pending.is_empty() {
                return Err("truncated git record".to_owned());
            }
            return Ok(());
        }
        // 10ms 상한은 취소·제한시간 확인 주기다(이전 sleep 폴링과 같다).
        crate::child::wait_readable(
            &[
                (!eof).then_some(&stdout as &dyn AsFd),
                (!stderr_eof).then_some(&stderr as &dyn AsFd),
            ],
            Duration::from_millis(10),
        );
    }
}

fn drain_final_stderr(
    stderr: &mut impl Read,
    errors: &mut Vec<u8>,
    buf: &mut [u8],
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<(), String> {
    while let Ok(n) = stderr.read(buf) {
        interrupted(cancel, deadline)?;
        if n == 0 {
            break;
        }
        errors.extend(
            buf.iter()
                .take(n)
                .take(MAX_STDERR.saturating_sub(errors.len())),
        );
    }
    Ok(())
}

#[cfg(test)]
#[cfg(unix)]
pub(super) mod tests;
