//! 워크트리 삭제(MK-043). 잠금·dirty 검사 → remove → branch -d → prune.
//! 브랜치 정리 실패는 작업 내용을 보존하고 경고한다. force로 잠금이나 미병합 브랜치를 무시하지 않는다.

use krill_proto::os::ffi::{OsStrExt, OsStringExt};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::control::{RemoveWorktree, RemovedWorktree, Reply};

use crate::child::PipeReader;

use super::{GitQueue, git, output_path, queue, text, transaction};

/// archive 훅 실행 상한. 잠금을 쥔 채 무기한 대기하지 않게 한다(OWE-751).
#[cfg(not(test))]
const ARCHIVE_HOOK_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const ARCHIVE_HOOK_TIMEOUT: Duration = Duration::from_millis(500);
/// 타임아웃 SIGKILL 전 SIGTERM 유예.
#[cfg(not(test))]
const ARCHIVE_TERMINATE_GRACE: Duration = Duration::from_secs(2);
#[cfg(test)]
const ARCHIVE_TERMINATE_GRACE: Duration = Duration::from_millis(100);
/// archive 훅 stdout·stderr 파이프 보관 상한(64 KiB) 및 에러 메시지 길이 상한(4 KiB).
const MAX_ARCHIVE_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_ARCHIVE_ERROR_CHARS: usize = 4096;

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
    let initial_entries = entries(q, &top)?;
    let main = initial_entries
        .first()
        .ok_or("repository has no worktrees")?;
    let path = fs::canonicalize(Path::new(OsStr::from_bytes(&req.path)))
        .map_err(|e| format!("cannot resolve worktree: {e}"))?;
    if path == main.path {
        return Err("cannot remove the main worktree".to_owned());
    }
    reject_nested_worktrees(&initial_entries, &path)?;
    let target = initial_entries.iter().find(|e| e.path == path);
    if target.is_some_and(|e| e.locked) {
        return Err("worktree is locked; unlock it explicitly before removal".to_owned());
    }
    // Work from the main tree even when the request originated inside the removed tree.
    let repo = &main.path;
    if target.is_none() {
        verify_git_link(q, repo, &path)?;
    }
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
    // Missing metadata makes status unknowable. Only explicit --force can discard
    // files in an ownership-verified orphan; a normal removal stays fail-closed.
    if !req.force || target.is_some() {
        text(status, "cannot check worktree status").map_err(|e| {
            format!(
                "cannot check worktree status: {e}; use --force only to discard an orphan's files"
            )
        })?;
    }
    if !req.force && dirty {
        return Err("worktree is dirty; use --force to discard its files".to_owned());
    }
    let mut warnings = Vec::new();
    if let Some(script) = config.worktree.archive.as_deref() {
        if !super::is_setup_trusted(state_dir, repo, script)? {
            let hash = super::hex(&super::digest(repo, script));
            let msg = format!(
                "archive hook is not trusted (SHA-256: {hash}); approve it in setup-trust (or pass --trust-archive {hash}) or use --force to remove without running it"
            );
            if req.force {
                warnings.push(msg);
            } else {
                return Err(msg);
            }
        } else if let Err(e) = run_archive_hook(q, &path, script) {
            if req.force {
                warnings.push(e);
            } else {
                return Err(e);
            }
        }
    }
    reject_nested_worktrees(&entries(q, repo)?, &path)?;
    let unlinked = unlink_symlinks(&path, &config.worktree.symlinks);
    let removal = (|| -> Result<(), String> {
        if target.is_none() {
            remove_orphan(q, repo, &path)?;
        } else {
            let mut args: Vec<OsString> = ["worktree", "remove"].map(OsString::from).into();
            if req.force {
                args.push("--force".into());
            }
            args.push("--".into());
            args.push(path.as_os_str().to_owned());
            if let Err(first_err) =
                git(q, repo, args).and_then(|out| text(out, "git worktree remove failed"))
            {
                if registered(q, repo, &path)? {
                    // A clean tree with initialized submodules needs a single --force.
                    // Files created after the dirty check (e.g. by the archive hook) must
                    // not be discarded by the retry, so look again before forcing.
                    if req.force || !worktree_is_clean(q, repo, &path) {
                        return Err(first_err);
                    }
                    let retry_args = [
                        OsString::from("worktree"),
                        OsString::from("remove"),
                        OsString::from("--force"),
                        OsString::from("--"),
                        path.as_os_str().to_owned(),
                    ];
                    if let Err(retry_err) =
                        git(q, repo, retry_args).and_then(|out| text(out, &first_err))
                    {
                        if registered(q, repo, &path)? {
                            return Err(retry_err);
                        }
                        remove_orphan(q, repo, &path)?;
                    }
                } else {
                    remove_orphan(q, repo, &path)?;
                }
            }
            // Even a successful Git exit is insufficient if a directory survived.
            if !directory_absent(&path)? {
                remove_orphan(q, repo, &path)?;
            }
        }
        Ok(())
    })();
    if let Err(e) = removal {
        restore_symlinks(&path, &unlinked);
        return Err(e);
    }
    if !directory_absent(&path)? {
        return Err(
            "worktree directory remains; preserving branch and registration metadata".to_owned(),
        );
    }

    let mut result = RemovedWorktree {
        branch: target.and_then(|e| e.branch.clone()),
        branch_deleted: false,
        warnings,
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
    if let Some(dir) = state_dir {
        prune_removed_worktree_from_projects(dir, &path);
    }
    Ok(result)
}

