// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Proactive "Look around" scanner generating structured [`BotSuggestion`]s from open items,
//! PR links, pending access requests, and workspace state.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use krill_proto::bots::{
    BotAccessRequestStatus, BotId, BotItemLink, BotItemStatus, BotSuggestion,
    DEFAULT_LOOK_AROUND_MAX_SUGGESTIONS, ExternalSource, soul_description,
};

use crate::bots::items::BotItemsStore;
use crate::bots::store::BotStore;
use crate::session::lock;

const STALE_WORKING_MS: u64 = 24 * 60 * 60 * 1_000;
static SUGG_SEQ: AtomicU64 = AtomicU64::new(1);

/// Tracks per-bot last Look Around timestamps for periodic scheduler ticks.
#[derive(Debug, Default)]
pub struct LookAroundTracker {
    last_run_ms: Mutex<HashMap<BotId, u64>>,
}

impl LookAroundTracker {
    #[must_use]
    pub fn is_due(
        &self,
        bot_id: &BotId,
        interval_secs: u64,
        weekdays_only: bool,
        now_ms: u64,
    ) -> bool {
        if weekdays_only && is_weekend_utc(now_ms / 1_000) {
            return false;
        }
        let interval_ms = interval_secs.max(60).saturating_mul(1_000);
        let guard = lock(&self.last_run_ms);
        guard
            .get(bot_id)
            .is_none_or(|&last| now_ms.saturating_sub(last) >= interval_ms)
    }

    pub fn record(&self, bot_id: &BotId, now_ms: u64) {
        lock(&self.last_run_ms).insert(bot_id.clone(), now_ms);
    }
}

/// Runs a proactive Look Around scan for `bot_id`, persisting and returning up to `max_suggestions`.
pub fn run_look_around(
    store: &BotStore,
    items: &BotItemsStore,
    bot_id: &BotId,
    now_ms: u64,
) -> io::Result<Vec<BotSuggestion>> {
    let bot = store.get_bot(bot_id)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("bot not found: {}", bot_id.as_str()),
        )
    })?;

    let cap = usize::try_from(if bot.look_around.max_suggestions == 0 {
        DEFAULT_LOOK_AROUND_MAX_SUGGESTIONS
    } else {
        bot.look_around.max_suggestions.min(20)
    })
    .unwrap_or(5);

    let mut suggestions = Vec::new();
    let focus_hint = bot.look_around.focus.trim();

    // 1. Scan open bot items (NeedsYou decisions, open PR links, stale Working items, Planned queue).
    let open_items = items.list_items(Some(bot_id), None, false, false, now_ms)?;
    for item in &open_items {
        if suggestions.len() >= cap {
            break;
        }
        let pr_link = item.links.iter().find_map(|l| match l {
            BotItemLink::Pr(url) => Some(url.clone()),
            _ => None,
        });
        let url_link = pr_link.clone().or_else(|| {
            item.links.iter().find_map(|l| match l {
                BotItemLink::Url(url) => Some(url.clone()),
                _ => None,
            })
        });

        match item.status {
            BotItemStatus::NeedsYou => {
                suggestions.push(make_suggestion(
                    bot_id,
                    format!("Unblock #{}: {}", item.number, item.title),
                    "This item is waiting on an owner decision before the bot can continue."
                        .to_owned(),
                    format!("Review item #{} and reply with your decision.", item.number),
                    url_link,
                    pr_link.map(|_| ExternalSource::GitHub),
                    now_ms,
                ));
            }
            BotItemStatus::Working
                if now_ms.saturating_sub(item.last_activity_at_ms) >= STALE_WORKING_MS =>
            {
                suggestions.push(make_suggestion(
                    bot_id,
                    format!("Check stale work on #{}: {}", item.number, item.title),
                    "This item has been marked Working with no recorded activity for over 24 hours."
                        .to_owned(),
                    format!(
                        "Ask {} for a progress update or mark item #{} done/canceled.",
                        bot.name, item.number
                    ),
                    url_link,
                    pr_link.map(|_| ExternalSource::GitHub),
                    now_ms,
                ));
            }
            BotItemStatus::Planned => {
                let why = if let Some(ref pr) = pr_link {
                    format!("Linked pull request ({pr}) is still queued in Planned.")
                } else if !focus_hint.is_empty() {
                    format!("Planned work aligned with focus: {focus_hint}")
                } else {
                    "Queued deliverable ready to start.".to_owned()
                };
                suggestions.push(make_suggestion(
                    bot_id,
                    format!("Start #{}: {}", item.number, item.title),
                    why,
                    format!("Kick off {} on item #{}.", bot.name, item.number),
                    url_link,
                    pr_link.map(|_| ExternalSource::GitHub),
                    now_ms,
                ));
            }
            _ => {}
        }
    }

    // 2. Pending external access requests needing owner approval.
    if suggestions.len() < cap {
        let pending_access: Vec<_> = store
            .list_access_requests(Some(bot_id))?
            .into_iter()
            .filter(|r| r.status == BotAccessRequestStatus::Pending)
            .collect();
        if !pending_access.is_empty() {
            let count = pending_access.len();
            let first = pending_access.first();
            suggestions.push(make_suggestion(
                bot_id,
                format!("Review {count} pending access request(s)"),
                format!(
                    "{} requested access via {}.",
                    first.map_or("An external user", |r| r.sender_name.as_str()),
                    first.map_or("connector", |r| r.source.as_str())
                ),
                "Approve or decline pending external senders in Connectors & Access.".to_owned(),
                None,
                first.map(|r| r.source),
                now_ms,
            ));
        }
    }

    // 3. Workspace health check (missing workspace path or dirty git worktree).
    if suggestions.len() < cap
        && let Some(ws) = bot.workspace.as_deref().filter(|s| !s.trim().is_empty())
    {
        let ws_path = Path::new(ws);
        if !ws_path.is_dir() {
            suggestions.push(make_suggestion(
                bot_id,
                "Fix missing bot workspace folder".to_owned(),
                "The configured workspace directory does not exist on this machine.".to_owned(),
                "Update the bot workspace path or recreate the folder.".to_owned(),
                None,
                None,
                now_ms,
            ));
        } else if ws_path.join(".git").exists()
            && let Some(dirty_count) = quick_git_dirty_count(ws_path)
            && dirty_count > 0
        {
            suggestions.push(make_suggestion(
                bot_id,
                format!("Review {dirty_count} uncommitted file(s) in workspace"),
                "The bot's configured git workspace has uncommitted changes.".to_owned(),
                format!(
                    "Ask {} to review or summarize local workspace changes.",
                    bot.name
                ),
                None,
                Some(ExternalSource::GitHub),
                now_ms,
            ));
        }
    }

    // 4. Uncustomized starter SOUL.md nudge if still empty suggestions.
    if suggestions.len() < cap
        && let Ok(soul) = store.get_soul(bot_id)
        && soul_description(&soul).is_none()
    {
        suggestions.push(make_suggestion(
            bot_id,
            format!("Personalize {}'s SOUL.md", bot.name),
            "SOUL.md still uses the default starter description.".to_owned(),
            "Describe what this bot watches, how it works, and what it must never do.".to_owned(),
            None,
            None,
            now_ms,
        ));
    }

    suggestions.truncate(cap);
    store.save_suggestions(bot_id, &suggestions)?;
    Ok(suggestions)
}

