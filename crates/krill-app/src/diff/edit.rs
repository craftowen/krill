//! MK-088 / OWE-738(MK-184) UI adapter. Keeps the provisional gpui/diff dependency inside `diff`.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::sync::Arc;

use gpui::{AnyElement, PromptLevel};
use gpui_component::Disableable as _;
use gpui_component::button::Button;
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use krill_proto::control::{FEAT_GIT_EDIT, FEAT_GIT_SYNC};
use krill_proto::git::{Edit, MAX_MESSAGE};

use super::*;

use crate::i18n::catalog::diff_edit as text;

pub(super) trait EditSource: Send + Sync {
    fn edit(&self, repo: &Path, edit: Edit) -> io::Result<()>;

    fn discard(&self, repo: &Path, home: &Path, file: &ChangedFile) -> io::Result<()> {
        discard_worktree_file(repo, home, file)
    }

    fn supports_sync(&self) -> bool {
        true
    }

    fn has_remote(&self, repo: &Path) -> bool {
        repo_has_remote(repo)
    }

    fn branch_sync(&self, repo: &Path) -> (u64, u64) {
        read_branch_sync(repo)
    }

    fn generate_commit_message(&self, repo: &Path) -> io::Result<String> {
        generate_commit_message_from_repo(repo)
    }
}
struct Daemon(Option<HostTarget>);
impl EditSource for Daemon {
    fn edit(&self, repo: &Path, edit: Edit) -> io::Result<()> {
        #[cfg(test)]
        if self.0.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "daemon disabled in unit tests",
            ));
        }
        let hello = daemon::client_hello(ClientRole::App, "krill-app-git");
        let target = match &self.0 {
            Some(target) => target.clone(),
            None => HostTarget::local_dev()?,
        };
        let conn = target.connect(&hello).map_err(io::Error::other)?;
        let mut sock = conn.stream;
        // The queue allows 30 seconds per git child; include repository discovery and hooks.
        sock.set_read_timeout(Some(Duration::from_secs(120)))?;
        sock.set_write_timeout(Some(TIMEOUT))?;
        exchange(
            conn.negotiated.features,
            &mut sock,
            &Request::GitEdit {
                repo: repo.as_os_str().as_bytes().to_vec(),
                edit,
            },
        )
    }

    fn supports_sync(&self) -> bool {
        #[cfg(test)]
        if self.0.is_none() {
            return true;
        }
        let hello = daemon::client_hello(ClientRole::App, "krill-app-git");
        let target = if let Some(target) = &self.0 {
            target.clone()
        } else {
            let Ok(target) = HostTarget::local_dev() else {
                return true;
            };
            target
        };
        let Ok(conn) = target.connect(&hello) else {
            return true;
        };
        conn.negotiated.features & FEAT_GIT_SYNC != 0
    }

    fn has_remote(&self, repo: &Path) -> bool {
        #[cfg(test)]
        if self.0.is_none() && !repo.exists() {
            return true;
        }
        repo_has_remote(repo)
    }
}

fn exchange(features: u64, sock: &mut (impl Read + Write), req: &Request) -> io::Result<()> {
    let need = match req {
        Request::GitEdit { edit, .. } => edit.required_features(),
        _ => FEAT_CONTROL | FEAT_GIT_EDIT,
    };
    if features & need != need {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            text::OUTDATED.get(),
        ));
    }
    sock.write_all(&req.encode()?)?;
    let (kind, body) = ipc::read_frame(sock)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    match Reply::decode(kind, &body)? {
        Reply::GitEdited(result) => result.map_err(io::Error::other),
        Reply::Failed(failure) => Err(io::Error::other(failure.to_string())),
        _ => Err(io::Error::other(text::FAILED.get())),
    }
}

pub(super) fn repo_has_remote(repo: &Path) -> bool {
    crate::pull_request::git_output(repo, &["remote"]).is_ok_and(|out| !out.trim().is_empty())
}

pub(super) fn read_branch_sync(repo: &Path) -> (u64, u64) {
    let Ok(out) = crate::pull_request::git_output(
        repo,
        &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
    ) else {
        return (0, 0);
    };
    parse_branch_sync(&out)
}

pub(super) fn parse_branch_sync(raw: &str) -> (u64, u64) {
    let mut parts = raw.split_whitespace();
    let ahead = parts
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let behind = parts
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    (ahead, behind)
}

pub(super) fn build_commit_prompt(stat: &str, diff: &str) -> String {
    format!(
        "Write a concise git commit message for the following changes.\n\
         Respond with ONLY the commit message text (a single imperative subject line <= 72 chars, \
         optionally followed by a blank line and brief bullet points). Do not wrap in markdown code fences.\n\n\
         Stat:\n{}\n\n\
         Diff:\n{}",
        crate::pull_request::truncate_chars(stat.trim(), 1_500),
        crate::pull_request::truncate_chars(diff.trim(), 8_000),
    )
}

pub(super) fn parse_commit_message(raw: &str) -> String {
    let trimmed = raw.trim();
    let unquoted = trimmed
        .strip_prefix("```")
        .and_then(|rest| {
            let after_lang = rest.split_once('\n').map_or(rest, |(_, body)| body);
            after_lang.strip_suffix("```")
        })
        .unwrap_or(trimmed)
        .trim();
    let cleaned = unquoted
        .strip_prefix("SUBJECT:")
        .or_else(|| unquoted.strip_prefix("TITLE:"))
        .unwrap_or(unquoted)
        .trim();
    cleaned.to_owned()
}

