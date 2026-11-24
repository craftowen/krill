// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Local GitHub (REST API or `gh` CLI) and Linear (GraphQL API) pollers for Krill Bots.
//!
//! Strictly lazy: spawns zero background threads until at least one bot enables `github` or
//! `linear`. Uses `krill_proto::os::background_command` (`CREATE_NO_WINDOW` on Windows) for `gh`.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::bots::{
    BotConfig, BotConnectorStatus, BotConnectorTestResult, BotErrorCategory, BotErrorDetail, BotId,
    BotItemActor, BotItemLink, BotItemStatus, ConnectorHealthState, CreateBotItemParams,
    ExternalSource,
};

use crate::bots::http_client::{self, HttpRequest, SsrfPolicy};
use crate::bots::items::BotItemsStore;
use crate::bots::secrets::SecretVault;
use crate::bots::store::BotStore;
use crate::session::lock;

const STACK_SIZE: usize = 128 * 1024;
const DEFAULT_POLL_SECS: u64 = 60;
const GH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_GH_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

fn policy_for_url(url: &str) -> SsrfPolicy {
    if cfg!(test)
        && (url.starts_with("http://127.0.0.1")
            || url.starts_with("http://localhost")
            || url.starts_with("http://[::1]"))
    {
        SsrfPolicy::AllowLoopback
    } else {
        SsrfPolicy::StrictExternal
    }
}

/// Overridable API endpoints for GitHub REST and Linear GraphQL (defaults to production URLs).
#[derive(Debug, Clone)]
pub struct TrackerEndpoints {
    pub github_api_base: String,
    pub linear_graphql_url: String,
}

impl Default for TrackerEndpoints {
    fn default() -> Self {
        Self {
            github_api_base: "https://api.github.com".to_owned(),
            linear_graphql_url: "https://api.linear.app/graphql".to_owned(),
        }
    }
}

