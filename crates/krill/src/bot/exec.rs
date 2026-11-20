// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Command execution for `krill bot` (offline catalog + daemon RPC dispatch).

use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use krill_proto::bots::catalog::{
    BOT_MCP_APPS, BUILTIN_BOT_SKILLS, BUILTIN_BOT_TEMPLATES, COMPOSIO_CURATED_APPS,
    bot_templates_by_category, find_bot_mcp_app, find_bot_skill, find_bot_template,
    generate_slack_socket_mode_manifest,
};
use krill_proto::bots::home::append_memory_fact;
use krill_proto::bots::package::{
    BotPackageExportOptions, SanitizedBotManifest, import_bot_zip, read_store_zip, write_store_zip,
};
use krill_proto::bots::{
    BotConfig, BotId, BotItemLink, BotItemStatus, BotMcpAuthKind, BotMcpToolConfig, BotRpcRequest,
    BotRpcResponse, BotSlug, CreateBotItemParams, CreateBotParams, FieldPatch,
    GoogleServicesConfig, MAX_RPC_ZIP_BYTES, UpdateBotItemParams, UpdateBotParams,
    validate_bot_slug,
};

use super::format;
use super::parse::{
    AppsKind, BotCommand, ConnectorPatch, ConnectorPatchArgs, GoogleServiceKind, ItemsAddArgs,
    ItemsUpdateArgs, NewBotArgs, SecretInputSource, TextInputSource, ToolsAddArgs, UpdateBotArgs,
};
use super::{BotClient, CliError};

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Executes purely offline commands directly from `krill_proto::bots::catalog`.
/// Returns `None` if the command requires talking to `krilld`.
pub(crate) fn try_execute_offline(cmd: &BotCommand) -> Option<Result<String, CliError>> {
    match cmd {
        BotCommand::Templates {
            category,
            show_id,
            json,
        } => {
            if let Some(id) = show_id {
                let Some(tpl) = find_bot_template(id) else {
                    return Some(Err(CliError::fail(format!("unknown template '{id}'"))));
                };
                Some(format::format_template_detail(tpl, *json))
            } else {
                let list = match category {
                    Some(cat) => bot_templates_by_category(*cat),
                    None => BUILTIN_BOT_TEMPLATES.iter().collect(),
                };
                Some(format::format_templates(&list, *json))
            }
        }
        BotCommand::Skills { show_name, json } => {
            if let Some(name) = show_name {
                let Some(skill) = find_bot_skill(name) else {
                    return Some(Err(CliError::fail(format!("unknown bot skill '{name}'"))));
                };
                Some(format::format_skill_detail(skill, *json))
            } else {
                Some(format::format_skills(BUILTIN_BOT_SKILLS, *json))
            }
        }
        BotCommand::Apps {
            kind,
            category,
            json,
        } => {
            let mcp: Vec<_> = match kind {
                AppsKind::Composio => Vec::new(),
                AppsKind::Mcp | AppsKind::All => BOT_MCP_APPS
                    .iter()
                    .filter(|app| category.is_none_or(|c| app.category == c))
                    .collect(),
            };
            let composio: Vec<_> = match kind {
                AppsKind::Mcp => Vec::new(),
                AppsKind::Composio | AppsKind::All => COMPOSIO_CURATED_APPS.iter().collect(),
            };
            Some(format::format_apps(&mcp, &composio, *json))
        }
        BotCommand::SlackManifest { bot_name, json } => {
            let yaml = generate_slack_socket_mode_manifest(bot_name);
            Some(format::format_slack_manifest(bot_name, &yaml, *json))
        }
        _ => None,
    }
}

