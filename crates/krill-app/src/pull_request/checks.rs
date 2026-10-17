//! OWE-739(MK-185): PR CI Checks 조회·실패 로그 핵심 꼬리 추출·미해결 리뷰 코멘트 조회·AI 수정 프롬프트 빌더.
//!
//! - CI 상태 매핑·실패 로그 핵심 꼬리 추출·AI 수정 프롬프트 구조 참고:
//!   `.ref/orca/src/main/github/get-pr-checks.ts`, `.ref/orca/src/main/github/get-pr-comments.ts`,
//!   `.ref/orca/src/shared/check-job-log-tail-slice.ts`,
//!   `.ref/orca/src/renderer/src/components/right-sidebar/checks/pr-checks-fix-prompt.ts`,
//!   `.ref/orca/src/renderer/src/components/right-sidebar/checks/pr-comments-resolution-prompt.ts`
//!   (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use super::{Status, TIMEOUT, execute, find_executable, truncate_chars};
use crate::i18n::catalog::pull_request as text;

const MAX_LOG_TAIL_LINES: usize = 60;
const MAX_LOG_TAIL_CHARS: usize = 4_000;
const MAX_ERROR_ANCHORS: usize = 6;
const ERROR_CONTEXT_BEFORE: usize = 1;
const ERROR_CONTEXT_AFTER: usize = 3;
const MAX_FAILED_LOG_FETCHES: usize = 3;

/// CI 체크 실행의 정규화된 상태.
/// 정렬 순서(`Failed` → `Pending` → `Passed` → `Skipped`)에 따라 실패 항목이 맨 위에 온다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckState {
    Failed,
    Pending,
    Passed,
    Skipped,
}

impl CheckState {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Passed => text::CHECK_PASSED.get(),
            Self::Pending => text::CHECK_PENDING.get(),
            Self::Failed => text::CHECK_FAILED.get(),
            Self::Skipped => text::CHECK_SKIPPED.get(),
        }
    }
}

/// 개별 PR CI 체크 항목 및 추출된 실패 로그 꼬리.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckItem {
    pub name: String,
    pub state: CheckState,
    pub description: Option<String>,
    pub details_url: Option<String>,
    pub run_id: Option<String>,
    pub job_id: Option<String>,
    pub log_tail: Option<String>,
}

/// PR 리뷰 스레드 내 답글.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewReply {
    pub author: String,
    pub is_bot: bool,
    pub body: String,
    pub created_at: String,
}

/// 미해결(`!is_resolved`) PR 인라인 리뷰 코멘트 스레드.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    pub id: String,
    pub thread_id: Option<String>,
    pub author: String,
    pub is_bot: bool,
    pub body: String,
    pub path: String,
    pub line: Option<u32>,
    pub original_line: Option<u32>,
    pub diff_hunk: String,
    pub created_at: String,
    pub url: String,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub replies: Vec<ReviewReply>,
}

impl ReviewComment {
    #[must_use]
    pub fn display_line(&self) -> Option<u32> {
        self.line.or(self.original_line)
    }

    #[must_use]
    pub fn location(&self) -> String {
        match self.display_line() {
            Some(line) => format!("{}:{line}", self.path),
            None => self.path.clone(),
        }
    }
}

/// 현재 워크트리 브랜치의 PR 메타데이터·CI 체크·미해결 리뷰 코멘트 스냅샷.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInspection {
    pub status: Status,
    pub number: Option<u64>,
    pub title: String,
    pub url: String,
    pub checks: Vec<CheckItem>,
    pub review_comments: Vec<ReviewComment>,
}

impl Default for PrInspection {
    fn default() -> Self {
        Self {
            status: Status::NoPr,
            number: None,
            title: String::new(),
            url: String::new(),
            checks: Vec::new(),
            review_comments: Vec::new(),
        }
    }
}

impl PrInspection {
    #[must_use]
    pub fn count(&self, state: CheckState) -> usize {
        self.checks.iter().filter(|c| c.state == state).count()
    }
}