pub(super) fn synthesize_commit_message(stat: &str, status_short: &str) -> String {
    let mut files = Vec::new();
    for line in status_short.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let path = trimmed
            .get(2..)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(trimmed);
        files.push(path);
    }
    if files.is_empty() {
        for line in stat.lines() {
            if let Some((file, _)) = line.split_once('|') {
                let name = file.trim();
                if !name.is_empty() {
                    files.push(name);
                }
            }
        }
    }
    match files.as_slice() {
        [] => "Update repository changes".to_owned(),
        [single] => format!("Update {single}"),
        [first, second] => format!("Update {first} and {second}"),
        [first, second, rest @ ..] => {
            format!("Update {first}, {second}, and {} more files", rest.len())
        }
    }
}

pub(super) fn generate_commit_message_from_repo(repo: &Path) -> io::Result<String> {
    let staged_stat = crate::pull_request::git_output(
        repo,
        &["diff", "--cached", "--no-color", "--no-ext-diff", "--stat"],
    )
    .unwrap_or_default();
    let staged_diff =
        crate::pull_request::git_output(repo, &["diff", "--cached", "--no-color", "--no-ext-diff"])
            .unwrap_or_default();
    let (stat, diff) = if !staged_stat.trim().is_empty() || !staged_diff.trim().is_empty() {
        (staged_stat, staged_diff)
    } else {
        let work_stat = crate::pull_request::git_output(
            repo,
            &["diff", "--no-color", "--no-ext-diff", "--stat"],
        )
        .unwrap_or_default();
        let work_diff =
            crate::pull_request::git_output(repo, &["diff", "--no-color", "--no-ext-diff"])
                .unwrap_or_default();
        (work_stat, work_diff)
    };
    let status_short =
        crate::pull_request::git_output(repo, &["status", "--short"]).unwrap_or_default();
    if stat.trim().is_empty() && diff.trim().is_empty() && status_short.trim().is_empty() {
        return Err(io::Error::other(text::AI_EMPTY_DIFF.get()));
    }
    let prompt = build_commit_prompt(
        if stat.trim().is_empty() {
            &status_short
        } else {
            &stat
        },
        &diff,
    );
    if let Ok(raw) = crate::pull_request::run_local_agent_prompt(repo, &prompt) {
        let msg = parse_commit_message(&raw);
        if !msg.is_empty() {
            return Ok(msg);
        }
    }
    Ok(synthesize_commit_message(&stat, &status_short))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FailureKind {
    Commit,
    CommitAndPush,
    Push,
    Pull,
    Sync,
}

impl FailureKind {
    pub fn from_edit(edit: &Edit) -> Option<Self> {
        match edit {
            Edit::Commit { .. } => Some(Self::Commit),
            Edit::CommitAndPush { .. } => Some(Self::CommitAndPush),
            Edit::Push => Some(Self::Push),
            Edit::Pull => Some(Self::Pull),
            Edit::Sync => Some(Self::Sync),
            Edit::Stage { .. } | Edit::Unstage { .. } => None,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Commit | Self::CommitAndPush => text::RECOVERY_COMMIT_FAILED.get(),
            Self::Push => text::RECOVERY_PUSH_FAILED.get(),
            Self::Pull => text::RECOVERY_PULL_FAILED.get(),
            Self::Sync => text::RECOVERY_SYNC_FAILED.get(),
        }
    }

    pub const fn operation_name(self) -> &'static str {
        match self {
            Self::Commit => "git commit",
            Self::CommitAndPush => "git commit && git push",
            Self::Push => "git push",
            Self::Pull => "git pull",
            Self::Sync => "git pull && git push",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GitFailure {
    pub kind: FailureKind,
    pub error: String,
}

pub(super) fn build_recovery_prompt(
    repo: &Path,
    kind: FailureKind,
    error: &str,
    changes: &Changes,
) -> String {
    let mut prompt = format!(
        "Git operation (`{}`) failed in worktree `{}`. Please inspect the error output below and fix the root cause. \
         Do not use destructive commands that discard user work (`git reset --hard`, `--no-verify`, etc.).\n\n\
         ### Failed command\n`{}`\n\n\
         ### Error output\n```\n{}\n```",
        kind.operation_name(),
        repo.display(),
        kind.operation_name(),
        crate::pull_request::truncate_chars(error.trim(), 4_000),
    );
    if !changes.files.is_empty() {
        prompt.push_str("\n\n### Changed files\n");
        for file in changes.files.iter().take(20) {
            let status = String::from_utf8_lossy(&file.status);
            let path = String::from_utf8_lossy(&file.path);
            writeln!(prompt, "- `{status}` {path}").ok();
        }
    }
    prompt
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyncCapability {
    Supported,
    Unsupported,
}

impl SyncCapability {
    const fn from_bool(supported: bool) -> Self {
        if supported {
            Self::Supported
        } else {
            Self::Unsupported
        }
    }
}

pub(super) fn discard_worktree_file(
    repo: &Path,
    home: &Path,
    file: &ChangedFile,
) -> io::Result<()> {
    let rel_path = Path::new(OsStr::from_bytes(&file.path));
    if rel_path.as_os_str().is_empty()
        || rel_path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid git path",
        ));
    }
    if file.status == *b"??" {
        let target = repo.join(rel_path);
        return krill_proto::usage::profiles::move_path_to_trash(home, repo, &target).map(|_| ());
    }
    let mut child = super::Reap(
        crate::platform::background_command("git")
            .current_dir(repo)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["--literal-pathspecs", "checkout", "--"])
            .arg(rel_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let stderr = child
        .0
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing git stderr"))?;
    let deadline = Instant::now() + TIMEOUT;
    let reader = crate::process_io::read(stderr, 64 * 1024, deadline)?;
    let status = loop {
        match child.0.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "git checkout timed out",
                ));
            }
            Err(e) => break Err(e),
        }
    };
    child.0.kill().ok();
    child.0.wait().ok();
    let captured = reader.finish(deadline + Duration::from_millis(50))?;
    if captured.timed_out {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "git checkout pipe timed out",
        ));
    }
    let err_bytes = captured.bytes;
    let status = status?;
    if status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&err_bytes).trim().to_owned();
        Err(io::Error::other(if msg.is_empty() {
            "git checkout failed".to_owned()
        } else {
            msg
        }))
    }
}