pub(crate) fn execute_online<R, F>(
    cmd: BotCommand,
    client: &mut dyn BotClient,
    stdin: &mut R,
    now_ms: u64,
    get_env: F,
) -> Result<String, CliError>
where
    R: Read,
    F: Fn(&str) -> Option<String>,
{
    match cmd {
        BotCommand::Help(_)
        | BotCommand::Templates { .. }
        | BotCommand::Skills { .. }
        | BotCommand::Apps { .. }
        | BotCommand::SlackManifest { .. } => {
            Err(CliError::fail("internal: offline command routed online"))
        }
        BotCommand::List { json } => {
            let resp = client.call(BotRpcRequest::ListBots)?;
            let BotRpcResponse::Bots(bots) = resp else {
                return Err(CliError::fail("unexpected reply for ListBots"));
            };
            format::format_bots(&bots, json)
        }
        BotCommand::New(args) => exec_new(args, client, stdin),
        BotCommand::Show { bot, json } => {
            let cfg = resolve_bot(client, &bot)?;
            format::format_bot_detail(&cfg, json)
        }
        BotCommand::Update(args) => exec_update(args, client),
        BotCommand::Remove {
            bot,
            delete_home,
            json,
        } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let resp = client.call(BotRpcRequest::DeleteBot {
                bot_id,
                delete_home,
            })?;
            let BotRpcResponse::Deleted { bot_id } = resp else {
                return Err(CliError::fail("unexpected reply for DeleteBot"));
            };
            format::format_deleted(&bot_id, delete_home, json)
        }
        BotCommand::Start {
            bot,
            prompt,
            cwd,
            json,
        } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let resp = client.call(BotRpcRequest::StartBotSession {
                bot_id,
                cwd,
                prompt,
            })?;
            let BotRpcResponse::SessionStarted {
                bot_id,
                session_id,
                pid,
            } = resp
            else {
                return Err(CliError::fail("unexpected reply for StartBotSession"));
            };
            format::format_session_started(&bot_id, session_id, pid, json)
        }
        BotCommand::SoulGet { bot, json } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let resp = client.call(BotRpcRequest::GetSoul {
                bot_id: bot_id.clone(),
            })?;
            let BotRpcResponse::Text(content) = resp else {
                return Err(CliError::fail("unexpected reply for GetSoul"));
            };
            format::format_soul(&bot_id, &content, json)
        }
        BotCommand::SoulSet { bot, source, json } => {
            let content = read_text_source(&source, stdin)?;
            let bot_id = resolve_bot_id(client, &bot)?;
            let resp = client.call(BotRpcRequest::SetSoul {
                bot_id: bot_id.clone(),
                content,
            })?;
            match resp {
                BotRpcResponse::Ok | BotRpcResponse::Text(_) => {
                    format::format_soul_saved(&bot_id, json)
                }
                _ => Err(CliError::fail("unexpected reply for SetSoul")),
            }
        }
        BotCommand::MemoryList { bot, json } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let resp = client.call(BotRpcRequest::GetMemory {
                bot_id: bot_id.clone(),
            })?;
            let BotRpcResponse::Text(content) = resp else {
                return Err(CliError::fail("unexpected reply for GetMemory"));
            };
            format::format_memory(&bot_id, &content, json)
        }
        BotCommand::MemoryAdd {
            bot,
            fact,
            date,
            source,
            json,
        } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let BotRpcResponse::Text(current) = client.call(BotRpcRequest::GetMemory {
                bot_id: bot_id.clone(),
            })?
            else {
                return Err(CliError::fail("unexpected reply for GetMemory"));
            };
            let date_str = date.unwrap_or_else(|| today_iso_utc(now_ms / 1000));
            let updated = append_memory_fact(&current, &fact, Some(&date_str), source.as_deref());
            let resp = client.call(BotRpcRequest::UpdateMemory {
                bot_id: bot_id.clone(),
                content: updated,
                allow_deletions: false,
            })?;
            let BotRpcResponse::MemoryUpdated {
                content,
                restored_facts,
                removed_count,
            } = resp
            else {
                return Err(CliError::fail("unexpected reply for UpdateMemory"));
            };
            format::format_memory_updated(&bot_id, &content, &restored_facts, removed_count, json)
        }
        BotCommand::MemoryForget { bot, fact, json } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let resp = client.call(BotRpcRequest::ForgetMemoryFact {
                bot_id: bot_id.clone(),
                fact,
            })?;
            let BotRpcResponse::MemoryUpdated {
                content,
                restored_facts,
                removed_count,
            } = resp
            else {
                return Err(CliError::fail("unexpected reply for ForgetMemoryFact"));
            };
            format::format_memory_updated(&bot_id, &content, &restored_facts, removed_count, json)
        }
        BotCommand::MemoryTidy { bot, json } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let resp = client.call(BotRpcRequest::RunMemoryTidy { bot_id })?;
            let BotRpcResponse::MemoryTidyResult {
                bot_id,
                summary,
                changes,
            } = resp
            else {
                return Err(CliError::fail("unexpected reply for RunMemoryTidy"));
            };
            format::format_memory_tidy(&bot_id, &summary, changes, json)
        }
        BotCommand::MemorySet {
            bot,
            source,
            allow_deletions,
            json,
        } => {
            let content = read_text_source(&source, stdin)?;
            let bot_id = resolve_bot_id(client, &bot)?;
            let resp = client.call(BotRpcRequest::UpdateMemory {
                bot_id: bot_id.clone(),
                content,
                allow_deletions,
            })?;
            let BotRpcResponse::MemoryUpdated {
                content,
                restored_facts,
                removed_count,
            } = resp
            else {
                return Err(CliError::fail("unexpected reply for UpdateMemory"));
            };
            format::format_memory_updated(&bot_id, &content, &restored_facts, removed_count, json)
        }
        BotCommand::ItemsList {
            bot,
            status,
            all,
            json,
        } => {
            let bot_id = match bot {
                Some(ref b) => Some(resolve_bot_id(client, b)?),
                None => None,
            };
            let include_closed =
                all || matches!(status, Some(BotItemStatus::Done | BotItemStatus::Canceled));
            let resp = client.call(BotRpcRequest::ListItems {
                bot_id,
                include_closed,
            })?;
            let BotRpcResponse::Items(mut items) = resp else {
                return Err(CliError::fail("unexpected reply for ListItems"));
            };
            if let Some(filter) = status {
                items.retain(|i| i.status == filter);
            }
            format::format_items(&items, json)
        }
        BotCommand::ItemsAdd(args) => exec_items_add(args, client),
        BotCommand::ItemsUpdate(args) => exec_items_update(args, client, now_ms),
        BotCommand::ItemsComment {
            bot,
            item_id,
            author,
            text,
            json,
        } => {
            let author = author.or(bot).unwrap_or_else(|| "owner".to_owned());
            let resp = client.call(BotRpcRequest::AddItemComment {
                item_id,
                author,
                text,
            })?;
            let BotRpcResponse::Item(item) = resp else {
                return Err(CliError::fail("unexpected reply for AddItemComment"));
            };
            format::format_item_detail(&item, json)
        }
        BotCommand::ItemsSnooze {
            bot: _,
            item_id,
            until,
            json,
        } => {
            let patch = parse_snooze_until(&until, now_ms)?;
            let resp = client.call(BotRpcRequest::UpdateItem(Box::new(UpdateBotItemParams {
                item_id,
                title: None,
                description: None,
                done_means: None,
                status: None,
                comment: None,
                decisions: Vec::new(),
                close_note: None,
                snoozed_until_ms: patch,
            })))?;
            let BotRpcResponse::Item(item) = resp else {
                return Err(CliError::fail("unexpected reply for UpdateItem"));
            };
            format::format_item_detail(&item, json)
        }
        BotCommand::ItemsUnsnooze {
            bot: _,
            item_id,
            json,
        } => {
            let resp = client.call(BotRpcRequest::UpdateItem(Box::new(UpdateBotItemParams {
                item_id,
                title: None,
                description: None,
                done_means: None,
                status: None,
                comment: None,
                decisions: Vec::new(),
                close_note: None,
                snoozed_until_ms: FieldPatch::Clear,
            })))?;
            let BotRpcResponse::Item(item) = resp else {
                return Err(CliError::fail("unexpected reply for UpdateItem"));
            };
            format::format_item_detail(&item, json)
        }
        BotCommand::ItemsLink {
            bot: _,
            item_id,
            link,
            label: _,
            json,
        } => {
            let resp = client.call(BotRpcRequest::AddItemLink { item_id, link })?;
            let BotRpcResponse::Item(item) = resp else {
                return Err(CliError::fail("unexpected reply for AddItemLink"));
            };
            format::format_item_detail(&item, json)
        }
        BotCommand::ItemsClose {
            bot: _,
            item_id,
            status,
            note,
            json,
        } => {
            let resp = client.call(BotRpcRequest::UpdateItem(Box::new(UpdateBotItemParams {
                item_id,
                title: None,
                description: None,
                done_means: None,
                status: Some(status),
                comment: None,
                decisions: Vec::new(),
                close_note: note,
                snoozed_until_ms: FieldPatch::Unchanged,
            })))?;
            let BotRpcResponse::Item(item) = resp else {
                return Err(CliError::fail("unexpected reply for UpdateItem"));
            };
            format::format_item_detail(&item, json)
        }
        BotCommand::SuggestRun { bot, json } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let resp = client.call(BotRpcRequest::RunLookAround { bot_id })?;
            let BotRpcResponse::LookAroundResult {
                bot_id,
                suggestions,
            } = resp
            else {
                return Err(CliError::fail("unexpected reply for RunLookAround"));
            };
            format::format_look_around(&bot_id, &suggestions, json)
        }
        BotCommand::SuggestPropose {
            bot,
            title,
            why,
            ask,
            link,
            json,
        } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let mut desc_parts = Vec::new();
            if !why.trim().is_empty() {
                desc_parts.push(format!("Why: {}", why.trim()));
            }
            if !ask.trim().is_empty() {
                desc_parts.push(format!("Ask: {}", ask.trim()));
            }
            let links = link.into_iter().map(BotItemLink::Url).collect();
            let resp = client.call(BotRpcRequest::CreateItem(Box::new(CreateBotItemParams {
                bot_id,
                title,
                description: (!desc_parts.is_empty()).then(|| desc_parts.join("\n")),
                done_means: None,
                status: Some(BotItemStatus::Planned),
                links,
            })))?;
            let BotRpcResponse::Item(item) = resp else {
                return Err(CliError::fail("unexpected reply for CreateItem"));
            };
            format::format_item_detail(&item, json)
        }
        BotCommand::Reply {
            bot,
            text,
            link_id,
            files,
            reaction,
            json,
        } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let resp = client.call(BotRpcRequest::PostBotReply {
                bot_id: bot_id.clone(),
                link_id,
                text,
                files,
                reaction,
            })?;
            let BotRpcResponse::ReplyPosted { external_ref } = resp else {
                return Err(CliError::fail("unexpected reply for PostBotReply"));
            };
            format::format_reply_posted(&bot_id, external_ref.as_deref(), json)
        }
        BotCommand::SendMail {
            from,
            to_bot,
            kind,
            subject,
            body,
            thread_id,
            json,
        } => {
            let effective_subject = match (kind, subject) {
                (Some(k), Some(s)) if !s.trim().is_empty() => format!("[{}] {}", k.as_str(), s),
                (Some(k), _) => format!("[{}]", k.as_str()),
                (None, Some(s)) => s,
                (None, None) => String::new(),
            };
            let resp = client.call(BotRpcRequest::SendBotMail {
                from: from.unwrap_or_else(|| "owner".to_owned()),
                to_bot: to_bot.clone(),
                subject: effective_subject,
                body,
                thread_id,
            })?;
            let BotRpcResponse::MailSent { message_id } = resp else {
                return Err(CliError::fail("unexpected reply for SendBotMail"));
            };
            format::format_mail_sent(&to_bot, &message_id, json)
        }
        BotCommand::GroupsList { json } => {
            let resp = client.call(BotRpcRequest::ListGroups)?;
            let BotRpcResponse::Groups(groups) = resp else {
                return Err(CliError::fail("unexpected reply for ListGroups"));
            };
            format::format_groups(&groups, json)
        }
        BotCommand::GroupsCreate {
            name,
            members,
            description,
            json,
        } => {
            let mut bot_ids = Vec::with_capacity(members.len());
            for m in &members {
                bot_ids.push(resolve_bot_id(client, m)?);
            }
            let resp = client.call(BotRpcRequest::CreateGroup {
                name,
                bot_ids,
                description,
            })?;
            let BotRpcResponse::Group(group) = resp else {
                return Err(CliError::fail("unexpected reply for CreateGroup"));
            };
            format::format_group_detail(&group, json)
        }
        BotCommand::ConnectorStatus { bot, json } => {
            let bot_id = match bot {
                Some(ref b) => Some(resolve_bot_id(client, b)?),
                None => None,
            };
            let resp = client.call(BotRpcRequest::ConnectorStatus { bot_id })?;
            let BotRpcResponse::ConnectorStatuses(statuses) = resp else {
                return Err(CliError::fail("unexpected reply for ConnectorStatus"));
            };
            format::format_connector_statuses(&statuses, json)
        }
        BotCommand::ConnectorReload { bot, json } => {
            let bot_id = match bot {
                Some(ref b) => Some(resolve_bot_id(client, b)?),
                None => None,
            };
            let resp = client.call(BotRpcRequest::ReloadConnectors { bot_id })?;
            match resp {
                BotRpcResponse::ConnectorStatuses(statuses) => {
                    format::format_connector_statuses(&statuses, json)
                }
                BotRpcResponse::Ok => format::format_ok("connectors reloaded", json),
                _ => Err(CliError::fail("unexpected reply for ReloadConnectors")),
            }
        }
        BotCommand::ConnectorSync { bot, source, json } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let resp = client.call(BotRpcRequest::TriggerSyncNow { bot_id, source })?;
            let BotRpcResponse::SyncTriggered {
                bot_id,
                sources_synced,
            } = resp
            else {
                return Err(CliError::fail("unexpected reply for TriggerSyncNow"));
            };
            format::format_sync_triggered(&bot_id, &sources_synced, json)
        }
        BotCommand::ConnectorConfigure(args) => exec_connector_configure(args, client),
        BotCommand::AccessList { bot, json } => {
            let bot_id = match bot {
                Some(ref b) => Some(resolve_bot_id(client, b)?),
                None => None,
            };
            let resp = client.call(BotRpcRequest::ListAccessRequests { bot_id })?;
            let BotRpcResponse::AccessRequests(requests) = resp else {
                return Err(CliError::fail("unexpected reply for ListAccessRequests"));
            };
            format::format_access_requests(&requests, json)
        }
        BotCommand::AccessDecide {
            token,
            approve,
            json,
        } => {
            let resp = client.call(BotRpcRequest::DecideAccessRequest { token, approve })?;
            let BotRpcResponse::AccessRequestDecided(req) = resp else {
                return Err(CliError::fail("unexpected reply for DecideAccessRequest"));
            };
            format::format_access_decided(&req, json)
        }
        BotCommand::SecretSet {
            bot,
            key,
            source,
            json,
        } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let secret_ref = build_secret_ref(&bot_id, &key);
            let value = match source {
                SecretInputSource::Stdin => {
                    let mut buf = String::new();
                    stdin
                        .read_to_string(&mut buf)
                        .map_err(|e| CliError::fail(format!("read secret from stdin: {e}")))?;
                    let trimmed = buf
                        .strip_suffix("\r\n")
                        .or_else(|| buf.strip_suffix('\n'))
                        .unwrap_or(&buf);
                    if trimmed.is_empty() {
                        return Err(CliError::usage("secret value from stdin must not be empty"));
                    }
                    trimmed.to_owned()
                }
                SecretInputSource::Env(var) => {
                    let val = get_env(&var).ok_or_else(|| {
                        CliError::usage(format!("environment variable '{var}' is not set"))
                    })?;
                    if val.is_empty() {
                        return Err(CliError::usage(format!(
                            "environment variable '{var}' is empty"
                        )));
                    }
                    val
                }
            };
            let resp = client.call(BotRpcRequest::SetSecret {
                secret_ref: secret_ref.clone(),
                value,
            })?;
            match resp {
                BotRpcResponse::Ok => format::format_secret_changed(&secret_ref, true, json),
                _ => Err(CliError::fail("unexpected reply for SetSecret")),
            }
        }
        BotCommand::SecretRemove { bot, key, json } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let secret_ref = build_secret_ref(&bot_id, &key);
            let resp = client.call(BotRpcRequest::DeleteSecret {
                secret_ref: secret_ref.clone(),
            })?;
            match resp {
                BotRpcResponse::Ok => format::format_secret_changed(&secret_ref, false, json),
                _ => Err(CliError::fail("unexpected reply for DeleteSecret")),
            }
        }
        BotCommand::ToolsList { bot, json } => {
            let cfg = resolve_bot(client, &bot)?;
            format::format_tools(&cfg.id, &cfg.mcp_tools, json)
        }
        BotCommand::ToolsAdd(args) => exec_tools_add(args, client),
        BotCommand::ToolsRemove { bot, app_id, json } => {
            let cfg = resolve_bot(client, &bot)?;
            let mut tools = cfg.mcp_tools.clone();
            let before = tools.len();
            tools.retain(|t| t.id != app_id);
            if tools.len() == before {
                return Err(CliError::fail(format!(
                    "tool '{app_id}' is not configured on bot '{}'",
                    cfg.slug
                )));
            }
            let resp = client.call(BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
                bot_id: cfg.id.clone(),
                name: None,
                description: None,
                emoji: FieldPatch::Unchanged,
                avatar: FieldPatch::Unchanged,
                agent_id: None,
                model: FieldPatch::Unchanged,
                effort: FieldPatch::Unchanged,
                permission_mode: None,
                workspace: FieldPatch::Unchanged,
                paused: None,
                first_run_completed: None,
                notifications: None,
                schedules: None,
                look_around: None,
                memory_tidy: None,
                mcp_tools: Some(tools),
            })))?;
            let BotRpcResponse::Bot(updated) = resp else {
                return Err(CliError::fail("unexpected reply for UpdateBot"));
            };
            format::format_tools(&updated.id, &updated.mcp_tools, json)
        }
        BotCommand::ToolsRelayStatus { json } => {
            let resp = client.call(BotRpcRequest::ToolRelayStatus)?;
            let BotRpcResponse::ToolRelayStatus(status) = resp else {
                return Err(CliError::fail("unexpected reply for ToolRelayStatus"));
            };
            format::format_tool_relay_status(&status, json)
        }
        BotCommand::ExportZip {
            bot,
            out_path,
            include_memory,
            include_avatar,
            json,
        } => {
            let bot_id = resolve_bot_id(client, &bot)?;
            let resp = client.call(BotRpcRequest::ExportBotZip {
                bot_id: bot_id.clone(),
                options: BotPackageExportOptions {
                    include_memory,
                    include_avatar,
                },
            })?;
            let BotRpcResponse::ExportedZip {
                filename,
                zip_bytes,
            } = resp
            else {
                return Err(CliError::fail("unexpected reply for ExportBotZip"));
            };
            let target_path = {
                let p = Path::new(&out_path);
                if p.is_dir() {
                    p.join(&filename)
                } else {
                    p.to_path_buf()
                }
            };
            if let Some(parent) = target_path.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target_path, &zip_bytes)?;
            format::format_exported_zip(
                &bot_id,
                &target_path.display().to_string(),
                &filename,
                zip_bytes.len(),
                json,
            )
        }
        BotCommand::ImportZip {
            in_path,
            slug,
            name,
            agent,
            json,
        } => {
            let raw_bytes =
                fs::read(&in_path).map_err(|e| CliError::fail(format!("read '{in_path}': {e}")))?;
            if raw_bytes.len() > MAX_RPC_ZIP_BYTES {
                return Err(CliError::fail(format!(
                    "package exceeds maximum RPC size ({} > {MAX_RPC_ZIP_BYTES} bytes)",
                    raw_bytes.len()
                )));
            }
            // Validate archive integrity, CRC32, and path allow-list locally before sending.
            import_bot_zip(&raw_bytes)
                .map_err(|e| CliError::fail(format!("invalid bot package: {e}")))?;
            let zip_bytes = if let Some(ref custom_slug) = slug {
                rewrite_zip_slug(&raw_bytes, custom_slug)?
            } else {
                raw_bytes
            };
            let name_override = name.or(slug);
            let resp = client.call(BotRpcRequest::ImportBotZip {
                zip_bytes,
                name_override,
                agent_override: agent,
            })?;
            let BotRpcResponse::Bot(cfg) = resp else {
                return Err(CliError::fail("unexpected reply for ImportBotZip"));
            };
            format::format_bot_detail(&cfg, json)
        }
        BotCommand::ImportExternal {
            source,
            path,
            apply,
            json,
        } => {
            let resp = client.call(BotRpcRequest::ImportExternalBots {
                source,
                path,
                dry_run: !apply,
            })?;
            let BotRpcResponse::ImportedExternalBots {
                candidates,
                imported,
            } = resp
            else {
                return Err(CliError::fail("unexpected reply for ImportExternalBots"));
            };
            format::format_imported_external(&candidates, &imported, !apply, json)
        }
    }
}