/// GitHub Actions 상세 URL에서 `(run_id, job_id)`를 추출한다.
#[must_use]
pub fn parse_actions_ids(url: &str) -> (Option<String>, Option<String>) {
    let run_id = url.split_once("/actions/runs/").and_then(|(_, rest)| {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (!digits.is_empty()).then_some(digits)
    });
    let job_id = url.split_once("/job/").and_then(|(_, rest)| {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (!digits.is_empty()).then_some(digits)
    });
    (run_id, job_id)
}

/// GitHub 및 GitHub Enterprise PR URL(`http(s)://<host>/.../<owner>/<repo>/pull(s)/<n>`)에서 `(owner, repo)`를 추출한다.
#[must_use]
pub fn parse_repo_owner_and_name(pr_url: &str) -> Option<(String, String)> {
    let trimmed = pr_url.trim();
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))?;
    let path_part = without_scheme
        .split_once(['?', '#'])
        .map_or(without_scheme, |(before, _)| before);
    let mut segments = path_part.split('/').filter(|s| !s.is_empty());
    let host = segments.next()?;
    if host.is_empty() {
        return None;
    }
    let path_segments: Vec<&str> = segments.collect();
    let pull_idx = path_segments
        .iter()
        .position(|&seg| seg == "pull" || seg == "pulls")?;
    if pull_idx < 2 {
        return None;
    }
    let owner = *path_segments.get(pull_idx - 2)?;
    let repo = *path_segments.get(pull_idx - 1)?;
    Some((owner.to_owned(), repo.to_owned()))
}

fn map_check_run_state(status: &str, conclusion: &str) -> CheckState {
    if !status.is_empty() && !status.eq_ignore_ascii_case("COMPLETED") {
        return CheckState::Pending;
    }
    match conclusion.trim().to_ascii_uppercase().as_str() {
        "SUCCESS" => CheckState::Passed,
        "NEUTRAL" | "SKIPPED" => CheckState::Skipped,
        "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "STALE" => {
            CheckState::Failed
        }
        _ => CheckState::Pending,
    }
}

fn map_status_context_state(state: &str, bucket: &str) -> CheckState {
    match bucket.trim().to_ascii_lowercase().as_str() {
        "pass" => return CheckState::Passed,
        "fail" | "cancel" => return CheckState::Failed,
        "pending" => return CheckState::Pending,
        "skipping" => return CheckState::Skipped,
        _ => {}
    }
    match state.trim().to_ascii_uppercase().as_str() {
        "SUCCESS" | "PASS" | "PASSED" => CheckState::Passed,
        "SKIPPED" | "NEUTRAL" => CheckState::Skipped,
        "FAILURE" | "ERROR" | "FAIL" | "FAILED" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
        | "STARTUP_FAILURE" => CheckState::Failed,
        _ => CheckState::Pending,
    }
}

#[derive(Deserialize)]
struct RawRollupEntry {
    #[serde(rename = "__typename", default)]
    typename: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    context: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "detailsUrl", default)]
    details_url: String,
    #[serde(rename = "targetUrl", default)]
    target_url: String,
    #[serde(default)]
    link: String,
}

impl RawRollupEntry {
    fn into_check_item(self) -> Option<CheckItem> {
        let is_check_run =
            self.typename == "CheckRun" || (!self.status.is_empty() || !self.conclusion.is_empty());
        let name = if !self.name.trim().is_empty() {
            self.name.trim().to_owned()
        } else if !self.context.trim().is_empty() {
            self.context.trim().to_owned()
        } else {
            return None;
        };
        let state = if is_check_run {
            map_check_run_state(&self.status, &self.conclusion)
        } else {
            map_status_context_state(&self.state, &self.bucket)
        };
        let url_raw = if !self.details_url.trim().is_empty() {
            self.details_url.trim()
        } else if !self.target_url.trim().is_empty() {
            self.target_url.trim()
        } else {
            self.link.trim()
        };
        let details_url = (!url_raw.is_empty()).then(|| url_raw.to_owned());
        let (run_id, job_id) = details_url
            .as_deref()
            .map_or((None, None), parse_actions_ids);
        let description =
            (!self.description.trim().is_empty()).then(|| self.description.trim().to_owned());
        Some(CheckItem {
            name,
            state,
            description,
            details_url,
            run_id,
            job_id,
            log_tail: None,
        })
    }
}

