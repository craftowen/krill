//! `krill worktree rm`: 삭제·브랜치 정리 결과를 데몬에서 받는다.

use krill_proto::os::ffi::OsStrExt;
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use krill_proto::control::{FEAT_CONTROL, FEAT_WORKTREE_REMOVE, RemoveWorktree, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg;

use crate::remote::Target;

const USAGE: &str =
    "usage: krill [--host <user@host>] worktree rm <path> [--repo <path>] [--force]";

pub fn run(target: &Target, args: &[OsString]) -> ExitCode {
    let req = match parse(args, target.is_remote()) {
        Ok(req) => req,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match remove(target, req) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("worktree: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse(args: &[OsString], remote: bool) -> Result<RemoveWorktree, String> {
    let mut args = args.iter();
    let mut repo = None;
    let mut path = None;
    let mut force = false;
    while let Some(arg) = args.next() {
        if arg == "--repo" {
            repo = Some(PathBuf::from(args.next().ok_or("missing --repo value")?));
        } else if arg == "--force" {
            force = true;
        } else if arg == "--" {
            if path.is_some() {
                return Err("unexpected arguments after path".to_owned());
            }
            path = Some(PathBuf::from(args.next().ok_or("missing worktree path")?));
            if args.next().is_some() {
                return Err("unexpected arguments after path".to_owned());
            }
        } else if !arg.as_bytes().starts_with(b"-") && path.is_none() {
            path = Some(PathBuf::from(arg));
        } else {
            return Err(format!("unexpected argument {}", arg.display()));
        }
    }
    let path = path.ok_or("missing worktree path")?;
    if remote {
        let Some(repo) = repo else {
            return Err(
                "remote worktree rm requires --repo <absolute path on the remote host>".to_owned(),
            );
        };
        if !repo.as_os_str().as_bytes().starts_with(b"/") {
            return Err("remote --repo must be an absolute path on the remote host".to_owned());
        }
        if !path.as_os_str().as_bytes().starts_with(b"/") {
            return Err(
                "remote worktree path must be an absolute path on the remote host".to_owned(),
            );
        }
        return Ok(RemoveWorktree {
            repo: repo.as_os_str().as_bytes().to_vec(),
            path: path.as_os_str().as_bytes().to_vec(),
            force,
        });
    }
    let repo = repo
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|e| e.to_string())?;
    let repo =
        std::fs::canonicalize(repo).map_err(|e| format!("cannot resolve repository: {e}"))?;
    let path = std::fs::canonicalize(path).map_err(|e| format!("cannot resolve worktree: {e}"))?;
    Ok(RemoveWorktree {
        repo: repo.as_os_str().as_bytes().to_vec(),
        path: path.as_os_str().as_bytes().to_vec(),
        force,
    })
}

fn remove(target: &Target, req: RemoveWorktree) -> Result<(), String> {
    let mut conn = target
        .connect(&daemon::client_hello(ClientRole::Cli, "krill worktree rm"))
        .map_err(|e| e.to_string())?;
    let required = FEAT_CONTROL | FEAT_WORKTREE_REMOVE;
    if conn.negotiated.features & required != required {
        return Err("krilld does not support worktree removal; update the daemon".to_owned());
    }
    let removed_path = PathBuf::from(std::ffi::OsStr::from_bytes(&req.path));
    conn.stream
        .write_all(
            &Request::RemoveWorktree(req)
                .encode()
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    let (kind, body) = ipc::read_frame(&mut conn.stream)
        .map_err(|e| e.to_string())?
        .ok_or("krilld disconnected; inspect git worktree list before retrying")?;
    match Reply::decode(kind, &body).map_err(|e| e.to_string())? {
        Reply::WorktreeRemoved(w) => {
            if !target.is_remote()
                && let Some(state_dir) = daemon::state_dir()
            {
                krill_proto::orchestration::WorktreeStore::mutate(&state_dir, |store| {
                    store.remove_worktree_lineage(&removed_path.to_string_lossy());
                    Ok(())
                })
                .ok();
            }
            println!("worktree removed");
            if let Some(branch) = w.branch {
                println!(
                    "branch {branch} {}",
                    if w.branch_deleted {
                        "deleted"
                    } else {
                        "preserved"
                    }
                );
            }
            for warning in w.warnings {
                eprintln!("warning: {warning}");
            }
            Ok(())
        }
        Reply::WorktreeFailed(e) => Err(e),
        Reply::Failed(e) => Err(e.to_string()),
        _ => Err("unexpected daemon reply".to_owned()),
    }
}
