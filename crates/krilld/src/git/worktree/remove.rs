//! 워크트리 삭제(MK-043). 잠금·dirty 검사 → remove → branch -d → prune.
//! 브랜치 정리 실패는 작업 내용을 보존하고 경고한다. force로 잠금이나 미병합 브랜치를 무시하지 않는다.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use krill_proto::control::{RemoveWorktree, RemovedWorktree, Reply};

use super::{GitQueue, git, output_path, queue, text, transaction};

/// 삭제 요청은 클라이언트 연결이 끊겨도 끝까지 처리한다.
pub fn serve_remove(req: &RemoveWorktree) -> Reply {
    let state = krill_proto::daemon::state_dir();
    match queue().and_then(|q| remove_with_state(q, req, state.as_deref())) {
        Ok(w) => Reply::WorktreeRemoved(w),
        Err(e) => Reply::WorktreeFailed(e),
    }
}

/// 등록된 링크된 워크트리를 지우고 안전하게 브랜치·오래된 등록 정보를 정리한다.
pub fn remove(q: &GitQueue, req: &RemoveWorktree) -> Result<RemovedWorktree, String> {
    remove_with_state(q, req, krill_proto::daemon::state_dir().as_deref())
}

/// 상태 디렉터리의 워크트리 심링크·훅 설정을 함께 반영해 링크된 워크트리를 삭제한다(MK-174).
pub fn remove_with_state(
    q: &GitQueue,
    req: &RemoveWorktree,
    state_dir: Option<&Path>,
) -> Result<RemovedWorktree, String> {
    let given = Path::new(OsStr::from_bytes(&req.repo));
    let top = output_path(
        git(q, given, ["rev-parse", "--show-toplevel"])?,
        "not a git repository",
    )?;
    let _transaction = transaction(q, &top)?;
    let entries = entries(q, &top)?;
    let main = entries.first().ok_or("repository has no worktrees")?;
    let path = fs::canonicalize(Path::new(OsStr::from_bytes(&req.path)))
        .map_err(|e| format!("cannot resolve worktree: {e}"))?;
    let target = entries
        .iter()
        .find(|e| e.path == path)
        .ok_or("path is not a registered worktree of this repository")?;
    if target.path == main.path {
        return Err("cannot remove the main worktree".to_owned());
    }
    if target.locked {
        return Err("worktree is locked; unlock it explicitly before removal".to_owned());
    }
    // 주 워크트리에서 실행해 삭제 대상에서 보낸 요청도 후속 git 호출을 할 수 있다.
    let repo = &main.path;
    let config = super::compat::read_config_with_state(repo, state_dir)?;
    let status = git(
        q,
        repo,
        [
            OsString::from("-C"),
            path.as_os_str().to_owned(),
            OsString::from("status"),
            OsString::from("--porcelain"),
            OsString::from("-z"),
            OsString::from("--untracked-files=all"),
            OsString::from("--ignored=matching"),
        ],
    )?;
    let dirty = is_dirty_ignoring_symlinks(&status.stdout, &path, &config.worktree.symlinks);
    text(status, "cannot check worktree status")?;
    if !req.force && dirty {
        return Err("worktree is dirty; use --force to discard its files".to_owned());
    }
    if let Some(script) = config.worktree.archive.as_deref() {
        run_archive_hook(q, &path, script)?;
    }
    unlink_symlinks(&path, &config.worktree.symlinks);
    let mut args: Vec<OsString> = ["worktree", "remove"].map(OsString::from).into();
    if req.force {
        args.push("--force".into());
    }
    args.push("--".into());
    args.push(path.into_os_string());
    text(git(q, repo, args)?, "git worktree remove failed")?;

    let mut result = RemovedWorktree {
        branch: target.branch.clone(),
        branch_deleted: false,
        warnings: Vec::new(),
    };
    if let Some(branch) = &result.branch {
        match delete_merged_branch(q, repo, branch) {
            Ok(()) => result.branch_deleted = true,
            Err(e) => result
                .warnings
                .push(format!("branch {branch:?} preserved: {e}")),
        }
    }
    // Git의 기본 만료 기간을 사용해 locked 등록이나 최근 사라진 워크트리는 보존한다.
    if let Err(e) =
        git(q, repo, ["worktree", "prune"]).and_then(|o| text(o, "git worktree prune failed"))
    {
        result.warnings.push(format!("worktree prune failed: {e}"));
    }
    Ok(result)
}

fn is_valid_worktree_symlink(wt_real: &Path, raw: &str) -> bool {
    let trimmed = raw.trim().trim_end_matches('/');
    let rel = Path::new(trimmed);
    if trimmed.is_empty()
        || trimmed.contains('\\')
        || !rel.components().all(|c| {
            matches!(
                c,
                std::path::Component::Normal(n) if !n.as_bytes().eq_ignore_ascii_case(b".git")
            )
        })
    {
        return false;
    }
    let parent = rel.parent().unwrap_or(Path::new(""));
    if super::safe_parent(wt_real, parent, false).is_err() {
        return false;
    }
    fs::symlink_metadata(wt_real.join(rel)).is_ok_and(|m| m.file_type().is_symlink())
}