fn directory_absent(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Ok(_) => Ok(false),
        Err(e) => Err(format!("cannot verify worktree directory removal: {e}")),
    }
}

fn reject_nested_worktrees(entries: &[Entry], path: &Path) -> Result<(), String> {
    if entries
        .iter()
        .any(|e| e.path != path && e.path.starts_with(path))
    {
        return Err("worktree contains another registered worktree; remove it first".to_owned());
    }
    Ok(())
}

fn registered(q: &GitQueue, repo: &Path, path: &Path) -> Result<bool, String> {
    Ok(entries(q, repo)?.iter().any(|e| e.path == path))
}

/// A stale .git *file* is the only evidence permitting deletion outside Git's list.
/// Resolve the existing parent, never require the deleted per-worktree metadata.
fn verify_git_link(q: &GitQueue, repo: &Path, path: &Path) -> Result<(), String> {
    let invalid =
        || "path is not an ownership-verified orphan worktree of this repository".to_owned();
    let link = path.join(".git");
    let meta = fs::symlink_metadata(&link).map_err(|_| invalid())?;
    if !meta.is_file() || meta.len() > 16384 {
        return Err(invalid());
    }
    let bytes = fs::read(&link).map_err(|_| invalid())?;
    let raw = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    let gitdir = raw
        .strip_prefix(b"gitdir: ")
        .filter(|p| !p.is_empty())
        .ok_or_else(invalid)?;
    if gitdir.iter().any(|b| matches!(b, 0 | b'\n' | b'\r')) {
        return Err(invalid());
    }
    let gitdir = path.join(OsStr::from_bytes(gitdir));
    // Reject traversal before canonicalization, including a missing tail after `..`.
    if gitdir
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(invalid());
    }
    let common = output_path(
        git(
            q,
            repo,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?,
        "cannot resolve repository common directory",
    )?;
    let common = fs::canonicalize(common).map_err(|_| invalid())?;
    let parent = gitdir.parent().ok_or_else(invalid)?;
    // `worktrees` can also have been pruned away. Canonicalize CommonDir and
    // reject symlinks in either remaining component rather than following them.
    let resolved_parent = match fs::canonicalize(parent) {
        Ok(real) => real,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let ancestor = parent.parent().ok_or_else(invalid)?;
            fs::canonicalize(ancestor)
                .map_err(|_| invalid())?
                .join(parent.file_name().ok_or_else(invalid)?)
        }
        Err(_) => return Err(invalid()),
    };
    if resolved_parent != common.join("worktrees") || gitdir.file_name().is_none() {
        return Err(invalid());
    }
    for dir in [parent, gitdir.as_path()] {
        match fs::symlink_metadata(dir) {
            Ok(m) if !m.is_dir() || m.file_type().is_symlink() => return Err(invalid()),
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(invalid()),
            _ => {}
        }
    }
    match fs::read(gitdir.join("gitdir")) {
        Ok(backlink) => {
            let backlink = backlink.strip_suffix(b"\n").unwrap_or(&backlink);
            if fs::canonicalize(Path::new(OsStr::from_bytes(backlink))).map_err(|_| invalid())?
                != link
            {
                return Err(invalid());
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(invalid()),
    }
    // A stale locked metadata directory still requires explicit unlocking.
    if gitdir.join("locked").exists() {
        return Err("worktree is locked; unlock it explicitly before removal".to_owned());
    }
    Ok(())
}

fn remove_orphan(q: &GitQueue, repo: &Path, path: &Path) -> Result<(), String> {
    let current = entries(q, repo)?;
    if current.iter().any(|e| e.path == path) {
        return Err("worktree is still registered; refusing filesystem recovery".to_owned());
    }
    reject_nested_worktrees(&current, path)?;
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        _ => return Err("cannot inspect orphan worktree directory".to_owned()),
    }
    verify_git_link(q, repo, path)?;
    fs::remove_dir_all(path).map_err(|e| format!("cannot remove orphan worktree directory: {e}"))
}