fn rewrite_zip_slug(archive: &[u8], new_slug: &str) -> Result<Vec<u8>, CliError> {
    let valid = BotSlug::new(new_slug).map_err(|e| CliError::usage(e.to_string()))?;
    let mut entries =
        read_store_zip(archive).map_err(|e| CliError::fail(format!("read zip: {e}")))?;
    let manifest_bytes = entries
        .get("bot.json")
        .ok_or_else(|| CliError::fail("bot package missing bot.json"))?;
    let mut manifest: SanitizedBotManifest = serde_json::from_slice(manifest_bytes)
        .map_err(|e| CliError::fail(format!("parse bot.json: {e}")))?;
    manifest.slug = valid;
    let mut rendered = serde_json::to_string_pretty(&manifest)
        .map_err(|e| CliError::fail(format!("encode bot.json: {e}")))?;
    rendered.push('\n');
    entries.insert("bot.json".to_owned(), rendered.into_bytes());
    let updated =
        write_store_zip(&entries).map_err(|e| CliError::fail(format!("write zip: {e}")))?;
    if updated.len() > MAX_RPC_ZIP_BYTES {
        return Err(CliError::fail(format!(
            "package exceeds maximum RPC size ({} > {MAX_RPC_ZIP_BYTES} bytes)",
            updated.len()
        )));
    }
    Ok(updated)
}

