//! MK-090: process-wide gh pacing shared by every window, refresh and create action.
//! No remote writes happen on refresh; creation opens gh's browser form only on user action.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui::{Context, Entity, EventEmitter, Task, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::{Disableable as _, Icon, Sizable as _};
use serde::Deserialize;

use crate::i18n::catalog::pull_request as text;
pub use crate::i18n::catalog::pull_request::{CREATE, FAILED, MISSING, PACED};

pub mod checks;
pub use checks::*;

const WINDOW: Duration = Duration::from_secs(300);
const GAP: Duration = Duration::from_secs(10);
const REFRESH: Duration = Duration::from_secs(60);
pub const MAX_CONCURRENT: usize = 4;
pub(super) const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Missing,
    Failed,
    NoPr,
    Open,
    Draft,
    Merged,
    Closed,
}

impl Status {
    pub fn badge(&self) -> Option<&'static str> {
        match self {
            Self::Open => Some(text::OPEN.get()),
            Self::Draft => Some(text::DRAFT.get()),
            Self::Merged => Some(text::MERGED.get()),
            Self::Closed => Some(text::CLOSED.get()),
            _ => None,
        }
    }

    pub fn guidance(&self) -> Option<&'static str> {
        match self {
            Self::Missing => Some(MISSING.get()),
            Self::Failed => Some(FAILED.get()),
            _ => None,
        }
    }
}

#[derive(Default)]
struct Budget(VecDeque<Instant>);
impl Budget {
    fn reserve(&mut self, now: Instant) -> bool {
        while self
            .0
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= WINDOW)
        {
            self.0.pop_front();
        }
        if self.0.len() >= 20
            || self
                .0
                .back()
                .is_some_and(|t| now.saturating_duration_since(*t) < GAP)
        {
            return false;
        }
        self.0.push_back(now);
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Refresh,
    Create,
}

/// OWE-738(MK-184): AI 생성 또는 사용자 입력 PR 초안(제목 + 마크다운 본문).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    pub body: String,
}

pub trait Runner: Send + Sync {
    fn run(&self, folder: &Path, branch: &str, action: Action) -> io::Result<Status>;

    fn create_pr(
        &self,
        folder: &Path,
        branch: &str,
        title: &str,
        body: &str,
    ) -> io::Result<Status> {
        let _ = (title, body);
        self.run(folder, branch, Action::Create)
    }

    fn generate_draft(&self, folder: &Path, branch: &str) -> io::Result<Draft> {
        generate_pr_draft(folder, branch)
    }

    fn inspect_pr(&self, folder: &Path, branch: &str) -> io::Result<PrInspection> {
        let _ = (folder, branch);
        Ok(PrInspection::default())
    }
}

struct Entry {
    branch: String,
    at: Instant,
    status: Status,
}
#[derive(Default)]
struct State {
    budget: Budget,
    in_flight: usize,
    cache: BTreeMap<PathBuf, Entry>,
}
impl State {
    fn reserve_refresh(&mut self, now: Instant) -> bool {
        if self.in_flight >= MAX_CONCURRENT || !self.budget.reserve(now) {
            return false;
        }
        self.in_flight += 1;
        true
    }

    fn reserve_interactive(&mut self) -> bool {
        if self.in_flight >= MAX_CONCURRENT {
            return false;
        }
        self.in_flight += 1;
        true
    }
}

struct SlotGuard<'a>(&'a Mutex<State>);
impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.in_flight = state.in_flight.saturating_sub(1);
        }
    }
}

pub struct Service {
    state: Mutex<State>,
    runner: Arc<dyn Runner>,
}
impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service").finish_non_exhaustive()
    }
}

