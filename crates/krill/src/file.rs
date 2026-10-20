//! `krill file open|diff`(MK-178, OWE-732): 실행 중인 Krill 앱의 중앙 에디터 탭이나 우측 Diff 패널로
//! 파일을 즉시 열도록 상태 디렉터리의 요청 큐(`file-requests.json`)에 기록한다.
//!
//! CLI 사양 참고: `.ref/orca/src/cli/specs/file.ts`, `.ref/orca/src/cli/handlers/file.ts`
//! (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use krill_proto::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use krill_proto::daemon;
use serde::{Deserialize, Serialize};

pub const FILE_REQUESTS_FILE: &str = "file-requests.json";
pub const FILE_REQUESTS_LOCK: &str = "file-requests.lock";
const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_QUEUE_LEN: usize = 64;

const USAGE: &str = "\
usage:
  krill file open <path> [--line <n> [--column <n>]] [--worktree <path>] [--state-dir <path>] [--focus] [--json]
  krill file diff <path> [--staged] [--worktree <path>] [--state-dir <path>] [--focus] [--json]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileActionKind {
    Open,
    Diff,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRequest {
    pub id: u64,
    pub kind: FileActionKind,
    pub worktree: PathBuf,
    pub path: PathBuf,
    pub relative_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(default)]
    pub staged: bool,
    #[serde(default)]
    pub focus: bool,
    #[serde(default)]
    pub created_at_ms: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRequestQueue {
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub requests: Vec<FileRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    pub kind: FileActionKind,
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub staged: bool,
    pub worktree: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub focus: bool,
    pub json: bool,
}

pub fn run(args: &[OsString]) -> ExitCode {
    // `open`/`diff` 서브커맨드 자리만 도움말로 본다. 파일 경로 인자의 `help`는 경로다.
    let help_here = matches!(
        args.first().and_then(|a| a.to_str()),
        Some("-h" | "--help" | "help")
    ) || matches!(args.get(1).and_then(|a| a.to_str()), Some("-h" | "--help"));
    if help_here {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("file: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Some(state_dir) = parsed.state_dir.clone().or_else(daemon::state_dir) else {
        eprintln!("file: HOME is not set");
        return ExitCode::FAILURE;
    };
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("file: {e}");
            return ExitCode::FAILURE;
        }
    };
    match execute(&state_dir, &cwd, &parsed) {
        Ok(req) => {
            print_result(&req, parsed.json);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("file: {e}");
            ExitCode::FAILURE
        }
    }
}

