//! `krill set --comment <text> [--worktree <path>]`(MK-093): 워크트리 코멘트를 상태 디렉터리에 저장한다.
//! 사이드바(`krill-app::sidebar`)가 같은 파일을 읽어 워크트리 행에 표시한다. 빈 문자열은 코멘트를 지운다.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use krill_proto::daemon;
use serde::{Deserialize, Serialize};

pub const COMMENTS_FILE: &str = "worktree-comments.toml";
const LOCK_FILE: &str = "worktree-comments.lock";
const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_COMMENTS: usize = 1_024;
const MAX_COMMENT_CHARS: usize = 256;

const USAGE: &str = "usage: krill set [--comment <text>] [--workspace-status <todo|in-progress|in-review|completed|none>] [--unread|--read] [--pr <number>] [--issue <key>] [--reference <url>] [--parent <path>|--clear-parent] [--worktree <path>] [--session <id>] [--json]";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    comments: BTreeMap<String, String>,
    /// 코멘트별 마지막 기록 시각(UNIX 초). 상한 도달 시 사전순이 아닌 가장 오래된 항목을
    /// 지우기 위한 기록이며, 구 파일에는 없어 0으로 읽혀 가장 먼저 지워진다.
    #[serde(default)]
    updated_at: BTreeMap<String, u64>,
    #[serde(default)]
    statuses: BTreeMap<String, String>,
    #[serde(default)]
    unread: BTreeMap<String, bool>,
    #[serde(default)]
    prs: BTreeMap<String, u64>,
    #[serde(default)]
    issues: BTreeMap<String, String>,
    #[serde(default)]
    references: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    symlinks: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    setup_scripts: BTreeMap<String, String>,
    #[serde(default)]
    archive_scripts: BTreeMap<String, String>,
    #[serde(default)]
    setup_prompt_dismissed: BTreeMap<String, bool>,
    #[serde(default)]
    parents: BTreeMap<String, String>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 상한 도달 시 가장 오래된 기록(시각 없음은 0, 동점은 경로 사전순)의 키를 고른다.
fn oldest_key(store: &Store) -> Option<String> {
    store
        .comments
        .keys()
        .min_by(|a, b| {
            let ta = store.updated_at.get(*a).copied().unwrap_or(0);
            let tb = store.updated_at.get(*b).copied().unwrap_or(0);
            ta.cmp(&tb).then_with(|| a.cmp(b))
        })
        .cloned()
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    worktree: Option<PathBuf>,
    session: Option<u64>,
    comment: String,
    has_comment: bool,
    workspace_status: Option<String>,
    unread: Option<bool>,
    pr: Option<u64>,
    issue: Option<String>,
    reference: Option<String>,
    parent: Option<String>,
    json: bool,
}