fn prune_removed_worktree_from_projects(state_dir: &Path, removed: &Path) {
    fn alias(path: &str) -> &str {
        path.strip_prefix("/private")
            .filter(|_| path.starts_with("/private/"))
            .unwrap_or(path)
    }
    use krill_proto::os::fs::OpenOptionsExt as _;
    use std::io::{Read as _, Write as _};
    let file_path = state_dir.join("projects.toml");
    let Ok(_guard) = krill_proto::config_document::lock_toml(&file_path) else {
        return;
    };
    let Ok(file) = fs::File::open(&file_path) else {
        return;
    };
    let mut raw = String::new();
    if file.take(512 * 1024 + 1).read_to_string(&mut raw).is_err() || raw.len() > 512 * 1024 {
        return;
    }
    let Ok(mut doc) = toml::from_str::<toml::Value>(&raw) else {
        return;
    };
    let Some(root) = doc.as_table_mut() else {
        return;
    };
    let removed_str = removed.to_string_lossy();
    let removed_trimmed = removed_str.trim();
    let removed_norm = alias(removed_trimmed);
    let matches_removed = |candidate: &str| {
        krill_proto::path::equivalent(Path::new(alias(candidate.trim())), Path::new(removed_norm))
    };
    let mut changed = false;
    if let Some(toml::Value::Table(worktrees)) = root.get_mut("worktrees") {
        let before = worktrees.len();
        worktrees.retain(|k, _| !matches_removed(k));
        changed |= worktrees.len() != before;
    }
    if let Some(toml::Value::Table(lineage)) = root.get_mut("lineage") {
        let before = lineage.len();
        lineage.retain(|k, v| {
            let wp = v
                .as_table()
                .and_then(|t| t.get("worktree_path"))
                .and_then(toml::Value::as_str)
                .unwrap_or(k);
            !(matches_removed(k) || matches_removed(wp))
        });
        changed |= lineage.len() != before;
    }
    if changed && let Ok(body) = toml::to_string_pretty(&doc) {
        let tmp = state_dir.join(format!(".projects.toml.rm.tmp.{}", std::process::id()));
        let write = || -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(body.as_bytes())?;
            file.sync_all()?;
            fs::rename(&tmp, &file_path)
        };
        if write().is_err() {
            fs::remove_file(&tmp).ok();
        }
    }
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

fn unlink_symlinks(wt_real: &Path, symlinks: &[String]) -> Vec<(PathBuf, PathBuf)> {
    let mut removed = Vec::new();
    for raw in symlinks {
        if is_valid_worktree_symlink(wt_real, raw) {
            let trimmed = raw.trim().trim_end_matches('/');
            let rel = Path::new(trimmed);
            let link = wt_real.join(rel);
            let target = fs::read_link(&link).ok();
            if fs::remove_file(&link).is_ok()
                && let Some(target) = target
            {
                removed.push((link, target));
            }
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
    removed
}

/// 삭제가 실패해 워크트리가 남으면 끊어 둔 심링크를 되살린다.
fn restore_symlinks(wt_real: &Path, removed: &[(PathBuf, PathBuf)]) {
    if !wt_real.is_dir() {
        return;
    }
    for (link, target) in removed {
        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent).ok();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).ok();
        #[cfg(windows)]
        {
            if target.is_dir() {
                std::os::windows::fs::symlink_dir(target, link).ok();
            } else {
                std::os::windows::fs::symlink_file(target, link).ok();
            }
        }
    }
}