fn exec_new<R: Read>(
    args: NewBotArgs,
    client: &mut dyn BotClient,
    stdin: &mut R,
) -> Result<String, CliError> {
    let template = match args.template_id.as_deref() {
        Some(id) => {
            let tpl = find_bot_template(id)
                .ok_or_else(|| CliError::usage(format!("unknown template '{id}'")))?;
            Some(tpl)
        }
        None => None,
    };
    let name = args
        .name
        .or_else(|| template.map(|t| t.name.to_owned()))
        .ok_or_else(|| CliError::usage("new requires <name> or --template <id>"))?;
    if let Some(ref slug) = args.slug {
        validate_bot_slug(slug).map_err(|e| CliError::usage(e.to_string()))?;
    }
    let initial_soul = match args.soul {
        Some(src) => Some(read_text_source(&src, stdin)?),
        None => None,
    };
    let description = args
        .description
        .or_else(|| template.map(|t| t.description.to_owned()))
        .unwrap_or_default();

    let resp = client.call(BotRpcRequest::CreateBot(Box::new(CreateBotParams {
        name,
        agent_id: args.agent_id,
        description,
        emoji: args.emoji,
        avatar: args.avatar,
        model: args.model,
        effort: args.effort,
        permission_mode: args.permission_mode,
        workspace: args.folder,
        template_id: args.template_id,
        initial_soul,
    })))?;
    let BotRpcResponse::Bot(cfg) = resp else {
        return Err(CliError::fail("unexpected reply for CreateBot"));
    };
    format::format_bot_detail(&cfg, args.json)
}

