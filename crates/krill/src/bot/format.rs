// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Human-readable and `--json` output formatters for `krill bot`.

use std::fmt::Write as _;

use krill_proto::SessionId;
use krill_proto::bots::catalog::{BotMcpAppCatalogEntry, BotTemplate, ComposioAppCatalogEntry};
use krill_proto::bots::home::{count_memory_lines, parse_memory_facts};
use krill_proto::bots::{
    BotAccessRequest, BotConfig, BotConnectorStatus, BotConnectorTestResult, BotGroupChat,
    BotGroupMessage, BotGroupRoomView, BotGroupView, BotId, BotItem, BotItemActor, BotItemLink,
    BotItemsPage, BotMcpOAuthState, BotMcpToolConfig, BotMemoryHistoryEntry, BotMemoryTidyStatus,
    BotMemoryTopicInfo, BotScheduleConfig, BotSessionSummary, BotSuggestion, BotToolRelayStatus,
    ComposioConnectionInfo, DefaultTrackerBots, ExternalBotCandidate, ExternalBotSourceApp,
    ExternalSource,
};
use krill_proto::orchestration::SkillEntry;
use serde::Serialize;

use super::CliError;

fn to_json<T: Serialize>(value: &T) -> Result<String, CliError> {
    serde_json::to_string_pretty(value).map_err(|e| CliError::fail(e.to_string()))
}

pub(crate) fn format_ok(message: &str, json: bool) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({ "ok": true, "message": message }))
    } else {
        Ok(message.to_owned())
    }
}