fn make_suggestion(
    bot_id: &BotId,
    title: String,
    why: String,
    ask: String,
    link: Option<String>,
    source: Option<ExternalSource>,
    now_ms: u64,
) -> BotSuggestion {
    let seq = SUGG_SEQ.fetch_add(1, Ordering::Relaxed);
    BotSuggestion {
        id: format!("sugg-{now_ms}-{seq}"),
        bot_id: bot_id.clone(),
        title,
        why,
        ask,
        link,
        source,
        dismissed: false,
        created_at_ms: now_ms,
    }
}

fn is_weekend_utc(unix_secs: u64) -> bool {
    let days = unix_secs / 86_400;
    let weekday = (days + 4) % 7; // 0 = Sun, 6 = Sat
    weekday == 0 || weekday == 6
}

fn quick_git_dirty_count(workspace: &Path) -> Option<usize> {
    let output = krill_proto::os::background_command("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let count = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    Some(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::{BotItemActor, CreateBotItemParams, CreateBotParams};
    use std::fs;
    use std::path::PathBuf;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-bot-look-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn look_around_generates_bounded_suggestions_from_items_and_access_requests() {
        let root = temp_dir("scan");
        let store = BotStore::new(root.clone());
        let items = BotItemsStore::new(root.join("bot-items.db"));

        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "Ops Scout".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: Some("# Ops Scout\n\nWatches open PRs.\n".to_owned()),
                },
                1_000,
            )
            .unwrap();

        let actor = BotItemActor::Owner { device: None };
        items
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot.id.clone(),
                    title: "Approve prod deploy".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    links: vec![BotItemLink::Pr(
                        "https://github.com/craftowen/krill/pull/99".to_owned(),
                    )],
                },
                &actor,
                1_500,
            )
            .unwrap();

        let _ = store
            .check_or_request_access(&bot.id, ExternalSource::Discord, "u1", "Bob", "ch1", 2_000)
            .unwrap();

        let suggestions = run_look_around(&store, &items, &bot.id, 3_000).unwrap();
        assert_eq!(suggestions.len(), 2);
        assert!(
            suggestions
                .iter()
                .any(|s| s.title.contains("Approve prod deploy"))
        );
        assert!(
            suggestions
                .iter()
                .any(|s| s.title.contains("access request"))
        );

        fs::remove_dir_all(&root).ok();
    }
}
