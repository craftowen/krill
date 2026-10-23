//! MK-088: index 변경·커밋은 데몬 큐에서 완료한다. 앱 소켓이 닫혀도 작업은 계속된다.

use krill_proto::os::ffi::{OsStrExt as _, OsStringExt as _};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::process::Output;
use std::sync::mpsc;

use krill_proto::control::Reply;
use krill_proto::git::{Edit, MAX_MESSAGE, MAX_PATH_BYTES};

use super::GitQueue;

pub fn serve(repo: &[u8], edit: Edit) -> Reply {
    Reply::GitEdited(
        super::queue().and_then(|q| apply(q, Path::new(OsStr::from_bytes(repo)), edit)),
    )
}

/// Execute from the worktree root and serialize with the main worktree queue key.
/// Repository discovery also uses the queue; filenames are always literal pathspecs.
pub fn apply(q: &GitQueue, repo: &Path, edit: Edit) -> Result<(), String> {
    validate(&edit)?;
    let root = resolve(q, repo, "--show-toplevel")?;
    let common = resolve(q, repo, "--git-common-dir")?;
    let key = if common.file_name().is_some_and(|name| name == ".git") {
        common.parent().unwrap_or(&common).to_path_buf()
    } else {
        common
    };
    let key = std::fs::canonicalize(&key).map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    let retained = match &edit {
        Edit::Stage { path, old_path } | Edit::Unstage { path, old_path } => path
            .len()
            .saturating_add(old_path.as_ref().map_or(0, Vec::len)),
        Edit::Commit { message } | Edit::CommitAndPush { message } => message.len(),
        Edit::Push | Edit::Pull | Edit::Sync => 0,
    };
    q.enqueue_key(
        root,
        key,
        retained,
        Box::new(move |program, repo| {
            tx.send(run(program, repo, edit)).ok();
        }),
    );
    rx.recv().map_err(|e| e.to_string())?
}