pub fn run(args: &[OsString]) -> ExitCode {
    if args
        .first()
        .is_some_and(|a| matches!(a.to_str(), Some("-h" | "--help" | "help")))
    {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Some(state) = daemon::state_dir() else {
        eprintln!("set: HOME is not set");
        return ExitCode::FAILURE;
    };
    let dir = match (parsed.worktree.clone(), parsed.session) {
        (Some(path), _) => Ok(path),
        (None, Some(sid)) => crate::session::resolve_session_cwd(sid),
        (None, None) => std::env::current_dir(),
    };
    let result = dir.and_then(|d| {
        let root = resolve_worktree(&d);
        apply_checkpoint(&state, &root, &parsed)
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("set: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse_session_id(val: &str) -> Result<u64, String> {
    val.strip_prefix('s')
        .unwrap_or(val)
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| "invalid --session id".to_owned())
}

fn parse_parent_arg(val: &str) -> String {
    let trimmed = val.trim();
    if matches!(trimmed, "" | "none" | "clear") {
        String::new()
    } else {
        resolve_worktree(Path::new(trimmed))
            .to_string_lossy()
            .into_owned()
    }
}

fn parse(args: &[OsString]) -> Result<Args, String> {
    let mut it = args.iter();
    let mut out = Args::default();
    while let Some(raw) = it.next() {
        let arg = utf8(raw)?;
        if let Some(val) = arg.strip_prefix("--comment=") {
            out.comment = sanitize(val);
            out.has_comment = true;
        } else if let Some(val) = arg
            .strip_prefix("--worktree=")
            .or_else(|| arg.strip_prefix("--cwd="))
        {
            out.worktree = parse_worktree(val);
        } else if let Some(val) = arg.strip_prefix("--session=") {
            out.session = Some(parse_session_id(val)?);
        } else if let Some(val) = arg.strip_prefix("--workspace-status=") {
            out.workspace_status = Some(validate_status(val)?);
        } else if let Some(val) = arg.strip_prefix("--pr=") {
            out.pr = Some(
                val.parse::<u64>()
                    .map_err(|_| "invalid --pr number".to_owned())?,
            );
        } else if let Some(val) = arg.strip_prefix("--issue=") {
            out.issue = Some(sanitize(val));
        } else if let Some(val) = arg.strip_prefix("--reference=") {
            out.reference = Some(sanitize(val));
        } else if let Some(val) = arg.strip_prefix("--parent=") {
            out.parent = Some(parse_parent_arg(val));
        } else {
            match arg {
                "--comment" | "-m" => {
                    let val = utf8(it.next().ok_or("missing --comment value")?)?;
                    out.comment = sanitize(val);
                    out.has_comment = true;
                }
                "--worktree" | "--cwd" => {
                    let val = utf8(it.next().ok_or("missing --worktree value")?)?;
                    out.worktree = parse_worktree(val);
                }
                "--session" => {
                    // Checkpoints belong to the selected worktree, including session callers.
                    let val = utf8(it.next().ok_or("missing --session value")?)?;
                    out.session = Some(parse_session_id(val)?);
                }
                "--clear-comment" => {
                    out.comment = String::new();
                    out.has_comment = true;
                }
                "--workspace-status" | "--status" => {
                    let val = utf8(it.next().ok_or("missing --workspace-status value")?)?;
                    out.workspace_status = Some(validate_status(val)?);
                }
                "--unread" => out.unread = Some(true),
                "--read" => out.unread = Some(false),
                "--pr" => {
                    let val = utf8(it.next().ok_or("missing --pr value")?)?;
                    out.pr = Some(
                        val.parse::<u64>()
                            .map_err(|_| "invalid --pr number".to_owned())?,
                    );
                }
                "--issue" => {
                    let val = utf8(it.next().ok_or("missing --issue value")?)?;
                    out.issue = Some(sanitize(val));
                }
                "--reference" | "--ref" => {
                    let val = utf8(it.next().ok_or("missing --reference value")?)?;
                    out.reference = Some(sanitize(val));
                }
                "--parent" => {
                    let val = utf8(it.next().ok_or("missing --parent value")?)?;
                    out.parent = Some(parse_parent_arg(val));
                }
                "--clear-parent" => {
                    out.parent = Some(String::new());
                }
                "--json" => out.json = true,
                _ => return Err(format!("unexpected argument {arg:?}")),
            }
        }
    }
    if !out.has_comment
        && out.workspace_status.is_none()
        && out.unread.is_none()
        && out.pr.is_none()
        && out.issue.is_none()
        && out.reference.is_none()
        && out.parent.is_none()
    {
        return Err("missing --comment".to_owned());
    }
    Ok(out)
}

fn validate_status(raw: &str) -> Result<String, String> {
    match raw.trim() {
        "" | "none" | "clear" => Ok(String::new()),
        "todo" => Ok("todo".to_owned()),
        "in-progress" | "in_progress" | "wip" => Ok("in-progress".to_owned()),
        "in-review" | "in_review" | "review" => Ok("in-review".to_owned()),
        "completed" | "done" => Ok("completed".to_owned()),
        _ => Err("workspace status must be todo|in-progress|in-review|completed|none".to_owned()),
    }
}

fn parse_worktree(val: &str) -> Option<PathBuf> {
    if matches!(val, "" | "." | "active") {
        None
    } else {
        Some(PathBuf::from(val))
    }
}

fn utf8(s: &OsStr) -> Result<&str, String> {
    s.to_str()
        .ok_or_else(|| "arguments must be UTF-8".to_owned())
}

/// 줄바꿈은 공백으로 바꾸고 제어문자를 뺀 한 줄 요약으로 정리한다.
pub fn sanitize(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !c.is_control())
        .collect();
    mapped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_COMMENT_CHARS)
        .collect()
}

/// 하위 폴더에서 실행해도 `.git`이 있는 가장 가까운 조상 워크트리 루트로 맞춘다.
pub fn resolve_worktree(path: &Path) -> PathBuf {
    PathBuf::from(krill_proto::orchestration::worktree_key(path))
}

#[cfg(test)]
pub fn save_comment(state_dir: &Path, worktree: &Path, comment: &str) -> io::Result<()> {
    let args = Args {
        worktree: Some(worktree.to_path_buf()),
        comment: comment.to_owned(),
        has_comment: true,
        ..Args::default()
    };
    apply_checkpoint(state_dir, worktree, &args)
}

fn apply_checkpoint(state_dir: &Path, worktree: &Path, args: &Args) -> io::Result<()> {
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
        .open(state_dir.join(LOCK_FILE))?;
    lock.lock()?;
    let file = state_dir.join(COMMENTS_FILE);
    let mut store = load_store(&file)?;
    let key = worktree.to_string_lossy().into_owned();
    let now = now_secs();

    if args.has_comment {
        let clean = sanitize(&args.comment);
        if clean.is_empty() {
            store.comments.remove(&key);
            store.updated_at.remove(&key);
        } else {
            if store.comments.len() >= MAX_COMMENTS
                && !store.comments.contains_key(&key)
                && let Some(oldest) = oldest_key(&store)
            {
                store.comments.remove(&oldest);
                store.updated_at.remove(&oldest);
            }
            store.comments.insert(key.clone(), clean);
            store.updated_at.insert(key.clone(), now);
        }
    }
    if let Some(ref status) = args.workspace_status {
        if status.is_empty() {
            store.statuses.remove(&key);
        } else {
            store.statuses.insert(key.clone(), status.clone());
            store.updated_at.insert(key.clone(), now);
        }
    }
    if let Some(unread) = args.unread {
        if unread {
            store.unread.insert(key.clone(), true);
        } else {
            store.unread.remove(&key);
        }
        store.updated_at.insert(key.clone(), now);
    }
    if let Some(pr) = args.pr {
        if pr == 0 {
            store.prs.remove(&key);
        } else {
            store.prs.insert(key.clone(), pr);
        }
        store.updated_at.insert(key.clone(), now);
    }
    if let Some(ref issue) = args.issue {
        if issue.is_empty() {
            store.issues.remove(&key);
        } else {
            store.issues.insert(key.clone(), issue.clone());
        }
        store.updated_at.insert(key.clone(), now);
    }
    if let Some(ref r) = args.reference
        && !r.is_empty()
    {
        let list = store.references.entry(key.clone()).or_default();
        if !list.iter().any(|u| u == r) {
            list.push(r.clone());
        }
        store.updated_at.insert(key.clone(), now);
    }
    if let Some(ref parent) = args.parent {
        if parent.is_empty() {
            store.parents.remove(&key);
            store.updated_at.insert(key.clone(), now);
        } else {
            if !can_assign_parent(&store.parents, &key, parent) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot assign worktree as its own parent or create a lineage cycle",
                ));
            }
            store.parents.insert(key.clone(), parent.clone());
            store.updated_at.insert(key.clone(), now);
        }
    }

    let body = krill_proto::config_document::save_toml(&file, &store)?;
    let tmp = file.with_extension("toml.tmp");
    let mut out = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    out.write_all(body.as_bytes())?;
    drop(out);
    fs::rename(tmp, file)?;

    if args.json {
        let payload = serde_json::json!({
            "worktree": key,
            "comment": store.comments.get(&key),
            "workspace_status": store.statuses.get(&key),
            "unread": store.unread.get(&key).copied().unwrap_or(false),
            "pr": store.prs.get(&key),
            "issue": store.issues.get(&key),
            "references": store.references.get(&key).cloned().unwrap_or_default(),
            "parent": store.parents.get(&key),
        });
        let text = serde_json::to_string_pretty(&payload).map_err(io::Error::other)?;
        println!("{text}");
    }
    Ok(())
}