fn worktree_is_clean(q: &GitQueue, repo: &Path, wt: &Path) -> bool {
    git(
        q,
        repo,
        [
            OsString::from("-C"),
            wt.as_os_str().to_owned(),
            OsString::from("status"),
            OsString::from("--porcelain"),
            OsString::from("-z"),
            OsString::from("--untracked-files=all"),
            OsString::from("--ignored=matching"),
        ],
    )
    .is_ok_and(|out| out.status.success() && out.stdout.is_empty())
}

fn run_archive_hook(q: &GitQueue, wt: &Path, script: &str) -> Result<(), String> {
    let envs = super::compat::environment_in(q, wt)?;
    let shell = super::script_shell(q, wt)?;
    let mut cmd = std::process::Command::new(shell);
    crate::child::own_group(&mut cmd)
        .arg("-c")
        .arg(script)
        .current_dir(wt)
        .env("PATH", OsStr::from_bytes(crate::shell::login_path()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(OsStr::from_bytes(&k), OsStr::from_bytes(&v));
    }
    let out = run_bounded_command(&mut cmd, ARCHIVE_HOOK_TIMEOUT)?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let raw_detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", out.status)
    };
    let detail = truncate_chars(&raw_detail, MAX_ARCHIVE_ERROR_CHARS);
    Err(format!("archive hook failed: {detail}"))
}