fn exec_update(args: UpdateBotArgs, client: &mut dyn BotClient) -> Result<String, CliError> {
    let needs_existing = args.look_around_enabled.is_some()
        || args.look_around_interval_secs.is_some()
        || args.look_around_max_suggestions.is_some()
        || args.look_around_weekdays_only.is_some()
        || args.look_around_focus.is_some()
        || args.memory_tidy_enabled.is_some()
        || args.memory_tidy_interval_secs.is_some()
        || args.notify_permission.is_some()
        || args.notify_complete.is_some()
        || args.notify_error.is_some();

    let (bot_id, look_around, memory_tidy, notifications) = if needs_existing {
        let cfg = resolve_bot(client, &args.bot)?;
        let mut la = cfg.look_around.clone();
        let mut la_changed = false;
        if let Some(v) = args.look_around_enabled {
            la.enabled = v;
            la_changed = true;
        }
        if let Some(v) = args.look_around_interval_secs {
            la.interval_secs = v;
            la_changed = true;
        }
        if let Some(v) = args.look_around_max_suggestions {
            la.max_suggestions = v;
            la_changed = true;
        }
        if let Some(v) = args.look_around_weekdays_only {
            la.weekdays_only = v;
            la_changed = true;
        }
        if let Some(v) = args.look_around_focus {
            la.focus = v;
            la_changed = true;
        }

        let mut mt = cfg.memory_tidy;
        let mut mt_changed = false;
        if let Some(v) = args.memory_tidy_enabled {
            mt.enabled = v;
            mt_changed = true;
        }
        if let Some(v) = args.memory_tidy_interval_secs {
            mt.interval_secs = v;
            mt_changed = true;
        }

        let mut notif = cfg.notifications;
        let mut notif_changed = false;
        if let Some(v) = args.notify_permission {
            notif.needs_permission = v;
            notif_changed = true;
        }
        if let Some(v) = args.notify_complete {
            notif.task_complete = v;
            notif_changed = true;
        }
        if let Some(v) = args.notify_error {
            notif.could_not_finish = v;
            notif_changed = true;
        }
        (
            cfg.id,
            la_changed.then_some(la),
            mt_changed.then_some(mt),
            notif_changed.then_some(notif),
        )
    } else {
        (resolve_bot_id(client, &args.bot)?, None, None, None)
    };

    let resp = client.call(BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
        bot_id,
        name: args.name,
        description: args.description,
        emoji: args.emoji,
        avatar: args.avatar,
        agent_id: args.agent_id,
        model: args.model,
        effort: args.effort,
        permission_mode: args.permission_mode,
        workspace: args.workspace,
        paused: args.paused,
        first_run_completed: None,
        notifications,
        schedules: None,
        look_around,
        memory_tidy,
        mcp_tools: None,
    })))?;
    let BotRpcResponse::Bot(updated) = resp else {
        return Err(CliError::fail("unexpected reply for UpdateBot"));
    };
    format::format_bot_detail(&updated, args.json)
}

fn exec_items_add(args: ItemsAddArgs, client: &mut dyn BotClient) -> Result<String, CliError> {
    let bot_id = resolve_bot_id(client, &args.bot)?;
    let resp = client.call(BotRpcRequest::CreateItem(Box::new(CreateBotItemParams {
        bot_id,
        title: args.title,
        description: args.body,
        done_means: args.done_means,
        status: args.status,
        links: args.links,
    })))?;
    let BotRpcResponse::Item(item) = resp else {
        return Err(CliError::fail("unexpected reply for CreateItem"));
    };
    format::format_item_detail(&item, args.json)
}