pub fn parse(args: &[OsString]) -> Result<ParsedCommand, String> {
    let mut it = args.iter();
    let Some(sub_raw) = it.next() else {
        return Err("expected 'open' or 'diff'".to_owned());
    };
    let kind = match utf8(sub_raw)? {
        "open" => FileActionKind::Open,
        "diff" => FileActionKind::Diff,
        other => {
            return Err(format!(
                "unknown subcommand {other:?}; expected 'open' or 'diff'"
            ));
        }
    };

    let mut path: Option<String> = None;
    let mut line: Option<u32> = None;
    let mut column: Option<u32> = None;
    let mut staged = false;
    let mut worktree: Option<PathBuf> = None;
    let mut state_dir: Option<PathBuf> = None;
    let mut focus = false;
    let mut json = false;

    while let Some(raw) = it.next() {
        let arg = utf8(raw)?;
        if let Some(val) = arg.strip_prefix("--path=") {
            set_path(&mut path, val)?;
        } else if let Some(val) = arg.strip_prefix("--line=") {
            if kind != FileActionKind::Open {
                return Err("--line is only supported for 'krill file open'".to_owned());
            }
            line = Some(parse_positive_u32(val, "--line")?);
        } else if let Some(val) = arg.strip_prefix("--column=") {
            if kind != FileActionKind::Open {
                return Err("--column is only supported for 'krill file open'".to_owned());
            }
            column = Some(parse_positive_u32(val, "--column")?);
        } else if let Some(val) = arg
            .strip_prefix("--worktree=")
            .or_else(|| arg.strip_prefix("--cwd="))
        {
            worktree = parse_worktree_arg(val);
        } else if let Some(val) = arg.strip_prefix("--state-dir=") {
            if val.trim().is_empty() {
                return Err("missing --state-dir value".to_owned());
            }
            state_dir = Some(PathBuf::from(val));
        } else {
            match arg {
                "--path" => {
                    let val = utf8(it.next().ok_or("missing --path value")?)?;
                    set_path(&mut path, val)?;
                }
                "--line" | "-l" => {
                    if kind != FileActionKind::Open {
                        return Err("--line is only supported for 'krill file open'".to_owned());
                    }
                    let val = utf8(it.next().ok_or("missing --line value")?)?;
                    line = Some(parse_positive_u32(val, "--line")?);
                }
                "--column" | "-c" => {
                    if kind != FileActionKind::Open {
                        return Err("--column is only supported for 'krill file open'".to_owned());
                    }
                    let val = utf8(it.next().ok_or("missing --column value")?)?;
                    column = Some(parse_positive_u32(val, "--column")?);
                }
                "--staged" | "--cached" => {
                    if kind != FileActionKind::Diff {
                        return Err("--staged is only supported for 'krill file diff'".to_owned());
                    }
                    staged = true;
                }
                "--worktree" | "--cwd" => {
                    let val = utf8(it.next().ok_or("missing --worktree value")?)?;
                    worktree = parse_worktree_arg(val);
                }
                "--state-dir" => {
                    let val = utf8(it.next().ok_or("missing --state-dir value")?)?;
                    if val.trim().is_empty() {
                        return Err("missing --state-dir value".to_owned());
                    }
                    state_dir = Some(PathBuf::from(val));
                }
                "--focus" => focus = true,
                "--json" => json = true,
                "--" => {
                    let val = utf8(it.next().ok_or("missing file path")?)?;
                    set_path(&mut path, val)?;
                    if it.next().is_some() {
                        return Err("unexpected arguments after path".to_owned());
                    }
                }
                s if !s.starts_with('-') && path.is_none() => {
                    set_path(&mut path, s)?;
                }
                other => return Err(format!("unexpected argument {other:?}")),
            }
        }
    }

    let raw_path = path.ok_or("missing file path")?;
    if column.is_some() && line.is_none() {
        return Err("--column requires --line".to_owned());
    }

    Ok(ParsedCommand {
        kind,
        path: raw_path,
        line,
        column,
        staged,
        worktree,
        state_dir,
        focus,
        json,
    })
}

fn set_path(slot: &mut Option<String>, val: &str) -> Result<(), String> {
    let trimmed = val.trim();
    if trimmed.is_empty() {
        return Err("file path must not be empty".to_owned());
    }
    if slot.is_some() {
        return Err("multiple file paths provided".to_owned());
    }
    *slot = Some(trimmed.to_owned());
    Ok(())
}

fn parse_positive_u32(raw: &str, flag: &str) -> Result<u32, String> {
    raw.trim()
        .parse::<u32>()
        .ok()
        .filter(|n| *n >= 1)
        .ok_or_else(|| format!("{flag} must be a positive integer"))
}