#[derive(Deserialize)]
struct RawPrDetail {
    #[serde(default)]
    number: Option<u64>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    state: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<RawRollupEntry>,
}

/// `gh pr list --json number,title,url,state,isDraft,statusCheckRollup` 출력을 파싱한다.
pub fn parse_checks_from_pr_json(bytes: &[u8]) -> io::Result<PrInspection> {
    let prs: Vec<RawPrDetail> = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    let Some(pr) = prs.into_iter().next() else {
        return Ok(PrInspection::default());
    };
    let status = match pr.state.as_str() {
        "OPEN" if pr.is_draft => Status::Draft,
        "OPEN" => Status::Open,
        "MERGED" => Status::Merged,
        "CLOSED" => Status::Closed,
        _ => return Err(io::Error::other("unknown PR state")),
    };
    let mut checks: Vec<CheckItem> = pr
        .status_check_rollup
        .into_iter()
        .filter_map(RawRollupEntry::into_check_item)
        .collect();
    checks.sort_by(|a, b| a.state.cmp(&b.state).then_with(|| a.name.cmp(&b.name)));
    Ok(PrInspection {
        status,
        number: pr.number,
        title: pr.title.trim().to_owned(),
        url: pr.url.trim().to_owned(),
        checks,
        review_comments: Vec::new(),
    })
}

/// ANSI CSI 이스케이프 시퀀스를 제거한다.
#[must_use]
pub fn strip_ansi_codes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn is_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains("0 failed")
        || lower.contains("failed: 0")
        || lower.contains("0 errors")
        || lower.contains("errors: 0")
    {
        return false;
    }
    lower.contains("##[error]")
        || lower.contains("::error")
        || lower.contains("error:")
        || lower.contains("error[")
        || lower.contains("fatal:")
        || lower.contains("panicked at")
        || lower.contains("test result: failed")
        || lower.contains("assertionerror")
        || lower.contains("process completed with exit code")
        || lower.contains("exit code")
        || lower.contains("failed")
        || lower.contains("err!")
}

/// CI 실패 원본 로그에서 ANSI 시퀀스를 제거하고, 앞쪽 핵심 오류 지점(문맥 포함)과
/// 마지막 꼬리 로그를 결합해 에이전트·사용자가 바로 원인을 파악할 수 있는 요약을 반환한다.
#[must_use]
pub fn extract_error_log_tail(raw_log: &str) -> String {
    let stripped = strip_ansi_codes(raw_log);
    let normalized = stripped.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized
        .lines()
        .map(str::trim_end)
        .skip_while(|l| l.trim().is_empty())
        .collect();
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |idx| idx + 1);
    let Some(lines) = lines.get(..end) else {
        return String::new();
    };
    if lines.is_empty() {
        return String::new();
    }

    let tail_start = lines.len().saturating_sub(MAX_LOG_TAIL_LINES);
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for (idx, line) in lines.iter().enumerate().take(tail_start) {
        if is_error_line(line) {
            let start = idx.saturating_sub(ERROR_CONTEXT_BEFORE);
            let finish = (idx + ERROR_CONTEXT_AFTER + 1).min(tail_start);
            if let Some(last) = windows.last_mut()
                && start <= last.1
            {
                last.1 = last.1.max(finish);
            } else if windows.len() < MAX_ERROR_ANCHORS {
                windows.push((start, finish));
            }
        }
    }

    let mut selected_lines: Vec<&str> = Vec::new();
    for (start, finish) in windows {
        if !selected_lines.is_empty() {
            selected_lines.push("... [error context] ...");
        }
        if let Some(slice) = lines.get(start..finish) {
            selected_lines.extend_from_slice(slice);
        }
    }
    if !selected_lines.is_empty() && tail_start > 0 {
        selected_lines.push("... [middle log omitted] ...");
    }
    if let Some(tail) = lines.get(tail_start..) {
        selected_lines.extend_from_slice(tail);
    }

    let joined = selected_lines.join("\n");
    let total_chars = joined.chars().count();
    if total_chars <= MAX_LOG_TAIL_CHARS {
        return joined;
    }
    let skip = total_chars.saturating_sub(MAX_LOG_TAIL_CHARS);
    let tail_chars: String = joined.chars().skip(skip).collect();
    format!("...[truncated]\n{}", tail_chars.trim_start())
}