fn exec_items_update(
    args: ItemsUpdateArgs,
    client: &mut dyn BotClient,
    now_ms: u64,
) -> Result<String, CliError> {
    let snoozed_until_ms = if args.unsnooze {
        FieldPatch::Clear
    } else if let Some(ref until) = args.snooze {
        parse_snooze_until(until, now_ms)?
    } else {
        FieldPatch::Unchanged
    };
    let resp = client.call(BotRpcRequest::UpdateItem(Box::new(UpdateBotItemParams {
        item_id: args.item_id,
        title: args.title,
        description: args.body,
        done_means: args.done_means,
        status: args.status,
        comment: args.comment,
        decisions: Vec::new(),
        close_note: args.close_note,
        snoozed_until_ms,
    })))?;
    let BotRpcResponse::Item(item) = resp else {
        return Err(CliError::fail("unexpected reply for UpdateItem"));
    };
    format::format_item_detail(&item, args.json)
}

fn exec_connector_configure(
    args: ConnectorPatchArgs,
    client: &mut dyn BotClient,
) -> Result<String, CliError> {
    let cfg = resolve_bot(client, &args.bot)?;
    let mut connectors = cfg.connectors.clone();
    let mut start_google_oauth = false;
    let mut config_changed = true;

    match args.patch {
        ConnectorPatch::Telegram {
            enabled,
            secret_ref,
            allowed_chats,
            allowed_users,
            acts_for,
        } => {
            if let Some(v) = enabled {
                connectors.telegram.enabled = v;
                if v && connectors.telegram.secret_ref.is_none() {
                    connectors.telegram.secret_ref = Some(build_secret_ref(&cfg.id, "telegram"));
                }
            }
            if let Some(r) = secret_ref {
                connectors.telegram.secret_ref = Some(build_secret_ref(&cfg.id, &r));
            }
            if let Some(chats) = allowed_chats {
                connectors.telegram.allowed_chat_ids = chats;
            }
            if let Some(users) = allowed_users {
                connectors.telegram.allowed_user_ids = users;
            }
            if let Some(a) = acts_for {
                connectors.telegram.acts_for = a;
            }
        }
        ConnectorPatch::Slack {
            enabled,
            app_token_ref,
            bot_token_ref,
            allowed_channels,
            allowed_users,
            mirror_desktop,
            acts_for,
        } => {
            if let Some(v) = enabled {
                connectors.slack.enabled = v;
                if v {
                    if connectors.slack.app_token_secret_ref.is_none() {
                        connectors.slack.app_token_secret_ref =
                            Some(build_secret_ref(&cfg.id, "slack-app"));
                    }
                    if connectors.slack.bot_token_secret_ref.is_none() {
                        connectors.slack.bot_token_secret_ref =
                            Some(build_secret_ref(&cfg.id, "slack-bot"));
                    }
                }
            }
            if let Some(r) = app_token_ref {
                connectors.slack.app_token_secret_ref = Some(build_secret_ref(&cfg.id, &r));
            }
            if let Some(r) = bot_token_ref {
                connectors.slack.bot_token_secret_ref = Some(build_secret_ref(&cfg.id, &r));
            }
            if let Some(ch) = allowed_channels {
                connectors.slack.allowed_channel_ids = ch;
            }
            if let Some(u) = allowed_users {
                connectors.slack.allowed_user_ids = u;
            }
            if let Some(m) = mirror_desktop {
                connectors.slack.mirror_desktop = m;
            }
            if let Some(a) = acts_for {
                connectors.slack.acts_for = a;
            }
        }
        ConnectorPatch::Discord {
            enabled,
            bot_token_ref,
            allowed_guilds,
            allowed_channels,
            allowed_users,
            acts_for,
        } => {
            if let Some(v) = enabled {
                connectors.discord.enabled = v;
                if v && connectors.discord.bot_token_secret_ref.is_none() {
                    connectors.discord.bot_token_secret_ref =
                        Some(build_secret_ref(&cfg.id, "discord"));
                }
            }
            if let Some(r) = bot_token_ref {
                connectors.discord.bot_token_secret_ref = Some(build_secret_ref(&cfg.id, &r));
            }
            if let Some(g) = allowed_guilds {
                connectors.discord.allowed_guild_ids = g;
            }
            if let Some(c) = allowed_channels {
                connectors.discord.allowed_channel_ids = c;
            }
            if let Some(u) = allowed_users {
                connectors.discord.allowed_user_ids = u;
            }
            if let Some(a) = acts_for {
                connectors.discord.acts_for = a;
            }
        }
        ConnectorPatch::Google {
            enabled,
            poll_secs,
            services,
            client_secret_ref,
            token_secret_ref,
            start_oauth,
        } => {
            start_google_oauth = start_oauth;
            config_changed = enabled.is_some()
                || poll_secs.is_some()
                || services.is_some()
                || client_secret_ref.is_some()
                || token_secret_ref.is_some();
            if let Some(v) = enabled {
                connectors.google.enabled = v;
                if v {
                    if connectors.google.oauth_client_secret_ref.is_none() {
                        connectors.google.oauth_client_secret_ref =
                            Some(build_secret_ref(&cfg.id, "google-oauth"));
                    }
                    if connectors.google.token_secret_ref.is_none() {
                        connectors.google.token_secret_ref =
                            Some(build_secret_ref(&cfg.id, "google-token"));
                    }
                }
            }
            if let Some(secs) = poll_secs {
                connectors.google.poll_interval_secs = secs;
            }
            if let Some(svcs) = services {
                let mut g = GoogleServicesConfig::default();
                for s in svcs {
                    match s {
                        GoogleServiceKind::Gmail => g.gmail = true,
                        GoogleServiceKind::Calendar => g.calendar = true,
                        GoogleServiceKind::Drive => g.drive = true,
                        GoogleServiceKind::Docs => g.docs = true,
                        GoogleServiceKind::Sheets => g.sheets = true,
                        GoogleServiceKind::Contacts => g.contacts = true,
                    }
                }
                connectors.google.services = g;
            }
            if let Some(r) = client_secret_ref {
                connectors.google.oauth_client_secret_ref = Some(build_secret_ref(&cfg.id, &r));
            }
            if let Some(r) = token_secret_ref {
                connectors.google.token_secret_ref = Some(build_secret_ref(&cfg.id, &r));
            }
        }
        ConnectorPatch::GitHub {
            enabled,
            repos,
            use_gh_cli,
            token_ref,
            poll_secs,
        } => {
            if let Some(v) = enabled {
                connectors.github.enabled = v;
            }
            if let Some(r) = repos {
                connectors.github.repos = r;
            }
            if let Some(gh) = use_gh_cli {
                connectors.github.use_gh_cli = gh;
            }
            if let Some(r) = token_ref {
                connectors.github.token_secret_ref = Some(build_secret_ref(&cfg.id, &r));
            } else if connectors.github.enabled
                && !connectors.github.use_gh_cli
                && connectors.github.token_secret_ref.is_none()
            {
                connectors.github.token_secret_ref = Some(build_secret_ref(&cfg.id, "github"));
            }
            if let Some(secs) = poll_secs {
                connectors.github.poll_interval_secs = secs;
            }
        }
        ConnectorPatch::Linear {
            enabled,
            teams,
            api_key_ref,
            poll_secs,
        } => {
            if let Some(v) = enabled {
                connectors.linear.enabled = v;
                if v && connectors.linear.api_key_secret_ref.is_none() {
                    connectors.linear.api_key_secret_ref =
                        Some(build_secret_ref(&cfg.id, "linear"));
                }
            }
            if let Some(t) = teams {
                connectors.linear.team_keys = t;
            }
            if let Some(r) = api_key_ref {
                connectors.linear.api_key_secret_ref = Some(build_secret_ref(&cfg.id, &r));
            }
            if let Some(secs) = poll_secs {
                connectors.linear.poll_interval_secs = secs;
            }
        }
        ConnectorPatch::Composio {
            enabled,
            apps,
            api_key_ref,
        } => {
            if let Some(v) = enabled {
                connectors.composio.enabled = v;
                if v && connectors.composio.api_key_secret_ref.is_none() {
                    connectors.composio.api_key_secret_ref =
                        Some(build_secret_ref(&cfg.id, "composio"));
                }
            }
            if let Some(a) = apps {
                connectors.composio.enabled_apps = a;
            }
            if let Some(r) = api_key_ref {
                connectors.composio.api_key_secret_ref = Some(build_secret_ref(&cfg.id, &r));
            }
        }
    }

    let mut updated_bot = cfg.clone();
    if config_changed || !start_google_oauth {
        let resp = client.call(BotRpcRequest::SetConnectorConfig {
            bot_id: cfg.id.clone(),
            connectors: Box::new(connectors),
        })?;
        match resp {
            BotRpcResponse::Bot(b) => updated_bot = *b,
            BotRpcResponse::Ok => {}
            _ => return Err(CliError::fail("unexpected reply for SetConnectorConfig")),
        }
    }

    if start_google_oauth {
        let resp = client.call(BotRpcRequest::StartGoogleOAuth {
            bot_id: updated_bot.id.clone(),
        })?;
        let BotRpcResponse::GoogleOAuthStarted { auth_url, state } = resp else {
            return Err(CliError::fail("unexpected reply for StartGoogleOAuth"));
        };
        return format::format_google_oauth_started(&updated_bot.id, &auth_url, &state, args.json);
    }

    format::format_bot_connectors(&updated_bot, args.json)
}