impl Service {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            state: Mutex::new(State::default()),
            runner,
        }
    }

    fn reserve_slot(&self, _now: Instant) -> Result<SlotGuard<'_>, &'static str> {
        let mut state = self.state.lock().map_err(|_| FAILED.get())?;
        if !state.reserve_interactive() {
            return Err(PACED.get());
        }
        drop(state);
        Ok(SlotGuard(&self.state))
    }

    /// Refresh at most one due branch; oldest entries win so large sidebars do not starve.
    pub fn refresh(
        &self,
        branches: &BTreeMap<PathBuf, String>,
        now: Instant,
    ) -> BTreeMap<PathBuf, Status> {
        let candidate = {
            let Ok(mut state) = self.state.lock() else {
                return BTreeMap::new();
            };
            state.cache.retain(|path, entry| {
                branches.get(path).map_or_else(
                    || now.saturating_duration_since(entry.at) < WINDOW * 3,
                    |branch| branch == &entry.branch,
                )
            });
            let candidate = branches
                .iter()
                .filter(|(path, _)| {
                    state
                        .cache
                        .get(*path)
                        .is_none_or(|entry| now.saturating_duration_since(entry.at) >= REFRESH)
                })
                .min_by_key(|(path, _)| state.cache.get(*path).map(|entry| entry.at))
                .map(|(path, branch)| (path.clone(), branch.clone()));
            candidate
                .filter(|_| state.reserve_refresh(now))
                .map(|(path, branch)| {
                    state
                        .cache
                        .entry(path.clone())
                        .and_modify(|e| e.at = now)
                        .or_insert(Entry {
                            branch: branch.clone(),
                            at: now,
                            status: Status::NoPr,
                        });
                    (path, branch, SlotGuard(&self.state))
                })
        };
        if let Some((path, branch, _slot)) = candidate {
            let status = self
                .runner
                .run(&path, &branch, Action::Refresh)
                .unwrap_or_else(|e| status_error(&e));
            if let Ok(mut state) = self.state.lock()
                && let Some(entry) = state.cache.get_mut(&path)
                && entry.branch == branch
            {
                entry.status = status;
            }
        }
        self.state
            .lock()
            .map(|state| {
                state
                    .cache
                    .iter()
                    .filter(|(p, _)| branches.contains_key(*p))
                    .map(|(p, e)| (p.clone(), e.status.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn create(&self, folder: &Path, branch: &str, now: Instant) -> Result<(), &'static str> {
        let _slot = self.reserve_slot(now)?;
        self.runner
            .run(folder, branch, Action::Create)
            .map(|_| ())
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    MISSING.get()
                } else {
                    FAILED.get()
                }
            })
    }

    pub fn create_with_draft(
        &self,
        folder: &Path,
        branch: &str,
        title: &str,
        body: &str,
        now: Instant,
    ) -> Result<(), &'static str> {
        let _slot = self.reserve_slot(now)?;
        self.runner
            .create_pr(folder, branch, title, body)
            .map(|_| ())
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    MISSING.get()
                } else {
                    FAILED.get()
                }
            })
    }

    pub fn generate_draft(&self, folder: &Path, branch: &str) -> Result<Draft, String> {
        self.runner
            .generate_draft(folder, branch)
            .map_err(|e| e.to_string())
    }

    pub fn inspect(&self, folder: &Path, branch: &str) -> Result<PrInspection, &'static str> {
        self.inspect_at(folder, branch, Instant::now())
    }

    pub fn inspect_at(
        &self,
        folder: &Path,
        branch: &str,
        now: Instant,
    ) -> Result<PrInspection, &'static str> {
        let _slot = self.reserve_slot(now)?;
        self.runner.inspect_pr(folder, branch).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                MISSING.get()
            } else {
                FAILED.get()
            }
        })
    }
}

fn status_error(error: &io::Error) -> Status {
    if error.kind() == io::ErrorKind::NotFound {
        Status::Missing
    } else {
        Status::Failed
    }
}

pub fn has_github_remote(folder: &Path) -> bool {
    let mut cmd = Command::new("git");
    cmd.current_dir(folder)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["config", "--get-regexp", r"^remote\..*\.(url|gh-resolved)$"]);
    let Ok(out) = execute(&mut cmd, TIMEOUT) else {
        return false;
    };
    has_github_remote_config(&String::from_utf8_lossy(&out))
}

pub(crate) fn has_github_remote_config(config_output: &str) -> bool {
    let custom_host = std::env::var("GH_HOST")
        .ok()
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty());
    config_output.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() {
            return false;
        }
        let Some((key, value)) = line.split_once(char::is_whitespace) else {
            return false;
        };
        let value = value.trim().to_ascii_lowercase();
        if key.ends_with(".gh-resolved") {
            return !value.is_empty() && value != "false";
        }
        value.contains("github")
            || custom_host
                .as_deref()
                .is_some_and(|host| value.contains(host))
    })
}