#[derive(Deserialize)]
struct GraphQlResponse {
    #[serde(default)]
    data: Option<GraphQlData>,
}

#[derive(Deserialize)]
struct GraphQlData {
    #[serde(default)]
    repository: Option<GraphQlRepository>,
}

#[derive(Deserialize)]
struct GraphQlRepository {
    #[serde(rename = "pullRequest", default)]
    pull_request: Option<GraphQlPullRequest>,
}

#[derive(Deserialize)]
struct GraphQlPullRequest {
    #[serde(rename = "reviewThreads", default)]
    review_threads: GraphQlThreadConnection,
}

#[derive(Default, Deserialize)]
struct GraphQlThreadConnection {
    #[serde(default)]
    nodes: Vec<Option<RawReviewThread>>,
}

#[derive(Deserialize)]
struct RawReviewThread {
    #[serde(default)]
    id: String,
    #[serde(rename = "isResolved", default)]
    is_resolved: bool,
    #[serde(rename = "isOutdated", default)]
    is_outdated: bool,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(rename = "originalLine", default)]
    original_line: Option<u32>,
    #[serde(default)]
    comments: RawCommentConnection,
}

#[derive(Default, Deserialize)]
struct RawCommentConnection {
    #[serde(default)]
    nodes: Vec<Option<RawCommentNode>>,
}

#[derive(Deserialize)]
struct RawAuthor {
    #[serde(default)]
    login: String,
    #[serde(rename = "__typename", default)]
    typename: String,
}

#[derive(Deserialize)]
struct RawCommentNode {
    #[serde(default)]
    id: String,
    #[serde(default)]
    body: String,
    #[serde(rename = "diffHunk", default)]
    diff_hunk: String,
    #[serde(rename = "createdAt", default)]
    created_at: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(rename = "originalLine", default)]
    original_line: Option<u32>,
    #[serde(default)]
    author: Option<RawAuthor>,
}

fn author_info(author: Option<RawAuthor>) -> (String, bool) {
    let Some(a) = author else {
        return ("ghost".into(), false);
    };
    let login = if a.login.trim().is_empty() {
        "ghost".to_owned()
    } else {
        a.login.trim().to_owned()
    };
    let is_bot = a.typename == "Bot" || login.ends_with("[bot]");
    (login, is_bot)
}