pub(crate) fn format_bots(bots: &[BotConfig], json: bool) -> Result<String, CliError> {
    if json {
        return to_json(&bots);
    }
    if bots.is_empty() {
        return Ok(
            "No bots configured. Run `krill bot templates` or `krill bot new <name>`.".to_owned(),
        );
    }
    let mut out = String::new();
    writeln!(
        out,
        "{:<24} {:<10} {:<8} {:<24} NAME",
        "SLUG", "AGENT", "STATE", "CONNECTORS"
    )
    .ok();
    for bot in bots {
        let state = if bot.paused { "paused" } else { "active" };
        let sources: Vec<&str> = bot
            .connectors
            .enabled_sources()
            .into_iter()
            .map(ExternalSource::as_str)
            .collect();
        let connectors = if sources.is_empty() {
            "-".to_owned()
        } else {
            sources.join(",")
        };
        writeln!(
            out,
            "{:<24} {:<10} {:<8} {:<24} {}",
            bot.slug, bot.agent_id, state, connectors, bot.name
        )
        .ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_bot_detail(bot: &BotConfig, json: bool) -> Result<String, CliError> {
    if json {
        return to_json(bot);
    }
    let mut out = String::new();
    writeln!(out, "id          {}", bot.id).ok();
    writeln!(out, "slug        {}", bot.slug).ok();
    writeln!(out, "name        {}", bot.name).ok();
    if !bot.description.is_empty() {
        writeln!(out, "description {}", bot.description).ok();
    }
    writeln!(out, "agent       {}", bot.agent_id).ok();
    if let Some(ref model) = bot.model {
        writeln!(out, "model       {model}").ok();
    }
    if let Some(ref effort) = bot.effort {
        writeln!(out, "effort      {effort}").ok();
    }
    if let Some(ref emoji) = bot.emoji {
        writeln!(out, "emoji       {emoji}").ok();
    }
    if let Some(ref avatar) = bot.avatar {
        writeln!(out, "avatar      {avatar}").ok();
    }
    if let Some(ref color) = bot.avatar_color {
        writeln!(out, "avatar_color {color}").ok();
    }
    writeln!(
        out,
        "state       {}",
        if bot.paused { "paused" } else { "active" }
    )
    .ok();
    writeln!(out, "first_run_completed {}", bot.first_run_completed).ok();
    writeln!(out, "folder      {}", bot.folder).ok();
    if let Some(ref ws) = bot.workspace {
        writeln!(out, "workspace   {ws}").ok();
    }
    if let Some(ref tpl) = bot.template_id {
        writeln!(out, "template    {tpl}").ok();
    }
    writeln!(
        out,
        "skills      {}",
        if bot.skills.is_empty() {
            "none".to_owned()
        } else {
            bot.skills.join(", ")
        }
    )
    .ok();
    writeln!(
        out,
        "members     {}",
        if bot.members.is_empty() {
            "none".to_owned()
        } else {
            bot.members
                .iter()
                .map(|m| format!("{} ({}:{})", m.name, m.source.as_str(), m.id))
                .collect::<Vec<_>>()
                .join(", ")
        }
    )
    .ok();
    writeln!(
        out,
        "notifications permission={} complete={} error={}",
        bot.notifications.needs_permission,
        bot.notifications.task_complete,
        bot.notifications.could_not_finish
    )
    .ok();
    let sources: Vec<&str> = bot
        .connectors
        .enabled_sources()
        .into_iter()
        .map(ExternalSource::as_str)
        .collect();
    writeln!(
        out,
        "connectors  {}",
        if sources.is_empty() {
            "none".to_owned()
        } else {
            sources.join(", ")
        }
    )
    .ok();
    writeln!(
        out,
        "mcp_tools   {}",
        if bot.mcp_tools.is_empty() {
            "none".to_owned()
        } else {
            bot.mcp_tools
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    )
    .ok();
    writeln!(
        out,
        "schedules   {}",
        if bot.schedules.is_empty() {
            "none".to_owned()
        } else {
            bot.schedules
                .iter()
                .map(|s| {
                    let state = if s.enabled { "enabled" } else { "disabled" };
                    format!("{}: {} ({}) [{}]", s.id, s.name, s.cron, state)
                })
                .collect::<Vec<_>>()
                .join("; ")
        }
    )
    .ok();
    writeln!(
        out,
        "look_around {} ({}s, max {})",
        if bot.look_around.enabled { "on" } else { "off" },
        bot.look_around.interval_secs,
        bot.look_around.max_suggestions
    )
    .ok();
    writeln!(
        out,
        "memory_tidy {} ({}s)",
        if bot.memory_tidy.enabled { "on" } else { "off" },
        bot.memory_tidy.interval_secs
    )
    .ok();
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_bot_connectors(bot: &BotConfig, json: bool) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "bot_id": bot.id,
            "slug": bot.slug,
            "connectors": bot.connectors,
        }));
    }
    let mut out = String::new();
    writeln!(out, "Connectors for {} ({}):", bot.name, bot.slug).ok();
    writeln!(
        out,
        "  telegram  enabled={} chats={} users={}",
        bot.connectors.telegram.enabled,
        bot.connectors.telegram.allowed_chat_ids.len(),
        bot.connectors.telegram.allowed_user_ids.len()
    )
    .ok();
    writeln!(
        out,
        "  slack     enabled={} mirror={} channels={} users={}",
        bot.connectors.slack.enabled,
        bot.connectors.slack.mirror_desktop,
        bot.connectors.slack.allowed_channel_ids.len(),
        bot.connectors.slack.allowed_user_ids.len()
    )
    .ok();
    writeln!(
        out,
        "  discord   enabled={} guilds={} channels={} users={}",
        bot.connectors.discord.enabled,
        bot.connectors.discord.allowed_guild_ids.len(),
        bot.connectors.discord.allowed_channel_ids.len(),
        bot.connectors.discord.allowed_user_ids.len()
    )
    .ok();
    writeln!(
        out,
        "  google    enabled={} poll={}s",
        bot.connectors.google.enabled, bot.connectors.google.poll_interval_secs
    )
    .ok();
    writeln!(
        out,
        "  github    enabled={} gh_cli={} repos={} poll={}s",
        bot.connectors.github.enabled,
        bot.connectors.github.use_gh_cli,
        bot.connectors.github.repos.len(),
        bot.connectors.github.poll_interval_secs
    )
    .ok();
    writeln!(
        out,
        "  linear    enabled={} teams={} poll={}s",
        bot.connectors.linear.enabled,
        bot.connectors.linear.team_keys.len(),
        bot.connectors.linear.poll_interval_secs
    )
    .ok();
    writeln!(
        out,
        "  composio  enabled={} apps={}",
        bot.connectors.composio.enabled,
        bot.connectors.composio.enabled_apps.len()
    )
    .ok();
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_deleted(
    bot_id: &BotId,
    delete_home: bool,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_id": bot_id,
            "deleted": true,
            "delete_home": delete_home,
        }))
    } else if delete_home {
        Ok(format!("deleted bot {bot_id} (home folder removed)"))
    } else {
        Ok(format!("deleted bot {bot_id} (home folder preserved)"))
    }
}

pub(crate) fn format_session_started(
    bot_id: &BotId,
    session_id: SessionId,
    pid: u32,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_id": bot_id,
            "session_id": session_id.0,
            "pid": pid,
        }))
    } else {
        Ok(format!(
            "started bot {bot_id}: session s{} (pid {pid})",
            session_id.0
        ))
    }
}

#[derive(Serialize)]
struct TemplateJson<'a> {
    id: &'a str,
    category: &'a str,
    name: &'a str,
    description: &'a str,
    first_chat: &'a str,
    mascot: &'a str,
    connectors: Vec<&'static str>,
    skills: &'a [&'a str],
    mcp_apps: &'a [&'a str],
    needs_setup: Option<&'a str>,
    example: [&'a str; 2],
}

fn template_json(tpl: &BotTemplate) -> TemplateJson<'_> {
    TemplateJson {
        id: tpl.id,
        category: tpl.category.as_str(),
        name: tpl.name,
        description: tpl.description,
        first_chat: tpl.first_chat,
        mascot: tpl.mascot,
        connectors: tpl.connectors.iter().map(|c| c.as_str()).collect(),
        skills: tpl.skills,
        mcp_apps: tpl.mcp_apps,
        needs_setup: tpl.needs_setup,
        example: tpl.example,
    }
}