pub fn shared() -> Arc<Service> {
    static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();
    SERVICE
        .get_or_init(|| Arc::new(Service::new(Arc::new(Gh))))
        .clone()
}

struct Gh;
impl Runner for Gh {
    fn run(&self, folder: &Path, branch: &str, action: Action) -> io::Result<Status> {
        if action == Action::Refresh && !has_github_remote(folder) {
            return Ok(Status::NoPr);
        }
        let bytes = execute(&mut command(folder, branch, action), TIMEOUT)?;
        if action == Action::Create {
            Ok(Status::NoPr)
        } else {
            parse(&bytes)
        }
    }

    fn create_pr(
        &self,
        folder: &Path,
        branch: &str,
        title: &str,
        body: &str,
    ) -> io::Result<Status> {
        let mut cmd = create_command(folder, branch, title, body);
        execute(&mut cmd, TIMEOUT)?;
        Ok(Status::NoPr)
    }

    fn inspect_pr(&self, folder: &Path, branch: &str) -> io::Result<PrInspection> {
        checks::inspect_with_gh(folder, branch)
    }
}

pub(super) fn find_executable(name: &str, fallbacks: &[&str]) -> PathBuf {
    let exe = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|p| p.join(name))
                .find(|p| executable(p))
        })
        .or_else(|| fallbacks.iter().map(PathBuf::from).find(|p| executable(p)))
        .unwrap_or_else(|| name.into());
    std::fs::canonicalize(&exe).unwrap_or(exe)
}

fn command(folder: &Path, branch: &str, action: Action) -> Command {
    // Finder's PATH may omit Homebrew. Still prefer the user's PATH installation.
    let executable = find_executable("gh", &["/opt/homebrew/bin/gh", "/usr/local/bin/gh"]);
    let mut cmd = Command::new(executable);
    cmd.current_dir(folder)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GH_REPO")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    if action == Action::Create {
        cmd.args(["pr", "create", "--web", "--head", branch]);
    } else {
        cmd.args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "all",
            "--limit",
            "1",
            "--json",
            "state,isDraft",
        ]);
    }
    cmd
}

