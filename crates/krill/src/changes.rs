//! `krill changes [--repo <path>]`: one-shot changed paths and staged/unstaged numstat via krilld.

use krill_proto::os::ffi::OsStrExt as _;
use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use krill_proto::control::{FEAT_CONTROL, FEAT_GIT_CHANGES, Reply, Request};
use krill_proto::daemon::{self, Paths};
use krill_proto::git::Numstat;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;

const USAGE: &str = "usage: krill changes [--repo <path>]";

pub fn run(args: &[OsString]) -> ExitCode {
    // `--repo <path>` 값 자리의 `help`는 경로이므로 첫 두 토큰만 본다.
    let head: Vec<_> = args.iter().take(2).collect();
    if head
        .iter()
        .any(|a| matches!(a.to_str(), Some("-h" | "--help" | "help")))
    {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let repo = match args {
        [] => std::env::current_dir(),
        [flag, repo] if flag == "--repo" => std::fs::canonicalize(repo),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = repo
        .map_err(|e| e.to_string())
        .and_then(|repo| query(&repo));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("changes: {e}");
            ExitCode::FAILURE
        }
    }
}

fn query(repo: &Path) -> Result<(), String> {
    let paths = Paths::dev().map_err(|e| e.to_string())?;
    let mut conn = daemon::connect(
        &paths,
        &daemon::client_hello(ClientRole::Cli, "krill changes"),
    )
    .map_err(|e| e.to_string())?;
    let required = FEAT_CONTROL | FEAT_GIT_CHANGES;
    if conn.negotiated.features & required != required {
        return Err("krilld does not support git changes; update the daemon".to_owned());
    }
    conn.stream
        .write_all(
            &Request::GitChanges {
                repo: repo.as_os_str().as_bytes().to_vec(),
            }
            .encode()
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    let (kind, body) = ipc::read_frame(&mut conn.stream)
        .map_err(|e| e.to_string())?
        .ok_or("krilld disconnected")?;
    match Reply::decode(kind, &body).map_err(|e| e.to_string())? {
        Reply::GitChanges(changes) => {
            for file in changes.files {
                // Debug string escaping keeps tabs/newlines from forging another output row.
                let path = OsStr::from_bytes(&file.path).to_string_lossy();
                let old = file
                    .old_path
                    .as_deref()
                    .map(|p| format!(" <- {:?}", OsStr::from_bytes(p).to_string_lossy()))
                    .unwrap_or_default();
                println!(
                    "{}{} staged={} unstaged={} {path:?}{old}",
                    char::from(file.status[0]),
                    char::from(file.status[1]),
                    stats(file.staged),
                    stats(file.unstaged)
                );
            }
            if changes.limited {
                eprintln!(
                    "warning: changed file list reached its limit; additional files may be omitted"
                );
            }
            Ok(())
        }
        Reply::GitChangesFailed(e) => Err(e),
        Reply::Failed(e) => Err(e.to_string()),
        _ => Err("unexpected daemon reply".to_owned()),
    }
}

fn stats(value: Option<Numstat>) -> String {
    match value {
        Some(Numstat {
            added: Some(a),
            deleted: Some(d),
        }) => format!("+{a}/-{d}"),
        Some(_) => "binary".to_owned(),
        None => "none".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn help_returns_zero() {
        assert_eq!(run(&[OsString::from("--help")]), ExitCode::SUCCESS);
        assert_eq!(run(&[OsString::from("-h")]), ExitCode::SUCCESS);
        assert_eq!(run(&[OsString::from("help")]), ExitCode::SUCCESS);
    }
}
