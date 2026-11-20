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
    BotConfig, BotConnectorStatus, BotErrorCategory, BotErrorDetail, BotId, BotItemActor,
    BotItemLink, BotItemStatus, ConnectorHealthState, CreateBotItemParams, ExternalSource,
};

use crate::bots::http_client::{self, HttpRequest};
use crate::bots::items::BotItemsStore;
use crate::bots::secrets::SecretVault;
use crate::bots::store::BotStore;
use crate::session::lock;

const STACK_SIZE: usize = 128 * 1024;
const DEFAULT_POLL_SECS: u64 = 60;
const GH_TIMEOUT: Duration = Duration::from_secs(15);

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

    if let Some(token) = token {
        let auth = format!("Bearer {}", *token);
        let base = endpoints.github_api_base.trim_end_matches('/');
        if bot.connectors.github.repos.is_empty() {
            let url = format!("{base}/notifications?per_page=10");
            let resp = http_client::send(
                &HttpRequest::get(&url)
                    .with_header("Authorization", &auth)
                    .with_header("Accept", "application/vnd.github+json")
                    .with_header("X-GitHub-Api-Version", "2022-11-28"),
            )
            .map_err(|err| tracker_transport_err("GitHub", &err))?;
            if resp.status == 401 || resp.status == 403 {
                return Err(BotErrorDetail {
                    category: BotErrorCategory::SignedOut,
                    agent_name: None,
                    message: format!("GitHub authentication failed (HTTP {})", resp.status),
                    resets_at_ms: None,
                });
            }
            if resp.is_success()
                && let Ok(list) = resp.json::<Vec<serde_json::Value>>()
            {
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
                    let link = if subject_type.eq_ignore_ascii_case("PullRequest") {
                        BotItemLink::Pr(raw_url.to_owned())
                    } else {
                        BotItemLink::Url(raw_url.to_owned())
                    };
                    let reason = notif
                        .get("reason")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    let status = if reason == "review_requested" || reason == "mention" {
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
                        .with_header("X-GitHub-Api-Version", "2022-11-28"),
                )
                .map_err(|err| tracker_transport_err("GitHub", &err))?;
                if resp.status == 401 || resp.status == 403 {
                    return Err(BotErrorDetail {
                        category: BotErrorCategory::SignedOut,
                        agent_name: None,
                        message: format!("GitHub authentication failed (HTTP {})", resp.status),
                        resets_at_ms: None,
                    });
                }
                if resp.is_success()
                    && let Ok(prs) = resp.json::<Vec<serde_json::Value>>()
                {
                    for pr in prs {
                        created = created.saturating_add(ingest_github_pr(
                            items, &bot.id, repo, &pr, &actor, current_ms,
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
                let raw_url = notif
                    .get("subject")
                    .and_then(|s| s.get("url"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if raw_url.is_empty() {
                    continue;
                }
                if create_dedup_item(
                    items,
                    &bot.id,
                    title,
                    None,
                    BotItemStatus::Planned,
                    BotItemLink::Url(raw_url.to_owned()),
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
        }
    }

    Ok(created)
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

    let deadline = Instant::now() + GH_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                krill_proto::os::kill_tree(&mut child);
                return Err(BotErrorDetail {
                    category: BotErrorCategory::AgentError,
                    agent_name: None,
                    message: "gh CLI timed out".to_owned(),
                    resets_at_ms: None,
                });
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(err) => {
                return Err(BotErrorDetail {
                    category: BotErrorCategory::AgentError,
                    agent_name: None,
                    message: format!("gh CLI wait failed: {}", err.kind()),
                    resets_at_ms: None,
                });
            }
        }
    }

    let output = child.wait_with_output().map_err(|err| BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: format!("gh CLI output error: {}", err.kind()),
        resets_at_ms: None,
    })?;
    if !output.status.success() {
        return Err(BotErrorDetail {
            category: BotErrorCategory::SignedOut,
            agent_name: None,
            message: "gh CLI exited non-zero (check `gh auth status`)".to_owned(),
            resets_at_ms: None,
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
            .with_header("Authorization", api_key.trim()),
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
    let clean_title: String = title.trim().chars().take(180).collect();
    if clean_title.is_empty() {
        return false;
    }
    items
        .create_item(
            &CreateBotItemParams {
                bot_id: bot_id.clone(),
                title: clean_title,
                description,
                done_means: None,
                status: Some(status),
                links: vec![link],
            },
            actor,
            now_ms,
        )
        .is_ok()
}

fn is_valid_github_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && !name.is_empty()
        && !name.contains('/')
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn tracker_transport_err(provider: &str, err: &io::Error) -> BotErrorDetail {
    BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: format!("{provider} poll network error: {}", err.kind()),
        resets_at_ms: None,
    }
}

/// Strictly-lazy 60s background poller for GitHub & Linear trackers.
pub struct TrackerPoller {
    store: Arc<BotStore>,
    secrets: Arc<SecretVault>,
    items: Arc<BotItemsStore>,
    endpoints: TrackerEndpoints,
    statuses: Arc<Mutex<BTreeMap<(BotId, ExternalSource), BotConnectorStatus>>>,
    stop: Arc<(Mutex<bool>, Condvar)>,
    running: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
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
            stop: Arc::new((Mutex::new(false), Condvar::new())),
            running: AtomicBool::new(false),
            thread: Mutex::new(None),
        }
    }

    /// Starts the poller thread if ≥1 bot enables GitHub or Linear; stops it otherwise.
    pub fn reconcile(&self) {
        let has_enabled = self
            .store
            .list_bots()
            .unwrap_or_default()
            .iter()
            .any(|b| !b.paused && (b.connectors.github.enabled || b.connectors.linear.enabled));
        if has_enabled {
            self.ensure_started();
        } else {
            self.stop();
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

    fn ensure_started(&self) {
        let mut handle_guard = lock(&self.thread);
        if handle_guard.is_some() {
            return;
        }
        *lock(&self.stop.0) = false;
        let store = Arc::clone(&self.store);
        let secrets = Arc::clone(&self.secrets);
        let items = Arc::clone(&self.items);
        let endpoints = self.endpoints.clone();
        let statuses = Arc::clone(&self.statuses);
        let stop = Arc::clone(&self.stop);

        if let Ok(handle) = thread::Builder::new()
            .name("bot-tracker-poll".to_owned())
            .stack_size(STACK_SIZE)
            .spawn(move || {
                let mut last_polled: BTreeMap<(BotId, ExternalSource), u64> = BTreeMap::new();
                loop {
                    let stopped = {
                        let guard = lock(&stop.0);
                        if *guard {
                            break;
                        }
                        let (guard, _) = stop
                            .1
                            .wait_timeout_while(
                                guard,
                                Duration::from_secs(DEFAULT_POLL_SECS),
                                |s| !*s,
                            )
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        *guard
                    };
                    if stopped {
                        break;
                    }
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let ts = now_ms();
                        let bots = store.list_bots().unwrap_or_default();
                        for bot in bots {
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
                                    let st = match poll_github_bot(
                                        &bot, &secrets, &items, &endpoints, ts,
                                    ) {
                                        Ok(_) => ok_status(&bot.id, ExternalSource::GitHub, ts),
                                        Err(d) => err_status(&bot.id, ExternalSource::GitHub, d),
                                    };
                                    lock(&statuses).insert(key, st);
                                }
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
                                    let st = match poll_linear_bot(
                                        &bot, &secrets, &items, &endpoints, ts,
                                    ) {
                                        Ok(_) => ok_status(&bot.id, ExternalSource::Linear, ts),
                                        Err(d) => err_status(&bot.id, ExternalSource::Linear, d),
                                    };
                                    lock(&statuses).insert(key, st);
                                }
                            }
                        }
                    }))
                    .ok();
                }
            })
        {
            self.running.store(true, Ordering::Release);
            *handle_guard = Some(handle);
        }
    }

    /// Stops the background tracker poller thread cleanly.
    pub fn stop(&self) {
        let handle = lock(&self.thread).take();
        if let Some(h) = handle {
            *lock(&self.stop.0) = true;
            self.stop.1.notify_all();
            h.join().ok();
            self.running.store(false, Ordering::Release);
        }
    }
}