pub(crate) fn format_templates(templates: &[&BotTemplate], json: bool) -> Result<String, CliError> {
    if json {
        let rows: Vec<_> = templates.iter().map(|t| template_json(t)).collect();
        return to_json(&rows);
    }
    let mut out = String::new();
    writeln!(
        out,
        "{:<20} {:<10} {:<24} DESCRIPTION",
        "ID", "CATEGORY", "NAME"
    )
    .ok();
    for tpl in templates {
        writeln!(
            out,
            "{:<20} {:<10} {:<24} {}",
            tpl.id,
            tpl.category.as_str(),
            tpl.name,
            tpl.description
        )
        .ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_template_detail(tpl: &BotTemplate, json: bool) -> Result<String, CliError> {
    if json {
        return to_json(&template_json(tpl));
    }
    let mut out = String::new();
    writeln!(out, "id          {}", tpl.id).ok();
    writeln!(out, "name        {}", tpl.name).ok();
    writeln!(out, "category    {}", tpl.category.as_str()).ok();
    writeln!(out, "description {}", tpl.description).ok();
    writeln!(out, "first_chat  {}", tpl.first_chat).ok();
    if !tpl.connectors.is_empty() {
        let cs: Vec<_> = tpl.connectors.iter().map(|c| c.as_str()).collect();
        writeln!(out, "connectors  {}", cs.join(", ")).ok();
    }
    if !tpl.skills.is_empty() {
        writeln!(out, "skills      {}", tpl.skills.join(", ")).ok();
    }
    if !tpl.mcp_apps.is_empty() {
        writeln!(out, "mcp_apps    {}", tpl.mcp_apps.join(", ")).ok();
    }
    if let Some(note) = tpl.needs_setup {
        writeln!(out, "setup       {note}").ok();
    }
    Ok(out.trim_end().to_owned())
}

#[derive(Serialize)]
struct SkillJson<'a> {
    name: &'a str,
    description: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a str>,
}

pub(crate) fn format_skills(skills: &[SkillEntry], json: bool) -> Result<String, CliError> {
    if json {
        let rows: Vec<_> = skills
            .iter()
            .map(|s| SkillJson {
                name: s.name,
                description: s.description,
                content: None,
            })
            .collect();
        return to_json(&rows);
    }
    let mut out = String::new();
    writeln!(out, "{:<24} DESCRIPTION", "SKILL").ok();
    for s in skills {
        writeln!(out, "{:<24} {}", s.name, s.description).ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_skill_detail(skill: &SkillEntry, json: bool) -> Result<String, CliError> {
    if json {
        return to_json(&SkillJson {
            name: skill.name,
            description: skill.description,
            content: Some(skill.content),
        });
    }
    Ok(skill.content.trim_end().to_owned())
}

#[derive(Serialize)]
struct McpAppJson<'a> {
    id: &'a str,
    name: &'a str,
    description: &'a str,
    endpoint_url: &'a str,
    auth_kind: krill_proto::bots::BotMcpAuthKind,
    header_name: Option<&'a str>,
    docs_url: &'a str,
    category: krill_proto::bots::BotMcpAppCategory,
}

#[derive(Serialize)]
struct ComposioAppJson<'a> {
    slug: &'a str,
    name: &'a str,
    description: &'a str,
}

pub(crate) fn format_apps(
    mcp: &[&BotMcpAppCatalogEntry],
    composio: &[&ComposioAppCatalogEntry],
    json: bool,
) -> Result<String, CliError> {
    if json {
        let mcp_rows: Vec<_> = mcp
            .iter()
            .map(|a| McpAppJson {
                id: a.id,
                name: a.name,
                description: a.description,
                endpoint_url: a.endpoint_url,
                auth_kind: a.auth_kind,
                header_name: a.header_name,
                docs_url: a.docs_url,
                category: a.category,
            })
            .collect();
        let composio_rows: Vec<_> = composio
            .iter()
            .map(|a| ComposioAppJson {
                slug: a.slug,
                name: a.name,
                description: a.description,
            })
            .collect();
        return to_json(&serde_json::json!({
            "mcp": mcp_rows,
            "composio": composio_rows,
        }));
    }
    let mut out = String::new();
    if !mcp.is_empty() {
        writeln!(out, "MCP Apps ({}):", mcp.len()).ok();
        for a in mcp {
            writeln!(
                out,
                "  {:<14} {:<14} {} ({})",
                a.id, a.name, a.description, a.endpoint_url
            )
            .ok();
        }
    }
    if !composio.is_empty() {
        if !out.is_empty() {
            writeln!(out).ok();
        }
        writeln!(out, "Composio Curated Apps ({}):", composio.len()).ok();
        for a in composio {
            writeln!(out, "  {:<18} {:<14} {}", a.slug, a.name, a.description).ok();
        }
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_slack_manifest(
    bot_name: &str,
    yaml: &str,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_name": bot_name,
            "socket_mode": true,
            "manifest_yaml": yaml,
        }))
    } else {
        Ok(yaml.trim_end().to_owned())
    }
}