fn dir_contains_only_active_symlinks(
    wt_real: &Path,
    rel_dir: &Path,
    active_symlinks: &[&str],
) -> bool {
    let dir_path = wt_real.join(rel_dir);
    let Ok(meta) = fs::symlink_metadata(&dir_path) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return rel_dir
            .to_str()
            .is_some_and(|s| active_symlinks.contains(&s.trim_end_matches('/')));
    }
    if !meta.is_dir() {
        return false;
    }
    let Ok(read_dir) = fs::read_dir(&dir_path) else {
        return false;
    };
    let mut has_entry = false;
    for entry in read_dir {
        let Ok(entry) = entry else {
            return false;
        };
        has_entry = true;
        let child_rel = rel_dir.join(entry.file_name());
        let Some(child_str) = child_rel.to_str() else {
            return false;
        };
        if active_symlinks.contains(&child_str) {
            continue;
        }
        if !dir_contains_only_active_symlinks(wt_real, &child_rel, active_symlinks) {
            return false;
        }
    }
    has_entry
}

fn is_dirty_ignoring_symlinks(stdout: &[u8], wt_real: &Path, symlinks: &[String]) -> bool {
    if stdout.is_empty() {
        return false;
    }
    if symlinks.is_empty() {
        return true;
    }
    let active_symlinks: Vec<&str> = symlinks
        .iter()
        .map(String::as_str)
        .filter(|s| is_valid_worktree_symlink(wt_real, s))
        .collect();
    if active_symlinks.is_empty() {
        return true;
    }
    for entry in stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let Some(rel_bytes) = entry
            .strip_prefix(b"?? ")
            .or_else(|| entry.strip_prefix(b"!! "))
        else {
            return true;
        };
        let Ok(rel_str) = std::str::from_utf8(rel_bytes) else {
            return true;
        };
        let trimmed = rel_str.trim_end_matches('/');
        if active_symlinks.contains(&trimmed) {
            continue;
        }
        if rel_str.ends_with('/')
            && dir_contains_only_active_symlinks(wt_real, Path::new(trimmed), &active_symlinks)
        {
            continue;
        }
        return true;
    }
    false
}

fn unlink_symlinks(wt_real: &Path, symlinks: &[String]) {
    for raw in symlinks {
        if is_valid_worktree_symlink(wt_real, raw) {
            let trimmed = raw.trim().trim_end_matches('/');
            let rel = Path::new(trimmed);
            fs::remove_file(wt_real.join(rel)).ok();
            let mut parent = rel.parent();
            while let Some(p) = parent {
                if p.as_os_str().is_empty() {
                    break;
                }
                if fs::remove_dir(wt_real.join(p)).is_err() {
                    break;
                }
                parent = p.parent();
            }
        }
    }
}

fn run_archive_hook(q: &GitQueue, wt: &Path, script: &str) -> Result<(), String> {
    let envs = super::compat::environment_in(q, wt)?;
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(script)
        .current_dir(wt)
        .stdin(std::process::Stdio::null());
    for (k, v) in envs {
        cmd.env(OsStr::from_bytes(&k), OsStr::from_bytes(&v));
    }
    let out = cmd
        .output()
        .map_err(|e| format!("archive hook failed to start: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", out.status)
    };
    Err(format!("archive hook failed: {detail}"))
}

fn delete_merged_branch(q: &GitQueue, repo: &Path, branch: &str) -> Result<(), String> {
    // branch -d는 upstream이 있으면 HEAD 대신 upstream만 검사한다. 주 워크트리 HEAD에도 합쳐졌는지
    // 먼저 확인해, 자기 upstream에만 올라간 커밋이 마지막 로컬 브랜치와 함께 사라지는 것을 막는다.
    let reference = format!("refs/heads/{branch}");
    let merged = git(
        q,
        repo,
        ["merge-base", "--is-ancestor", "--", &reference, "HEAD"],
    )?;
    if merged.status.code() == Some(1) {
        return Err("not merged into the main worktree HEAD".to_owned());
    }
    text(merged, "cannot check whether branch is merged")?;
    text(
        git(q, repo, ["branch", "-d", "--", branch])?,
        "git branch -d failed",
    )?;
    Ok(())
}

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    branch: Option<String>,
    locked: bool,
}

fn entries(q: &GitQueue, repo: &Path) -> Result<Vec<Entry>, String> {
    let out = git(q, repo, ["worktree", "list", "--porcelain", "-z"])?;
    if !out.status.success() {
        return text(out, "git worktree list failed").map(|_| Vec::new());
    }
    parse_entries(&out.stdout)
}

fn parse_entries(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for field in bytes.split(|b| *b == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            entries.push(Entry {
                path: PathBuf::from(OsString::from_vec(path.to_vec())),
                branch: None,
                locked: false,
            });
        } else if let Some(entry) = entries.last_mut() {
            if let Some(branch) = field.strip_prefix(b"branch refs/heads/") {
                entry.branch = Some(
                    String::from_utf8(branch.to_vec())
                        .map_err(|e| format!("branch is not UTF-8: {e}"))?,
                );
            } else if field == b"locked" || field.starts_with(b"locked ") {
                entry.locked = true;
            }
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::control::Request;
    use krill_proto::screen::Msg;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn arbitrary_worktree_output_and_control_bodies_do_not_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            drop(parse_entries(&bytes));
            drop(Request::decode(krill_proto::control::kind::REQUEST, &bytes));
            drop(Reply::decode(krill_proto::control::kind::REPLY, &bytes));
        }
    }
}