pub fn extract_patch_header_and_hunks(raw_patch: &str) -> (String, Vec<String>) {
    let mut header = String::new();
    let mut hunks = Vec::new();
    let mut current_hunk: Option<String> = None;
    for line in raw_patch.lines() {
        if line.starts_with("@@ ") {
            if let Some(h) = current_hunk.take() {
                hunks.push(h);
            }
            let mut h = String::new();
            h.push_str(line);
            h.push('\n');
            current_hunk = Some(h);
        } else if let Some(ref mut h) = current_hunk {
            h.push_str(line);
            h.push('\n');
        } else {
            header.push_str(line);
            header.push('\n');
        }
    }
    if let Some(h) = current_hunk {
        hunks.push(h);
    }
    (header, hunks)
}

#[must_use]
pub fn build_hunk_patch(raw_patch: &str, hunk_index: usize) -> Option<String> {
    let (header, hunks) = extract_patch_header_and_hunks(raw_patch);
    if header.trim().is_empty() {
        return None;
    }
    let hunk = hunks.get(hunk_index)?;
    let mut out = header;
    out.push_str(hunk);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

fn parse_hunk_header_starts(header_line: &str) -> Option<(usize, usize, String)> {
    let after_at = header_line.strip_prefix("@@ ")?;
    let (coords, suffix) = after_at.split_once(" @@")?;
    let mut parts = coords.split_whitespace();
    let old_part = parts.next()?.strip_prefix('-')?;
    let new_part = parts.next()?.strip_prefix('+')?;
    let old_start = old_part
        .split_once(',')
        .map_or(old_part, |(s, _)| s)
        .parse::<usize>()
        .ok()?;
    let new_start = new_part
        .split_once(',')
        .map_or(new_part, |(s, _)| s)
        .parse::<usize>()
        .ok()?;
    Some((old_start, new_start, suffix.to_owned()))
}

#[must_use]
pub fn build_line_patch(
    raw_patch: &str,
    is_reverse: bool,
    start_line: usize,
    end_line: usize,
    use_new_line_numbers: bool,
) -> Option<String> {
    let (lo, hi) = if start_line <= end_line {
        (start_line, end_line)
    } else {
        (end_line, start_line)
    };
    let (header, hunks) = extract_patch_header_and_hunks(raw_patch);
    if header.trim().is_empty() || hunks.is_empty() {
        return None;
    }
    let mut out_hunks = Vec::new();
    for hunk in &hunks {
        let mut lines = hunk.lines();
        let Some(hunk_header) = lines.next() else {
            continue;
        };
        let Some((old_start, new_start, suffix)) = parse_hunk_header_starts(hunk_header) else {
            continue;
        };
        let mut old_ln = old_start;
        let mut new_ln = new_start;
        let mut body_lines: Vec<String> = Vec::new();
        let mut changed = false;
        let mut minus_group: Vec<(usize, String, bool)> = Vec::new();
        let mut plus_group: Vec<(usize, String, bool)> = Vec::new();
        let mut last_kind: Option<char> = None;
        let flush_group = |minus_group: &mut Vec<(usize, String, bool)>,
                           plus_group: &mut Vec<(usize, String, bool)>,
                           body_lines: &mut Vec<String>,
                           changed: &mut bool| {
            if minus_group.is_empty() && plus_group.is_empty() {
                return;
            }
            let max_len = minus_group.len().max(plus_group.len());
            for i in 0..max_len {
                let m = minus_group.get(i);
                let p = plus_group.get(i);
                let selected = if use_new_line_numbers {
                    p.is_some_and(|(ln, _, _)| *ln >= lo && *ln <= hi)
                        || (p.is_none() && m.is_some_and(|(ln, _, _)| *ln >= lo && *ln <= hi))
                } else {
                    m.is_some_and(|(ln, _, _)| *ln >= lo && *ln <= hi)
                        || (m.is_none() && p.is_some_and(|(ln, _, _)| *ln >= lo && *ln <= hi))
                };
                if selected {
                    if let Some((_, text, no_nl)) = m {
                        body_lines.push(format!("-{text}"));
                        if *no_nl {
                            body_lines.push("\\ No newline at end of file".to_owned());
                        }
                        *changed = true;
                    }
                    if let Some((_, text, no_nl)) = p {
                        body_lines.push(format!("+{text}"));
                        if *no_nl {
                            body_lines.push("\\ No newline at end of file".to_owned());
                        }
                        *changed = true;
                    }
                } else if !is_reverse {
                    if let Some((_, text, no_nl)) = m {
                        body_lines.push(format!(" {text}"));
                        if *no_nl {
                            body_lines.push("\\ No newline at end of file".to_owned());
                        }
                    }
                } else if let Some((_, text, no_nl)) = p {
                    body_lines.push(format!(" {text}"));
                    if *no_nl {
                        body_lines.push("\\ No newline at end of file".to_owned());
                    }
                }
            }
            minus_group.clear();
            plus_group.clear();
        };
        for raw_line in lines {
            if let Some(rest) = raw_line.strip_prefix('-') {
                if !plus_group.is_empty() {
                    flush_group(
                        &mut minus_group,
                        &mut plus_group,
                        &mut body_lines,
                        &mut changed,
                    );
                }
                minus_group.push((old_ln, rest.to_owned(), false));
                old_ln += 1;
                last_kind = Some('-');
            } else if let Some(rest) = raw_line.strip_prefix('+') {
                plus_group.push((new_ln, rest.to_owned(), false));
                new_ln += 1;
                last_kind = Some('+');
            } else if let Some(rest) = raw_line.strip_prefix(' ') {
                flush_group(
                    &mut minus_group,
                    &mut plus_group,
                    &mut body_lines,
                    &mut changed,
                );
                body_lines.push(format!(" {rest}"));
                old_ln += 1;
                new_ln += 1;
                last_kind = Some(' ');
            } else if raw_line.starts_with('\\') {
                match last_kind {
                    Some('-') => {
                        if let Some(last) = minus_group.last_mut() {
                            last.2 = true;
                        }
                    }
                    Some('+') => {
                        if let Some(last) = plus_group.last_mut() {
                            last.2 = true;
                        }
                    }
                    Some(' ') => {
                        body_lines.push("\\ No newline at end of file".to_owned());
                    }
                    _ => {}
                }
            }
        }
        flush_group(
            &mut minus_group,
            &mut plus_group,
            &mut body_lines,
            &mut changed,
        );
        if !changed {
            continue;
        }
        let old_count = body_lines
            .iter()
            .filter(|l| l.starts_with(' ') || l.starts_with('-'))
            .count();
        let new_count = body_lines
            .iter()
            .filter(|l| l.starts_with(' ') || l.starts_with('+'))
            .count();
        let mut hunk_out =
            format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@{suffix}\n");
        for bl in body_lines {
            hunk_out.push_str(&bl);
            hunk_out.push('\n');
        }
        out_hunks.push(hunk_out);
    }
    if out_hunks.is_empty() {
        return None;
    }
    let mut out = header;
    for h in out_hunks {
        out.push_str(&h);
    }
    Some(out)
}

#[must_use]
pub fn hunk_index_for_line(
    raw_patch: &str,
    line: usize,
    use_new_line_numbers: bool,
) -> Option<usize> {
    let (_, hunks) = extract_patch_header_and_hunks(raw_patch);
    for (idx, hunk) in hunks.iter().enumerate() {
        let mut lines = hunk.lines();
        let Some(hunk_header) = lines.next() else {
            continue;
        };
        let Some((old_start, new_start, _)) = parse_hunk_header_starts(hunk_header) else {
            continue;
        };
        let mut old_ln = old_start;
        let mut new_ln = new_start;
        for raw_line in lines {
            if raw_line.starts_with('-') {
                if (!use_new_line_numbers && old_ln == line)
                    || (use_new_line_numbers && new_ln == line)
                {
                    return Some(idx);
                }
                old_ln += 1;
            } else if raw_line.starts_with('+') {
                if (use_new_line_numbers && new_ln == line)
                    || (!use_new_line_numbers && old_ln == line)
                {
                    return Some(idx);
                }
                new_ln += 1;
            } else if raw_line.starts_with(' ') {
                if (use_new_line_numbers && new_ln == line)
                    || (!use_new_line_numbers && old_ln == line)
                {
                    return Some(idx);
                }
                old_ln += 1;
                new_ln += 1;
            }
        }
    }
    None
}

pub fn apply_git_patch(
    repo: &Path,
    patch_text: &str,
    cached: bool,
    reverse: bool,
) -> io::Result<()> {
    let mut cmd = crate::platform::background_command("git");
    cmd.current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["apply", "--unidiff-zero", "--whitespace=nowarn"]);
    if cached {
        cmd.arg("--cached");
    }
    if reverse {
        cmd.arg("--reverse");
    }
    cmd.arg("-");
    let mut child = super::Reap(
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    if let Some(mut stdin) = child.0.stdin.take() {
        stdin.write_all(patch_text.as_bytes())?;
    }
    let stderr = child
        .0
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing git apply stderr"))?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.take(64 * 1024).read_to_end(&mut bytes)?;
        Ok::<_, io::Error>(bytes)
    });
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.0.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "git apply timed out",
                ));
            }
            Err(e) => break Err(e),
        }
    };
    child.0.kill().ok();
    child.0.wait().ok();
    let err_bytes = reader
        .join()
        .map_err(|_| io::Error::other("git apply reader failed"))??;
    let status = status?;
    if status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&err_bytes).trim().to_owned();
        Err(io::Error::other(if msg.is_empty() {
            "git apply failed".to_owned()
        } else {
            msg
        }))
    }
}