fn exec_tools_add(args: ToolsAddArgs, client: &mut dyn BotClient) -> Result<String, CliError> {
    let cfg = resolve_bot(client, &args.bot)?;
    let curated = find_bot_mcp_app(&args.app_id);

    let endpoint_url = args
        .url
        .or_else(|| curated.map(|app| app.endpoint_url.to_owned()));
    if endpoint_url.is_none() && args.command.is_empty() {
        return Err(CliError::usage(format!(
            "unknown curated MCP app '{}'; specify --url <https://...> or --cmd <bin>",
            args.app_id
        )));
    }
    if let Some(ref url) = endpoint_url
        && !(url.starts_with("https://")
            || url.starts_with("http://127.0.0.1")
            || url.starts_with("http://localhost"))
    {
        return Err(CliError::usage(
            "MCP endpoint --url must use https:// (or loopback http://127.0.0.1 / http://localhost)",
        ));
    }

    let name = args
        .name
        .or_else(|| curated.map(|app| app.name.to_owned()))
        .unwrap_or_else(|| args.app_id.clone());
    let mut header_names = args.headers;
    if header_names.is_empty()
        && let Some(h) = curated.and_then(|app| app.header_name)
    {
        header_names.push(h.to_owned());
    }
    let auth_kind = args.auth_kind.unwrap_or_else(|| {
        if let Some(app) = curated {
            app.auth_kind
        } else if !header_names.is_empty() || args.secret_ref.is_some() {
            BotMcpAuthKind::BearerHeader
        } else {
            BotMcpAuthKind::None
        }
    });
    let secret_ref = match args.secret_ref {
        Some(r) => Some(build_secret_ref(&cfg.id, &r)),
        None if auth_kind != BotMcpAuthKind::None => {
            Some(build_secret_ref(&cfg.id, &format!("mcp:{}", args.app_id)))
        }
        None => None,
    };

    let entry = BotMcpToolConfig {
        id: args.app_id,
        name,
        endpoint_url,
        command: args.command,
        auth_kind,
        header_names,
        secret_ref,
        enabled: true,
    };

    let mut tools = cfg.mcp_tools;
    if let Some(slot) = tools.iter_mut().find(|t| t.id == entry.id) {
        *slot = entry;
    } else {
        tools.push(entry);
    }

    let resp = client.call(BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
        bot_id: cfg.id,
        name: None,
        description: None,
        emoji: FieldPatch::Unchanged,
        avatar: FieldPatch::Unchanged,
        agent_id: None,
        model: FieldPatch::Unchanged,
        effort: FieldPatch::Unchanged,
        permission_mode: None,
        workspace: FieldPatch::Unchanged,
        paused: None,
        first_run_completed: None,
        notifications: None,
        schedules: None,
        look_around: None,
        memory_tidy: None,
        mcp_tools: Some(tools),
    })))?;
    let BotRpcResponse::Bot(updated) = resp else {
        return Err(CliError::fail("unexpected reply for UpdateBot"));
    };
    format::format_tools(&updated.id, &updated.mcp_tools, args.json)
}

pub(crate) fn build_secret_ref(bot_id: &BotId, key: &str) -> String {
    let trimmed = key.trim();
    let bot_prefix = format!("{}:", bot_id.as_str());
    if trimmed.starts_with(&bot_prefix) || trimmed.starts_with("keychain:") {
        trimmed.to_owned()
    } else {
        format!("{bot_prefix}{trimmed}")
    }
}

fn read_text_source<R: Read>(source: &TextInputSource, stdin: &mut R) -> Result<String, CliError> {
    match source {
        TextInputSource::Inline(s) => Ok(s.clone()),
        TextInputSource::File(path) => {
            fs::read_to_string(path).map_err(|e| CliError::fail(format!("read '{path}': {e}")))
        }
        TextInputSource::Stdin => {
            let mut buf = String::new();
            stdin
                .read_to_string(&mut buf)
                .map_err(|e| CliError::fail(format!("read stdin: {e}")))?;
            Ok(buf)
        }
    }
}