fn resolve(q: &GitQueue, repo: &Path, option: &str) -> Result<PathBuf, String> {
    let output = q
        .submit(repo, ["rev-parse", "--path-format=absolute", option])
        .recv()
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let mut bytes = checked(output)?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err("missing repository root".into());
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn validate(edit: &Edit) -> Result<(), String> {
    match edit {
        Edit::Stage { path, old_path } | Edit::Unstage { path, old_path } => {
            for bytes in std::iter::once(path).chain(old_path.iter()) {
                if bytes.is_empty()
                    || bytes.len() > MAX_PATH_BYTES
                    || bytes.contains(&0)
                    || Path::new(OsStr::from_bytes(bytes))
                        .components()
                        .any(|p| !matches!(p, Component::Normal(_)))
                {
                    return Err("invalid git path".into());
                }
            }
        }
        Edit::Commit { message } | Edit::CommitAndPush { message } => {
            if message.trim().is_empty() || message.len() > MAX_MESSAGE || message.contains('\0') {
                return Err("commit message must contain text and be at most 16 KiB".into());
            }
        }
        Edit::Push | Edit::Pull | Edit::Sync => {}
    }
    Ok(())
}

fn run(program: &Path, repo: &Path, edit: Edit) -> Result<(), String> {
    let mut args: Vec<OsString> = vec!["--literal-pathspecs".into()];
    match edit {
        Edit::Commit { message } => run_commit(program, repo, message),
        Edit::CommitAndPush { message } => {
            run_commit(program, repo, message)?;
            run_push(program, repo)
        }
        Edit::Push => run_push(program, repo),
        Edit::Pull => run_pull(program, repo),
        Edit::Sync => {
            run_pull(program, repo)?;
            run_push(program, repo)
        }
        Edit::Stage { path, old_path } => {
            args.extend(["add".into(), "-A".into(), "--".into()]);
            // A staged rename's old name is already absent from the index. Passing it to
            // add would fail even when the destination has unstaged edits.
            let old_path = match old_path {
                Some(old) => {
                    let tracked = super::run(
                        program,
                        repo,
                        &[
                            "--literal-pathspecs".into(),
                            "ls-files".into(),
                            "--error-unmatch".into(),
                            "--".into(),
                            OsString::from_vec(old.clone()),
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                    tracked.status.success().then_some(old)
                }
                None => None,
            };
            paths(&mut args, path, old_path);
            checked(super::run(program, repo, &args).map_err(|e| e.to_string())?).map(|_| ())
        }
        Edit::Unstage { path, old_path } => {
            let head = super::run(
                program,
                repo,
                &["rev-parse".into(), "--verify".into(), "HEAD".into()],
            )
            .map_err(|e| e.to_string())?;
            if head.status.success() {
                args.extend(["reset".into(), "-q".into(), "HEAD".into(), "--".into()]);
            } else {
                // Unborn HEAD: remove only index entries and preserve every working file.
                args.extend([
                    "rm".into(),
                    "--cached".into(),
                    "-f".into(),
                    "--ignore-unmatch".into(),
                    "--".into(),
                ]);
            }
            paths(&mut args, path, old_path);
            checked(super::run(program, repo, &args).map_err(|e| e.to_string())?).map(|_| ())
        }
    }
}

fn run_commit(program: &Path, repo: &Path, message: String) -> Result<(), String> {
    // Respect identity, hooks and signing configuration; never use --no-verify/--amend/-a.
    let args: Vec<OsString> = vec![
        "--literal-pathspecs".into(),
        "commit".into(),
        "-m".into(),
        message.into(),
    ];
    checked(super::run(program, repo, &args).map_err(|e| e.to_string())?).map(|_| ())
}

fn run_push(program: &Path, repo: &Path) -> Result<(), String> {
    let upstream = super::run(
        program,
        repo,
        &[
            "rev-parse".into(),
            "--abbrev-ref".into(),
            "--symbolic-full-name".into(),
            "@{u}".into(),
        ],
    )
    .map_err(|e| e.to_string())?;
    let args: Vec<OsString> = if upstream.status.success() {
        vec!["push".into()]
    } else {
        let origin = super::run(
            program,
            repo,
            &["remote".into(), "get-url".into(), "origin".into()],
        )
        .map_err(|e| e.to_string())?;
        if origin.status.success() {
            vec![
                "push".into(),
                "--set-upstream".into(),
                "origin".into(),
                "HEAD".into(),
            ]
        } else {
            vec!["push".into()]
        }
    };
    checked(super::run(program, repo, &args).map_err(|e| e.to_string())?).map(|_| ())
}

fn run_pull(program: &Path, repo: &Path) -> Result<(), String> {
    let rebase_cfg = super::run(
        program,
        repo,
        &["config".into(), "--get".into(), "pull.rebase".into()],
    )
    .map_err(|e| e.to_string())?;
    let use_rebase = rebase_cfg.status.success()
        && String::from_utf8_lossy(&rebase_cfg.stdout)
            .trim()
            .eq_ignore_ascii_case("true");
    let args: Vec<OsString> = if use_rebase {
        vec!["pull".into(), "--rebase".into()]
    } else {
        vec!["pull".into(), "--no-rebase".into(), "--no-edit".into()]
    };
    checked(super::run(program, repo, &args).map_err(|e| e.to_string())?).map(|_| ())
}

fn paths(args: &mut Vec<OsString>, path: Vec<u8>, old_path: Option<Vec<u8>>) {
    args.push(OsString::from_vec(path));
    args.extend(old_path.map(OsString::from_vec));
}

fn checked(output: Output) -> Result<Vec<u8>, String> {
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr: Vec<_> = output.stderr.into_iter().take(8192).collect();
    let stdout: Vec<_> = output.stdout.into_iter().take(8192).collect();
    let stderr = String::from_utf8_lossy(&stderr);
    let stdout = String::from_utf8_lossy(&stdout);
    let combined = match (stderr.trim(), stdout.trim()) {
        ("", "") => "git operation failed".to_owned(),
        (err, "") => err.to_owned(),
        ("", out) => out.to_owned(),
        (err, out) => format!("{err}\n{out}"),
    };
    Err(combined)
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