fn ok_status(bot_id: &BotId, source: ExternalSource, ts: u64) -> BotConnectorStatus {
    BotConnectorStatus {
        bot_id: bot_id.clone(),
        source,
        enabled: true,
        connected: true,
        state: ConnectorHealthState::Connected,
        account_label: None,
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
        let addr = mock.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..3 {
                let (mut sock, _) = mock.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(buf.get(..n).unwrap_or_default());
                let target = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let body = if target.contains("/pulls") {
                    r#"[{"number":101,"title":"Fix race condition","html_url":"https://github.com/craftowen/krill/pull/101","requested_reviewers":[{"login":"owen"}]}]"#
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
        // Second GitHub poll must deduplicate the same PR link.
        assert_eq!(
            poll_github_bot(&bot, &secrets, &items, &endpoints, 3_000).unwrap(),
            0
        );
        assert_eq!(
            poll_linear_bot(&bot, &secrets, &items, &endpoints, 4_000).unwrap(),
            1
        );

        let all = items
            .list_items(Some(&bot_id), None, true, true, 5_000)
            .unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|i| i.status == BotItemStatus::NeedsYou
            && i.links.contains(&BotItemLink::Pr(
                "https://github.com/craftowen/krill/pull/101".to_owned()
            ))));
        assert!(all.iter().any(|i| i.title.contains("OWE-900")));

        server.join().unwrap();
        fs::remove_dir_all(&dir).ok();
    }
}