/// GitHub GraphQL `reviewThreads` 응답에서 미해결(`!isResolved`) 리뷰 코멘트만 추출한다.
pub fn parse_review_threads_json(bytes: &[u8]) -> io::Result<Vec<ReviewComment>> {
    let parsed: GraphQlResponse = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    let threads = parsed
        .data
        .and_then(|d| d.repository)
        .and_then(|r| r.pull_request)
        .map(|pr| pr.review_threads.nodes)
        .unwrap_or_default();

    let mut out = Vec::new();
    for thread in threads.into_iter().flatten() {
        if thread.is_resolved {
            continue;
        }
        let mut nodes = thread
            .comments
            .nodes
            .into_iter()
            .flatten()
            .filter(|c| !c.body.trim().is_empty());
        let Some(first) = nodes.next() else {
            continue;
        };
        let (author, is_bot) = author_info(first.author);
        let path = thread
            .path
            .or(first.path)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let line = thread.line.or(first.line);
        let original_line = thread.original_line.or(first.original_line);
        let replies = nodes
            .map(|reply| {
                let (reply_author, reply_bot) = author_info(reply.author);
                ReviewReply {
                    author: reply_author,
                    is_bot: reply_bot,
                    body: reply.body.trim().to_owned(),
                    created_at: reply.created_at,
                }
            })
            .collect();
        out.push(ReviewComment {
            id: if first.id.is_empty() {
                thread.id.clone()
            } else {
                first.id
            },
            thread_id: (!thread.id.is_empty()).then_some(thread.id),
            author,
            is_bot,
            body: first.body.trim().to_owned(),
            path,
            line,
            original_line,
            diff_hunk: first.diff_hunk.trim().to_owned(),
            created_at: first.created_at,
            url: first.url,
            is_resolved: false,
            is_outdated: thread.is_outdated,
            replies,
        });
    }
    Ok(out)
}

/// 선택한 CI 실패 항목과 미해결 PR 리뷰 코멘트를 묶어 활성 에이전트에 전달할 구조화 프롬프트를 생성한다.
#[must_use]
pub fn build_fix_with_ai_prompt(
    repo: &Path,
    pr: &PrInspection,
    selected_checks: &[&CheckItem],
    selected_comments: &[&ReviewComment],
) -> String {
    let mut out = String::new();
    let pr_label = match pr.number {
        Some(num) if !pr.title.is_empty() => format!("PR #{num} ({})", pr.title),
        Some(num) => format!("PR #{num}"),
        None if !pr.title.is_empty() => pr.title.clone(),
        None => "current Pull Request".to_owned(),
    };
    writeln!(
        out,
        "Please fix the selected Pull Request issues for {pr_label} in worktree `{}`.",
        repo.display()
    )
    .ok();
    if !pr.url.is_empty() {
        writeln!(out, "PR URL: {}", pr.url).ok();
    }

    if !selected_checks.is_empty() {
        writeln!(out, "\n## Failed CI Checks ({})", selected_checks.len()).ok();
        for (idx, check) in selected_checks.iter().enumerate() {
            writeln!(
                out,
                "\n### {}. {} [{}]",
                idx + 1,
                check.name,
                check.state.label()
            )
            .ok();
            if let Some(desc) = &check.description
                && !desc.trim().is_empty()
            {
                writeln!(out, "- Summary: {}", desc.trim()).ok();
            }
            if let Some(url) = &check.details_url
                && !url.trim().is_empty()
            {
                writeln!(out, "- Details: {}", url.trim()).ok();
            }
            if let Some(log) = &check.log_tail
                && !log.trim().is_empty()
            {
                writeln!(
                    out,
                    "- Extracted failure log tail:\n```text\n{}\n```",
                    truncate_chars(log.trim(), MAX_LOG_TAIL_CHARS)
                )
                .ok();
            }
        }
    }

    if !selected_comments.is_empty() {
        writeln!(
            out,
            "\n## Unresolved PR Review Comments ({})",
            selected_comments.len()
        )
        .ok();
        for (idx, comment) in selected_comments.iter().enumerate() {
            let bot_tag = if comment.is_bot { " [bot]" } else { "" };
            let outdated_tag = if comment.is_outdated {
                " [outdated]"
            } else {
                ""
            };
            writeln!(
                out,
                "\n### {}. `{}` — @{}{}{}",
                idx + 1,
                comment.location(),
                comment.author,
                bot_tag,
                outdated_tag,
            )
            .ok();
            if !comment.url.is_empty() {
                writeln!(out, "- Link: {}", comment.url).ok();
            }
            if !comment.diff_hunk.is_empty() {
                writeln!(
                    out,
                    "- Diff context:\n```diff\n{}\n```",
                    truncate_chars(&comment.diff_hunk, 800)
                )
                .ok();
            }
            writeln!(
                out,
                "- Review comment:\n{}",
                truncate_chars(&comment.body, 2_000)
            )
            .ok();
            for reply in &comment.replies {
                let rbot = if reply.is_bot { " [bot]" } else { "" };
                writeln!(
                    out,
                    "  - Reply by @{}{}: {}",
                    reply.author,
                    rbot,
                    truncate_chars(&reply.body, 800)
                )
                .ok();
            }
        }
    }

    writeln!(
        out,
        "\nInspect the referenced files and failure logs, implement the minimal fixes, and run the relevant checks/tests to verify."
    )
    .ok();
    out.trim().to_owned()
}