pub(crate) fn format_soul(bot_id: &BotId, content: &str, json: bool) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_id": bot_id,
            "content": content,
        }))
    } else {
        Ok(content.trim_end().to_owned())
    }
}

pub(crate) fn format_soul_saved(bot_id: &BotId, json: bool) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_id": bot_id,
            "updated": true,
        }))
    } else {
        Ok(format!("updated SOUL.md for {bot_id}"))
    }
}

#[derive(Serialize)]
struct FactJson {
    line: usize,
    date: Option<String>,
    text: String,
    source: Option<String>,
}

pub(crate) fn format_memory(bot_id: &BotId, content: &str, json: bool) -> Result<String, CliError> {
    if json {
        let facts: Vec<_> = parse_memory_facts(content)
            .into_iter()
            .map(|f| FactJson {
                line: f.line,
                date: f.date,
                text: f.text,
                source: f.source,
            })
            .collect();
        return to_json(&serde_json::json!({
            "bot_id": bot_id,
            "line_count": count_memory_lines(content),
            "facts": facts,
            "content": content,
        }));
    }
    Ok(content.trim_end().to_owned())
}

pub(crate) fn format_memory_updated(
    bot_id: &BotId,
    content: &str,
    restored_facts: &[String],
    removed_count: usize,
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "bot_id": bot_id,
            "removed_count": removed_count,
            "restored_facts": restored_facts,
            "line_count": count_memory_lines(content),
            "content": content,
        }));
    }
    let mut out = format!(
        "updated MEMORY.md for {bot_id} ({} lines, {removed_count} removed)",
        count_memory_lines(content)
    );
    if !restored_facts.is_empty() {
        write!(out, "; restored {} guarded fact(s)", restored_facts.len()).ok();
    }
    Ok(out)
}

pub(crate) fn format_memory_tidy(
    bot_id: &BotId,
    summary: &str,
    changes: u32,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_id": bot_id,
            "changes": changes,
            "summary": summary,
        }))
    } else {
        Ok(format!(
            "memory tidy for {bot_id}: {changes} change(s) ({summary})"
        ))
    }
}

pub(crate) fn format_items(items: &[BotItem], json: bool) -> Result<String, CliError> {
    if json {
        return to_json(&items);
    }
    if items.is_empty() {
        return Ok("No items found.".to_owned());
    }
    let mut out = String::new();
    writeln!(out, "{:<8} {:<16} {:<10} TITLE", "NUM", "BOT", "STATUS").ok();
    for item in items {
        let snooze = if item.snoozed_until_ms.is_some() {
            " [snoozed]"
        } else {
            ""
        };
        let wait_suffix = item
            .waiting_reason
            .as_deref()
            .filter(|_| item.status == krill_proto::bots::BotItemStatus::Waiting)
            .map_or_else(String::new, |reason| format!(" (waiting: {reason})"));
        writeln!(
            out,
            "#{:<7} {:<16} {:<10} {}{wait_suffix}{snooze}",
            item.number,
            item.owner_bot_id,
            item.status.as_str(),
            item.title
        )
        .ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_items_page(page: &BotItemsPage, json: bool) -> Result<String, CliError> {
    if json {
        return to_json(page);
    }
    let table = format_items(&page.items, false)?;
    let cursor = page.next_cursor.as_deref().unwrap_or("-");
    Ok(format!(
        "{table}\n\ntotal={} unseen={} next_cursor={cursor}",
        page.total_count, page.unseen_count
    ))
}

pub(crate) fn format_item_detail(item: &BotItem, json: bool) -> Result<String, CliError> {
    if json {
        return to_json(item);
    }
    let mut out = String::new();
    writeln!(
        out,
        "#{} [{}] {} ({})",
        item.number,
        item.status.as_str(),
        item.title,
        item.uid
    )
    .ok();
    if let Some(ref desc) = item.description {
        writeln!(out, "  description: {desc}").ok();
    }
    if let Some(ref done) = item.done_means {
        writeln!(out, "  done_means:  {done}").ok();
    }
    if let Some(ref reason) = item.waiting_reason {
        writeln!(out, "  waiting_on:  {reason}").ok();
    }
    if let Some(until) = item.snoozed_until_ms {
        writeln!(out, "  snoozed_ms:  {until}").ok();
    }
    if let Some(ref note) = item.close_note {
        writeln!(out, "  close_note:  {note}").ok();
    }
    for link in &item.links {
        writeln!(out, "  link:        {}", format_link(link)).ok();
    }
    if !item.events.is_empty() {
        writeln!(out, "  events ({}):", item.events.len()).ok();
        for ev in &item.events {
            let actor = match &ev.actor {
                BotItemActor::Owner { device: Some(d) } => format!("owner:{d}"),
                BotItemActor::Owner { device: None } => "owner".to_owned(),
                BotItemActor::Bot { bot_id, .. } => format!("bot:{bot_id}"),
                BotItemActor::System { reason } => format!("system:{reason}"),
            };
            let kind = format!("{:?}", ev.kind).to_ascii_lowercase();
            if ev.text.is_empty() {
                writeln!(
                    out,
                    "    - [{}] {kind} by {actor} @{}",
                    ev.id, ev.created_at_ms
                )
                .ok();
            } else {
                writeln!(
                    out,
                    "    - [{}] {kind} by {actor} @{}: {}",
                    ev.id, ev.created_at_ms, ev.text
                )
                .ok();
            }
            for dec in &ev.decisions {
                writeln!(out, "      question: {}", dec.question).ok();
                for (i, opt) in dec.options.iter().enumerate() {
                    let rec = if dec.recommended == Some(i) {
                        " (recommended)"
                    } else {
                        ""
                    };
                    writeln!(out, "        [{i}] {opt}{rec}").ok();
                }
            }
        }
    }
    Ok(out.trim_end().to_owned())
}

fn format_link(link: &BotItemLink) -> String {
    match link {
        BotItemLink::Dispatch(v) => format!("dispatch:{v}"),
        BotItemLink::Pr(v) => format!("pr:{v}"),
        BotItemLink::Url(v) => format!("url:{v}"),
        BotItemLink::File(v) => format!("file:{v}"),
    }
}

pub(crate) fn format_look_around(
    bot_id: &BotId,
    suggestions: &[BotSuggestion],
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "bot_id": bot_id,
            "suggestions": suggestions,
        }));
    }
    if suggestions.is_empty() {
        return Ok(format!("Look around for {bot_id}: no suggestions."));
    }
    let mut out = String::new();
    writeln!(
        out,
        "Look around for {bot_id} ({} suggestion(s)):",
        suggestions.len()
    )
    .ok();
    for s in suggestions {
        writeln!(out, "  - {}: {} ({})", s.title, s.why, s.ask).ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_reply_posted(
    bot_id: &BotId,
    external_ref: Option<&str>,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_id": bot_id,
            "posted": true,
            "external_ref": external_ref,
        }))
    } else if let Some(ext) = external_ref {
        Ok(format!("posted reply for {bot_id} ({ext})"))
    } else {
        Ok(format!("posted reply for {bot_id}"))
    }
}