fn create_command(folder: &Path, branch: &str, title: &str, body: &str) -> Command {
    let mut cmd = command(folder, branch, Action::Create);
    let title = title.trim();
    if !title.is_empty() {
        cmd.args(["--title", title]);
    }
    let body = body.trim();
    if !body.is_empty() {
        cmd.args(["--body", body]);
    }
    cmd
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}
pub(super) fn execute(cmd: &mut Command, timeout: Duration) -> io::Result<Vec<u8>> {
    let mut child = Reap(
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing gh stdout"))?;
    // Nonblocking pipe reads avoid pipe backpressure and reader threads surviving a timeout.
    rustix::fs::fcntl_setfl(&stdout, rustix::fs::OFlags::NONBLOCK)?;
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    loop {
        let mut buf = [0; 4096];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    bytes.extend_from_slice(&buf[..n]);
                    if bytes.len() as u64 > MAX_OUTPUT {
                        return Err(io::Error::other("gh output too large"));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        if let Some(status) = child.0.try_wait()? {
            if !status.success() {
                return Err(io::Error::other("gh failed"));
            }
            // The direct child exited; a retained descendant pipe must never block the caller.
            stdout.take(MAX_OUTPUT + 1).read_to_end(&mut bytes)?;
            return if bytes.len() as u64 <= MAX_OUTPUT {
                Ok(bytes)
            } else {
                Err(io::Error::other("gh output too large"))
            };
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "gh timed out"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Deserialize)]
struct PullRequest {
    state: String,
    #[serde(rename = "isDraft")]
    draft: bool,
}
fn parse(bytes: &[u8]) -> io::Result<Status> {
    let prs: Vec<PullRequest> = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    match prs.first() {
        None => Ok(Status::NoPr),
        Some(pr) => match pr.state.as_str() {
            "OPEN" if pr.draft => Ok(Status::Draft),
            "OPEN" => Ok(Status::Open),
            "MERGED" => Ok(Status::Merged),
            "CLOSED" => Ok(Status::Closed),
            _ => Err(io::Error::other("unknown PR state")),
        },
    }
}

const AI_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_DIFF_CHARS: usize = 8_000;

pub(crate) fn git_output(folder: &Path, args: &[&str]) -> io::Result<String> {
    let exe = find_executable(
        "git",
        &[
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
            "/usr/bin/git",
        ],
    );
    let mut cmd = Command::new(exe);
    cmd.current_dir(folder)
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    let bytes = execute(&mut cmd, TIMEOUT)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("\n...[truncated]");
            break;
        }
        out.push(ch);
    }
    out
}

/// Runs a non-interactive text generation prompt against a local AI CLI (`claude`, `codex`, `gemini`)
/// or an explicit override in `KRILL_AI_CLI` (used by deterministic subprocess tests).
pub(crate) fn run_local_agent_prompt(folder: &Path, prompt: &str) -> io::Result<String> {
    if let Some(custom) = std::env::var_os("KRILL_AI_CLI")
        && !custom.is_empty()
    {
        let mut cmd = Command::new(custom);
        cmd.current_dir(folder).arg(prompt);
        let bytes = execute(&mut cmd, AI_TIMEOUT)?;
        let out = String::from_utf8_lossy(&bytes).trim().to_owned();
        if !out.is_empty() {
            return Ok(out);
        }
        return Err(io::Error::other("empty AI CLI output"));
    }
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "offline mode enabled",
        ));
    }
    let candidates: [(&str, &[&str]); 3] = [
        (
            "claude",
            &["/opt/homebrew/bin/claude", "/usr/local/bin/claude"],
        ),
        (
            "codex",
            &["/opt/homebrew/bin/codex", "/usr/local/bin/codex"],
        ),
        (
            "gemini",
            &["/opt/homebrew/bin/gemini", "/usr/local/bin/gemini"],
        ),
    ];
    for (name, fallbacks) in candidates {
        let exe = find_executable(name, fallbacks);
        if !executable(&exe) {
            continue;
        }
        let mut cmd = Command::new(exe);
        cmd.current_dir(folder)
            .env("CI", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        match name {
            "claude" => {
                cmd.args(["-p", prompt, "--output-format", "text"]);
            }
            "codex" => {
                cmd.args(["exec", "--skip-git-repo-check", prompt]);
            }
            _ => {
                cmd.args(["-p", prompt]);
            }
        }
        if let Ok(bytes) = execute(&mut cmd, AI_TIMEOUT) {
            let text = String::from_utf8_lossy(&bytes).trim().to_owned();
            if !text.is_empty() {
                return Ok(text);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no local agent CLI available",
    ))
}

pub fn build_pr_prompt(branch: &str, commits: &str, diff_summary: &str) -> String {
    format!(
        "Write a concise Pull Request title and Markdown body for branch `{branch}`.\n\
         Respond in the exact format:\n\
         TITLE: <one-line title>\n\
         BODY:\n\
         <markdown summary bullet points>\n\n\
         Commits:\n{}\n\n\
         Diff:\n{}",
        truncate_chars(commits.trim(), 2_000),
        truncate_chars(diff_summary.trim(), MAX_DIFF_CHARS),
    )
}

pub fn parse_pr_draft(raw: &str, fallback_branch: &str) -> Draft {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("TITLE:") {
        let (title_line, remainder) = rest.split_once('\n').unwrap_or((rest, ""));
        let body = remainder
            .trim()
            .strip_prefix("BODY:")
            .unwrap_or(remainder)
            .trim()
            .to_owned();
        let title = title_line.trim().trim_start_matches('#').trim().to_owned();
        if !title.is_empty() {
            return Draft { title, body };
        }
    }
    let mut lines = trimmed.lines();
    let first = lines
        .next()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .filter(|l| !l.is_empty())
        .unwrap_or(fallback_branch)
        .to_owned();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    Draft { title: first, body }
}

pub fn synthesize_pr_draft(branch: &str, commits: &str, diff_stat: &str) -> Draft {
    let subjects: Vec<&str> = commits
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let title = subjects
        .first()
        .copied()
        .unwrap_or(if branch.is_empty() { "Update" } else { branch })
        .to_owned();
    let mut body = String::from("## Summary\n");
    if subjects.is_empty() {
        body.push_str("- Update branch changes\n");
    } else {
        for subject in subjects.iter().take(10) {
            body.push_str("- ");
            body.push_str(subject);
            body.push('\n');
        }
    }
    let stat = diff_stat.trim();
    if !stat.is_empty() {
        body.push_str("\n## Changes\n```\n");
        body.push_str(&truncate_chars(stat, 1_500));
        body.push_str("\n```");
    }
    Draft {
        title,
        body: body.trim().to_owned(),
    }
}

pub fn generate_pr_draft(folder: &Path, branch: &str) -> io::Result<Draft> {
    let commits = git_output(
        folder,
        &[
            "log",
            "--no-color",
            "-n",
            "15",
            "--pretty=format:%s",
            "@{u}..HEAD",
        ],
    )
    .or_else(|_| {
        git_output(
            folder,
            &[
                "log",
                "--no-color",
                "-n",
                "15",
                "--pretty=format:%s",
                "origin/HEAD..HEAD",
            ],
        )
    })
    .or_else(|_| {
        git_output(
            folder,
            &["log", "--no-color", "-n", "5", "--pretty=format:%s", "HEAD"],
        )
    })
    .unwrap_or_default();
    let diff_stat = git_output(
        folder,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--stat",
            "@{u}...HEAD",
        ],
    )
    .or_else(|_| {
        git_output(
            folder,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--stat",
                "HEAD~1..HEAD",
            ],
        )
    })
    .or_else(|_| git_output(folder, &["status", "--short"]))
    .unwrap_or_default();
    let diff_patch = git_output(
        folder,
        &["diff", "--no-color", "--no-ext-diff", "@{u}...HEAD"],
    )
    .or_else(|_| {
        git_output(
            folder,
            &["diff", "--no-color", "--no-ext-diff", "HEAD~1..HEAD"],
        )
    })
    .or_else(|_| git_output(folder, &["diff", "--no-color", "--no-ext-diff", "HEAD"]))
    .unwrap_or_default();
    if commits.trim().is_empty() && diff_stat.trim().is_empty() && diff_patch.trim().is_empty() {
        return Err(io::Error::other(text::AI_EMPTY_DIFF.get()));
    }
    let combined_diff = if diff_patch.trim().is_empty() {
        diff_stat.clone()
    } else {
        format!("{}\n\n{}", diff_stat.trim(), diff_patch.trim())
    };
    let prompt = build_pr_prompt(branch, &commits, &combined_diff);
    if let Ok(raw) = run_local_agent_prompt(folder, &prompt) {
        let draft = parse_pr_draft(&raw, branch);
        if !draft.title.trim().is_empty() {
            return Ok(draft);
        }
    }
    Ok(synthesize_pr_draft(branch, &commits, &diff_stat))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Created,
    Close,
}