fn parse_worktree_arg(val: &str) -> Option<PathBuf> {
    let trimmed = val.trim();
    if matches!(trimmed, "" | "." | "active") {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn utf8(s: &OsStr) -> Result<&str, String> {
    s.to_str()
        .ok_or_else(|| "arguments must be UTF-8".to_owned())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `path:line[:col]` 표기를 분리한다(원문 경로가 디스크에 없을 때만 보조로 쓴다).
fn split_line_col_suffix(raw: &str) -> Option<(&str, u32, Option<u32>)> {
    let (left, last) = raw.rsplit_once(':')?;
    if left.is_empty() {
        return None;
    }
    let last_num = last.parse::<u32>().ok().filter(|n| *n >= 1)?;
    if let Some((base, mid)) = left.rsplit_once(':')
        && !base.is_empty()
        && let Some(line_num) = mid.parse::<u32>().ok().filter(|n| *n >= 1)
    {
        return Some((base, line_num, Some(last_num)));
    }
    Some((left, last_num, None))
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn canonicalize_allowing_missing_leaf(path: &Path) -> PathBuf {
    if let Ok(canon) = fs::canonicalize(path) {
        return canon;
    }
    let normalized = normalize_lexical(path);
    let mut cursor = normalized.as_path();
    let mut tail = Vec::new();
    while let Some(name) = cursor.file_name() {
        if let Ok(canon_parent) = fs::canonicalize(cursor) {
            let mut rebuilt = canon_parent;
            for part in tail.into_iter().rev() {
                rebuilt.push(part);
            }
            return rebuilt;
        }
        tail.push(name.to_os_string());
        let Some(parent) = cursor.parent() else {
            break;
        };
        cursor = parent;
    }
    normalized
}

pub fn execute(state_dir: &Path, cwd: &Path, cmd: &ParsedCommand) -> Result<FileRequest, String> {
    let base_dir = match &cmd.worktree {
        Some(wt) if wt.is_absolute() => wt.clone(),
        Some(wt) => cwd.join(wt),
        None => cwd.to_path_buf(),
    };
    let base_dir = fs::canonicalize(&base_dir).unwrap_or_else(|_| normalize_lexical(&base_dir));
    let mut line = cmd.line;
    let mut column = cmd.column;

    let raw_candidate = {
        let p = Path::new(&cmd.path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            base_dir.join(p)
        }
    };

    let target_path = if cmd.kind == FileActionKind::Open
        && !raw_candidate.exists()
        && line.is_none()
        && let Some((stripped, parsed_line, parsed_col)) = split_line_col_suffix(&cmd.path)
    {
        let alt = if Path::new(stripped).is_absolute() {
            PathBuf::from(stripped)
        } else {
            base_dir.join(stripped)
        };
        if alt.exists() {
            line = Some(parsed_line);
            column = parsed_col;
            alt
        } else {
            raw_candidate
        }
    } else {
        raw_candidate
    };

    let (resolved_file, worktree_root, relative_path) = match cmd.kind {
        FileActionKind::Open => {
            let canon = fs::canonicalize(&target_path)
                .map_err(|_| format!("file not found: {}", cmd.path))?;
            let meta = fs::metadata(&canon)
                .map_err(|e| format!("cannot stat {}: {e}", canon.display()))?;
            if meta.is_dir() {
                return Err(format!(
                    "target path is a directory, not a file: {}",
                    cmd.path
                ));
            }
            let wt = if cmd.worktree.is_some() {
                let explicit_wt = crate::set::resolve_worktree(&base_dir);
                if canon.starts_with(&explicit_wt) {
                    explicit_wt
                } else {
                    return Err(format!(
                        "target path is outside worktree {}: {}",
                        explicit_wt.display(),
                        canon.display()
                    ));
                }
            } else {
                crate::set::resolve_worktree(&canon)
            };
            let rel = canon
                .strip_prefix(&wt)
                .ok()
                .and_then(|p| {
                    (!p.as_os_str().is_empty()).then(|| p.to_string_lossy().replace('\\', "/"))
                })
                .unwrap_or_else(|| {
                    canon
                        .file_name()
                        .map_or_else(|| cmd.path.clone(), |n| n.to_string_lossy().into_owned())
                });
            (canon, wt, rel)
        }
        FileActionKind::Diff => {
            let resolved = canonicalize_allowing_missing_leaf(&target_path);
            if let Ok(meta) = fs::metadata(&resolved)
                && meta.is_dir()
            {
                return Err(format!(
                    "target path is a directory, not a file: {}",
                    cmd.path
                ));
            }
            let wt = crate::set::resolve_worktree(&base_dir);
            let Ok(stripped) = resolved.strip_prefix(&wt) else {
                return Err(format!(
                    "target path is outside worktree {}: {}",
                    wt.display(),
                    resolved.display()
                ));
            };
            if stripped.as_os_str().is_empty() {
                return Err("target path is a directory, not a file".to_owned());
            }
            let rel = stripped.to_string_lossy().replace('\\', "/");
            (resolved, wt, rel)
        }
    };

    enqueue_request(
        state_dir,
        cmd.kind,
        worktree_root,
        resolved_file,
        relative_path,
        line,
        column,
        cmd.staged,
        cmd.focus,
    )
    .map_err(|e| e.to_string())
}

#[expect(
    clippy::too_many_arguments,
    reason = "CLI 요청 큐 엔트리의 직렬화 필드들을 그대로 전달"
)]
pub fn enqueue_request(
    state_dir: &Path,
    kind: FileActionKind,
    worktree: PathBuf,
    path: PathBuf,
    relative_path: String,
    line: Option<u32>,
    column: Option<u32>,
    staged: bool,
    focus: bool,
) -> io::Result<FileRequest> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state_dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(state_dir.join(FILE_REQUESTS_LOCK))?;
    lock.lock()?;

    let file = state_dir.join(FILE_REQUESTS_FILE);
    let mut queue = load_queue(&file)?;
    let id = queue.next_id.saturating_add(1);
    queue.next_id = id;
    let req = FileRequest {
        id,
        kind,
        worktree,
        path,
        relative_path,
        line,
        column,
        staged,
        focus,
        created_at_ms: now_ms(),
    };
    queue.requests.push(req.clone());
    if queue.requests.len() > MAX_QUEUE_LEN {
        let drop_count = queue.requests.len().saturating_sub(MAX_QUEUE_LEN);
        queue.requests.drain(0..drop_count);
    }
    save_queue(&file, &queue)?;
    Ok(req)
}

fn load_queue(file: &Path) -> io::Result<FileRequestQueue> {
    let mut bytes = Vec::new();
    match fs::File::open(file) {
        Ok(f) => {
            f.take(MAX_FILE_BYTES.saturating_add(1))
                .read_to_end(&mut bytes)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(FileRequestQueue::default()),
        Err(e) => return Err(e),
    }
    if u64::try_from(bytes.len()).map_or(true, |n| n > MAX_FILE_BYTES) {
        return Ok(FileRequestQueue::default());
    }
    Ok(serde_json::from_slice(&bytes).unwrap_or_default())
}

fn save_queue(file: &Path, queue: &FileRequestQueue) -> io::Result<()> {
    let body = serde_json::to_vec_pretty(queue).map_err(io::Error::other)?;
    let tmp = file.with_extension("json.tmp");
    let mut out = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    out.write_all(&body)?;
    drop(out);
    fs::rename(tmp, file)
}

fn print_result(req: &FileRequest, json: bool) {
    if json {
        let payload = match req.kind {
            FileActionKind::Open => serde_json::json!({
                "opened": true,
                "worktreePath": req.worktree.to_string_lossy(),
                "filePath": req.path.to_string_lossy(),
                "relativePath": req.relative_path,
                "line": req.line,
                "column": req.column,
            }),
            FileActionKind::Diff => serde_json::json!({
                "opened": true,
                "worktreePath": req.worktree.to_string_lossy(),
                "filePath": req.path.to_string_lossy(),
                "relativePath": req.relative_path,
                "staged": req.staged,
            }),
        };
        if let Ok(text) = serde_json::to_string_pretty(&payload) {
            println!("{text}");
        }
        return;
    }
    match req.kind {
        FileActionKind::Open => {
            let loc = match (req.line, req.column) {
                (Some(l), Some(c)) => format!(":{l}:{c}"),
                (Some(l), None) => format!(":{l}"),
                (None, _) => String::new(),
            };
            println!(
                "Opened {}{} in {}",
                req.relative_path,
                loc,
                req.worktree.display()
            );
        }
        FileActionKind::Diff => {
            let prefix = if req.staged { "staged diff" } else { "diff" };
            println!(
                "Opened {} for {} in {}",
                prefix,
                req.relative_path,
                req.worktree.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_returns_zero() {
        assert_eq!(run(&[OsString::from("--help")]), ExitCode::SUCCESS);
        assert_eq!(
            run(&[OsString::from("open"), OsString::from("--help")]),
            ExitCode::SUCCESS
        );
        assert_eq!(
            run(&[OsString::from("diff"), OsString::from("-h")]),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn parse_open_and_diff_arguments() {
        let open = parse(&[
            OsString::from("open"),
            OsString::from("src/lib.rs"),
            OsString::from("--line"),
            OsString::from("42"),
            OsString::from("--column=7"),
            OsString::from("--focus"),
            OsString::from("--json"),
        ])
        .unwrap();
        assert_eq!(open.kind, FileActionKind::Open);
        assert_eq!(open.path, "src/lib.rs");
        assert_eq!(open.line, Some(42));
        assert_eq!(open.column, Some(7));
        assert!(open.focus);
        assert!(open.json);

        let diff = parse(&[
            OsString::from("diff"),
            OsString::from("--path"),
            OsString::from("src/main.rs"),
            OsString::from("--staged"),
            OsString::from("--worktree=/repo/wt"),
        ])
        .unwrap();
        assert_eq!(diff.kind, FileActionKind::Diff);
        assert_eq!(diff.path, "src/main.rs");
        assert!(diff.staged);
        assert_eq!(diff.worktree, Some(PathBuf::from("/repo/wt")));

        // --column requires --line
        assert!(
            parse(&[
                OsString::from("open"),
                OsString::from("a.rs"),
                OsString::from("--column"),
                OsString::from("3"),
            ])
            .is_err()
        );
        // --staged rejected on open
        assert!(
            parse(&[
                OsString::from("open"),
                OsString::from("a.rs"),
                OsString::from("--staged"),
            ])
            .is_err()
        );
        // --line rejected on diff
        assert!(
            parse(&[
                OsString::from("diff"),
                OsString::from("a.rs"),
                OsString::from("--line"),
                OsString::from("1"),
            ])
            .is_err()
        );
    }

    #[test]
    fn execute_open_and_diff_writes_request_queue() {
        let tmp = std::env::temp_dir().join(format!("krill-file-cli-test-{}", std::process::id()));
        fs::remove_dir_all(&tmp).ok();
        let state_dir = tmp.join("state");
        let repo = tmp.join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("src")).unwrap();
        let file_path = repo.join("src/hello.rs");
        fs::write(&file_path, "fn main() {}\n").unwrap();

        let open_cmd =
            parse(&[OsString::from("open"), OsString::from("src/hello.rs:12:4")]).unwrap();
        let req1 = execute(&state_dir, &repo, &open_cmd).unwrap();
        assert_eq!(req1.kind, FileActionKind::Open);
        assert_eq!(req1.relative_path, "src/hello.rs");
        assert_eq!(req1.line, Some(12));
        assert_eq!(req1.column, Some(4));

        // Diff works even for a deleted file path inside the worktree
        let diff_cmd = parse(&[
            OsString::from("diff"),
            OsString::from("src/deleted.rs"),
            OsString::from("--staged"),
        ])
        .unwrap();
        let req2 = execute(&state_dir, &repo, &diff_cmd).unwrap();
        assert_eq!(req2.kind, FileActionKind::Diff);
        assert_eq!(req2.relative_path, "src/deleted.rs");
        assert!(req2.staged);

        let queue = load_queue(&state_dir.join(FILE_REQUESTS_FILE)).unwrap();
        assert_eq!(queue.requests.len(), 2);
        assert_eq!(queue.requests[0].id, 1);
        assert_eq!(queue.requests[1].id, 2);

        fs::remove_dir_all(&tmp).ok();
    }
}