/// Polls GitHub PRs/issues/notifications for `bot` via REST token or local `gh` CLI.
/// Returns the number of newly created items.
pub fn poll_github_bot(
    bot: &BotConfig,
    secrets: &SecretVault,
    items: &BotItemsStore,
    endpoints: &TrackerEndpoints,
    current_ms: u64,
) -> Result<usize, BotErrorDetail> {
    let token = secrets
        .resolve_secret(
            &bot.id,
            bot.connectors.github.token_secret_ref.as_deref(),
            "github_token",
        )
        .map_err(|err| BotErrorDetail {
            category: BotErrorCategory::NotSetUp,
            agent_name: None,
            message: format!("failed reading GitHub token from vault: {}", err.kind()),
            resets_at_ms: None,
        })?;

    let actor = BotItemActor::System {
        reason: "github-poller".to_owned(),
    };
    let mut created = 0usize;

    let token = token.and_then(|t| {
        let trimmed = t.trim();
        (!trimmed.is_empty()).then(|| zeroize::Zeroizing::new(trimmed.to_owned()))
    });

    if let Some(token) = token {
        let auth = format!("Bearer {}", *token);
        let base = endpoints.github_api_base.trim_end_matches('/');
        if bot.connectors.github.repos.is_empty() {
            let url = format!("{base}/notifications?per_page=10");
            let resp = http_client::send(
                &HttpRequest::get(&url)
                    .with_header("Authorization", &auth)
                    .with_header("Accept", "application/vnd.github+json")
                    .with_header("X-GitHub-Api-Version", "2022-11-28")
                    .with_ssrf_policy(policy_for_url(&url)),
            )
            .map_err(|err| tracker_transport_err("GitHub", &err))?;
            if !resp.is_success() {
                return Err(github_http_error(&resp));
            }
            if let Ok(list) = resp.json::<Vec<serde_json::Value>>() {
                for notif in list {
                    let title = notif
                        .get("subject")
                        .and_then(|s| s.get("title"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("GitHub notification");
                    let subject_type = notif
                        .get("subject")
                        .and_then(|s| s.get("type"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    let raw_url = notif
                        .get("subject")
                        .and_then(|s| s.get("url"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    if raw_url.is_empty() {
                        continue;
                    }
                    let html_url = github_api_url_to_html_url(raw_url);
                    let link = if subject_type.eq_ignore_ascii_case("PullRequest") {
                        BotItemLink::Pr(html_url)
                    } else {
                        BotItemLink::Url(html_url)
                    };
                    let reason = notif
                        .get("reason")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    let status = if matches!(reason, "review_requested" | "mention" | "assign") {
                        BotItemStatus::NeedsYou
                    } else {
                        BotItemStatus::Planned
                    };
                    if create_dedup_item(
                        items, &bot.id, title, None, status, link, &actor, current_ms,
                    ) {
                        created = created.saturating_add(1);
                    }
                }
            }
        } else {
            for repo in &bot.connectors.github.repos {
                let repo = repo.trim();
                if !is_valid_github_repo(repo) {
                    continue;
                }
                let prs_url = format!("{base}/repos/{repo}/pulls?state=open&per_page=10");
                let resp = http_client::send(
                    &HttpRequest::get(&prs_url)
                        .with_header("Authorization", &auth)
                        .with_header("Accept", "application/vnd.github+json")
                        .with_header("X-GitHub-Api-Version", "2022-11-28")
                        .with_ssrf_policy(policy_for_url(&prs_url)),
                )
                .map_err(|err| tracker_transport_err("GitHub", &err))?;
                if !resp.is_success() {
                    return Err(github_http_error(&resp));
                }
                if let Ok(prs) = resp.json::<Vec<serde_json::Value>>() {
                    for pr in prs {
                        created = created.saturating_add(ingest_github_pr(
                            items, &bot.id, repo, &pr, &actor, current_ms,
                        ));
                    }
                }

                let issues_url = format!("{base}/repos/{repo}/issues?state=open&per_page=10");
                let issues_resp = http_client::send(
                    &HttpRequest::get(&issues_url)
                        .with_header("Authorization", &auth)
                        .with_header("Accept", "application/vnd.github+json")
                        .with_header("X-GitHub-Api-Version", "2022-11-28")
                        .with_ssrf_policy(policy_for_url(&issues_url)),
                )
                .map_err(|err| tracker_transport_err("GitHub", &err))?;
                if !issues_resp.is_success() {
                    return Err(github_http_error(&issues_resp));
                }
                if let Ok(issues) = issues_resp.json::<Vec<serde_json::Value>>() {
                    for issue in issues {
                        created = created.saturating_add(ingest_github_issue(
                            items, &bot.id, repo, &issue, &actor, current_ms,
                        ));
                    }
                }
            }
        }
        return Ok(created);
    }

    if !bot.connectors.github.use_gh_cli {
        return Err(BotErrorDetail {
            category: BotErrorCategory::NotSetUp,
            agent_name: None,
            message: "GitHub token not set and gh CLI fallback is disabled".to_owned(),
            resets_at_ms: None,
        });
    }

    // Fallback to local `gh` CLI (`CREATE_NO_WINDOW` via `background_command`).
    if bot.connectors.github.repos.is_empty() {
        let json = run_gh_cli(&["api", "notifications?per_page=10"])?;
        if let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&json) {
            for notif in list {
                let title = notif
                    .get("subject")
                    .and_then(|s| s.get("title"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("GitHub notification");
                let subject_type = notif
                    .get("subject")
                    .and_then(|s| s.get("type"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let raw_url = notif
                    .get("subject")
                    .and_then(|s| s.get("url"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if raw_url.is_empty() {
                    continue;
                }
                let html_url = github_api_url_to_html_url(raw_url);
                let link = if subject_type.eq_ignore_ascii_case("PullRequest") {
                    BotItemLink::Pr(html_url)
                } else {
                    BotItemLink::Url(html_url)
                };
                if create_dedup_item(
                    items,
                    &bot.id,
                    title,
                    None,
                    BotItemStatus::Planned,
                    link,
                    &actor,
                    current_ms,
                ) {
                    created = created.saturating_add(1);
                }
            }
        }
    } else {
        for repo in &bot.connectors.github.repos {
            let repo = repo.trim();
            if !is_valid_github_repo(repo) {
                continue;
            }
            let endpoint = format!("repos/{repo}/pulls?state=open&per_page=10");
            let json = run_gh_cli(&["api", &endpoint])?;
            if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(&json) {
                for pr in prs {
                    created = created.saturating_add(ingest_github_pr(
                        items, &bot.id, repo, &pr, &actor, current_ms,
                    ));
                }
            }
            let issues_endpoint = format!("repos/{repo}/issues?state=open&per_page=10");
            let issues_json = run_gh_cli(&["api", &issues_endpoint])?;
            if let Ok(issues) = serde_json::from_str::<Vec<serde_json::Value>>(&issues_json) {
                for issue in issues {
                    created = created.saturating_add(ingest_github_issue(
                        items, &bot.id, repo, &issue, &actor, current_ms,
                    ));
                }
            }
        }
    }

    Ok(created)
}

fn ingest_github_issue(
    items: &BotItemsStore,
    bot_id: &BotId,
    repo: &str,
    issue: &serde_json::Value,
    actor: &BotItemActor,
    current_ms: u64,
) -> usize {
    // GitHub `/issues` endpoint also includes PRs marked with a `pull_request` object.
    if issue.get("pull_request").is_some_and(|v| !v.is_null()) {
        return 0;
    }
    let Some(html_url) = issue.get("html_url").and_then(serde_json::Value::as_str) else {
        return 0;
    };
    let number = issue.get("number").and_then(serde_json::Value::as_u64);
    let raw_title = issue
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Issue");
    let title = match number {
        Some(n) => format!("{repo} #{n}: {raw_title}"),
        None => format!("{repo}: {raw_title}"),
    };
    let has_assignee = issue
        .get("assignees")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|a| !a.is_empty());
    let status = if has_assignee {
        BotItemStatus::NeedsYou
    } else {
        BotItemStatus::Planned
    };
    let body = issue
        .get("body")
        .and_then(serde_json::Value::as_str)
        .map(|s| s.chars().take(1_000).collect());
    usize::from(create_dedup_item(
        items,
        bot_id,
        &title,
        body,
        status,
        BotItemLink::Url(html_url.to_owned()),
        actor,
        current_ms,
    ))
}

fn github_api_url_to_html_url(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix("https://api.github.com/repos/") {
        // Handle `/repos/{owner}/{repo}/pulls/{n}` and `/repos/{owner}/{repo}/issues/{n}`
        let parts: Vec<&str> = rest.split('/').collect();
        if let (Some(owner), Some(repo), Some(kind), Some(id)) = (
            parts.first().copied(),
            parts.get(1).copied(),
            parts.get(2).copied(),
            parts.get(3).copied(),
        ) {
            let html_kind = if kind == "pulls" { "pull" } else { kind };
            return format!("https://github.com/{owner}/{repo}/{html_kind}/{id}");
        }
        return format!("https://github.com/{rest}");
    }
    raw.to_owned()
}

fn ingest_github_pr(
    items: &BotItemsStore,
    bot_id: &BotId,
    repo: &str,
    pr: &serde_json::Value,
    actor: &BotItemActor,
    current_ms: u64,
) -> usize {
    let Some(html_url) = pr.get("html_url").and_then(serde_json::Value::as_str) else {
        return 0;
    };
    let number = pr.get("number").and_then(serde_json::Value::as_u64);
    let raw_title = pr
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Pull Request");
    let title = match number {
        Some(n) => format!("{repo} #{n}: {raw_title}"),
        None => format!("{repo}: {raw_title}"),
    };
    let has_reviewers = pr
        .get("requested_reviewers")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|a| !a.is_empty());
    let status = if has_reviewers {
        BotItemStatus::NeedsYou
    } else {
        BotItemStatus::Planned
    };
    let body = pr
        .get("body")
        .and_then(serde_json::Value::as_str)
        .map(|s| s.chars().take(1_000).collect());
    usize::from(create_dedup_item(
        items,
        bot_id,
        &title,
        body,
        status,
        BotItemLink::Pr(html_url.to_owned()),
        actor,
        current_ms,
    ))
}

fn drain_bounded_pipe<R: io::Read + Send + 'static>(
    pipe: Option<R>,
) -> io::Result<JoinHandle<Vec<u8>>> {
    thread::Builder::new()
        .name("bot-gh-pipe".to_owned())
        .stack_size(64 * 1024)
        .spawn(move || {
            let Some(mut reader) = pipe else {
                return Vec::new();
            };
            let mut out = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let remain = MAX_GH_OUTPUT_BYTES.saturating_sub(out.len());
                        if remain > 0 {
                            out.extend_from_slice(buf.get(..n.min(remain)).unwrap_or_default());
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            out
        })
}

fn run_gh_cli(args: &[&str]) -> Result<String, BotErrorDetail> {
    let mut child = krill_proto::os::background_command("gh")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| BotErrorDetail {
            category: BotErrorCategory::NotSetUp,
            agent_name: None,
            message: format!("failed to launch gh CLI: {}", err.kind()),
            resets_at_ms: None,
        })?;

    // Drain stdout & stderr concurrently BEFORE waiting so >64 KiB output never deadlocks pipes.
    let stdout_handle = match drain_bounded_pipe(child.stdout.take()) {
        Ok(h) => h,
        Err(err) => {
            krill_proto::os::kill_tree(&mut child);
            return Err(BotErrorDetail {
                category: BotErrorCategory::AgentError,
                agent_name: None,
                message: format!("failed to spawn gh stdout drain thread: {}", err.kind()),
                resets_at_ms: None,
            });
        }
    };
    let stderr_handle = match drain_bounded_pipe(child.stderr.take()) {
        Ok(h) => h,
        Err(err) => {
            krill_proto::os::kill_tree(&mut child);
            stdout_handle.join().ok();
            return Err(BotErrorDetail {
                category: BotErrorCategory::AgentError,
                agent_name: None,
                message: format!("failed to spawn gh stderr drain thread: {}", err.kind()),
                resets_at_ms: None,
            });
        }
    };

    let deadline = Instant::now() + GH_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if Instant::now() >= deadline => {
                krill_proto::os::kill_tree(&mut child);
                stdout_handle.join().ok();
                stderr_handle.join().ok();
                return Err(BotErrorDetail {
                    category: BotErrorCategory::AgentError,
                    agent_name: None,
                    message: "gh CLI timed out".to_owned(),
                    resets_at_ms: None,
                });
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(err) => {
                krill_proto::os::kill_tree(&mut child);
                stdout_handle.join().ok();
                stderr_handle.join().ok();
                return Err(BotErrorDetail {
                    category: BotErrorCategory::AgentError,
                    agent_name: None,
                    message: format!("gh CLI wait failed: {}", err.kind()),
                    resets_at_ms: None,
                });
            }
        }
    };

    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let _stderr_bytes = stderr_handle.join().unwrap_or_default();
    if !status.success() {
        return Err(BotErrorDetail {
            category: BotErrorCategory::SignedOut,
            agent_name: None,
            message: "gh CLI exited non-zero (check `gh auth status`)".to_owned(),
            resets_at_ms: None,
        });
    }
    Ok(String::from_utf8_lossy(&stdout_bytes).into_owned())
}

/// Polls Linear assigned/active issues via GraphQL API. Returns the number of newly created items.
pub fn poll_linear_bot(
    bot: &BotConfig,
    secrets: &SecretVault,
    items: &BotItemsStore,
    endpoints: &TrackerEndpoints,
    current_ms: u64,
) -> Result<usize, BotErrorDetail> {
    let api_key = secrets
        .resolve_secret(
            &bot.id,
            bot.connectors.linear.api_key_secret_ref.as_deref(),
            "linear_api_key",
        )
        .map_err(|err| BotErrorDetail {
            category: BotErrorCategory::NotSetUp,
            agent_name: None,
            message: format!("failed reading Linear API key: {}", err.kind()),
            resets_at_ms: None,
        })?
        .ok_or_else(|| BotErrorDetail {
            category: BotErrorCategory::NotSetUp,
            agent_name: None,
            message: "Linear API key not configured in secret vault".to_owned(),
            resets_at_ms: None,
        })?;

    let query = serde_json::json!({
        "query": "query KrillBotIssues { viewer { assignedIssues(first: 15, filter: { state: { type: { nin: [\"completed\", \"canceled\"] } } }) { nodes { id identifier title description url team { key } } } } }"
    });
    let payload = serde_json::to_vec(&query).map_err(|_| BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: "failed to encode Linear GraphQL request".to_owned(),
        resets_at_ms: None,
    })?;

    let resp = http_client::send(
        &HttpRequest::post_json(&endpoints.linear_graphql_url, payload)
            .with_header("Authorization", api_key.trim())
            .with_ssrf_policy(policy_for_url(&endpoints.linear_graphql_url)),
    )
    .map_err(|err| tracker_transport_err("Linear", &err))?;

    if resp.status == 401 || resp.status == 403 {
        return Err(BotErrorDetail {
            category: BotErrorCategory::SignedOut,
            agent_name: None,
            message: format!("Linear authentication rejected (HTTP {})", resp.status),
            resets_at_ms: None,
        });
    }
    if !resp.is_success() {
        return Err(BotErrorDetail {
            category: BotErrorCategory::AgentError,
            agent_name: None,
            message: format!("Linear GraphQL returned HTTP {}", resp.status),
            resets_at_ms: None,
        });
    }

    let val: serde_json::Value = resp.json().map_err(|_| BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: "invalid Linear GraphQL JSON response".to_owned(),
        resets_at_ms: None,
    })?;

    if let Some(err) = check_graphql_errors(&val) {
        return Err(err);
    }

    let nodes = val
        .get("data")
        .and_then(|d| d.get("viewer"))
        .and_then(|v| v.get("assignedIssues"))
        .and_then(|i| i.get("nodes"))
        .and_then(serde_json::Value::as_array);
    let Some(nodes) = nodes else {
        return Ok(0);
    };

    let team_filter = &bot.connectors.linear.team_keys;
    let actor = BotItemActor::System {
        reason: "linear-poller".to_owned(),
    };
    let mut created = 0usize;

    for node in nodes {
        let team_key = node
            .get("team")
            .and_then(|t| t.get("key"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if !team_filter.is_empty() && !team_filter.iter().any(|k| k.eq_ignore_ascii_case(team_key))
        {
            continue;
        }
        let Some(url) = node.get("url").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let identifier = node
            .get("identifier")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("LINEAR");
        let raw_title = node
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Linear issue");
        let title = format!("{identifier}: {raw_title}");
        let desc = node
            .get("description")
            .and_then(serde_json::Value::as_str)
            .map(|s| s.chars().take(1_000).collect());

        if create_dedup_item(
            items,
            &bot.id,
            &title,
            desc,
            BotItemStatus::Planned,
            BotItemLink::Url(url.to_owned()),
            &actor,
            current_ms,
        ) {
            created = created.saturating_add(1);
        }
    }

    Ok(created)
}

#[expect(clippy::too_many_arguments, reason = "item deduplication helper")]
fn create_dedup_item(
    items: &BotItemsStore,
    bot_id: &BotId,
    title: &str,
    description: Option<String>,
    status: BotItemStatus,
    link: BotItemLink,
    actor: &BotItemActor,
    now_ms: u64,
) -> bool {
    if items
        .find_item_by_link(bot_id, &link)
        .ok()
        .flatten()
        .is_some()
    {
        return false;
    }
    let trimmed_title = title.trim();
    if trimmed_title.is_empty() {
        return false;
    }
    let clean_title = crate::bots::items::clamp_item_title(trimmed_title);
    let clean_desc = description.and_then(|d| crate::bots::items::clamp_item_description(&d));
    items
        .create_item(
            &CreateBotItemParams {
                bot_id: bot_id.clone(),
                title: clean_title,
                description: clean_desc,
                done_means: None,
                status: Some(status),
                links: vec![link],
            },
            actor,
            now_ms,
        )
        .is_ok()
}

pub(crate) fn is_valid_github_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && !name.is_empty()
        && !name.contains('/')
        && owner != "."
        && owner != ".."
        && name != "."
        && name != ".."
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn github_http_error(resp: &http_client::HttpResponse) -> BotErrorDetail {
    if resp.status == 429
        || (resp.status == 403 && resp.header("x-ratelimit-remaining") == Some("0"))
    {
        return BotErrorDetail {
            category: BotErrorCategory::UsageLimit,
            agent_name: None,
            message: format!("GitHub API rate limited (HTTP {})", resp.status),
            resets_at_ms: None,
        };
    }
    if resp.status == 401 || resp.status == 403 {
        return BotErrorDetail {
            category: BotErrorCategory::SignedOut,
            agent_name: None,
            message: format!("GitHub authentication failed (HTTP {})", resp.status),
            resets_at_ms: None,
        };
    }
    BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: format!("GitHub API returned HTTP {}", resp.status),
        resets_at_ms: None,
    }
}

fn tracker_transport_err(provider: &str, err: &io::Error) -> BotErrorDetail {
    BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: format!("{provider} poll network error: {}", err.kind()),
        resets_at_ms: None,
    }
}

fn check_graphql_errors(val: &serde_json::Value) -> Option<BotErrorDetail> {
    let errors = val.get("errors").and_then(serde_json::Value::as_array)?;
    let first = errors.first()?;
    let msg = first
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Linear GraphQL returned errors");
    let lower = msg.to_ascii_lowercase();
    let category = if lower.contains("auth")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("token")
    {
        BotErrorCategory::SignedOut
    } else {
        BotErrorCategory::AgentError
    };
    Some(BotErrorDetail {
        category,
        agent_name: None,
        message: msg.to_owned(),
        resets_at_ms: None,
    })
}

/// Tests GitHub connectivity (`GET /user` or `gh auth status`) for `TestConnector`.
#[must_use]
pub fn test_github(
    bot: &BotConfig,
    secrets: &SecretVault,
    endpoints: &TrackerEndpoints,
) -> BotConnectorTestResult {
    let resolved = secrets
        .resolve_secret(
            &bot.id,
            bot.connectors.github.token_secret_ref.as_deref(),
            "github_token",
        )
        .map(|opt| {
            opt.and_then(|t| {
                let trimmed = t.trim();
                (!trimmed.is_empty()).then(|| zeroize::Zeroizing::new(trimmed.to_owned()))
            })
        });
    match resolved {
        Ok(Some(token)) => {
            let base = endpoints.github_api_base.trim_end_matches('/');
            let url = format!("{base}/user");
            let auth = format!("Bearer {}", *token);
            match http_client::send(
                &HttpRequest::get(&url)
                    .with_header("Authorization", auth)
                    .with_header("Accept", "application/vnd.github+json")
                    .with_header("X-GitHub-Api-Version", "2022-11-28")
                    .with_ssrf_policy(policy_for_url(&url)),
            ) {
                Ok(resp) if resp.is_success() => {
                    let login = resp.json::<serde_json::Value>().ok().and_then(|v| {
                        v.get("login")
                            .and_then(serde_json::Value::as_str)
                            .map(|s| format!("@{s}"))
                    });
                    BotConnectorTestResult {
                        source: ExternalSource::GitHub,
                        ok: true,
                        account_label: login.clone(),
                        detail: format!(
                            "GitHub REST API connected ({})",
                            login.unwrap_or_else(|| "token valid".to_owned())
                        ),
                    }
                }
                Ok(resp) => BotConnectorTestResult {
                    source: ExternalSource::GitHub,
                    ok: false,
                    account_label: None,
                    detail: format!("GitHub authentication failed (HTTP {})", resp.status),
                },
                Err(err) => BotConnectorTestResult {
                    source: ExternalSource::GitHub,
                    ok: false,
                    account_label: None,
                    detail: format!("GitHub network error: {}", err.kind()),
                },
            }
        }
        Ok(None) if bot.connectors.github.use_gh_cli => {
            match run_gh_cli(&["api", "user", "--jq", ".login"]) {
                Ok(login) => {
                    let clean = login.trim();
                    let label = (!clean.is_empty()).then(|| format!("@{clean}"));
                    BotConnectorTestResult {
                        source: ExternalSource::GitHub,
                        ok: true,
                        account_label: label,
                        detail: "GitHub CLI (`gh`) authenticated".to_owned(),
                    }
                }
                Err(detail) => BotConnectorTestResult {
                    source: ExternalSource::GitHub,
                    ok: false,
                    account_label: None,
                    detail: detail.message,
                },
            }
        }
        Ok(None) => BotConnectorTestResult {
            source: ExternalSource::GitHub,
            ok: false,
            account_label: None,
            detail: "GitHub token missing and gh CLI fallback disabled".to_owned(),
        },
        Err(err) => BotConnectorTestResult {
            source: ExternalSource::GitHub,
            ok: false,
            account_label: None,
            detail: format!("Failed reading GitHub secret: {}", err.kind()),
        },
    }
}

/// Tests Linear GraphQL connectivity (`viewer { id name email }`) for `TestConnector`.
#[must_use]
pub fn test_linear(
    bot: &BotConfig,
    secrets: &SecretVault,
    endpoints: &TrackerEndpoints,
) -> BotConnectorTestResult {
    let api_key = match secrets.resolve_secret(
        &bot.id,
        bot.connectors.linear.api_key_secret_ref.as_deref(),
        "linear_api_key",
    ) {
        Ok(Some(k)) => k,
        Ok(None) => {
            return BotConnectorTestResult {
                source: ExternalSource::Linear,
                ok: false,
                account_label: None,
                detail: "Linear API key not configured in secret vault".to_owned(),
            };
        }
        Err(err) => {
            return BotConnectorTestResult {
                source: ExternalSource::Linear,
                ok: false,
                account_label: None,
                detail: format!("Failed reading Linear secret: {}", err.kind()),
            };
        }
    };
    let payload = serde_json::to_vec(&serde_json::json!({
        "query": "query KrillViewer { viewer { id name email } }"
    }))
    .unwrap_or_default();
    let resp = match http_client::send(
        &HttpRequest::post_json(&endpoints.linear_graphql_url, payload)
            .with_header("Authorization", api_key.trim())
            .with_ssrf_policy(policy_for_url(&endpoints.linear_graphql_url)),
    ) {
        Ok(r) => r,
        Err(err) => {
            return BotConnectorTestResult {
                source: ExternalSource::Linear,
                ok: false,
                account_label: None,
                detail: format!("Linear network error: {}", err.kind()),
            };
        }
    };
    if !resp.is_success() {
        return BotConnectorTestResult {
            source: ExternalSource::Linear,
            ok: false,
            account_label: None,
            detail: format!("Linear GraphQL returned HTTP {}", resp.status),
        };
    }
    let val: serde_json::Value = match resp.json() {
        Ok(v) => v,
        Err(_) => {
            return BotConnectorTestResult {
                source: ExternalSource::Linear,
                ok: false,
                account_label: None,
                detail: "invalid Linear GraphQL JSON response".to_owned(),
            };
        }
    };
    if let Some(err) = check_graphql_errors(&val) {
        return BotConnectorTestResult {
            source: ExternalSource::Linear,
            ok: false,
            account_label: None,
            detail: err.message,
        };
    }
    let label = val
        .get("data")
        .and_then(|d| d.get("viewer"))
        .and_then(|v| {
            v.get("email")
                .or_else(|| v.get("name"))
                .and_then(serde_json::Value::as_str)
        })
        .map(ToOwned::to_owned)
        .or_else(|| Some("Linear GraphQL".to_owned()));
    BotConnectorTestResult {
        source: ExternalSource::Linear,
        ok: true,
        account_label: label,
        detail: "Linear GraphQL API connected".to_owned(),
    }
}

/// Callback invoked when background tracker polling discovers new items (`has_needs_you` indicates actionable gate).
pub type TrackerNewItemsCallback = Arc<dyn Fn(&BotConfig, ExternalSource, usize) + Send + Sync>;

#[derive(Default)]
struct TrackerWakeState {
    stop: bool,
    wake_now: bool,
}

struct RunningTrackerPoller {
    stop: Arc<(Mutex<TrackerWakeState>, Condvar)>,
    handle: JoinHandle<()>,
}

/// Strictly-lazy 60s background poller for GitHub & Linear trackers.
pub struct TrackerPoller {
    store: Arc<BotStore>,
    secrets: Arc<SecretVault>,
    items: Arc<BotItemsStore>,
    endpoints: TrackerEndpoints,
    statuses: Arc<Mutex<BTreeMap<(BotId, ExternalSource), BotConnectorStatus>>>,
    on_new_items: Arc<Mutex<Option<TrackerNewItemsCallback>>>,
    running: AtomicBool,
    reconcile_lock: Mutex<()>,
    lifecycle: Mutex<Option<RunningTrackerPoller>>,
}

impl fmt::Debug for TrackerPoller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrackerPoller")
            .field("running", &self.running.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl Drop for TrackerPoller {
    fn drop(&mut self) {
        self.stop();
    }
}

impl TrackerPoller {
    /// Creates a lazy tracker poller with ZERO threads until `reconcile` finds an enabled tracker.
    #[must_use]
    pub fn new(
        store: Arc<BotStore>,
        secrets: Arc<SecretVault>,
        items: Arc<BotItemsStore>,
        endpoints: TrackerEndpoints,
    ) -> Self {
        Self {
            store,
            secrets,
            items,
            endpoints,
            statuses: Arc::new(Mutex::new(BTreeMap::new())),
            on_new_items: Arc::new(Mutex::new(None)),
            running: AtomicBool::new(false),
            reconcile_lock: Mutex::new(()),
            lifecycle: Mutex::new(None),
        }
    }

    /// Registers a callback invoked when background tracker polling discovers new items.
    pub fn set_on_new_items(&self, cb: TrackerNewItemsCallback) {
        *lock(&self.on_new_items) = Some(cb);
    }

    /// Starts the poller thread if ≥1 bot enables GitHub or Linear and wakes immediately; stops it otherwise.
    pub fn reconcile(&self) {
        let _reconcile = lock(&self.reconcile_lock);
        let bots = self.store.list_bots().unwrap_or_default();
        let active_keys: std::collections::BTreeSet<(BotId, ExternalSource)> = bots
            .iter()
            .filter(|b| !b.paused)
            .flat_map(|b| {
                let mut keys = Vec::new();
                if b.connectors.github.enabled {
                    keys.push((b.id.clone(), ExternalSource::GitHub));
                }
                if b.connectors.linear.enabled {
                    keys.push((b.id.clone(), ExternalSource::Linear));
                }
                keys
            })
            .collect();
        if active_keys.is_empty() {
            self.stop_locked();
            // Prune AFTER joining the poller thread so an in-flight poll cannot resurrect stale entries.
            lock(&self.statuses).retain(|k, _| active_keys.contains(k));
        } else {
            lock(&self.statuses).retain(|k, _| active_keys.contains(k));
            let mut guard = lock(&self.lifecycle);
            if let Some(ref running) = *guard {
                let mut st = lock(&running.stop.0);
                st.wake_now = true;
                running.stop.1.notify_all();
            } else {
                self.spawn_locked(&mut guard);
            }
        }
    }

    /// Immediately syncs GitHub for `bot` and records health status.
    pub fn sync_github_now(&self, bot: &BotConfig, current_ms: u64) -> BotConnectorStatus {
        let st = match poll_github_bot(bot, &self.secrets, &self.items, &self.endpoints, current_ms)
        {
            Ok(_) => BotConnectorStatus {
                bot_id: bot.id.clone(),
                source: ExternalSource::GitHub,
                enabled: bot.connectors.github.enabled,
                connected: true,
                state: ConnectorHealthState::Connected,
                account_label: Some(if bot.connectors.github.repos.is_empty() {
                    "GitHub notifications".to_owned()
                } else {
                    format!("{} repo(s)", bot.connectors.github.repos.len())
                }),
                last_sync_ms: Some(current_ms),
                last_error: None,
                error_category: None,
            },
            Err(detail) => BotConnectorStatus {
                bot_id: bot.id.clone(),
                source: ExternalSource::GitHub,
                enabled: bot.connectors.github.enabled,
                connected: false,
                state: ConnectorHealthState::Error,
                account_label: None,
                last_sync_ms: None,
                last_error: Some(detail.message),
                error_category: Some(detail.category),
            },
        };
        lock(&self.statuses).insert((bot.id.clone(), ExternalSource::GitHub), st.clone());
        st
    }

    /// Immediately syncs Linear for `bot` and records health status.
    pub fn sync_linear_now(&self, bot: &BotConfig, current_ms: u64) -> BotConnectorStatus {
        let st = match poll_linear_bot(bot, &self.secrets, &self.items, &self.endpoints, current_ms)
        {
            Ok(_) => BotConnectorStatus {
                bot_id: bot.id.clone(),
                source: ExternalSource::Linear,
                enabled: bot.connectors.linear.enabled,
                connected: true,
                state: ConnectorHealthState::Connected,
                account_label: Some("Linear GraphQL".to_owned()),
                last_sync_ms: Some(current_ms),
                last_error: None,
                error_category: None,
            },
            Err(detail) => BotConnectorStatus {
                bot_id: bot.id.clone(),
                source: ExternalSource::Linear,
                enabled: bot.connectors.linear.enabled,
                connected: false,
                state: ConnectorHealthState::Error,
                account_label: None,
                last_sync_ms: None,
                last_error: Some(detail.message),
                error_category: Some(detail.category),
            },
        };
        lock(&self.statuses).insert((bot.id.clone(), ExternalSource::Linear), st.clone());
        st
    }

    /// Returns tracked GitHub & Linear statuses, optionally filtered by `bot_id`.
    #[must_use]
    pub fn statuses(&self, bot_id: Option<&BotId>) -> Vec<BotConnectorStatus> {
        lock(&self.statuses)
            .values()
            .filter(|s| bot_id.is_none_or(|id| &s.bot_id == id))
            .cloned()
            .collect()
    }

    /// Returns whether the background poller thread is currently running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    fn spawn_locked(&self, lifecycle_guard: &mut Option<RunningTrackerPoller>) {
        if lifecycle_guard.is_some() {
            return;
        }
        // Fresh per-generation stop/condvar state prevents stop/restart races (MB-17).
        let stop = Arc::new((
            Mutex::new(TrackerWakeState {
                stop: false,
                wake_now: true,
            }),
            Condvar::new(),
        ));
        let store = Arc::clone(&self.store);
        let secrets = Arc::clone(&self.secrets);
        let items = Arc::clone(&self.items);
        let endpoints = self.endpoints.clone();
        let statuses = Arc::clone(&self.statuses);
        let on_new_items = Arc::clone(&self.on_new_items);
        let stop_bg = Arc::clone(&stop);

        if let Ok(handle) = thread::Builder::new()
            .name("bot-tracker-poll".to_owned())
            .stack_size(STACK_SIZE)
            .spawn(move || {
                let mut last_polled: BTreeMap<(BotId, ExternalSource), u64> = BTreeMap::new();
                loop {
                    let min_sleep_secs = store
                        .list_bots()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|b| !b.paused)
                        .filter_map(|b| {
                            let gh = b
                                .connectors
                                .github
                                .enabled
                                .then_some(b.connectors.github.poll_interval_secs.max(15));
                            let lin = b
                                .connectors
                                .linear
                                .enabled
                                .then_some(b.connectors.linear.poll_interval_secs.max(15));
                            match (gh, lin) {
                                (Some(a), Some(c)) => Some(a.min(c)),
                                (Some(a), None) | (None, Some(a)) => Some(a),
                                (None, None) => None,
                            }
                        })
                        .min()
                        .unwrap_or(DEFAULT_POLL_SECS);
                    let stopped = {
                        let mut guard = lock(&stop_bg.0);
                        if guard.stop {
                            break;
                        }
                        if guard.wake_now {
                            guard.wake_now = false;
                            false
                        } else {
                            let (mut guard, _) = stop_bg
                                .1
                                .wait_timeout_while(
                                    guard,
                                    Duration::from_secs(min_sleep_secs),
                                    |s| !s.stop && !s.wake_now,
                                )
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            guard.wake_now = false;
                            guard.stop
                        }
                    };
                    if stopped {
                        break;
                    }
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let ts = now_ms();
                        let bots = store.list_bots().unwrap_or_default();
                        for bot in bots {
                            if lock(&stop_bg.0).stop {
                                break;
                            }
                            if bot.paused {
                                continue;
                            }
                            if bot.connectors.github.enabled {
                                let interval_ms = bot
                                    .connectors
                                    .github
                                    .poll_interval_secs
                                    .max(15)
                                    .saturating_mul(1_000);
                                let key = (bot.id.clone(), ExternalSource::GitHub);
                                if last_polled
                                    .get(&key)
                                    .is_none_or(|&prev| ts.saturating_sub(prev) >= interval_ms)
                                {
                                    last_polled.insert(key.clone(), ts);
                                    let prev_label = lock(&statuses)
                                        .get(&key)
                                        .and_then(|s| s.account_label.clone())
                                        .or_else(|| {
                                            Some(if bot.connectors.github.repos.is_empty() {
                                                "GitHub notifications".to_owned()
                                            } else {
                                                format!(
                                                    "{} repo(s)",
                                                    bot.connectors.github.repos.len()
                                                )
                                            })
                                        });
                                    let res =
                                        poll_github_bot(&bot, &secrets, &items, &endpoints, ts);
                                    let still_active = !lock(&stop_bg.0).stop
                                        && store.get_bot(&bot.id).ok().flatten().is_some_and(
                                            |live| !live.paused && live.connectors.github.enabled,
                                        );
                                    if still_active {
                                        let st = match res {
                                            Ok(new_count) => {
                                                if new_count > 0
                                                    && let Some(cb) =
                                                        lock(&on_new_items).as_ref().cloned()
                                                {
                                                    cb(&bot, ExternalSource::GitHub, new_count);
                                                }
                                                ok_status(
                                                    &bot.id,
                                                    ExternalSource::GitHub,
                                                    prev_label,
                                                    ts,
                                                )
                                            }
                                            Err(d) => {
                                                err_status(&bot.id, ExternalSource::GitHub, d)
                                            }
                                        };
                                        lock(&statuses).insert(key, st);
                                    } else {
                                        lock(&statuses).remove(&key);
                                    }
                                }
                            }
                            if lock(&stop_bg.0).stop {
                                break;
                            }
                            if bot.connectors.linear.enabled {
                                let interval_ms = bot
                                    .connectors
                                    .linear
                                    .poll_interval_secs
                                    .max(15)
                                    .saturating_mul(1_000);
                                let key = (bot.id.clone(), ExternalSource::Linear);
                                if last_polled
                                    .get(&key)
                                    .is_none_or(|&prev| ts.saturating_sub(prev) >= interval_ms)
                                {
                                    last_polled.insert(key.clone(), ts);
                                    let prev_label = lock(&statuses)
                                        .get(&key)
                                        .and_then(|s| s.account_label.clone())
                                        .or_else(|| Some("Linear GraphQL".to_owned()));
                                    let res =
                                        poll_linear_bot(&bot, &secrets, &items, &endpoints, ts);
                                    let still_active = !lock(&stop_bg.0).stop
                                        && store.get_bot(&bot.id).ok().flatten().is_some_and(
                                            |live| !live.paused && live.connectors.linear.enabled,
                                        );
                                    if still_active {
                                        let st = match res {
                                            Ok(new_count) => {
                                                if new_count > 0
                                                    && let Some(cb) =
                                                        lock(&on_new_items).as_ref().cloned()
                                                {
                                                    cb(&bot, ExternalSource::Linear, new_count);
                                                }
                                                ok_status(
                                                    &bot.id,
                                                    ExternalSource::Linear,
                                                    prev_label,
                                                    ts,
                                                )
                                            }
                                            Err(d) => {
                                                err_status(&bot.id, ExternalSource::Linear, d)
                                            }
                                        };
                                        lock(&statuses).insert(key, st);
                                    } else {
                                        lock(&statuses).remove(&key);
                                    }
                                }
                            }
                        }
                    }))
                    .ok();
                }
            })
        {
            self.running.store(true, Ordering::Release);
            *lifecycle_guard = Some(RunningTrackerPoller { stop, handle });
        }
    }

    /// Stops the background tracker poller thread cleanly.
    pub fn stop(&self) {
        let _reconcile = lock(&self.reconcile_lock);
        self.stop_locked();
    }

    fn stop_locked(&self) {
        let running = {
            let mut guard = lock(&self.lifecycle);
            let taken = guard.take();
            if let Some(ref r) = taken {
                lock(&r.stop.0).stop = true;
                r.stop.1.notify_all();
            }
            self.running.store(false, Ordering::Release);
            taken
        };
        if let Some(r) = running {
            r.handle.join().ok();
        }
    }
}

fn ok_status(
    bot_id: &BotId,
    source: ExternalSource,
    account_label: Option<String>,
    ts: u64,
) -> BotConnectorStatus {
    BotConnectorStatus {
        bot_id: bot_id.clone(),
        source,
        enabled: true,
        connected: true,
        state: ConnectorHealthState::Connected,
        account_label,
        last_sync_ms: Some(ts),
        last_error: None,
        error_category: None,
    }
}

fn err_status(
    bot_id: &BotId,
    source: ExternalSource,
    detail: BotErrorDetail,
) -> BotConnectorStatus {
    BotConnectorStatus {
        bot_id: bot_id.clone(),
        source,
        enabled: true,
        connected: false,
        state: ConnectorHealthState::Error,
        account_label: None,
        last_sync_ms: None,
        last_error: Some(detail.message),
        error_category: Some(detail.category),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    use zeroize::Zeroizing;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-bot-trackers-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn github_and_linear_pollers_ingest_and_deduplicate_items() {
        let dir = temp_dir("poll");
        let secrets = SecretVault::with_static_key(dir.clone(), [0x88; 32]);
        let items = BotItemsStore::new(dir.join("bot-items.db"));

        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        mock.set_nonblocking(true).unwrap();
        let addr = mock.local_addr().unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let server = thread::spawn(move || {
            let mut issues_hits = 0usize;
            while !stop_bg.load(std::sync::atomic::Ordering::Acquire) {
                let (mut sock, _) = match mock.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                sock.set_nonblocking(false).ok();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    continue;
                }
                let req = String::from_utf8_lossy(buf.get(..n).unwrap_or_default());
                let target = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let body = if target.contains("/pulls") {
                    r#"[{"number":101,"title":"Fix race condition","html_url":"https://github.com/craftowen/krill/pull/101","requested_reviewers":[{"login":"owen"}]}]"#
                } else if target.contains("/issues") {
                    issues_hits = issues_hits.saturating_add(1);
                    if issues_hits >= 2 {
                        r#"[{"number":202,"title":"Add issue poller","html_url":"https://github.com/craftowen/krill/issues/202","assignees":[{"login":"owen"}]},{"number":101,"title":"PR dupe","html_url":"https://github.com/craftowen/krill/pull/101","pull_request":{"url":"..."}}]"#
                    } else {
                        "[]"
                    }
                } else {
                    r#"{"data":{"viewer":{"assignedIssues":{"nodes":[{"id":"l1","identifier":"OWE-900","title":"Ship bots","description":"Track bot work","url":"https://linear.app/owen/issue/OWE-900","team":{"key":"OWE"}}]}}}}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                sock.write_all(resp.as_bytes()).unwrap();
            }
        });

        let bot_id = BotId::new("bot-trk").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "Tracker Bot", "claude", 1_000).unwrap();
        bot.connectors.github.enabled = true;
        bot.connectors.github.repos = vec!["craftowen/krill".to_owned()];
        bot.connectors.linear.enabled = true;
        bot.connectors.linear.team_keys = vec!["OWE".to_owned()];

        secrets
            .set_secret(
                &bot_id,
                "github_token",
                Zeroizing::new("ghp_test".to_owned()),
            )
            .unwrap();
        secrets
            .set_secret(
                &bot_id,
                "linear_api_key",
                Zeroizing::new("lin_api_test".to_owned()),
            )
            .unwrap();

        let endpoints = TrackerEndpoints {
            github_api_base: format!("http://{addr}"),
            linear_graphql_url: format!("http://{addr}/graphql"),
        };

        assert_eq!(
            poll_github_bot(&bot, &secrets, &items, &endpoints, 2_000).unwrap(),
            1
        );
        // Second GitHub poll picks up issue #202 (PR #101 deduplicated and skipped via pull_request check).
        assert_eq!(
            poll_github_bot(&bot, &secrets, &items, &endpoints, 3_000).unwrap(),
            1
        );
        assert_eq!(
            poll_linear_bot(&bot, &secrets, &items, &endpoints, 4_000).unwrap(),
            1
        );

        let all = items
            .list_items(Some(&bot_id), None, true, true, 5_000)
            .unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().any(|i| i.status == BotItemStatus::NeedsYou
            && i.links.contains(&BotItemLink::Pr(
                "https://github.com/craftowen/krill/pull/101".to_owned()
            ))));
        assert!(all.iter().any(|i| i.title.contains("#202")));
        assert!(all.iter().any(|i| i.title.contains("OWE-900")));

        stop.store(true, std::sync::atomic::Ordering::Release);
        server.join().unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trackers_graphql_errors_api_to_html_url_and_tests_work() {
        assert_eq!(
            github_api_url_to_html_url("https://api.github.com/repos/a/b/pulls/77"),
            "https://github.com/a/b/pull/77"
        );
        assert_eq!(
            github_api_url_to_html_url("https://api.github.com/repos/a/b/issues/12"),
            "https://github.com/a/b/issues/12"
        );

        let dir = temp_dir("gql-err");
        let secrets = SecretVault::with_static_key(dir.clone(), [0x22; 32]);
        let items = BotItemsStore::new(dir.join("items.db"));
        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = mock.local_addr().unwrap();

        let srv = thread::spawn(move || {
            for idx in 0..3 {
                let (mut sock, _) = mock.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                assert!(n > 0);
                let body = if idx == 0 {
                    r#"{"errors":[{"message":"Authentication failed: invalid token"}]}"#
                } else if idx == 1 {
                    r#"{"login":"octocat"}"#
                } else {
                    r#"{"data":{"viewer":{"id":"v1","name":"Owen","email":"owen@example.com"}}}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                sock.write_all(resp.as_bytes()).unwrap();
            }
        });

        let bot_id = BotId::new("bot-trk-err").unwrap();
        let bot = BotConfig::new(bot_id.clone(), "TrkErr", "claude", 1_000).unwrap();
        secrets
            .set_secret(&bot_id, "github_token", Zeroizing::new("ghp_1".to_owned()))
            .unwrap();
        secrets
            .set_secret(
                &bot_id,
                "linear_api_key",
                Zeroizing::new("lin_api_1".to_owned()),
            )
            .unwrap();
        let endpoints = TrackerEndpoints {
            github_api_base: format!("http://{addr}"),
            linear_graphql_url: format!("http://{addr}/graphql"),
        };

        let err = poll_linear_bot(&bot, &secrets, &items, &endpoints, 1_000).unwrap_err();
        assert_eq!(err.category, BotErrorCategory::SignedOut);

        let gh_res = test_github(&bot, &secrets, &endpoints);
        assert!(gh_res.ok);
        assert_eq!(gh_res.account_label.as_deref(), Some("@octocat"));

        let lin_res = test_linear(&bot, &secrets, &endpoints);
        assert!(lin_res.ok);
        assert_eq!(lin_res.account_label.as_deref(), Some("owen@example.com"));

        srv.join().unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn poll_github_propagates_404_429_and_5xx_errors_instead_of_reporting_ok() {
        let dir = temp_dir("gh-non-2xx");
        let secrets = SecretVault::with_static_key(dir.clone(), [0x33; 32]);
        let items = BotItemsStore::new(dir.join("items.db"));
        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = mock.local_addr().unwrap();

        let srv = thread::spawn(move || {
            let responses = [
                ("404 Not Found", r#"{"message":"Not Found"}"#),
                ("429 Too Many Requests", r#"{"message":"rate limit"}"#),
                ("502 Bad Gateway", r#"{"message":"upstream down"}"#),
            ];
            for (status, body) in responses {
                let (mut sock, _) = mock.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                assert!(n > 0);
                let wire = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).unwrap();
            }
        });

        let bot_id = BotId::new("bot-gh-non2xx").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "GhNon2xx", "claude", 1_000).unwrap();
        bot.connectors.github.repos = vec!["acme/missing".to_owned()];
        secrets
            .set_secret(&bot_id, "github_token", Zeroizing::new("ghp_1".to_owned()))
            .unwrap();
        let endpoints = TrackerEndpoints {
            github_api_base: format!("http://{addr}"),
            linear_graphql_url: format!("http://{addr}/graphql"),
        };

        // 404 -> AgentError
        let e404 = poll_github_bot(&bot, &secrets, &items, &endpoints, 1_000).unwrap_err();
        assert_eq!(e404.category, BotErrorCategory::AgentError);
        assert!(e404.message.contains("404"));

        // 429 -> UsageLimit
        let e429 = poll_github_bot(&bot, &secrets, &items, &endpoints, 2_000).unwrap_err();
        assert_eq!(e429.category, BotErrorCategory::UsageLimit);
        assert!(e429.message.contains("429"));

        // 502 on notifications -> AgentError
        bot.connectors.github.repos.clear();
        let e502 = poll_github_bot(&bot, &secrets, &items, &endpoints, 3_000).unwrap_err();
        assert_eq!(e502.category, BotErrorCategory::AgentError);
        assert!(e502.message.contains("502"));

        srv.join().unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tracker_trims_github_token_clamps_cjk_and_prunes_disabled_statuses() {
        let dir = temp_dir("mb17-39-41-44");
        let store = Arc::new(BotStore::new(dir.clone()));
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x44; 32]));
        let items = Arc::new(BotItemsStore::new(dir.join("items.db")));

        let bot = store
            .create_bot(
                krill_proto::bots::CreateBotParams {
                    name: "Tracker Regression".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: None,
                },
                1_000,
            )
            .unwrap();

        // MB-39: >200B CJK title + >20KB description must succeed via UTF-8 byte clamping.
        let cjk_title = "이슈제목".repeat(40); // 480 bytes
        let huge_desc = "설".repeat(8_000); // 24,000 bytes
        let created = create_dedup_item(
            &items,
            &bot.id,
            &cjk_title,
            Some(huge_desc),
            BotItemStatus::Planned,
            BotItemLink::Url("https://github.com/acme/repo/issues/99".to_owned()),
            &BotItemActor::System {
                reason: "test".to_owned(),
            },
            1_000,
        );
        assert!(created, "clamped CJK title/desc item must be created");

        // MB-44: GitHub token with surrounding CRLF/whitespace must be trimmed before HTTP header.
        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = mock.local_addr().unwrap();
        let srv = thread::spawn(move || {
            let (mut sock, _) = mock.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(buf.get(..n).unwrap_or_default()).into_owned();
            assert!(
                req.contains("Authorization: Bearer ghp_trimmed_tok\r\n"),
                "token was not trimmed cleanly: {req}"
            );
            let body = r#"{"login":"trimuser"}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(wire.as_bytes()).unwrap();
        });
        secrets
            .set_secret(
                &bot.id,
                "github_token",
                Zeroizing::new("  ghp_trimmed_tok\r\n ".to_owned()),
            )
            .unwrap();
        let ep = TrackerEndpoints {
            github_api_base: format!("http://{addr}"),
            linear_graphql_url: format!("http://{addr}/graphql"),
        };
        let res = test_github(&bot, &secrets, &ep);
        assert!(res.ok, "{res:?}");
        srv.join().unwrap();

        // MB-17 & MB-41: reconcile prunes disabled statuses and cleanly handles stop/restart cycles.
        let poller = TrackerPoller::new(Arc::clone(&store), secrets, items, ep);
        lock(&poller.statuses).insert(
            (bot.id.clone(), ExternalSource::GitHub),
            ok_status(&bot.id, ExternalSource::GitHub, None, 1_000),
        );
        assert_eq!(poller.statuses(None).len(), 1);
        poller.reconcile();
        assert!(
            poller.statuses(None).is_empty(),
            "disabled tracker status must be pruned on reconcile"
        );
        assert!(!poller.is_running());

        // Enable, start, reconcile/stop rapidly to ensure zero stop/restart race.
        let mut conn = bot.connectors.clone();
        conn.github.enabled = true;
        store.set_connector_config(&bot.id, conn, 2_000).unwrap();
        poller.reconcile();
        assert!(poller.is_running());
        poller.stop();
        assert!(!poller.is_running());
        poller.reconcile();
        assert!(poller.is_running());
        poller.stop();
        assert!(!poller.is_running());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn r2m07_r2m10_rejects_traversal_github_repos_and_prevents_disabled_poller_resurrection() {
        // R2M-07: Path traversal and dotted owners rejected.
        assert!(is_valid_github_repo("craftowen/krill"));
        assert!(is_valid_github_repo("org_1/repo.rs"));
        assert!(!is_valid_github_repo("../users"));
        assert!(!is_valid_github_repo("owner/.."));
        assert!(!is_valid_github_repo("./repo"));
        assert!(!is_valid_github_repo("owner/."));
        assert!(!is_valid_github_repo("owner.dot/repo"));
        assert!(!is_valid_github_repo("a/b/c"));

        let dir = temp_dir("r2m07-10");
        let store = Arc::new(BotStore::new(dir.clone()));
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x45; 32]));
        let items = Arc::new(BotItemsStore::new(dir.join("items.db")));
        let bot = store
            .create_bot(
                krill_proto::bots::CreateBotParams {
                    name: "Tracker R2".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: None,
                },
                1_000,
            )
            .unwrap();
        let mut bad_conn = bot.connectors.clone();
        bad_conn.github.repos = vec!["../users".to_owned()];
        assert_eq!(
            store
                .set_connector_config(&bot.id, bad_conn, 1_100)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );

        // R2M-10: Disabling a bot while its HTTP poll is in flight must NOT resurrect status or emit on_new_items.
        let poll_started = Arc::new(AtomicBool::new(false));
        let poll_started_bg = Arc::clone(&poll_started);
        let release_poll = Arc::new(AtomicBool::new(false));
        let release_poll_bg = Arc::clone(&release_poll);
        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = mock.local_addr().unwrap();
        let srv = thread::spawn(move || {
            let (mut sock, _) = mock.accept().unwrap();
            let mut buf = [0u8; 4096];
            assert!(sock.read(&mut buf).unwrap_or(0) > 0);
            poll_started_bg.store(true, Ordering::Release);
            let until = std::time::Instant::now() + Duration::from_secs(3);
            while !release_poll_bg.load(Ordering::Acquire) && std::time::Instant::now() < until {
                thread::sleep(Duration::from_millis(10));
            }
            let body = r#"[{"id":"n1","subject":{"title":"PR #1","type":"PullRequest","url":"https://api.github.com/repos/o/r/pulls/1"}}]"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(wire.as_bytes()).ok();
        });

        secrets
            .set_secret(&bot.id, "github_token", Zeroizing::new("ghp_ok".to_owned()))
            .unwrap();
        let mut conn = bot.connectors.clone();
        conn.github.enabled = true;
        store
            .set_connector_config(&bot.id, conn.clone(), 1_200)
            .unwrap();

        let ep = TrackerEndpoints {
            github_api_base: format!("http://{addr}"),
            linear_graphql_url: format!("http://{addr}/graphql"),
        };
        let poller = TrackerPoller::new(Arc::clone(&store), secrets, items, ep);
        let cb_fired = Arc::new(AtomicBool::new(false));
        let cb_fired_bg = Arc::clone(&cb_fired);
        poller.set_on_new_items(Arc::new(move |_, _, _| {
            cb_fired_bg.store(true, Ordering::Release);
        }));
        poller.reconcile();

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !poll_started.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(poll_started.load(Ordering::Acquire));

        // Disable bot while HTTP poll is in flight, then release HTTP response and reconcile.
        conn.github.enabled = false;
        store.set_connector_config(&bot.id, conn, 1_300).unwrap();
        release_poll.store(true, Ordering::Release);
        poller.reconcile();

        assert!(
            !cb_fired.load(Ordering::Acquire),
            "on_new_items must NOT fire for a bot disabled during in-flight poll"
        );
        assert!(
            poller.statuses(None).is_empty(),
            "disabled bot status must NOT resurrect after in-flight poll finishes"
        );
        srv.join().ok();
        fs::remove_dir_all(&dir).ok();
    }
}