/// OWE-738(MK-184): PR 생성 모달 대화상자(제목·본문 입력 및 반짝이 AI 자동 작성).
pub struct Dialog {
    language: crate::i18n::Language,
    folder: PathBuf,
    branch: String,
    pub(crate) title: Entity<InputState>,
    pub(crate) body: Entity<TextareaState>,
    service: Arc<Service>,
    pub(crate) generating: bool,
    pub(crate) submitting: bool,
    pub(crate) error: Option<String>,
    task: Option<Task<()>>,
    generate_task: Option<Task<()>>,
}

impl EventEmitter<Event> for Dialog {}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dialog")
            .field("folder", &self.folder)
            .field("branch", &self.branch)
            .field("generating", &self.generating)
            .field("submitting", &self.submitting)
            .finish_non_exhaustive()
    }
}

impl Dialog {
    pub fn new(
        folder: PathBuf,
        branch: String,
        service: Arc<Service>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title =
            cx.new(|cx| InputState::new(window, cx).placeholder(text::TITLE_PLACEHOLDER.get()));
        let body =
            cx.new(|cx| TextareaState::new(window, cx).placeholder(text::BODY_PLACEHOLDER.get()));
        title.update(cx, |input, cx| input.focus(window, cx));
        Self {
            language: crate::i18n::language(),
            folder,
            branch,
            title,
            body,
            service,
            generating: false,
            submitting: false,
            error: None,
            task: None,
            generate_task: None,
        }
    }