pub(crate) fn format_mail_sent(
    to_bot: &str,
    message_id: &str,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "to": format!("bot:{to_bot}"),
            "message_id": message_id,
        }))
    } else {
        Ok(format!("sent message {message_id} to bot:{to_bot}"))
    }
}

pub(crate) fn format_groups(groups: &[BotGroupChat], json: bool) -> Result<String, CliError> {
    if json {
        return to_json(&groups);
    }
    if groups.is_empty() {
        return Ok("No bot groups configured.".to_owned());
    }
    let mut out = String::new();
    writeln!(out, "{:<18} {:<20} MEMBERS", "ID", "NAME").ok();
    for g in groups {
        let members: Vec<_> = g.bot_ids.iter().map(BotId::as_str).collect();
        writeln!(out, "{:<18} {:<20} {}", g.id, g.name, members.join(",")).ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_group_detail(group: &BotGroupChat, json: bool) -> Result<String, CliError> {
    if json {
        return to_json(group);
    }
    let members: Vec<_> = group.bot_ids.iter().map(BotId::as_str).collect();
    Ok(format!(
        "group {} ({}): members [{}]",
        group.name,
        group.id,
        members.join(", ")
    ))
}

pub(crate) fn format_connector_statuses(
    statuses: &[BotConnectorStatus],
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&statuses);
    }
    if statuses.is_empty() {
        return Ok("No connector statuses available.".to_owned());
    }
    let mut out = String::new();
    writeln!(
        out,
        "{:<18} {:<10} {:<8} {:<12} DETAIL",
        "BOT", "SOURCE", "ENABLED", "STATE"
    )
    .ok();
    for s in statuses {
        let detail = s
            .last_error
            .as_deref()
            .or(s.account_label.as_deref())
            .unwrap_or("-");
        writeln!(
            out,
            "{:<18} {:<10} {:<8} {:<12} {}",
            s.bot_id,
            s.source.as_str(),
            if s.enabled { "yes" } else { "no" },
            format!("{:?}", s.state).to_ascii_lowercase(),
            detail
        )
        .ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_sync_triggered(
    bot_id: &BotId,
    sources: &[ExternalSource],
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_id": bot_id,
            "sources_synced": sources,
        }))
    } else {
        let names: Vec<_> = sources.iter().map(|s| s.as_str()).collect();
        Ok(format!(
            "synced {bot_id}: {}",
            if names.is_empty() {
                "no active sources".to_owned()
            } else {
                names.join(", ")
            }
        ))
    }
}