pub(super) struct State {
    pub source: Arc<dyn EditSource>,
    pub pr_service: Arc<crate::pull_request::Service>,
    pub busy: bool,
    pub generating: bool,
    pub has_remote: Option<bool>,
    sync_generation: u64,
    pub sync_capability: SyncCapability,
    pub ahead: u64,
    pub behind: u64,
    pub failure: Option<GitFailure>,
    pub pr_dialog: Option<(Entity<crate::pull_request::Dialog>, Subscription)>,
    language: crate::i18n::Language,
    input: Option<Entity<TextareaState>>,
    input_enter_sub: Option<Subscription>,
    pub note: String,
    task: Option<Task<()>>,
    sync_task: Option<Task<()>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            source: Arc::new(Daemon(None)),
            pr_service: crate::pull_request::shared(),
            busy: false,
            generating: false,
            // Unknown until the background probe completes; sync actions fail closed.
            has_remote: None,
            sync_generation: 0,
            sync_capability: SyncCapability::Supported,
            ahead: 0,
            behind: 0,
            failure: None,
            pr_dialog: None,
            language: crate::i18n::language(),
            input: None,
            input_enter_sub: None,
            note: String::new(),
            task: None,
            sync_task: None,
        }
    }
}

impl State {
    pub const fn has_remote(&self) -> bool {
        matches!(self.has_remote, Some(true))
    }

    pub const fn sync_supported(&self) -> bool {
        matches!(self.sync_capability, SyncCapability::Supported)
    }