fn run_bounded_command(
    cmd: &mut std::process::Command,
    timeout: Duration,
) -> Result<Output, String> {
    let mut child =
        crate::child::spawn(cmd).map_err(|e| format!("archive hook failed to start: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "archive hook failed: missing stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "archive hook failed: missing stderr".to_owned())?;
    let mut stdout = PipeReader::new(stdout).map_err(|e| format!("archive hook failed: {e}"))?;
    let mut stderr = PipeReader::new(stderr).map_err(|e| format!("archive hook failed: {e}"))?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut buf = [0u8; 8192];
    let deadline = Instant::now() + timeout;
    loop {
        let a = drain_capped(&mut stdout, &mut out, &mut buf);
        let b = drain_capped(&mut stderr, &mut err, &mut buf);
        if let Err(e) = a.and(b) {
            return Err(format!("archive hook failed: {e}"));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                drain_capped(&mut stdout, &mut out, &mut buf).ok();
                drain_capped(&mut stderr, &mut err, &mut buf).ok();
                return Ok(Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            Ok(None) => {}
            Err(e) => return Err(format!("archive hook failed: {e}")),
        }
        if Instant::now() >= deadline {
            terminate_gracefully(&mut child);
            return Err("archive hook failed: timed out".to_owned());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn drain_capped(pipe: &mut impl io::Read, dst: &mut Vec<u8>, buf: &mut [u8]) -> io::Result<()> {
    let mut read = 0;
    while read < super::super::DRAIN_BYTES {
        match pipe.read(buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                read += n;
                let remaining = MAX_ARCHIVE_OUTPUT_BYTES.saturating_sub(dst.len());
                if remaining > 0 {
                    dst.extend(buf.iter().take(n.min(remaining)).copied());
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn terminate_gracefully(child: &mut Child) {
    crate::child::terminate_gracefully(child, ARCHIVE_TERMINATE_GRACE);
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}… (truncated)")
    } else {
        head
    }
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
    let mut listed = parse_entries(&out.stdout)?;
    for entry in &mut listed {
        match fs::canonicalize(&entry.path) {
            Ok(real) => entry.path = real,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot resolve registered worktree: {e}")),
        }
    }
    Ok(listed)
}

fn parse_entries(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for field in bytes.split(|b| *b == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            entries.push(Entry {
                path: {
                    let path = PathBuf::from(OsString::from_vec(path.to_vec()));
                    fs::canonicalize(&path).unwrap_or(path)
                },
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

    #[test]
    fn projects_prune_removes_only_deleted_worktree_and_preserves_future_metadata() {
        let dir = std::env::temp_dir().join(format!("krill-projects-prune-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("projects.toml");
        fs::write(
            &path,
            r#"
future = "keep"
[worktrees."/tmp/deleted"]
name = "deleted"
[worktrees."/privateer/tmp/deleted"]
name = "unrelated"
[lineage.child]
worktree_path = "/tmp/deleted"
[lineage.other]
worktree_path = "/tmp/other"
"#,
        )
        .unwrap();
        prune_removed_worktree_from_projects(&dir, Path::new("/private/tmp/deleted"));
        let doc: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["future"].as_str(), Some("keep"));
        let worktrees = doc["worktrees"].as_table().unwrap();
        assert!(!worktrees.contains_key("/tmp/deleted"));
        assert!(worktrees.contains_key("/privateer/tmp/deleted"));
        let lineage = doc["lineage"].as_table().unwrap();
        assert!(!lineage.contains_key("child"));
        assert!(lineage.contains_key("other"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    fn fixture_worktree(tag: &str) -> (super::super::tests::Fixture, PathBuf, RemoveWorktree) {
        let f = super::super::tests::Fixture::new(tag, "");
        let wt = f.root.join("child");
        fs::create_dir_all(&f.root).unwrap();
        f.git(&["worktree", "add", "-b", "child", wt.to_str().unwrap()]);
        let wt = fs::canonicalize(wt).unwrap();
        let req = RemoveWorktree {
            repo: f.repo.as_os_str().as_bytes().to_vec(),
            path: wt.as_os_str().as_bytes().to_vec(),
            force: true,
        };
        (f, wt, req)
    }

    #[cfg(unix)]
    fn orphan_metadata(wt: &Path) -> PathBuf {
        PathBuf::from(
            fs::read_to_string(wt.join(".git"))
                .unwrap()
                .trim()
                .strip_prefix("gitdir: ")
                .unwrap(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn worktree_orphan_recovery_requires_force_when_status_cannot_be_checked() {
        let (f, wt, mut req) = fixture_worktree("orphan-status");
        fs::remove_dir_all(orphan_metadata(&wt)).unwrap();
        fs::write(wt.join("untracked"), "preserve unless forced").unwrap();
        req.force = false;
        assert!(
            remove_with_state(&f.queue, &req, None)
                .unwrap_err()
                .contains("cannot check worktree status")
        );
        assert!(wt.join("untracked").exists());
        req.force = true;
        let result = remove_with_state(&f.queue, &req, None).unwrap();
        assert!(!wt.exists());
        assert_eq!(result.branch, None); // No reliable branch identity after metadata loss.
        assert!(f.git(&["branch", "--list", "child"]).contains("child"));
    }

    #[cfg(unix)]
    #[test]
    fn worktree_unlisted_but_checkable_tree_still_requires_clean_status() {
        use std::os::unix::fs::PermissionsExt;
        let (f, wt, mut req) = fixture_worktree("unlisted-clean");
        req.force = false;
        let script = f.dir.join("git-wrapper");
        let repo = fs::canonicalize(&f.repo).unwrap();
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
case " $* " in
  *" worktree list "*) printf 'worktree %s\000\000' {}; exit 0 ;;
esac
exec git "$@"
"#,
                krill_proto::agent::hooks::shell_quote(repo.to_str().unwrap())
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let queue = GitQueue::with_program(script, 1).unwrap();
        fs::write(wt.join("untracked"), "keep").unwrap();
        assert!(
            remove_with_state(&queue, &req, None)
                .unwrap_err()
                .contains("dirty")
        );
        assert!(wt.join("untracked").exists());
        fs::remove_file(wt.join("untracked")).unwrap();
        assert!(remove_with_state(&queue, &req, None).is_ok());
        assert!(!wt.exists());
    }

    #[cfg(unix)]
    #[test]
    fn worktree_orphan_never_deletes_unregistered_folders_or_foreign_links() {
        let (f, wt, req) = fixture_worktree("orphan-unowned");
        let metadata = orphan_metadata(&wt);
        fs::remove_dir_all(&metadata).unwrap();
        fs::remove_file(wt.join(".git")).unwrap();
        assert!(remove_with_state(&f.queue, &req, None).is_err());
        fs::create_dir(wt.join(".git")).unwrap();
        assert!(remove_with_state(&f.queue, &req, None).is_err());
        fs::remove_dir(wt.join(".git")).unwrap();
        for bad in [
            format!(
                "gitdir: {}\n",
                f.dir.join("foreign/.git/worktrees/child").display()
            ),
            format!("gitdir: {}\n", metadata.join("nested").display()),
            format!("gitdir: {}/../child\n", metadata.display()),
            format!("gitdir: {}\nextra\n", metadata.display()),
        ] {
            fs::write(wt.join(".git"), bad).unwrap();
            assert!(remove_with_state(&f.queue, &req, None).is_err());
            assert!(wt.exists());
        }
        let outside = f.dir.join("link-file");
        fs::write(&outside, format!("gitdir: {}\n", metadata.display())).unwrap();
        fs::remove_file(wt.join(".git")).unwrap();
        std::os::unix::fs::symlink(&outside, wt.join(".git")).unwrap();
        assert!(remove_with_state(&f.queue, &req, None).is_err());
        assert!(outside.exists());
        assert!(wt.exists());
    }

    #[cfg(unix)]
    #[test]
    fn worktree_recovery_rejects_registered_nested_worktree_even_with_force() {
        let (f, wt, req) = fixture_worktree("orphan-nested");
        let nested = wt.join("nested");
        f.git(&["worktree", "add", "-b", "nested", nested.to_str().unwrap()]);
        assert!(
            remove_with_state(&f.queue, &req, None)
                .unwrap_err()
                .contains("another registered worktree")
        );
        fs::remove_dir_all(orphan_metadata(&wt)).unwrap();
        assert!(
            remove_with_state(&f.queue, &req, None)
                .unwrap_err()
                .contains("another registered worktree")
        );
        assert!(nested.join(".git").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn worktree_same_call_partial_failure_recovers_before_prune_and_branch_delete() {
        use std::os::unix::fs::PermissionsExt;
        for force in [false, true] {
            let (f, wt, mut req) = fixture_worktree(if force {
                "partial-force"
            } else {
                "partial-clean"
            });
            req.force = force;
            let metadata = orphan_metadata(&wt);
            let script = f.dir.join("git-wrapper");
            let quoted = |p: &Path| krill_proto::agent::hooks::shell_quote(p.to_str().unwrap());
            // Simulate Git unregistering the tree before failing to remove its directory.
            // Assert that later cleanup commands cannot run while the directory exists.
            fs::write(
                &script,
                format!(
                    r#"#!/bin/sh
case " $* " in
  *" worktree remove "*) rm -rf -- {}; exit 1 ;;
  *" worktree prune "*|*" branch -d "*) test ! -e {} || exit 99 ;;
esac
exec git "$@"
"#,
                    quoted(&metadata),
                    quoted(&wt)
                ),
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
            let queue = GitQueue::with_program(script, 1).unwrap();
            let result = remove_with_state(&queue, &req, None).unwrap();
            assert!(!wt.exists());
            assert!(result.branch_deleted, "{result:?}");
            assert!(result.warnings.is_empty(), "{result:?}");
            assert!(f.git(&["branch", "--list", "child"]).is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn worktree_failed_recovery_preserves_branch_and_skips_prune() {
        use std::os::unix::fs::PermissionsExt;
        let (f, wt, req) = fixture_worktree("partial-no-link");
        let metadata = orphan_metadata(&wt);
        let script = f.dir.join("git-wrapper");
        let marker = f.dir.join("premature-cleanup");
        let quoted = |p: &Path| krill_proto::agent::hooks::shell_quote(p.to_str().unwrap());
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
case " $* " in
  *" worktree remove "*) rm -rf -- {}; rm -- {}; exit 1 ;;
  *" worktree prune "*|*" branch -d "*) touch {}; exit 99 ;;
esac
exec git "$@"
"#,
                quoted(&metadata),
                quoted(&wt.join(".git")),
                quoted(&marker)
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let queue = GitQueue::with_program(script, 1).unwrap();
        assert!(remove_with_state(&queue, &req, None).is_err());
        assert!(wt.exists());
        assert!(!marker.exists());
        assert!(f.git(&["branch", "--list", "child"]).contains("child"));
    }

    proptest! {
        #[test]
        fn arbitrary_worktree_output_and_control_bodies_do_not_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            drop(parse_entries(&bytes));
            drop(Request::decode(krill_proto::control::kind::REQUEST, &bytes));
            drop(Reply::decode(krill_proto::control::kind::REPLY, &bytes));
        }
    }
}