pub(crate) fn format_google_oauth_started(
    bot_id: &BotId,
    auth_url: &str,
    state: &str,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_id": bot_id,
            "auth_url": auth_url,
            "state": state,
        }))
    } else {
        Ok(format!(
            "Open this URL in your browser to authorize Google for {bot_id}:\n{auth_url}"
        ))
    }
}

pub(crate) fn format_access_requests(
    requests: &[BotAccessRequest],
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&requests);
    }
    if requests.is_empty() {
        return Ok("No access requests.".to_owned());
    }
    let mut out = String::new();
    writeln!(
        out,
        "{:<14} {:<16} {:<10} {:<10} SENDER",
        "TOKEN", "BOT", "SOURCE", "STATUS"
    )
    .ok();
    for r in requests {
        writeln!(
            out,
            "{:<14} {:<16} {:<10} {:<10} {} ({})",
            r.token,
            r.bot_id,
            r.source.as_str(),
            format!("{:?}", r.status).to_ascii_lowercase(),
            r.sender_name,
            r.sender_id
        )
        .ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_access_decided(
    req: &BotAccessRequest,
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(req);
    }
    Ok(format!(
        "access request {} for {} ({}) -> {:?}",
        req.token, req.bot_id, req.sender_name, req.status
    ))
}

/// Never prints or echoes secret values — reports only the opaque secret reference.
pub(crate) fn format_secret_changed(
    secret_ref: &str,
    stored: bool,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "secret_ref": secret_ref,
            "stored": stored,
        }))
    } else if stored {
        Ok(format!("stored secret {secret_ref}"))
    } else {
        Ok(format!("removed secret {secret_ref}"))
    }
}

pub(crate) fn format_tools(
    bot_id: &BotId,
    tools: &[BotMcpToolConfig],
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "bot_id": bot_id,
            "tools": tools,
        }));
    }
    if tools.is_empty() {
        return Ok(format!(
            "No MCP tools configured for {bot_id}. Run `krill bot apps --kind mcp` to browse curated apps."
        ));
    }
    let mut out = String::new();
    writeln!(
        out,
        "{:<16} {:<18} {:<8} {:<12} ENDPOINT",
        "ID", "NAME", "ENABLED", "AUTH"
    )
    .ok();
    for t in tools {
        let ep = t
            .endpoint_url
            .as_deref()
            .unwrap_or(if t.command.is_empty() { "-" } else { "<cmd>" });
        writeln!(
            out,
            "{:<16} {:<18} {:<8} {:<12} {}",
            t.id,
            t.name,
            if t.enabled { "yes" } else { "no" },
            format!("{:?}", t.auth_kind).to_ascii_lowercase(),
            ep
        )
        .ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_tool_relay_status(
    status: &BotToolRelayStatus,
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(status);
    }
    let mut out = String::new();
    writeln!(
        out,
        "relay      {}",
        if status.running { "running" } else { "stopped" }
    )
    .ok();
    if let Some(ref addr) = status.listen_addr {
        writeln!(out, "listen     {addr}").ok();
    }
    if let Some(ref url) = status.endpoint_url {
        writeln!(out, "endpoint   {url}").ok();
    }
    if let Some(ref err) = status.last_error {
        writeln!(out, "last_error {err}").ok();
    }
    if !status.connected_tools.is_empty() {
        writeln!(out, "tools ({}):", status.connected_tools.len()).ok();
        for t in &status.connected_tools {
            writeln!(
                out,
                "  - {} ({}): connected={} tools={}",
                t.id, t.endpoint_url, t.connected, t.tool_count
            )
            .ok();
        }
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_exported_zip(
    bot_id: &BotId,
    path: &str,
    filename: &str,
    bytes: usize,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_id": bot_id,
            "path": path,
            "filename": filename,
            "bytes": bytes,
        }))
    } else {
        Ok(format!("exported {bot_id} to {path} ({bytes} bytes)"))
    }
}