    pub fn connect(&mut self, target: impl Into<HostTarget>) {
        self.sync_generation = self.sync_generation.wrapping_add(1);
        self.sync_task = None;
        self.has_remote = None;
        self.source = Arc::new(Daemon(Some(target.into())));
    }
    pub fn reset_repo(&mut self) {
        // Cancel a pending confirmation before switching its target repository. A mutation
        // already accepted by krilld still completes there; its stale UI result is discarded.
        let source = Arc::clone(&self.source);
        let pr_service = Arc::clone(&self.pr_service);
        let sync_capability = self.sync_capability;
        let sync_generation = self.sync_generation.wrapping_add(1);
        *self = Self {
            source,
            pr_service,
            sync_generation,
            sync_capability,
            ..Self::default()
        };
    }
}

impl Panel {
    pub(super) fn refresh_branch_sync(&mut self, cx: &mut Context<Self>) {
        self.editing.sync_generation = self.editing.sync_generation.wrapping_add(1);
        let generation = self.editing.sync_generation;
        let source = Arc::clone(&self.editing.source);
        let repo = self.repo.clone();
        let expected_repo = repo.clone();
        self.editing.sync_task = Some(cx.spawn(async move |this, cx| {
            let (sync_supported, has_remote, (ahead, behind)) = cx
                .background_executor()
                .spawn(async move {
                    (
                        source.supports_sync(),
                        source.has_remote(&repo),
                        source.branch_sync(&repo),
                    )
                })
                .await;
            let sync_capability = SyncCapability::from_bool(sync_supported);
            this.update(cx, |this, cx| {
                if this.repo != expected_repo || this.editing.sync_generation != generation {
                    return;
                }
                if this.editing.sync_capability != sync_capability
                    || this.editing.has_remote != Some(has_remote)
                    || this.editing.ahead != ahead
                    || this.editing.behind != behind
                {
                    this.editing.sync_capability = sync_capability;
                    this.editing.has_remote = Some(has_remote);
                    this.editing.ahead = ahead;
                    this.editing.behind = behind;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    pub(super) fn edit_file(
        &mut self,
        index: usize,
        comparison: Comparison,
        cx: &mut Context<Self>,
    ) {
        if self.editing.busy {
            return;
        }
        let Some(file) = self.changes.files.get(index) else {
            return;
        };
        if !Self::matches_comparison(file, comparison) {
            return;
        }
        let edit = match comparison {
            Comparison::Worktree => Edit::Stage {
                path: file.path.clone(),
                old_path: file.old_path.clone(),
            },
            Comparison::Index => Edit::Unstage {
                path: file.path.clone(),
                old_path: file.old_path.clone(),
            },
        };
        self.editing.busy = true;
        self.submit_edit(edit, cx);
    }

    pub(super) fn confirm_discard_file(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing.busy || self.editing.generating {
            return;
        }
        let Some(file) = self.changes.files.get(index).cloned() else {
            return;
        };
        if !Self::matches_comparison(&file, Comparison::Worktree) {
            return;
        }
        self.editing.busy = true;
        let path_display = String::from_utf8_lossy(&file.path).into_owned();
        let prompt_title = if file.status == *b"??" {
            text::CONFIRM_DISCARD_UNTRACKED.format(&[("path", &path_display)])
        } else {
            text::CONFIRM_DISCARD.format(&[("path", &path_display)])
        };
        let detail = self.repo.display().to_string();
        let answer = window.prompt(
            PromptLevel::Warning,
            &prompt_title,
            Some(&detail),
            &[text::DISCARD.get(), text::CANCEL.get()],
            cx,
        );
        self.editing.task = Some(cx.spawn(async move |this, cx| {
            let confirmed = answer.await == Ok(0);
            this.update(cx, |this, cx| {
                if confirmed {
                    this.submit_discard(file, cx);
                } else {
                    this.editing.busy = false;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn submit_discard(&mut self, file: ChangedFile, cx: &mut Context<Self>) {
        self.editing.note = text::BUSY.get().into();
        self.editing.failure = None;
        let source = Arc::clone(&self.editing.source);
        let repo = self.repo.clone();
        let home = self.explorer.home.clone();
        self.editing.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { source.discard(&repo, &home, &file) })
                .await;
            this.update(cx, |this, cx| {
                this.editing.busy = false;
                match result {
                    Ok(()) => {
                        this.editing.failure = None;
                        this.editing.note = text::DONE.get().into();
                        this.explorer.refresh();
                        this.refresh(cx);
                    }
                    Err(e) => {
                        this.editing.note = e.to_string();
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn stage_hunk(&mut self, hunk_index: usize, cx: &mut Context<Self>) {
        if self.editing.busy || self.comparison != Comparison::Worktree {
            return;
        }
        let Some(patch_text) = build_hunk_patch(&self.raw_patch, hunk_index) else {
            return;
        };
        self.submit_patch_apply(patch_text, true, false, cx);
    }

    pub fn unstage_hunk(&mut self, hunk_index: usize, cx: &mut Context<Self>) {
        if self.editing.busy || self.comparison != Comparison::Index {
            return;
        }
        let Some(patch_text) = build_hunk_patch(&self.raw_patch, hunk_index) else {
            return;
        };
        self.submit_patch_apply(patch_text, true, true, cx);
    }

    pub fn discard_hunk(&mut self, hunk_index: usize, cx: &mut Context<Self>) {
        if self.editing.busy || self.comparison != Comparison::Worktree {
            return;
        }
        let Some(patch_text) = build_hunk_patch(&self.raw_patch, hunk_index) else {
            return;
        };
        self.submit_patch_apply(patch_text, false, true, cx);
    }

    pub fn stage_selected_lines(&mut self, cx: &mut Context<Self>) {
        if self.editing.busy || self.comparison != Comparison::Worktree {
            return;
        }
        let Some(range) = self.state.read(cx).selected_lines() else {
            return;
        };
        let use_new = matches!(range.side(), gpui_component::diff::DiffSide::Modified);
        let Some(patch_text) =
            build_line_patch(&self.raw_patch, false, range.start(), range.end(), use_new)
        else {
            return;
        };
        self.submit_patch_apply(patch_text, true, false, cx);
    }

    pub fn unstage_selected_lines(&mut self, cx: &mut Context<Self>) {
        if self.editing.busy || self.comparison != Comparison::Index {
            return;
        }
        let Some(range) = self.state.read(cx).selected_lines() else {
            return;
        };
        let use_new = matches!(range.side(), gpui_component::diff::DiffSide::Modified);
        let Some(patch_text) =
            build_line_patch(&self.raw_patch, true, range.start(), range.end(), use_new)
        else {
            return;
        };
        self.submit_patch_apply(patch_text, true, true, cx);
    }

    pub fn discard_selected_lines(&mut self, cx: &mut Context<Self>) {
        if self.editing.busy || self.comparison != Comparison::Worktree {
            return;
        }
        let Some(range) = self.state.read(cx).selected_lines() else {
            return;
        };
        let use_new = matches!(range.side(), gpui_component::diff::DiffSide::Modified);
        let Some(patch_text) =
            build_line_patch(&self.raw_patch, true, range.start(), range.end(), use_new)
        else {
            return;
        };
        self.submit_patch_apply(patch_text, false, true, cx);
    }

    fn submit_patch_apply(
        &mut self,
        patch_text: String,
        cached: bool,
        reverse: bool,
        cx: &mut Context<Self>,
    ) {
        self.editing.busy = true;
        self.editing.note = text::BUSY.get().into();
        self.editing.failure = None;
        let repo = self.repo.clone();
        self.editing.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { apply_git_patch(&repo, &patch_text, cached, reverse) })
                .await;
            this.update(cx, |this, cx| {
                this.editing.busy = false;
                match result {
                    Ok(()) => {
                        this.editing.failure = None;
                        this.editing.note = text::DONE.get().into();
                        this.refresh(cx);
                    }
                    Err(e) => {
                        this.editing.note = e.to_string();
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn run_git_action(&mut self, edit: Edit, cx: &mut Context<Self>) {
        if self.editing.busy || self.editing.generating {
            return;
        }
        if edit.requires_sync() && !self.editing.sync_supported() {
            self.editing.note = text::OUTDATED.get().into();
            cx.notify();
            return;
        }
        if edit.requires_sync() && !self.editing.has_remote() {
            self.editing.note = text::NO_REMOTE.get().into();
            cx.notify();
            return;
        }
        self.editing.busy = true;
        self.submit_edit(edit, cx);
    }

    fn submit_edit(&mut self, edit: Edit, cx: &mut Context<Self>) {
        if edit.requires_sync() && !self.editing.sync_supported() {
            self.editing.busy = false;
            self.editing.note = text::OUTDATED.get().into();
            cx.notify();
            return;
        }
        if edit.requires_sync() && !self.editing.has_remote() {
            self.editing.busy = false;
            self.editing.note = text::NO_REMOTE.get().into();
            cx.notify();
            return;
        }
        self.editing.note = text::BUSY.get().into();
        self.editing.failure = None;
        let source = Arc::clone(&self.editing.source);
        let repo = self.repo.clone();
        let committed = matches!(edit, Edit::Commit { .. } | Edit::CommitAndPush { .. });
        let requires_sync = edit.requires_sync();
        let failure_kind = FailureKind::from_edit(&edit);
        self.editing.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if requires_sync && !source.supports_sync() {
                        return Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            text::OUTDATED.get(),
                        ));
                    }
                    source.edit(&repo, edit)
                })
                .await;
            this.update(cx, |this, cx| {
                this.editing.busy = false;
                match result {
                    Ok(()) => {
                        if committed {
                            this.editing.input = None;
                            this.editing.input_enter_sub = None;
                        }
                        this.editing.failure = None;
                        this.editing.note = text::DONE.get().into();
                        this.refresh_branch_sync(cx);
                        this.refresh(cx);
                    }
                    Err(e) => {
                        if requires_sync && e.kind() == io::ErrorKind::Unsupported {
                            this.editing.sync_capability = SyncCapability::Unsupported;
                        }
                        let msg = e.to_string();
                        this.editing.note.clone_from(&msg);
                        this.editing.failure =
                            failure_kind.map(|kind| GitFailure { kind, error: msg });
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn confirm_commit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_commit_inner(false, cx);
    }

    fn confirm_commit_and_push(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_commit_inner(true, cx);
    }

    fn confirm_commit_inner(&mut self, and_push: bool, cx: &mut Context<Self>) {
        if self.editing.busy
            || self.editing.generating
            || !self.has_staged()
            || (and_push && (!self.editing.sync_supported() || !self.editing.has_remote()))
        {
            return;
        }
        let Some(input) = &self.editing.input else {
            return;
        };
        let message = input.read(cx).value().to_string();
        if message.trim().is_empty() || message.len() > MAX_MESSAGE || message.contains('\0') {
            self.editing.note = text::INVALID.get().into();
            cx.notify();
            return;
        }
        self.editing.busy = true;
        let edit = if and_push {
            Edit::CommitAndPush { message }
        } else {
            Edit::Commit { message }
        };
        self.submit_edit(edit, cx);
    }

    pub(super) fn generate_commit_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.busy || self.editing.generating {
            return;
        }
        self.editing.generating = true;
        self.editing.note = text::AI_GENERATING.get().into();
        let source = Arc::clone(&self.editing.source);
        let repo = self.repo.clone();
        self.editing.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { source.generate_commit_message(&repo) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.editing.generating = false;
                match result {
                    Ok(msg) => {
                        let input = this.ensure_commit_input(window, cx);
                        input.update(cx, |input, cx| input.set_value(msg, window, cx));
                        this.editing.note.clear();
                    }
                    Err(e) => {
                        this.editing.note = e.to_string();
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn request_ai_recovery(&mut self, cx: &mut Context<Self>) {
        let Some(failure) = &self.editing.failure else {
            return;
        };
        let prompt = build_recovery_prompt(&self.repo, failure.kind, &failure.error, &self.changes);
        cx.emit(SendComments::git_recovery(prompt));
        cx.notify();
    }

    pub(super) fn open_pr_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let branch =
            crate::pull_request::git_output(&self.repo, &["rev-parse", "--abbrev-ref", "HEAD"])
                .map(|s| s.trim().to_owned())
                .ok()
                .filter(|s| !s.is_empty() && s != "HEAD")
                .unwrap_or_else(|| "HEAD".to_owned());
        let repo = self.repo.clone();
        let service = Arc::clone(&self.editing.pr_service);
        let dialog =
            cx.new(|cx| crate::pull_request::Dialog::new(repo, branch, service, window, cx));
        let sub = cx.subscribe(
            &dialog,
            |this, _, event: &crate::pull_request::Event, cx| {
                match event {
                    crate::pull_request::Event::Created => {
                        this.editing.note = text::DONE.get().into();
                    }
                    crate::pull_request::Event::Close => {
                        this.editing.pr_dialog = None;
                    }
                }
                cx.notify();
            },
        );
        self.editing.pr_dialog = Some((dialog, sub));
        cx.notify();
    }

    fn has_staged(&self) -> bool {
        self.changes
            .files
            .iter()
            .any(|file| Self::matches_comparison(file, Comparison::Index))
    }

    pub(super) fn discard_button(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(file) = self.changes.files.get(index) else {
            return div().into_any_element();
        };
        let path = file.path.clone();
        let enabled = Self::matches_comparison(file, Comparison::Worktree);
        let label = text::DISCARD.get();
        Button::new(("git-discard", index))
            .debug_selector(move || format!("git-discard-{index}"))
            .icon(Icon::default().data(crate::ui::icons::ROTATE_CCW_SVG))
            .tooltip(label)
            .accessibility_label(label)
            .xsmall()
            .ghost()
            .disabled(self.editing.busy || !enabled)
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                if this
                    .changes
                    .files
                    .get(index)
                    .is_some_and(|file| file.path == path)
                {
                    this.confirm_discard_file(index, window, cx);
                }
            }))
            .into_any_element()
    }

    pub(super) fn edit_button(
        &self,
        index: usize,
        comparison: Comparison,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(file) = self.changes.files.get(index) else {
            return div().into_any_element();
        };
        let path = file.path.clone();
        let stage = comparison == Comparison::Worktree;
        let enabled = Self::matches_comparison(file, comparison);
        let label = if stage {
            text::STAGE.get()
        } else {
            text::UNSTAGE.get()
        };
        Button::new((if stage { "git-stage" } else { "git-unstage" }, index))
            .debug_selector(move || {
                format!("git-{}-{index}", if stage { "stage" } else { "unstage" })
            })
            .icon(if stage {
                IconName::Plus
            } else {
                IconName::Minus
            })
            .tooltip(label)
            .accessibility_label(label)
            .xsmall()
            .ghost()
            .disabled(self.editing.busy || !enabled)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if this
                    .changes
                    .files
                    .get(index)
                    .is_some_and(|file| file.path == path)
                {
                    this.edit_file(index, comparison, cx);
                }
            }))
            .into_any_element()
    }

    /// Commit message input, created lazily. `submit_on_enter(true)` suppresses
    /// the native newline for `⌘Enter` (`secondary-enter`) so it submits
    /// without leaving a stray newline in the message; plain/shift `Enter`
    /// still insert exactly one newline natively. Element-level
    /// `capture_key_down` cannot intercept it: the input's `secondary-enter`
    /// binding consumes the keystroke before element handlers run, so this
    /// helper subscribes to `InputEvent::PressEnter` instead.
    fn ensure_commit_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextareaState> {
        let input = self
            .editing
            .input
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .placeholder(text::MESSAGE.get())
                        .submit_on_enter(true)
                })
            })
            .clone();
        input.update(cx, |state, cx| state.set_submit_on_enter(true, cx));
        if self.editing.input_enter_sub.is_none() {
            let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { secondary, shift } = event
                    && *secondary
                {
                    if *shift {
                        this.confirm_commit_and_push(window, cx);
                    } else {
                        this.confirm_commit(window, cx);
                    }
                }
            });
            self.editing.input_enter_sub = Some(sub);
        }
        input
    }

    /// 변경 사항 탭으로 전환하고 커밋 메시지 입력창에 포커스를 둔다.
    /// `show_files`/`show_search`와 대칭: 탭만 바꾸면 이전 탭의 입력창에
    /// 남아 있던 포커스가 언마운트와 함께 사라져 이후 단축키가 동작하지 않는다.
    pub fn show_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_tab(super::PanelTab::Changes, cx);
        let input = self.ensure_commit_input(window, cx);
        input.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    pub(super) fn render_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = crate::ui::palette(cx);
        let input = self.ensure_commit_input(window, cx);
        if self.editing.language != crate::i18n::language() {
            self.editing.language = crate::i18n::language();
            input.update(cx, |input, cx| {
                input.set_placeholder(text::MESSAGE.get(), window, cx);
            });
        }
        let busy = self.editing.busy || self.editing.generating;
        let sync_supported = self.editing.sync_supported();
        let has_remote = self.editing.has_remote();
        let remote_unknown = self.editing.has_remote.is_none();
        let sync_disabled = busy || !sync_supported || !has_remote;
        let has_staged = self.has_staged();
        let (ahead, behind) = (self.editing.ahead, self.editing.behind);
        let pull_label = if behind > 0 {
            format!("{} ↓{behind}", text::PULL.get())
        } else {
            text::PULL.get().to_owned()
        };
        let push_label = if ahead > 0 {
            format!("{} ↑{ahead}", text::PUSH.get())
        } else {
            text::PUSH.get().to_owned()
        };
        let recovery_banner = self.editing.failure.as_ref().map(|failure| {
            div()
                .id("git-recovery-banner")
                .debug_selector(|| "git-recovery-banner".into())
                .p_2()
                .rounded(crate::ui::RADIUS_SM)
                .border_1()
                .border_color(p.destructive.opacity(0.3))
                .bg(p.destructive.opacity(0.1))
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_1()
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.destructive)
                                .child(failure.kind.title()),
                        )
                        .child(
                            Button::new("git-recovery-dismiss")
                                .debug_selector(|| "git-recovery-dismiss".into())
                                .xsmall()
                                .ghost()
                                .label(text::RECOVERY_DISMISS.get())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.editing.failure = None;
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .id("git-recovery-log")
                        .debug_selector(|| "git-recovery-log".into())
                        .max_h(px(120.))
                        .overflow_y_scroll()
                        .p_1p5()
                        .rounded(crate::ui::RADIUS_XS)
                        .bg(p.background.opacity(0.6))
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(p.destructive)
                        .child(crate::i18n::cached_notice(&failure.error)),
                )
                .child(
                    div().flex().justify_end().child(
                        Button::new("git-recovery-fix")
                            .debug_selector(|| "git-recovery-fix".into())
                            .xsmall()
                            .outline()
                            .icon(Icon::default().data(crate::ui::icons::SPARKLES_SVG))
                            .label(text::RECOVERY_ASK_AI.get())
                            .on_click(cx.listener(|this, _, _, cx| this.request_ai_recovery(cx))),
                    ),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .p_2()
            .border_t_1()
            .border_color(p.sidebar_border)
            .child(
                div()
                    .id("git-commit-input")
                    .debug_selector(|| "git-commit-input".into())
                    .w_full()
                    .child(
                        Textarea::new(&input)
                            .h(crate::ui::SPACE_4 * 3.5)
                            .disabled(busy),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                Button::new("git-ai-commit")
                                    .debug_selector(|| "git-ai-commit".into())
                                    .xsmall()
                                    .ghost()
                                    .icon(Icon::default().data(crate::ui::icons::SPARKLES_SVG))
                                    .tooltip(text::AI_GENERATE.get())
                                    .accessibility_label(text::AI_GENERATE.get())
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.generate_commit_message(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("git-commit-push")
                                    .debug_selector(|| "git-commit-push".into())
                                    .xsmall()
                                    .outline()
                                    .label(text::COMMIT_AND_PUSH.get())
                                    .tooltip(if !sync_supported {
                                        text::OUTDATED.get()
                                    } else if remote_unknown {
                                        super::text::LOADING.get()
                                    } else if !has_remote {
                                        text::NO_REMOTE.get()
                                    } else {
                                        text::COMMIT_AND_PUSH.get()
                                    })
                                    .disabled(sync_disabled || !has_staged)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.confirm_commit_and_push(window, cx);
                                    })),
                            ),
                    )
                    .child(
                        Button::new("git-commit")
                            .debug_selector(|| "git-commit".into())
                            .small()
                            .label(text::COMMIT.get())
                            .primary()
                            .disabled(busy || !has_staged)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.confirm_commit(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1()
                            .child(
                                Button::new("git-pull")
                                    .debug_selector(|| "git-pull".into())
                                    .xsmall()
                                    .ghost()
                                    .label(pull_label)
                                    .tooltip(if !sync_supported {
                                        text::OUTDATED.get()
                                    } else if remote_unknown {
                                        super::text::LOADING.get()
                                    } else if !has_remote {
                                        text::NO_REMOTE.get()
                                    } else {
                                        text::BEHIND_TOOLTIP.get()
                                    })
                                    .disabled(sync_disabled)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.run_git_action(Edit::Pull, cx);
                                    })),
                            )
                            .child(
                                Button::new("git-push")
                                    .debug_selector(|| "git-push".into())
                                    .xsmall()
                                    .ghost()
                                    .label(push_label)
                                    .tooltip(if !sync_supported {
                                        text::OUTDATED.get()
                                    } else if remote_unknown {
                                        super::text::LOADING.get()
                                    } else if !has_remote {
                                        text::NO_REMOTE.get()
                                    } else {
                                        text::AHEAD_TOOLTIP.get()
                                    })
                                    .disabled(sync_disabled)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.run_git_action(Edit::Push, cx);
                                    })),
                            )
                            .child(
                                Button::new("git-sync")
                                    .debug_selector(|| "git-sync".into())
                                    .xsmall()
                                    .ghost()
                                    .label(text::SYNC.get())
                                    .tooltip(if !sync_supported {
                                        text::OUTDATED.get()
                                    } else if remote_unknown {
                                        super::text::LOADING.get()
                                    } else if !has_remote {
                                        text::NO_REMOTE.get()
                                    } else {
                                        text::SYNC.get()
                                    })
                                    .disabled(sync_disabled)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.run_git_action(Edit::Sync, cx);
                                    })),
                            )
                            .child(
                                Button::new("git-create-pr")
                                    .debug_selector(|| "git-create-pr".into())
                                    .xsmall()
                                    .ghost()
                                    .label(text::CREATE_PR.get())
                                    .tooltip(text::CREATE_PR_TOOLTIP.get())
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_pr_dialog(window, cx);
                                    })),
                            ),
                    )
                    .when(ahead > 0 || behind > 0, |d| {
                        d.child(
                            div()
                                .id("git-sync-counts")
                                .debug_selector(|| "git-sync-counts".into())
                                .child(crate::ui::badge(
                                    p,
                                    crate::ui::BadgeTone::Neutral,
                                    format!("↓{behind} ↑{ahead}"),
                                )),
                        )
                    }),
            )
            .children(recovery_banner)
            .when(!self.editing.note.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(crate::i18n::cached_notice(&self.editing.note)),
                )
            })
            .children(
                self.editing
                    .pr_dialog
                    .as_ref()
                    .map(|(dialog, _)| dialog.clone()),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
