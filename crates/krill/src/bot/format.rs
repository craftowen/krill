// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Human-readable and `--json` output formatters for `krill bot`.

use std::fmt::Write as _;

use krill_proto::SessionId;
use krill_proto::bots::catalog::{BotMcpAppCatalogEntry, BotTemplate, ComposioAppCatalogEntry};
use krill_proto::bots::home::{count_memory_lines, parse_memory_facts};
use krill_proto::bots::{
    BotAccessRequest, BotConfig, BotConnectorStatus, BotGroupChat, BotId, BotItem, BotItemLink,
    BotMcpToolConfig, BotSuggestion, BotToolRelayStatus, ExternalBotCandidate,
    ExternalBotSourceApp, ExternalSource,
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
    writeln!(
        out,
        "state       {}",
        if bot.paused { "paused" } else { "active" }
    )
    .ok();
    writeln!(out, "folder      {}", bot.folder).ok();
    if let Some(ref ws) = bot.workspace {
        writeln!(out, "workspace   {ws}").ok();
    }
    if let Some(ref tpl) = bot.template_id {
        writeln!(out, "template    {tpl}").ok();
    }
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
        writeln!(
            out,
            "#{:<7} {:<16} {:<10} {}{snooze}",
            item.number,
            item.owner_bot_id,
            item.status.as_str(),
            item.title
        )
        .ok();
    }
    Ok(out.trim_end().to_owned())
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
    if let Some(until) = item.snoozed_until_ms {
        writeln!(out, "  snoozed_ms:  {until}").ok();
    }
    if let Some(ref note) = item.close_note {
        writeln!(out, "  close_note:  {note}").ok();
    }
    for link in &item.links {
        writeln!(out, "  link:        {}", format_link(link)).ok();
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
    writeln!(out, "{:<16} {:<18} {:<12} ENDPOINT", "ID", "NAME", "AUTH").ok();
    for t in tools {
        let ep = t
            .endpoint_url
            .as_deref()
            .unwrap_or(if t.command.is_empty() { "-" } else { "<cmd>" });
        writeln!(
            out,
            "{:<16} {:<18} {:<12} {}",
            t.id,
            t.name,
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