pub(crate) fn format_imported_external(
    candidates: &[ExternalBotCandidate],
    imported: &[BotConfig],
    dry_run: bool,
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "dry_run": dry_run,
            "candidates": candidates,
            "imported": imported,
        }));
    }
    let mode = if dry_run { "DRY-RUN" } else { "APPLIED" };
    let mut out = String::new();
    writeln!(
        out,
        "External bot import [{mode}]: {} candidate(s) found, {} imported",
        candidates.len(),
        imported.len()
    )
    .ok();
    for c in candidates {
        let app = match c.app {
            ExternalBotSourceApp::Orca => "orca",
            ExternalBotSourceApp::OpenClaw => "openclaw",
            ExternalBotSourceApp::Hermes => "hermes",
            ExternalBotSourceApp::OpenBot => "openbot",
        };
        writeln!(out, "  - [{app}] {} ({}) — {}", c.name, c.agent, c.path).ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_schedules(
    bot_id: &BotId,
    schedules: &[BotScheduleConfig],
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "bot_id": bot_id,
            "schedules": schedules,
        }));
    }
    if schedules.is_empty() {
        return Ok(format!("No schedules configured for {bot_id}."));
    }
    let mut out = String::new();
    writeln!(
        out,
        "{:<18} {:<20} {:<16} {:<8} TZ",
        "ID", "NAME", "CRON", "ENABLED"
    )
    .ok();
    for s in schedules {
        writeln!(
            out,
            "{:<18} {:<20} {:<16} {:<8} {}",
            s.id,
            s.name,
            s.cron,
            if s.enabled { "yes" } else { "no" },
            s.timezone.as_deref().unwrap_or("-")
        )
        .ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_memory_history(
    bot_id: &BotId,
    entries: &[BotMemoryHistoryEntry],
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "bot_id": bot_id,
            "history": entries,
        }));
    }
    if entries.is_empty() {
        return Ok(format!("No memory history for {bot_id}."));
    }
    let mut out = String::new();
    writeln!(
        out,
        "{:<16} {:<12} {:<12} {:<7} DIFF",
        "ENTRY", "ACTION", "ACTOR", "UNDO"
    )
    .ok();
    for e in entries {
        let undo_state = if e.undone {
            "undone"
        } else if e.undoable {
            "yes"
        } else {
            "no"
        };
        writeln!(
            out,
            "{:<16} {:<12} {:<12} {:<7} +{} -{}",
            e.entry_id,
            e.action,
            e.actor,
            undo_state,
            e.added.len(),
            e.removed.len()
        )
        .ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_memory_tidy_status(
    bot_id: &BotId,
    status: &BotMemoryTidyStatus,
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "bot_id": bot_id,
            "status": status,
        }));
    }
    Ok(format!(
        "memory tidy for {bot_id}: enabled={} interval={}s last_run_ms={} last_tidy_id={} failures={} auto_disabled={} undoable={}",
        status.enabled,
        status.interval_secs,
        status
            .last_run_ms
            .map_or_else(|| "-".to_owned(), |v| v.to_string()),
        status.last_tidy_id.as_deref().unwrap_or("-"),
        status.consecutive_failures,
        status.auto_disabled,
        status.undoable
    ))
}

pub(crate) fn format_memory_topics(
    bot_id: &BotId,
    topics: &[BotMemoryTopicInfo],
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "bot_id": bot_id,
            "topics": topics,
        }));
    }
    if topics.is_empty() {
        return Ok(format!("No memory topics for {bot_id}."));
    }
    let mut out = String::new();
    writeln!(out, "{:<24} {:<10} UPDATED_MS", "TOPIC", "BYTES").ok();
    for t in topics {
        writeln!(out, "{:<24} {:<10} {}", t.topic, t.bytes, t.updated_at_ms).ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_memory_topic_content(
    bot_id: &BotId,
    topic: &str,
    content: &str,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_id": bot_id,
            "topic": topic,
            "content": content,
        }))
    } else if content.trim().is_empty() {
        Ok(format!("Topic '{topic}' for {bot_id} is empty."))
    } else {
        Ok(content.trim_end().to_owned())
    }
}

pub(crate) fn format_suggestions(
    bot_id: Option<&BotId>,
    suggestions: &[BotSuggestion],
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "bot_id": bot_id,
            "suggestions": suggestions,
        }));
    }
    if suggestions.is_empty() {
        return Ok("No suggestions found.".to_owned());
    }
    let mut out = String::new();
    writeln!(out, "{:<16} {:<14} {:<10} TITLE", "ID", "BOT", "DISMISSED").ok();
    for s in suggestions {
        writeln!(
            out,
            "{:<16} {:<14} {:<10} {}",
            s.id,
            s.bot_id,
            if s.dismissed { "yes" } else { "no" },
            s.title
        )
        .ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_sessions(
    sessions: &[BotSessionSummary],
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&sessions);
    }
    if sessions.is_empty() {
        return Ok("No bot sessions found.".to_owned());
    }
    let mut out = String::new();
    writeln!(
        out,
        "{:<8} {:<16} {:<8} {:<6} {:<8} LABEL",
        "ID", "BOT", "PID", "LIVE", "WORKING"
    )
    .ok();
    for s in sessions {
        writeln!(
            out,
            "s{:<7} {:<16} {:<8} {:<6} {:<8} {}",
            s.session_id.0,
            s.bot_id,
            s.pid,
            if s.live { "yes" } else { "no" },
            if s.working { "yes" } else { "no" },
            if s.label.is_empty() { "-" } else { &s.label }
        )
        .ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_group_view(view: &BotGroupView, json: bool) -> Result<String, CliError> {
    if json {
        return to_json(view);
    }
    let mut out = String::new();
    let members: Vec<_> = view.group.bot_ids.iter().map(BotId::as_str).collect();
    writeln!(
        out,
        "group {} ({}) [active_run={}] members=[{}]",
        view.group.name,
        view.group.id,
        view.active_run,
        members.join(", ")
    )
    .ok();
    if !view.threads.is_empty() {
        writeln!(out, "threads ({}):", view.threads.len()).ok();
        for t in &view.threads {
            let title = if t.title.is_empty() {
                "(untitled)"
            } else {
                &t.title
            };
            writeln!(out, "  - {}: {title}", t.thread_id).ok();
        }
    }
    if !view.messages.is_empty() {
        writeln!(out, "messages ({}):", view.messages.len()).ok();
        for m in &view.messages {
            let suffix = if m.in_progress { " [streaming]" } else { "" };
            writeln!(
                out,
                "  [{}] {}: {}{suffix}",
                m.thread_id, m.sender_name, m.body
            )
            .ok();
        }
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_group_message_posted(
    message: &BotGroupMessage,
    dispatched_bots: &[BotId],
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "message": message,
            "dispatched_bots": dispatched_bots,
        }));
    }
    let dispatched: Vec<_> = dispatched_bots.iter().map(BotId::as_str).collect();
    Ok(format!(
        "posted group message {} in thread {} (dispatched: [{}])",
        message.message_id,
        message.thread_id,
        dispatched.join(", ")
    ))
}