/// Resolves a bot reference (exact ID, slug, or case-insensitive display name) to its `BotConfig`.
fn resolve_bot(client: &mut dyn BotClient, selector: &str) -> Result<BotConfig, CliError> {
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        return Err(CliError::usage("bot selector must not be empty"));
    }
    if let Ok(bot_id) = BotId::new(trimmed)
        && let Ok(BotRpcResponse::Bot(cfg)) = client.call(BotRpcRequest::GetBot { bot_id })
    {
        return Ok(*cfg);
    }
    let resp = client.call(BotRpcRequest::ListBots)?;
    let BotRpcResponse::Bots(bots) = resp else {
        return Err(CliError::fail("unexpected reply for ListBots"));
    };
    if let Some(found) = bots
        .iter()
        .find(|b| b.id.as_str() == trimmed || b.slug.as_str() == trimmed)
    {
        return Ok(found.clone());
    }
    let name_matches: Vec<&BotConfig> = bots
        .iter()
        .filter(|b| b.name.eq_ignore_ascii_case(trimmed))
        .collect();
    match name_matches.as_slice() {
        [only] => Ok((*only).clone()),
        [] => Err(CliError::fail(format!("bot '{trimmed}' not found"))),
        _ => Err(CliError::fail(format!(
            "ambiguous bot name '{trimmed}'; specify its slug or ID"
        ))),
    }
}

fn resolve_bot_id(client: &mut dyn BotClient, selector: &str) -> Result<BotId, CliError> {
    Ok(resolve_bot(client, selector)?.id)
}

/// Parses a snooze deadline (`30m`, `2h`, `1d`, `1w`, epoch secs/ms, ISO `YYYY-MM-DD[THH:MM[:SS]Z]`,
/// or `clear`/`none`/`off`) into a `FieldPatch<u64>` epoch-millisecond timestamp.
pub(crate) fn parse_snooze_until(raw: &str, now_ms: u64) -> Result<FieldPatch<u64>, CliError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CliError::usage("snooze duration or timestamp is empty"));
    }
    if matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "clear" | "none" | "off"
    ) {
        return Ok(FieldPatch::Clear);
    }
    // Pure integer: epoch ms (>= 1e12) or epoch secs (>= 1e9)
    if trimmed.bytes().all(|b| b.is_ascii_digit()) {
        let num = trimmed
            .parse::<u64>()
            .map_err(|_| CliError::usage(format!("invalid snooze timestamp '{trimmed}'")))?;
        let ms = if num >= 1_000_000_000_000 {
            num
        } else {
            num.saturating_mul(1000)
        };
        return Ok(FieldPatch::Set(ms));
    }
    // Relative suffix: s / m / h / d / w
    if let Some(unit) = trimmed.bytes().last()
        && let Some(prefix) = trimmed.get(..trimmed.len().saturating_sub(1))
        && !prefix.is_empty()
        && prefix.bytes().all(|b| b.is_ascii_digit())
    {
        let mult_secs: Option<u64> = match unit.to_ascii_lowercase() {
            b's' => Some(1),
            b'm' => Some(60),
            b'h' => Some(3_600),
            b'd' => Some(86_400),
            b'w' => Some(7 * 86_400),
            _ => None,
        };
        if let Some(mult) = mult_secs {
            let count = prefix
                .parse::<u64>()
                .map_err(|_| CliError::usage(format!("invalid snooze duration '{trimmed}'")))?;
            let delta_ms = count.saturating_mul(mult).saturating_mul(1000);
            return Ok(FieldPatch::Set(now_ms.saturating_add(delta_ms)));
        }
    }
    // ISO date or UTC datetime: YYYY-MM-DD or YYYY-MM-DDTHH:MM(:SS)?Z?
    if let Some(ms) = parse_iso_utc_ms(trimmed) {
        return Ok(FieldPatch::Set(ms));
    }
    Err(CliError::usage(format!(
        "invalid snooze value '{trimmed}' (expected e.g. 30m, 2h, 1d, 2026-11-25, 2026-11-25T09:00Z, or clear)"
    )))
}

fn parse_iso_utc_ms(raw: &str) -> Option<u64> {
    let (date_part, time_part) = match raw.split_once('T').or_else(|| raw.split_once('t')) {
        Some((d, t)) => (d, Some(t.trim_end_matches(['Z', 'z']))),
        None => (raw, None),
    };
    let mut date_it = date_part.split('-');
    let year: i64 = date_it.next()?.parse().ok()?;
    let month: u32 = date_it.next()?.parse().ok()?;
    let day: u32 = date_it.next()?.parse().ok()?;
    if date_it.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (hour, minute, second) = match time_part {
        None => (0u64, 0u64, 0u64),
        Some(tp) => {
            let mut t_it = tp.split(':');
            let h: u64 = t_it.next()?.parse().ok()?;
            let m: u64 = t_it.next()?.parse().ok()?;
            let s: u64 = match t_it.next() {
                Some(sec) => sec.parse().ok()?,
                None => 0,
            };
            if t_it.next().is_some() || h > 23 || m > 59 || s > 59 {
                return None;
            }
            (h, m, s)
        }
    };
    let days = days_from_civil(year, month, day)?;
    if days < 0 {
        return None;
    }
    let secs = u64::try_from(days)
        .ok()?
        .checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)?;
    secs.checked_mul(1000)
}

/// Howard Hinnant's civil-to-days algorithm (days since 1970-01-01).
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    let y = if month <= 2 {
        year.checked_sub(1)?
    } else {
        year
    };
    let era = y.div_euclid(400);
    let yoe = u64::try_from(y.rem_euclid(400)).ok()?;
    let m = u64::from(month);
    let d = u64::from(day);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let total = era
        .checked_mul(146_097)?
        .checked_add(i64::try_from(doe).ok()?)?
        .checked_sub(719_468)?;
    Some(total)
}

/// Howard Hinnant's days-to-civil algorithm: formats UTC epoch seconds as `YYYY-MM-DD`.
pub(crate) fn today_iso_utc(epoch_secs: u64) -> String {
    let z = i64::try_from(epoch_secs / 86_400)
        .unwrap_or(0)
        .saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}")
}