    pub fn generate_with_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.generating || self.submitting {
            return;
        }
        self.generating = true;
        self.error = None;
        cx.notify();
        let service = Arc::clone(&self.service);
        let folder = self.folder.clone();
        let branch = self.branch.clone();
        self.generate_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { service.generate_draft(&folder, &branch) })
                .await;
            this.update_in(cx, |d, window, cx| {
                d.generating = false;
                match result {
                    Ok(draft) => {
                        d.title
                            .update(cx, |input, cx| input.set_value(draft.title, window, cx));
                        d.body
                            .update(cx, |input, cx| input.set_value(draft.body, window, cx));
                    }
                    Err(e) => {
                        d.error = Some(e);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if self.submitting || self.generating {
            return;
        }
        let title = self.title.read(cx).value().trim().to_owned();
        if title.is_empty() {
            self.error = Some(text::EMPTY_TITLE.get().to_owned());
            cx.notify();
            return;
        }
        let body = self.body.read(cx).value().trim().to_owned();
        self.submitting = true;
        self.error = None;
        cx.notify();
        let service = Arc::clone(&self.service);
        let folder = self.folder.clone();
        let branch = self.branch.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    service.create_with_draft(&folder, &branch, &title, &body, Instant::now())
                })
                .await;
            this.update(cx, |d, cx| {
                d.submitting = false;
                match result {
                    Ok(()) => {
                        cx.emit(Event::Created);
                        cx.emit(Event::Close);
                    }
                    Err(e) => {
                        d.error = Some(e.to_owned());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }
}

impl Render for Dialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language != crate::i18n::language() {
            self.language = crate::i18n::language();
            self.title.update(cx, |input, cx| {
                input.set_placeholder(text::TITLE_PLACEHOLDER.get(), window, cx);
            });
            self.body.update(cx, |input, cx| {
                input.set_placeholder(text::BODY_PLACEHOLDER.get(), window, cx);
            });
        }
        let p = crate::ui::palette(cx);
        let busy = self.generating || self.submitting;
        let ai_label = if self.generating {
            text::AI_GENERATING.get()
        } else {
            text::AI_GENERATE.get()
        };
        let submit_label = if self.submitting {
            text::SUBMITTING.get()
        } else {
            text::SUBMIT.get()
        };
        let card = div()
            .id("pr-dialog")
            .debug_selector(|| "pr-dialog".into())
            .occlude()
            .w(crate::ui::DIALOG_WIDTH)
            .max_w(window.viewport_size().width - crate::ui::SPACE_4 * 2.)
            .p_6()
            .rounded(crate::ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .capture_key_down(cx.listener(|_, e: &gpui::KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(Event::Close);
                }
            }))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(Event::Close)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_LG)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(text::MODAL_TITLE.get()),
                            )
                            .child(
                                div()
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(self.branch.clone()),
                            ),
                    )
                    .child(
                        Button::new("pr-ai-generate")
                            .debug_selector(|| "pr-ai-generate".into())
                            .small()
                            .outline()
                            .icon(Icon::default().data(crate::ui::icons::SPARKLES_SVG))
                            .label(ai_label)
                            .disabled(busy)
                            .on_click(
                                cx.listener(|d, _, window, cx| d.generate_with_ai(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(crate::ui::field_label(text::TITLE_LABEL.get(), None, p))
                            .child(Input::new(&self.title).disabled(busy)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(crate::ui::field_label(text::BODY_LABEL.get(), None, p))
                            .child(Textarea::new(&self.body).h(px(120.)).disabled(busy)),
                    )
                    .children(
                        self.error
                            .as_ref()
                            .map(|e| crate::ui::error_banner(p, crate::i18n::cached_notice(e))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("pr-dialog-close")
                            .debug_selector(|| "pr-dialog-close".into())
                            .ghost()
                            .label(text::CLOSE.get())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close))),
                    )
                    .child(
                        Button::new("pr-dialog-submit")
                            .debug_selector(|| "pr-dialog-submit".into())
                            .primary()
                            .label(submit_label)
                            .disabled(busy)
                            .on_click(cx.listener(|d, _, _, cx| d.submit(cx))),
                    ),
            );
        crate::ui::modal_backdrop(window.viewport_size(), card)
    }
}

#[cfg(test)]
mod tests;