pub(crate) fn format_group_cleared(
    room: &BotGroupRoomView,
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "cleared": true,
            "group": room,
        }));
    }
    let tid = room.active_thread_id.as_deref().unwrap_or("(none)");
    Ok(format!(
        "cleared group {} (new thread {})",
        room.group.id, tid
    ))
}

pub(crate) fn format_connector_test(
    bot_id: &BotId,
    res: &BotConnectorTestResult,
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "bot_id": bot_id,
            "result": res,
        }));
    }
    let label = res.account_label.as_deref().unwrap_or("-");
    let detail = if res.detail.is_empty() {
        "-"
    } else {
        &res.detail
    };
    Ok(format!(
        "connector test {bot_id} {}: ok={} account={label} detail={detail}",
        res.source.as_str(),
        res.ok
    ))
}

pub(crate) fn format_mcp_oauth(state: &BotMcpOAuthState, json: bool) -> Result<String, CliError> {
    if json {
        return to_json(state);
    }
    let mut out = format!(
        "mcp oauth {} for {}: status={}",
        state.tool_id, state.bot_id, state.status
    );
    if let Some(ref url) = state.auth_url {
        write!(out, "\nauth_url: {url}").ok();
    }
    if let Some(ref err) = state.error {
        write!(out, "\nerror: {err}").ok();
    }
    Ok(out)
}

pub(crate) fn format_composio_connections(
    bot_id: &BotId,
    connections: &[ComposioConnectionInfo],
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(&serde_json::json!({
            "bot_id": bot_id,
            "connections": connections,
        }));
    }
    if connections.is_empty() {
        return Ok(format!("No Composio connections for {bot_id}."));
    }
    let mut out = String::new();
    writeln!(out, "{:<20} {:<14} {:<12} ACCOUNT", "ID", "APP", "STATUS").ok();
    for c in connections {
        writeln!(
            out,
            "{:<20} {:<14} {:<12} {}",
            c.id,
            c.app_id,
            c.status,
            c.account_label.as_deref().unwrap_or("-")
        )
        .ok();
    }
    Ok(out.trim_end().to_owned())
}

pub(crate) fn format_composio_connect_started(
    bot_id: &BotId,
    connection_id: &str,
    redirect_url: &str,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_id": bot_id,
            "connection_id": connection_id,
            "redirect_url": redirect_url,
        }))
    } else {
        Ok(format!(
            "Composio connect started for {bot_id} ({connection_id}):\n{redirect_url}"
        ))
    }
}

pub(crate) fn format_tracker_defaults(
    defaults: &DefaultTrackerBots,
    json: bool,
) -> Result<String, CliError> {
    if json {
        return to_json(defaults);
    }
    Ok(format!(
        "github: {}\nlinear: {}",
        defaults
            .github
            .as_ref()
            .map_or("-".to_owned(), ToString::to_string),
        defaults
            .linear
            .as_ref()
            .map_or("-".to_owned(), ToString::to_string),
    ))
}

pub(crate) fn format_avatar_saved(
    bot_id: &BotId,
    path: &str,
    mime: &str,
    bytes: usize,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "bot_id": bot_id,
            "path": path,
            "mime": mime,
            "bytes": bytes,
        }))
    } else {
        Ok(format!(
            "saved avatar for {bot_id} to {path} ({bytes} bytes)"
        ))
    }
}

pub(crate) fn format_avatar_updated(
    bot_id: &BotId,
    mime: &str,
    bytes: usize,
    json: bool,
) -> Result<String, CliError> {
    if json {
        to_json(&serde_json::json!({
            "ok": true,
            "bot_id": bot_id,
            "mime": mime,
            "bytes": bytes,
        }))
    } else {
        Ok(format!(
            "updated avatar for {bot_id} ({mime}, {bytes} bytes)"
        ))
    }
}