fn can_assign_parent(
    parents: &BTreeMap<String, String>,
    child: &str,
    candidate_parent: &str,
) -> bool {
    if child.is_empty() || candidate_parent.is_empty() || child == candidate_parent {
        return false;
    }
    let mut cursor = Some(candidate_parent);
    let mut visited = std::collections::BTreeSet::new();
    while let Some(cur) = cursor {
        if cur == child {
            return false;
        }
        if !visited.insert(cur) {
            return false;
        }
        cursor = parents.get(cur).map(String::as_str);
    }
    true
}

fn load_store(path: &Path) -> io::Result<Store> {
    let mut bytes = Vec::new();
    match fs::File::open(path) {
        Ok(f) => {
            f.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Store::default()),
        Err(e) => return Err(e),
    }
    if u64::try_from(bytes.len()).map_or(true, |n| n > MAX_FILE_BYTES) {
        return corrupt_store(path, "comments file exceeds size limit");
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return corrupt_store(path, "comments file is not UTF-8");
    };
    match toml::from_str(text) {
        Ok(store) => Ok(store),
        Err(_) => corrupt_store(path, "invalid comments TOML"),
    }
}

/// 원본과 최초 손상 사본을 덮어쓰지 않고 저장을 거부한다.
fn corrupt_store(path: &Path, reason: &str) -> io::Result<Store> {
    let backup = path.with_extension("toml.corrupt");
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(backup)
    {
        Ok(mut out) => {
            io::copy(&mut fs::File::open(path)?, &mut out)?;
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_session_hint() {
        for flag in [vec!["--session", "1"], vec!["--session=1"]] {
            let raw: Vec<_> = flag
                .into_iter()
                .chain(["--workspace-status", "in-progress"])
                .map(OsString::from)
                .collect();
            let args = parse(&raw).unwrap();
            assert_eq!(args.session, Some(1));
            assert_eq!(args.workspace_status.as_deref(), Some("in-progress"));
        }
        assert!(parse(&[OsString::from("--session")]).is_err());
        assert!(parse(&[OsString::from("--session=invalid")]).is_err());
    }

    #[test]
    fn apply_checkpoint_round_trip() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-set-checkpoint-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let worktree = Path::new("/repo/checkpoint");
        let key = worktree.to_string_lossy();
        let args = Args {
            session: Some(1),
            comment: "Ready for review".to_owned(),
            has_comment: true,
            workspace_status: Some("in-review".to_owned()),
            unread: Some(true),
            pr: Some(128),
            issue: Some("OWE-626".to_owned()),
            reference: Some("https://example.com/pr/128".to_owned()),
            parent: Some("/repo/main".to_owned()),
            ..Args::default()
        };
        apply_checkpoint(&dir, worktree, &args)?;
        apply_checkpoint(&dir, worktree, &args)?;
        let store = load_store(&dir.join(COMMENTS_FILE))?;
        assert_eq!(store.comments.get(key.as_ref()), Some(&args.comment));
        assert_eq!(
            store.statuses.get(key.as_ref()),
            args.workspace_status.as_ref()
        );
        assert_eq!(store.unread.get(key.as_ref()), Some(&true));
        assert_eq!(store.prs.get(key.as_ref()), Some(&128));
        assert_eq!(store.issues.get(key.as_ref()), args.issue.as_ref());
        assert_eq!(
            store.references.get(key.as_ref()),
            Some(&vec!["https://example.com/pr/128".to_owned()])
        );
        assert_eq!(
            store.parents.get(key.as_ref()).map(String::as_str),
            Some("/repo/main")
        );
        let shared = krill_proto::orchestration::WorktreeStore::load(&dir)?;
        assert_eq!(
            shared.checkpoint(key.as_ref()).workspace_status,
            args.workspace_status
        );
        assert_eq!(
            shared.checkpoint(key.as_ref()).parent.as_deref(),
            Some("/repo/main")
        );

        // Reject cycle (/repo/main -> /repo/checkpoint)
        assert!(
            apply_checkpoint(
                &dir,
                Path::new("/repo/main"),
                &Args {
                    parent: Some("/repo/checkpoint".to_owned()),
                    ..Args::default()
                },
            )
            .is_err()
        );

        apply_checkpoint(
            &dir,
            worktree,
            &Args {
                has_comment: true,
                workspace_status: Some(String::new()),
                unread: Some(false),
                pr: Some(0),
                issue: Some(String::new()),
                parent: Some(String::new()),
                ..Args::default()
            },
        )?;
        let store = load_store(&dir.join(COMMENTS_FILE))?;
        assert!(!store.comments.contains_key(key.as_ref()));
        assert!(!store.statuses.contains_key(key.as_ref()));
        assert!(!store.unread.contains_key(key.as_ref()));
        assert!(!store.prs.contains_key(key.as_ref()));
        assert!(!store.issues.contains_key(key.as_ref()));
        assert!(!store.parents.contains_key(key.as_ref()));
        assert!(store.references.contains_key(key.as_ref()));
        fs::remove_dir_all(dir)
    }

    #[test]
    fn parse_and_sanitize_comments() {
        let args = [
            OsString::from("--comment"),
            OsString::from("  첫 줄\n둘째\x07 줄  "),
            OsString::from("--worktree"),
            OsString::from("active"),
        ];
        let parsed = parse(&args).unwrap();
        assert_eq!(parsed.worktree, None);
        assert_eq!(parsed.comment, "첫 줄 둘째 줄");

        let inline = [
            OsString::from("--worktree=/repo/wt"),
            OsString::from("--comment=진행 중"),
        ];
        let parsed = parse(&inline).unwrap();
        assert_eq!(parsed.worktree, Some(PathBuf::from("/repo/wt")));
        assert_eq!(parsed.comment, "진행 중");
    }

    #[test]
    fn parse_checkpoints_and_references() {
        let args = [
            OsString::from("--workspace-status"),
            OsString::from("in-review"),
            OsString::from("--unread"),
            OsString::from("--pr"),
            OsString::from("128"),
            OsString::from("--issue"),
            OsString::from("OWE-626"),
            OsString::from("--reference"),
            OsString::from("https://example.com/pr/128"),
            OsString::from("--json"),
        ];
        let parsed = parse(&args).unwrap();
        assert_eq!(parsed.workspace_status.as_deref(), Some("in-review"));
        assert_eq!(parsed.unread, Some(true));
        assert_eq!(parsed.pr, Some(128));
        assert_eq!(parsed.issue.as_deref(), Some("OWE-626"));
        assert_eq!(
            parsed.reference.as_deref(),
            Some("https://example.com/pr/128")
        );
        assert!(parsed.json);
    }

    #[test]
    fn eviction_uses_oldest_timestamp_not_lexicographic() {
        assert_eq!(MAX_COMMENTS, 1_024);
        let store = Store {
            comments: ["zzz", "aaa", "mmm"]
                .into_iter()
                .map(|k| (k.to_owned(), "c".to_owned()))
                .collect(),
            updated_at: [("zzz", 30), ("aaa", 10), ("mmm", 20)]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            ..Store::default()
        };
        let store2 = Store {
            comments: ["aaa", "zzz"]
                .into_iter()
                .map(|k| (k.to_owned(), "c".to_owned()))
                .collect(),
            updated_at: [("aaa", 99), ("zzz", 1)]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            ..Store::default()
        };
        assert_eq!(oldest_key(&store2).as_deref(), Some("zzz"));
        let store3 = Store {
            comments: ["new", "old"]
                .into_iter()
                .map(|k| (k.to_owned(), "c".to_owned()))
                .collect(),
            updated_at: [("new", now_secs())]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            ..Store::default()
        };
        assert_eq!(oldest_key(&store3).as_deref(), Some("old"));
        assert_eq!(oldest_key(&store).as_deref(), Some("aaa"));
    }

    #[test]
    fn save_comment_evicts_oldest_and_records_time() {
        let dir = std::env::temp_dir().join(format!("mk-set-evict-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let seed = Store {
            comments: ["zzz", "aaa"]
                .into_iter()
                .map(|k| (k.to_owned(), "old".to_owned()))
                .collect(),
            updated_at: [("zzz", 1), ("aaa", 2)]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            ..Store::default()
        };
        std::fs::write(dir.join(COMMENTS_FILE), toml::to_string(&seed).unwrap()).unwrap();
        save_comment(&dir, Path::new("/repo/new"), "새 코멘트").unwrap();
        let raw = std::fs::read_to_string(dir.join(COMMENTS_FILE)).unwrap();
        let back: Store = toml::from_str(&raw).unwrap();
        assert!(back.comments.contains_key("/repo/new"));
        assert!(back.updated_at.contains_key("/repo/new"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
#[test]
fn as12_cli_comment_save_preserves_future_fields() {
    let dir = std::env::temp_dir().join(format!("as12-cli-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(COMMENTS_FILE);
    fs::write(
        &path,
        "# keep\nfuture = { mode = 'v2' }\n[comments]\n'/work' = 'old'\n",
    )
    .unwrap();
    save_comment(&dir, Path::new("/work"), "edited").unwrap();
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.contains("# keep"));
    let value: toml::Value = toml::from_str(&body).unwrap();
    assert_eq!(value["future"]["mode"].as_str(), Some("v2"));
    assert_eq!(value["comments"]["/work"].as_str(), Some("edited"));
    fs::remove_dir_all(dir).unwrap();
}