fn gh_base_command(folder: &Path) -> Command {
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
    cmd
}

const REVIEW_THREADS_QUERY: &str = r"
query($owner: String!, $name: String!, $prNumber: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $prNumber) {
      reviewThreads(first: 100) {
        nodes {
          id
          isResolved
          isOutdated
          path
          line
          originalLine
          comments(first: 20) {
            nodes {
              id
              body
              diffHunk
              createdAt
              url
              author {
                login
                __typename
              }
            }
          }
        }
      }
    }
  }
}
";

pub(super) fn inspect_with_gh(folder: &Path, branch: &str) -> io::Result<PrInspection> {
    if branch.trim().is_empty() {
        return Ok(PrInspection::default());
    }
    let mut list_cmd = gh_base_command(folder);
    list_cmd.args([
        "pr",
        "list",
        "--head",
        branch,
        "--state",
        "all",
        "--limit",
        "1",
        "--json",
        "number,title,url,state,isDraft,statusCheckRollup",
    ]);
    let bytes = execute(&mut list_cmd, TIMEOUT)?;
    let mut inspection = parse_checks_from_pr_json(&bytes)?;
    let Some(pr_number) = inspection.number else {
        return Ok(inspection);
    };

    let mut fetched_logs = 0usize;
    for check in &mut inspection.checks {
        if check.state != CheckState::Failed || check.log_tail.is_some() {
            continue;
        }
        if fetched_logs >= MAX_FAILED_LOG_FETCHES {
            break;
        }
        let log_bytes = if let Some(job_id) = check.job_id.as_deref() {
            let mut cmd = gh_base_command(folder);
            cmd.args(["run", "view", "--job", job_id, "--log-failed"]);
            execute(&mut cmd, TIMEOUT).ok()
        } else if let Some(run_id) = check.run_id.as_deref() {
            let mut cmd = gh_base_command(folder);
            cmd.args(["run", "view", run_id, "--log-failed"]);
            execute(&mut cmd, TIMEOUT).ok()
        } else {
            None
        };
        if let Some(raw_bytes) = log_bytes {
            let tail = extract_error_log_tail(&String::from_utf8_lossy(&raw_bytes));
            if !tail.is_empty() {
                check.log_tail = Some(tail);
            }
            fetched_logs += 1;
        }
    }

    if let Some((owner, name)) = parse_repo_owner_and_name(&inspection.url) {
        let mut gql_cmd = graphql_review_threads_command(folder, &owner, &name, pr_number);
        if let Ok(gql_bytes) = execute(&mut gql_cmd, TIMEOUT)
            && let Ok(comments) = parse_review_threads_json(&gql_bytes)
        {
            inspection.review_comments = comments;
        }
    }

    Ok(inspection)
}

pub(super) fn graphql_review_threads_command(
    folder: &Path,
    owner: &str,
    name: &str,
    pr_number: u64,
) -> Command {
    let mut gql_cmd = gh_base_command(folder);
    gql_cmd.args([
        "api",
        "graphql",
        "-f",
        &format!("owner={owner}"),
        "-f",
        &format!("name={name}"),
        "-F",
        &format!("prNumber={pr_number}"),
        "-f",
        &format!("query={REVIEW_THREADS_QUERY}"),
    ]);
    gql_cmd
}
