//! `krill worktree new|rm`: 생성·삭제는 krilld에 맡긴다. setup은 내용을 보여 주고 해시를 승인받은 뒤에만 실행한다.

mod cleanup;
mod remove;
mod space;

use krill_proto::os::ffi::OsStrExt;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use krill_proto::control::{
    FEAT_CONTROL, FEAT_WORKTREE, NewWorktree, Reply, Request, Setup, Worktree,
};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg;

use crate::remote::Target;

const USAGE: &str = "usage: krill worktree new <name> [--host <user@host>] [--repo <path>] [--parent <path>] [--base <ref>] [--setup run|skip] [--trust-setup <hash>]\n       krill worktree rm <path> [--host <user@host>] [--repo <path>] [--force] [--trust-archive <hash>]\n       krill worktree list [--repo <path>] [--json]\n       krill worktree show [<path>] [--json]\n       krill worktree current [--json]\n       krill worktree ps [--worktree <path>] [--json]\n       krill worktree cleanup [--repo <path>] [--pr merged|closed|merged-or-closed|any] [--merged] [--closed] [--idle-days <7|14|30|N>] [--exclude-dirty|--include-dirty|--force] [--dry-run] [--yes] [--json]\n       krill worktree space [--repo <path>] [--json]\n       krill worktree set [--comment <text>] [--workspace-status <todo|in-progress|in-review|completed|none>] [--unread|--read] [--pr <number>] [--issue <key>] [--reference <url>] [--parent <path>|--clear-parent] [--worktree <path>] [--session <id>] [--json]\n       krill worktree symlink <list|add|rm> [<path>] [--repo <path>] [--json]\n       krill worktree hooks [--repo <path>] [--setup <cmd>] [--archive <cmd>] [--clear-setup] [--clear-archive] [--trust <hash>] [--json]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedNew {
    req: NewWorktree,
    parent: Option<PathBuf>,
}

