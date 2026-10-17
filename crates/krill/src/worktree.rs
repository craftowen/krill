//! `krill worktree new|rm`: 생성·삭제는 krilld에 맡긴다. setup은 내용을 보여 주고 해시를 승인받은 뒤에만 실행한다.

mod cleanup;
mod remove;
mod space;

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{self, IsTerminal, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::ExitCode;

use krill_proto::control::{
    FEAT_CONTROL, FEAT_WORKTREE, NewWorktree, Reply, Request, Setup, Worktree,
};
use krill_proto::daemon::{self, Paths};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg;

const USAGE: &str = "usage: krill worktree new <name> [--repo <path>] [--parent <path>] [--base <ref>] [--setup run|skip] [--trust-setup <hash>]\n       krill worktree symlink <list|add|rm> [<path>] [--repo <path>] [--json]\n       krill worktree hooks [--repo <path>] [--setup <cmd>] [--archive <cmd>] [--clear-setup] [--clear-archive] [--json]";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedNew {
    req: NewWorktree,
    parent: Option<PathBuf>,
}

pub fn run(args: &[OsString]) -> ExitCode {
    if args.first().is_some_and(|a| a == "rm") {
        return remove::run(&args[1..]);
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
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let parent = parsed.parent;
    match create(parsed.req) {
        Ok(w) => {
            let created_path = PathBuf::from(OsStr::from_bytes(&w.path));
            if let Some(parent_path) = parent
                && let Some(state_dir) = daemon::state_dir()
                && let Err(e) =
                    krill_proto::orchestration::WorktreeStore::mutate(&state_dir, |store| {
                        store.set_parent(
                            &created_path.to_string_lossy(),
                            Some(&parent_path.to_string_lossy()),
                        )
                    })
            {
                eprintln!("warning: could not record parent worktree: {e}");
            }
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

fn parse(args: &[OsString]) -> Result<ParsedNew, String> {
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

fn utf8(s: &OsStr) -> Result<&str, String> {
    s.to_str()
        .ok_or_else(|| "name and options must be UTF-8".to_owned())
}

pub(super) fn create(mut req: NewWorktree) -> Result<Worktree, String> {
    let paths = Paths::dev().map_err(|e| e.to_string())?;
    let mut conn = daemon::connect(
        &paths,
        &daemon::client_hello(ClientRole::Cli, "krill worktree new"),
    )
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

fn run_hooks(args: &[OsString]) -> ExitCode {
    let mut it = args.iter();
    let mut repo = None;
    let mut setup: Option<String> = None;
    let mut archive: Option<String> = None;
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
}