pub fn run(raw_args: &[OsString]) -> ExitCode {
    let (host, args_vec) = match crate::remote::extract_host(raw_args) {
        Ok(parsed) => parsed,
        Err(text) => {
            eprintln!("{text}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let args = args_vec.as_slice();
    // 서브커맨드 자리(`worktree --help`, `worktree new --help`)만 도움말로 본다.
    // `new <name>` 이름 자리의 `help`는 워크트리 이름이다.
    let help_here = matches!(
        args.first().and_then(|a| a.to_str()),
        Some("-h" | "--help" | "help")
    ) || matches!(args.get(1).and_then(|a| a.to_str()), Some("-h" | "--help"));
    if help_here {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let target = match Target::establish(host) {
        Ok(target) => target,
        Err(e) => {
            eprintln!("remote: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args.first().is_some_and(|a| a == "rm") {
        return remove::run(&target, &args[1..]);
    }
    if args.first().is_some_and(|a| a == "list" || a == "ls") {
        return run_list(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "show") {
        return run_show(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "current") {
        return run_current(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "ps") {
        return run_ps(&target, &args[1..]);
    }
    if args.first().is_some_and(|a| a == "cleanup" || a == "clean") {
        return cleanup::run_cleanup(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "space") {
        return cleanup::run_space(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "set") {
        return crate::set::run(&args[1..]);
    }
    if args
        .first()
        .is_some_and(|a| a == "symlink" || a == "symlinks")
    {
        return run_symlink(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "hooks" || a == "hook") {
        return run_hooks(&args[1..]);
    }
    let parsed = match parse_with_remote(args, target.is_remote()) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match create_new_on_target(&target, parsed) {
        Ok(w) => {
            let created_path = PathBuf::from(OsStr::from_bytes(&w.path));
            println!("{}", created_path.display());
            println!("branch {} (base {})", w.branch, w.base);
            if let Some(id) = w.setup {
                println!("setup session {}", id.0);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("worktree: {e}");
            ExitCode::FAILURE
        }
    }
}

/// CLI와 worker-start가 같은 생성·부모 기록 경로를 사용한다.
pub(super) fn create_new_on_target(target: &Target, parsed: ParsedNew) -> Result<Worktree, String> {
    let w = create_on_target(target, parsed.req)?;
    let created_path = PathBuf::from(OsStr::from_bytes(&w.path));
    if !target.is_remote()
        && let Some(parent_path) = parsed.parent
        && let Some(state_dir) = daemon::state_dir()
        && let Err(e) = krill_proto::orchestration::WorktreeStore::mutate(&state_dir, |store| {
            store.set_parent(
                &created_path.to_string_lossy(),
                Some(&parent_path.to_string_lossy()),
            )
        })
    {
        eprintln!("warning: could not record parent worktree: {e}");
    }
    Ok(w)
}

pub(super) fn parse_new_child(
    parent: &std::path::Path,
    name: &str,
    options: &[OsString],
) -> Result<ParsedNew, String> {
    let mut args = vec![
        OsString::from("new"),
        OsString::from("--name"),
        OsString::from(name),
        OsString::from("--parent"),
        parent.as_os_str().to_owned(),
    ];
    args.extend_from_slice(options);
    parse_with_remote(&args, false)
}

#[cfg(test)]
fn parse(args: &[OsString]) -> Result<ParsedNew, String> {
    parse_with_remote(args, false)
}

fn parse_with_remote(args: &[OsString], remote: bool) -> Result<ParsedNew, String> {
    let mut args = args.iter();
    if args.next().is_none_or(|a| a != "new") {
        return Err("expected 'new'".to_owned());
    }
    let mut repo = None;
    let mut parent = None;
    let mut name = None;
    let mut base = None;
    let mut setup = Setup::Ask;
    while let Some(arg) = args.next() {
        let flag = utf8(arg)?;
        if let Some(val) = flag.strip_prefix("--parent=") {
            if val.trim().is_empty() {
                return Err("missing --parent value".to_owned());
            }
            parent = Some(PathBuf::from(val.trim()));
            continue;
        }
        if let Some(val) = flag.strip_prefix("--repo=") {
            if val.trim().is_empty() {
                return Err("missing --repo value".to_owned());
            }
            repo = Some(PathBuf::from(val.trim()));
            continue;
        }
        match flag {
            "--repo" => repo = Some(PathBuf::from(args.next().ok_or("missing --repo value")?)),
            "--parent" => {
                parent = Some(PathBuf::from(args.next().ok_or("missing --parent value")?));
            }
            "--name" => name = Some(utf8(args.next().ok_or("missing --name value")?)?.to_owned()),
            "--base" => base = Some(utf8(args.next().ok_or("missing --base value")?)?.to_owned()),
            "--setup" => {
                setup = match utf8(args.next().ok_or("missing --setup value")?)? {
                    "run" | "ask" => Setup::Ask,
                    "skip" => Setup::Skip,
                    _ => return Err("--setup must be run or skip".to_owned()),
                };
            }
            "--trust-setup" => {
                let hash = utf8(args.next().ok_or("missing --trust-setup value")?)?;
                if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("--trust-setup requires a 64-digit SHA-256 hash".to_owned());
                }
                let bytes = hash
                    .as_bytes()
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| {
                        let digit = |b: u8| char::from(b).to_digit(16).unwrap_or(0);
                        u8::try_from(digit(pair[0]) * 16 + digit(pair[1])).unwrap_or(0)
                    })
                    .collect();
                setup = Setup::Approve(bytes);
            }
            "--" => {
                name = Some(utf8(args.next().ok_or("missing name")?)?.to_owned());
                if args.next().is_some() {
                    return Err("unexpected arguments after name".to_owned());
                }
            }
            s if !s.starts_with('-') && name.is_none() => name = Some(s.to_owned()),
            _ => return Err(format!("unexpected argument {flag:?}")),
        }
    }
    if remote {
        let repo_path = repo.or(parent).ok_or_else(|| {
            "remote worktree new requires --repo <absolute path on the remote host>".to_owned()
        })?;
        if !repo_path.as_os_str().as_bytes().starts_with(b"/") {
            return Err("remote --repo must be an absolute path on the remote host".to_owned());
        }
        return Ok(ParsedNew {
            req: NewWorktree {
                repo: repo_path.as_os_str().as_bytes().to_vec(),
                name: name.ok_or("missing worktree name")?,
                base,
                setup,
            },
            parent: None,
        });
    }
    let resolved_parent = match parent {
        Some(p) => {
            let canon = std::fs::canonicalize(&p)
                .map_err(|e| format!("cannot resolve parent worktree: {e}"))?;
            Some(crate::set::resolve_worktree(&canon))
        }
        None => None,
    };
    let repo = repo
        .or_else(|| resolved_parent.clone())
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|e| e.to_string())?;
    // 데몬의 cwd와 다르므로 요청 전에 절대 경로로 고정한다.
    let repo =
        std::fs::canonicalize(repo).map_err(|e| format!("cannot resolve repository: {e}"))?;
    let base = base.or_else(|| resolved_parent.as_deref().and_then(parent_branch));
    Ok(ParsedNew {
        req: NewWorktree {
            repo: repo.as_os_str().as_bytes().to_vec(),
            name: name.ok_or("missing worktree name")?,
            base,
            setup,
        },
        parent: resolved_parent,
    })
}

/// 명시한 기준 ref가 없을 때만 부모의 현재 브랜치를 쓴다. detached HEAD·조회 실패는 데몬 기본값으로 폴백한다.
fn parent_branch(parent: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(parent)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8(output.stdout).ok()?;
    let branch = branch.trim();
    (!branch.is_empty()).then(|| branch.to_owned())
}

fn utf8(s: &OsStr) -> Result<&str, String> {
    s.to_str()
        .ok_or_else(|| "name and options must be UTF-8".to_owned())
}

pub(super) fn create_on_target(target: &Target, mut req: NewWorktree) -> Result<Worktree, String> {
    let mut conn = target
        .connect(&daemon::client_hello(ClientRole::Cli, "krill worktree new"))
        .map_err(|e| e.to_string())?;
    let required = FEAT_CONTROL | FEAT_WORKTREE;
    if conn.negotiated.features & required != required {
        return Err("krilld does not support worktree creation; update the daemon".to_owned());
    }
    loop {
        conn.stream
            .write_all(
                &Request::NewWorktree(req.clone())
                    .encode()
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        let (kind, body) = ipc::read_frame(&mut conn.stream)
            .map_err(|e| e.to_string())?
            .ok_or("krilld disconnected; inspect git worktree list before retrying")?;
        match Reply::decode(kind, &body).map_err(|e| e.to_string())? {
            Reply::WorktreeCreated(w) => {
                for warning in &w.warnings {
                    eprintln!("warning: {warning}");
                }
                return Ok(w);
            }
            Reply::SetupUntrusted { script, hash } => {
                let hash_text = hex(&hash);
                eprintln!("Setup script for this repository:\n{script}\nSHA-256: {hash_text}");
                if !io::stdin().is_terminal() {
                    return Err(format!(
                        "setup approval required; review the script and retry with --trust-setup {hash_text}, or use --setup skip"
                    ));
                }
                eprint!("Trust and run this script? [y/N] ");
                io::stderr().flush().map_err(|e| e.to_string())?;
                let mut answer = String::new();
                io::stdin()
                    .read_line(&mut answer)
                    .map_err(|e| e.to_string())?;
                if !matches!(answer.trim(), "y" | "Y" | "yes") {
                    return Err(
                        "setup was not approved; use --setup skip to create without it".to_owned(),
                    );
                }
                req.setup = Setup::Approve(hash);
            }
            Reply::WorktreeFailed(e) => return Err(e),
            Reply::Failed(e) => return Err(e.to_string()),
            _ => return Err("unexpected daemon reply".to_owned()),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        write!(s, "{b:02x}").ok();
        s
    })
}

fn list_git_worktrees(repo: &std::path::Path) -> Result<Vec<(String, String, String)>, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|e| format!("git worktree list failed: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut items = Vec::new();
    let mut cur_path = String::new();
    let mut cur_head = String::new();
    let mut cur_branch = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            if !cur_path.is_empty() {
                items.push((
                    std::mem::take(&mut cur_path),
                    std::mem::take(&mut cur_branch),
                    std::mem::take(&mut cur_head),
                ));
            }
            rest.trim().clone_into(&mut cur_path);
        } else if let Some(rest) = line.strip_prefix("HEAD ") {
            cur_head = rest.trim().chars().take(8).collect();
        } else if let Some(rest) = line.strip_prefix("branch ") {
            rest.trim()
                .strip_prefix("refs/heads/")
                .unwrap_or(rest.trim())
                .clone_into(&mut cur_branch);
        } else if line.trim() == "detached" && cur_branch.is_empty() {
            "(detached)".clone_into(&mut cur_branch);
        }
    }
    if !cur_path.is_empty() {
        items.push((cur_path, cur_branch, cur_head));
    }
    Ok(items)
}

fn run_list(args: &[OsString]) -> ExitCode {
    let mut repo = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(raw) = it.next() {
        let Ok(arg) = utf8(raw) else {
            eprintln!("arguments must be UTF-8\n{USAGE}");
            return ExitCode::from(2);
        };
        if let Some(val) = arg.strip_prefix("--repo=") {
            repo = Some(PathBuf::from(val));
        } else {
            match arg {
                "--repo" => {
                    let Some(v) = it.next() else {
                        eprintln!("missing --repo value\n{USAGE}");
                        return ExitCode::from(2);
                    };
                    repo = Some(PathBuf::from(v));
                }
                "--json" => json = true,
                _ => {
                    eprintln!("unexpected argument {arg:?}\n{USAGE}");
                    return ExitCode::from(2);
                }
            }
        }
    }
    let root = match resolve_repo_arg(repo) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("worktree list: {e}");
            return ExitCode::FAILURE;
        }
    };
    let worktrees = match list_git_worktrees(&root) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("worktree list: {e}");
            return ExitCode::FAILURE;
        }
    };
    let store = daemon::state_dir()
        .and_then(|d| krill_proto::orchestration::WorktreeStore::load(&d).ok())
        .unwrap_or_default();
    if json {
        let rows: Vec<_> = worktrees
            .into_iter()
            .map(|(path, branch, head)| {
                let cp = store.checkpoint(&path);
                serde_json::json!({
                    "path": path,
                    "branch": branch,
                    "head": head,
                    "status": cp.workspace_status,
                    "comment": cp.comment,
                    "pr": cp.pr,
                    "issue": cp.issue,
                    "parent": cp.parent,
                    "unread": cp.unread,
                })
            })
            .collect();
        match serde_json::to_string_pretty(&rows) {
            Ok(out) => println!("{out}"),
            Err(e) => {
                eprintln!("worktree list: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        println!("PATH\tBRANCH\tHEAD\tSTATUS\tPR\tCOMMENT");
        for (path, branch, head) in worktrees {
            let cp = store.checkpoint(&path);
            println!(
                "{path}\t{branch}\t{head}\t{}\t{}\t{}",
                cp.workspace_status.as_deref().unwrap_or("-"),
                cp.pr.map_or_else(|| "-".to_owned(), |n| format!("#{n}")),
                cp.comment.as_deref().unwrap_or("-")
            );
        }
    }
    ExitCode::SUCCESS
}

fn run_current(args: &[OsString]) -> ExitCode {
    let mut json = false;
    for raw in args {
        let Ok(arg) = utf8(raw) else {
            eprintln!("arguments must be UTF-8\n{USAGE}");
            return ExitCode::from(2);
        };
        if arg == "--json" {
            json = true;
        } else {
            eprintln!("unexpected argument {arg:?}\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    let base_dir = std::env::var_os("MK_PANE_ID")
        .and_then(|s| s.to_str()?.parse::<u64>().ok())
        .and_then(|sid| crate::session::resolve_session_cwd(sid).ok())
        .or_else(|| std::env::current_dir().ok());
    let Some(dir) = base_dir else {
        eprintln!("worktree current: cannot determine current directory");
        return ExitCode::FAILURE;
    };
    let root = crate::set::resolve_worktree(&dir);
    show_worktree_info(&root, json)
}

fn run_show(args: &[OsString]) -> ExitCode {
    let mut path: Option<PathBuf> = None;
    let mut json = false;
    for raw in args {
        let Ok(arg) = utf8(raw) else {
            eprintln!("arguments must be UTF-8\n{USAGE}");
            return ExitCode::from(2);
        };
        if arg == "--json" {
            json = true;
        } else if !arg.starts_with('-') && path.is_none() {
            path = Some(PathBuf::from(arg));
        } else {
            eprintln!("unexpected argument {arg:?}\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    let dir = match path {
        Some(p) => p,
        None => match std::env::current_dir() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("worktree show: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let canon = std::fs::canonicalize(&dir).unwrap_or(dir);
    let root = crate::set::resolve_worktree(&canon);
    show_worktree_info(&root, json)
}

fn show_worktree_info(root: &std::path::Path, json: bool) -> ExitCode {
    let key = krill_proto::orchestration::worktree_key(root);
    let branch = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map_or_else(
            || "-".to_owned(),
            |o| String::from_utf8_lossy(&o.stdout).trim().to_owned(),
        );
    let store = daemon::state_dir()
        .and_then(|d| krill_proto::orchestration::WorktreeStore::load(&d).ok())
        .unwrap_or_default();
    let cp = store.checkpoint(&key);
    if json {
        let payload = serde_json::json!({
            "worktree": key,
            "branch": branch,
            "workspace_status": cp.workspace_status,
            "comment": cp.comment,
            "pr": cp.pr,
            "issue": cp.issue,
            "parent": cp.parent,
            "references": cp.references,
            "unread": cp.unread,
            "updated_at_ms": cp.updated_at_ms,
        });
        match serde_json::to_string_pretty(&payload) {
            Ok(out) => println!("{out}"),
            Err(e) => {
                eprintln!("worktree: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        println!("worktree:   {key}");
        println!("branch:     {branch}");
        println!(
            "status:     {}",
            cp.workspace_status.as_deref().unwrap_or("-")
        );
        println!("comment:    {}", cp.comment.as_deref().unwrap_or("-"));
        println!(
            "pr:         {}",
            cp.pr.map_or_else(|| "-".to_owned(), |n| format!("#{n}"))
        );
        println!("issue:      {}", cp.issue.as_deref().unwrap_or("-"));
        println!("parent:     {}", cp.parent.as_deref().unwrap_or("-"));
    }
    ExitCode::SUCCESS
}

fn run_ps(target: &Target, args: &[OsString]) -> ExitCode {
    let mut wt: Option<OsString> = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(raw) = it.next() {
        let Ok(arg) = utf8(raw) else {
            eprintln!("arguments must be UTF-8\n{USAGE}");
            return ExitCode::from(2);
        };
        match arg {
            "--worktree" | "--cwd" => {
                let Some(v) = it.next() else {
                    eprintln!("missing --worktree value\n{USAGE}");
                    return ExitCode::from(2);
                };
                wt = Some(v.clone());
            }
            "--json" => json = true,
            s if !s.starts_with('-') && wt.is_none() => wt = Some(raw.clone()),
            _ => {
                eprintln!("unexpected argument {arg:?}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let wt_arg = match wt {
        Some(w) => w,
        None => match std::env::current_dir() {
            Ok(c) => crate::set::resolve_worktree(&c).into_os_string(),
            Err(e) => {
                eprintln!("worktree ps: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let mut ls_args = vec![OsString::from("ls"), OsString::from("--worktree"), wt_arg];
    if json {
        ls_args.push(OsString::from("--json"));
    }
    crate::session::run_with_target(target, &ls_args)
}

fn resolve_repo_arg(repo: Option<PathBuf>) -> Result<PathBuf, String> {
    let dir = repo
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|e| e.to_string())?;
    let canon = std::fs::canonicalize(&dir).unwrap_or(dir);
    Ok(PathBuf::from(krill_proto::orchestration::worktree_key(
        &canon,
    )))
}

fn run_symlink(args: &[OsString]) -> ExitCode {
    let mut it = args.iter();
    let Some(sub_os) = it.next() else {
        eprintln!("missing symlink subcommand (list|add|rm)\n{USAGE}");
        return ExitCode::from(2);
    };
    let Ok(sub) = utf8(sub_os) else {
        eprintln!("arguments must be UTF-8\n{USAGE}");
        return ExitCode::from(2);
    };
    let mut repo = None;
    let mut target = None;
    let mut json = false;
    while let Some(raw) = it.next() {
        let Ok(arg) = utf8(raw) else {
            eprintln!("arguments must be UTF-8\n{USAGE}");
            return ExitCode::from(2);
        };
        if let Some(val) = arg.strip_prefix("--repo=") {
            repo = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--path=") {
            target = Some(val.to_owned());
        } else {
            match arg {
                "--repo" => {
                    let Some(v) = it.next() else {
                        eprintln!("missing --repo value\n{USAGE}");
                        return ExitCode::from(2);
                    };
                    repo = Some(PathBuf::from(v));
                }
                "--path" => {
                    let Some(v) = it.next().and_then(|s| s.to_str()) else {
                        eprintln!("missing --path value\n{USAGE}");
                        return ExitCode::from(2);
                    };
                    target = Some(v.to_owned());
                }
                "--json" => json = true,
                s if !s.starts_with('-') && target.is_none() => target = Some(s.to_owned()),
                _ => {
                    eprintln!("unexpected argument {arg:?}\n{USAGE}");
                    return ExitCode::from(2);
                }
            }
        }
    }

    let Some(state) = daemon::state_dir() else {
        eprintln!("worktree symlink: HOME is not set");
        return ExitCode::FAILURE;
    };
    let root = match resolve_repo_arg(repo) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("worktree symlink: {e}");
            return ExitCode::FAILURE;
        }
    };
    let key = root.to_string_lossy().into_owned();

    match sub {
        "list" | "ls" => {}
        "add" => {
            let Some(ref path) = target else {
                eprintln!("missing symlink path\n{USAGE}");
                return ExitCode::from(2);
            };
            if let Err(e) = krill_proto::orchestration::WorktreeStore::mutate(&state, |store| {
                store.add_symlink(&key, path).map(|_| ())
            }) {
                eprintln!("worktree symlink: {e}");
                return ExitCode::from(2);
            }
        }
        "rm" | "remove" => {
            let Some(ref path) = target else {
                eprintln!("missing symlink path\n{USAGE}");
                return ExitCode::from(2);
            };
            if let Err(e) = krill_proto::orchestration::WorktreeStore::mutate(&state, |store| {
                store.remove_symlink(&key, path);
                Ok(())
            }) {
                eprintln!("worktree symlink: {e}");
                return ExitCode::FAILURE;
            }
        }
        _ => {
            eprintln!("unknown symlink subcommand {sub:?} (expected list|add|rm)\n{USAGE}");
            return ExitCode::from(2);
        }
    }

    let summary = krill_proto::orchestration::inspect_repo_hooks(&root, Some(&state));
    if json {
        match serde_json::to_string_pretty(&summary) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("worktree symlink: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        for item in &summary.effective_symlinks {
            println!("{item}");
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn record_setup_trust(
    state_dir: &std::path::Path,
    raw_hash: &str,
) -> Result<(), String> {
    let hash = raw_hash.trim().to_ascii_lowercase();
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("hook trust requires a 64-digit SHA-256 hash".to_owned());
    }
    let trust_file = state_dir.join("setup-trust");
    if let Ok(existing) = std::fs::read_to_string(&trust_file)
        && existing.lines().any(|line| line.trim() == hash)
    {
        return Ok(());
    }
    std::fs::create_dir_all(state_dir).map_err(|e| format!("cannot create state dir: {e}"))?;
    let mut opts = std::fs::OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    opts.open(&trust_file)
        .and_then(|mut f| writeln!(f, "{hash}"))
        .map_err(|e| format!("cannot record setup trust: {e}"))
}

fn run_hooks(args: &[OsString]) -> ExitCode {
    let mut it = args.iter();
    let mut repo = None;
    let mut setup: Option<String> = None;
    let mut archive: Option<String> = None;
    let mut trust_hashes: Vec<String> = Vec::new();
    let mut json = false;
    while let Some(raw) = it.next() {
        let Ok(arg) = utf8(raw) else {
            eprintln!("arguments must be UTF-8\n{USAGE}");
            return ExitCode::from(2);
        };
        if let Some(val) = arg.strip_prefix("--repo=") {
            repo = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--setup=") {
            setup = Some(val.to_owned());
        } else if let Some(val) = arg.strip_prefix("--archive=") {
            archive = Some(val.to_owned());
        } else if let Some(val) = arg
            .strip_prefix("--trust=")
            .or_else(|| arg.strip_prefix("--trust-archive="))
            .or_else(|| arg.strip_prefix("--trust-setup="))
        {
            trust_hashes.push(val.to_owned());
        } else {
            match arg {
                "--repo" => {
                    let Some(v) = it.next() else {
                        eprintln!("missing --repo value\n{USAGE}");
                        return ExitCode::from(2);
                    };
                    repo = Some(PathBuf::from(v));
                }
                "--setup" => {
                    let Some(v) = it.next().and_then(|s| s.to_str()) else {
                        eprintln!("missing --setup value\n{USAGE}");
                        return ExitCode::from(2);
                    };
                    setup = Some(v.to_owned());
                }
                "--archive" => {
                    let Some(v) = it.next().and_then(|s| s.to_str()) else {
                        eprintln!("missing --archive value\n{USAGE}");
                        return ExitCode::from(2);
                    };
                    archive = Some(v.to_owned());
                }
                "--trust" | "--trust-archive" | "--trust-setup" => {
                    let Some(v) = it.next().and_then(|s| s.to_str()) else {
                        eprintln!("missing {arg} value\n{USAGE}");
                        return ExitCode::from(2);
                    };
                    trust_hashes.push(v.to_owned());
                }
                "--clear-setup" => setup = Some(String::new()),
                "--clear-archive" => archive = Some(String::new()),
                "--json" => json = true,
                _ => {
                    eprintln!("unexpected argument {arg:?}\n{USAGE}");
                    return ExitCode::from(2);
                }
            }
        }
    }

    let Some(state) = daemon::state_dir() else {
        eprintln!("worktree hooks: HOME is not set");
        return ExitCode::FAILURE;
    };
    for hash in &trust_hashes {
        if let Err(e) = record_setup_trust(&state, hash) {
            eprintln!("worktree hooks: {e}");
            return ExitCode::from(2);
        }
    }
    let root = match resolve_repo_arg(repo) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("worktree hooks: {e}");
            return ExitCode::FAILURE;
        }
    };
    let key = root.to_string_lossy().into_owned();

    if (setup.is_some() || archive.is_some())
        && let Err(e) = krill_proto::orchestration::WorktreeStore::mutate(&state, |store| {
            if let Some(ref s) = setup {
                store.set_setup_script(&key, s)?;
            }
            if let Some(ref a) = archive {
                store.set_archive_script(&key, a)?;
            }
            Ok(())
        })
    {
        eprintln!("worktree hooks: {e}");
        return ExitCode::from(2);
    }

    let summary = krill_proto::orchestration::inspect_repo_hooks(&root, Some(&state));
    if json {
        match serde_json::to_string_pretty(&summary) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("worktree hooks: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        println!(
            "setup: {}",
            summary.effective_setup.as_deref().unwrap_or("(none)")
        );
        println!(
            "archive: {}",
            summary.effective_archive.as_deref().unwrap_or("(none)")
        );
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_position_help_returns_zero() {
        assert_eq!(run(&[OsString::from("--help")]), ExitCode::SUCCESS);
        assert_eq!(
            run(&[OsString::from("new"), OsString::from("--help")]),
            ExitCode::SUCCESS
        );
        assert_eq!(
            run(&[OsString::from("cleanup"), OsString::from("-h")]),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn parse_new_worktree_with_parent() {
        let dir = std::env::temp_dir().join(format!("mk-wt-parse-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let parent_dir = dir.join("parent-wt");
        std::fs::create_dir_all(parent_dir.join(".git")).unwrap();
        let canon_parent = std::fs::canonicalize(&parent_dir).unwrap();

        let args = [
            OsString::from("new"),
            OsString::from("child-feature"),
            OsString::from("--parent"),
            parent_dir.as_os_str().to_owned(),
            OsString::from("--setup"),
            OsString::from("skip"),
        ];
        let parsed = parse(&args).unwrap();
        assert_eq!(parsed.req.name, "child-feature");
        assert_eq!(parsed.req.setup, Setup::Skip);
        assert_eq!(parsed.parent.as_deref(), Some(canon_parent.as_path()));
        assert_eq!(parsed.req.repo, canon_parent.as_os_str().as_bytes());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parent_branch_is_default_but_explicit_base_and_detached_head_are_preserved() {
        let dir = std::env::temp_dir().join(format!("krill-parent-base-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-b", "parent-feature"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ]);
        let mut args = vec![
            OsString::from("new"),
            OsString::from("child"),
            OsString::from("--parent"),
            dir.join("nested").into_os_string(),
        ];
        let parsed = parse(&args).unwrap();
        assert_eq!(parsed.req.base.as_deref(), Some("parent-feature"));
        assert_eq!(parsed.parent, Some(std::fs::canonicalize(&dir).unwrap()));
        args.extend([OsString::from("--base"), OsString::from("HEAD~0")]);
        assert_eq!(parse(&args).unwrap().req.base.as_deref(), Some("HEAD~0"));
        args.truncate(4);
        git(&["checkout", "--detach"]);
        assert_eq!(parse(&args).unwrap().req.base, None);
        assert_eq!(parent_branch(&dir.join("missing")), None);
        let without_parent = parse(&[
            OsString::from("new"),
            OsString::from("top-level"),
            OsString::from("--repo"),
            dir.clone().into_os_string(),
        ])
        .unwrap();
        assert_eq!(without_parent.req.base, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn worker_child_uses_worktree_new_parser_and_setup_approval() {
        let dir = std::env::temp_dir().join(format!("krill-child-parse-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        let parent = dir.join("nested");
        let hash = "a".repeat(64);
        let options = [
            "--base",
            "feature",
            "--setup",
            "run",
            "--trust-setup",
            &hash,
        ]
        .map(OsString::from);
        let child = parse_new_child(&parent, "child", &options).unwrap();
        let mut command = vec![
            OsString::from("new"),
            OsString::from("child"),
            OsString::from("--parent"),
            parent.into_os_string(),
        ];
        command.extend_from_slice(&options);
        assert_eq!(child, parse(&command).unwrap());
        assert_eq!(child.parent, Some(std::fs::canonicalize(&dir).unwrap()));
        assert_eq!(child.req.base.as_deref(), Some("feature"));
        assert_eq!(child.req.setup, Setup::Approve(vec![0xaa; 32]));
        assert_eq!(
            parse_new_child(
                &dir,
                "child",
                &[OsString::from("--setup"), OsString::from("skip")]
            )
            .unwrap()
            .req
            .setup,
            Setup::Skip
        );
        assert!(
            parse_new_child(
                &dir,
                "child",
                &[OsString::from("--trust-setup"), OsString::from("bad")]
            )
            .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn worktree_help_exits_zero_and_lists_all_subcommands() {
        for flag in ["-h", "--help", "help"] {
            assert_eq!(run(&[OsString::from(flag)]), ExitCode::SUCCESS, "{flag}");
        }
        for sub in ["new", "rm", "cleanup", "space", "set", "symlink", "hooks"] {
            assert_eq!(
                run(&[OsString::from(sub), OsString::from("--help")]),
                ExitCode::SUCCESS,
                "{sub} --help"
            );
            assert!(
                USAGE.contains(&format!("krill worktree {sub}")),
                "missing {sub} in USAGE: {USAGE}"
            );
        }
        assert_eq!(run(&[]), ExitCode::from(2));
    }

    #[test]
    fn record_setup_trust_deduplicates_and_validates_sha256() {
        let dir = std::env::temp_dir().join(format!("mk-wt-trust-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        assert!(record_setup_trust(&dir, "not-a-hash").is_err());
        let hash = "A".repeat(64);
        record_setup_trust(&dir, &hash).unwrap();
        record_setup_trust(&dir, &hash.to_ascii_lowercase()).unwrap();
        let saved = std::fs::read_to_string(dir.join("setup-trust")).unwrap();
        assert_eq!(saved, format!("{}\n", "a".repeat(64)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
