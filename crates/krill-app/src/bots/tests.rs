// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px};
use krill_proto::SessionId;
use krill_proto::agent::Permission;
use krill_proto::bots::{
    BOT_MCP_APPS, BUILTIN_BOT_TEMPLATES, BotAccessRequest, BotAccessRequestStatus, BotConfig,
    BotConnectorStatus, BotConnectorTestResult, BotGroupChat, BotGroupMessage, BotGroupThread,
    BotGroupView, BotId, BotItem, BotItemActor, BotItemDecision, BotItemEvent, BotItemEventKind,
    BotItemLink, BotItemStatus, BotMcpAuthKind, BotMcpOAuthState, BotMemoryHistoryEntry,
    BotMemoryTidyStatus, BotMemoryTopicInfo, BotRpcRequest, BotRpcResponse, BotSessionSummary,
    BotSuggestion, BotTemplateCategory, BotToolRelayStatus, BotToolServerStatus,
    COMPOSIO_CURATED_APPS, ComposioConnectionInfo, ConnectorHealthState, DefaultTrackerBots,
    ExternalBotCandidate, ExternalBotSourceApp, ExternalSource, forget_memory_fact,
    generate_slack_socket_mode_manifest,
};

use super::items::ItemFilter;
use super::rpc::{
    BOT_RUNTIMES, BotRpcCall, format_csv_i64s, format_csv_strings, parse_csv_i64s,
    parse_csv_strings, runtime_models, sidebar_snapshot,
};
use super::templates::filter_templates;
use super::{BadgeChanged, Close, DetailTab, OpenSession, Screen, View};

#[derive(Default)]
struct MockState {
    bots: Vec<BotConfig>,
    items: Vec<BotItem>,
    soul: String,
    memory: String,
    memory_history: Vec<BotMemoryHistoryEntry>,
    tidy_status: BotMemoryTidyStatus,
    topics: BTreeMap<String, String>,
    sessions: Vec<BotSessionSummary>,
    groups: Vec<BotGroupChat>,
    group_threads: Vec<BotGroupThread>,
    group_messages: Vec<BotGroupMessage>,
    statuses: Vec<BotConnectorStatus>,
    access_requests: Vec<BotAccessRequest>,
    relay: BotToolRelayStatus,
    secrets: Vec<(String, String)>,
    requests: Vec<BotRpcRequest>,
    candidates: Vec<ExternalBotCandidate>,
    suggestions: Vec<BotSuggestion>,
    oauth_states: BTreeMap<(BotId, String), BotMcpOAuthState>,
    composio_connections: Vec<ComposioConnectionInfo>,
    default_trackers: DefaultTrackerBots,
    fail_get_soul: Option<String>,
    fail_get_memory: Option<String>,
    fail_create_item: Option<String>,
    fail_reload_connectors: Option<String>,
    fail_set_connector_config: Option<String>,
    /// `ListAgentModels` replies by agent; a missing agent answers like a daemon that predates it.
    agent_models: BTreeMap<String, krill_proto::bots::BotAgentModels>,
}

fn sample_bot(id: &str, name: &str) -> BotConfig {
    let mut cfg =
        BotConfig::new(BotId::new(id).unwrap(), name, "claude", 1_700_000_000_000).unwrap();
    cfg.description = "Reviews pull requests automatically".to_owned();
    cfg.emoji = Some("🤖".to_owned());
    cfg.connectors.slack.enabled = true;
    cfg
}

fn sample_item(bot_id: &str, uid: &str, number: u64, status: BotItemStatus) -> BotItem {
    BotItem {
        uid: uid.to_owned(),
        number,
        owner_bot_id: BotId::new(bot_id).unwrap(),
        title: format!("Task #{number}"),
        description: Some("Detailed context for task".to_owned()),
        done_means: Some("Tests pass and PR merged".to_owned()),
        status,
        close_note: None,
        links: vec![BotItemLink::Pr(
            "https://github.com/acme/repo/pull/42".to_owned(),
        )],
        events: vec![BotItemEvent {
            id: 1,
            item_uid: uid.to_owned(),
            kind: BotItemEventKind::Commented,
            actor: krill_proto::bots::BotItemActor::Owner { device: None },
            text: "Initial comment".to_owned(),
            decisions: Vec::new(),
            created_at_ms: 1_700_000_000_000,
        }],
        snoozed_until_ms: None,
        version: 1,
        last_activity_at_ms: 1_700_000_000_000,
        created_at_ms: 1_700_000_000_000,
        updated_at_ms: 1_700_000_000_000,
        closed_at_ms: None,
    }
}

fn make_mock_rpc(state: Arc<Mutex<MockState>>) -> BotRpcCall {
    Arc::new(move |req: &BotRpcRequest| {
        let mut st = state.lock().unwrap();
        st.requests.push(req.clone());
        match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(st.bots.clone())),
            BotRpcRequest::CreateBot(params) => {
                let id = format!("bot-{}", st.bots.len() + 1);
                let mut cfg = BotConfig::new(
                    BotId::new(&id).unwrap(),
                    &params.name,
                    &params.agent_id,
                    1_700_000_000_000,
                )
                .unwrap();
                cfg.description.clone_from(&params.description);
                cfg.emoji.clone_from(&params.emoji);
                cfg.model.clone_from(&params.model);
                cfg.workspace.clone_from(&params.workspace);
                cfg.template_id.clone_from(&params.template_id);
                if let Some(s) = &params.initial_soul {
                    st.soul.clone_from(s);
                }
                st.bots.push(cfg.clone());
                Ok(BotRpcResponse::Bot(Box::new(cfg)))
            }
            BotRpcRequest::CreateBotV2(v2) => {
                let base = &v2.base;
                let id_str = v2.slug.as_ref().map_or_else(
                    || format!("bot-{}", st.bots.len() + 1),
                    |s| s.as_str().to_owned(),
                );
                let mut cfg = BotConfig::new(
                    BotId::new(&id_str).unwrap(),
                    &base.name,
                    &base.agent_id,
                    1_700_000_000_000,
                )
                .unwrap();
                if let Some(slug) = &v2.slug {
                    cfg.slug = slug.clone();
                }
                cfg.description.clone_from(&base.description);
                cfg.emoji.clone_from(&base.emoji);
                cfg.avatar.clone_from(&base.avatar);
                cfg.avatar_color.clone_from(&v2.avatar_color);
                cfg.model.clone_from(&base.model);
                cfg.effort.clone_from(&base.effort);
                if let Some(perm) = base.permission_mode {
                    cfg.permission_mode = perm;
                }
                cfg.workspace.clone_from(&base.workspace);
                cfg.template_id.clone_from(&base.template_id);
                cfg.skills.clone_from(&v2.skills);
                if let Some(s) = &base.initial_soul {
                    st.soul.clone_from(s);
                }
                st.bots.push(cfg.clone());
                Ok(BotRpcResponse::Bot(Box::new(cfg)))
            }
            BotRpcRequest::UpdateBot(params) => {
                let bot = st
                    .bots
                    .iter_mut()
                    .find(|b| b.id == params.bot_id)
                    .ok_or_else(|| "bot not found".to_owned())?;
                if let Some(name) = &params.name {
                    bot.name.clone_from(name);
                }
                if let Some(desc) = &params.description {
                    bot.description.clone_from(desc);
                }
                params.emoji.clone().apply(&mut bot.emoji);
                params.avatar.clone().apply(&mut bot.avatar);
                if let Some(agent_id) = &params.agent_id {
                    bot.agent_id.clone_from(agent_id);
                }
                params.model.clone().apply(&mut bot.model);
                params.effort.clone().apply(&mut bot.effort);
                if let Some(perm) = params.permission_mode {
                    bot.permission_mode = perm;
                }
                params.workspace.clone().apply(&mut bot.workspace);
                if let Some(p) = params.paused {
                    bot.paused = p;
                }
                if let Some(n) = params.notifications {
                    bot.notifications = n;
                }
                if let Some(schedules) = &params.schedules {
                    bot.schedules.clone_from(schedules);
                }
                if let Some(look_around) = &params.look_around {
                    bot.look_around = look_around.clone();
                }
                if let Some(memory_tidy) = params.memory_tidy {
                    bot.memory_tidy = memory_tidy;
                }
                if let Some(tools) = &params.mcp_tools {
                    bot.mcp_tools.clone_from(tools);
                }
                Ok(BotRpcResponse::Bot(Box::new(bot.clone())))
            }
            BotRpcRequest::UpdateBotExtras(params) => {
                let bot = st
                    .bots
                    .iter_mut()
                    .find(|b| b.id == params.bot_id)
                    .ok_or_else(|| "bot not found".to_owned())?;
                params.avatar_color.clone().apply(&mut bot.avatar_color);
                if let Some(skills) = &params.skills {
                    bot.skills.clone_from(skills);
                }
                Ok(BotRpcResponse::Bot(Box::new(bot.clone())))
            }
            BotRpcRequest::DeleteBot { bot_id, .. } => {
                st.bots.retain(|b| &b.id != bot_id);
                Ok(BotRpcResponse::Deleted {
                    bot_id: bot_id.clone(),
                })
            }
            BotRpcRequest::UploadBotAvatar { bot_id, png_bytes } => {
                if let Some(b) = st.bots.iter_mut().find(|b| &b.id == bot_id) {
                    b.avatar = Some("avatar.png".to_owned());
                }
                Ok(BotRpcResponse::BotAvatar(
                    krill_proto::bots::BotAvatarData {
                        bot_id: bot_id.clone(),
                        mime: "image/png".to_owned(),
                        png_bytes: png_bytes.clone(),
                    },
                ))
            }
            BotRpcRequest::StartBotSession { bot_id, .. } => {
                let sid = SessionId(77 + st.sessions.len() as u64);
                st.sessions.push(BotSessionSummary {
                    session_id: sid,
                    bot_id: bot_id.clone(),
                    link_id: None,
                    pid: 4242,
                    live: true,
                    working: false,
                    label: "Bot Chat".to_owned(),
                    created_at_ms: 1_700_000_000_000,
                });
                Ok(BotRpcResponse::SessionStarted {
                    bot_id: bot_id.clone(),
                    session_id: sid,
                    pid: 4242,
                })
            }
            BotRpcRequest::ListBotSessions { bot_id } => {
                let filtered = st
                    .sessions
                    .iter()
                    .filter(|s| bot_id.as_ref().is_none_or(|b| &s.bot_id == b))
                    .cloned()
                    .collect();
                Ok(BotRpcResponse::BotSessions(filtered))
            }
            BotRpcRequest::StopBotSession { session_id, .. } => {
                if let Some(s) = st.sessions.iter_mut().find(|s| s.session_id == *session_id) {
                    s.live = false;
                }
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::GetSoul { .. } => {
                if let Some(err) = st.fail_get_soul.clone() {
                    return Err(err);
                }
                Ok(BotRpcResponse::Text(st.soul.clone()))
            }
            BotRpcRequest::SetSoul { content, .. } => {
                st.soul.clone_from(content);
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::GetMemory { .. } => {
                if let Some(err) = st.fail_get_memory.clone() {
                    return Err(err);
                }
                Ok(BotRpcResponse::Text(st.memory.clone()))
            }
            BotRpcRequest::UpdateMemory { content, .. } => {
                st.memory.clone_from(content);
                Ok(BotRpcResponse::MemoryUpdated {
                    content: st.memory.clone(),
                    restored_facts: Vec::new(),
                    removed_count: 0,
                })
            }
            BotRpcRequest::ForgetMemoryFact { fact, .. } => {
                let (next, removed) = forget_memory_fact(&st.memory, fact);
                st.memory = next.clone();
                Ok(BotRpcResponse::MemoryUpdated {
                    content: next,
                    restored_facts: Vec::new(),
                    removed_count: removed,
                })
            }
            BotRpcRequest::EditMemoryFact {
                bot_id,
                old_fact,
                new_fact,
            } => {
                let before = st.memory.clone();
                st.memory = st.memory.replace(old_fact, new_fact);
                let after = st.memory.clone();
                let idx = st.memory_history.len() + 1;
                st.memory_history.push(BotMemoryHistoryEntry {
                    entry_id: format!("hist-{idx}"),
                    bot_id: bot_id.clone(),
                    timestamp_ms: 1_700_000_000_000,
                    actor: "owner".to_owned(),
                    action: "edit".to_owned(),
                    added: vec![new_fact.clone()],
                    removed: vec![old_fact.clone()],
                    before_content: before,
                    after_content: after.clone(),
                    undoable: true,
                    undone: false,
                });
                Ok(BotRpcResponse::MemoryUpdated {
                    content: after,
                    restored_facts: Vec::new(),
                    removed_count: 0,
                })
            }
            BotRpcRequest::GetMemoryHistory { .. } => {
                Ok(BotRpcResponse::MemoryHistory(st.memory_history.clone()))
            }
            BotRpcRequest::UndoMemoryChange { entry_id, .. } => {
                let restored = st
                    .memory_history
                    .iter_mut()
                    .find(|e| e.entry_id == *entry_id)
                    .map(|entry| {
                        entry.undone = true;
                        entry.before_content.clone()
                    });
                if let Some(before) = restored {
                    st.memory = before;
                }
                Ok(BotRpcResponse::MemoryUpdated {
                    content: st.memory.clone(),
                    restored_facts: Vec::new(),
                    removed_count: 0,
                })
            }
            BotRpcRequest::RunMemoryTidy { bot_id } => {
                st.tidy_status.last_tidy_id = Some("tidy-1".to_owned());
                st.tidy_status.undoable = true;
                Ok(BotRpcResponse::MemoryTidyResult {
                    bot_id: bot_id.clone(),
                    summary: "Compacted 1 duplicate fact".to_owned(),
                    changes: 1,
                })
            }
            BotRpcRequest::GetMemoryTidyStatus { .. } => {
                Ok(BotRpcResponse::MemoryTidyStatus(st.tidy_status.clone()))
            }
            BotRpcRequest::UndoMemoryTidy { .. } => Ok(BotRpcResponse::MemoryUpdated {
                content: st.memory.clone(),
                restored_facts: Vec::new(),
                removed_count: 0,
            }),
            BotRpcRequest::ListMemoryTopics { .. } => {
                let list = st
                    .topics
                    .iter()
                    .map(|(topic, body)| BotMemoryTopicInfo {
                        topic: topic.clone(),
                        bytes: body.len() as u64,
                        updated_at_ms: 1_700_000_000_000,
                    })
                    .collect();
                Ok(BotRpcResponse::MemoryTopics(list))
            }
            BotRpcRequest::GetMemoryTopic { topic, .. } => Ok(BotRpcResponse::Text(
                st.topics.get(topic).cloned().unwrap_or_default(),
            )),
            BotRpcRequest::SetMemoryTopic { topic, content, .. } => {
                st.topics.insert(topic.clone(), content.clone());
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::RunLookAround { bot_id } => Ok(BotRpcResponse::LookAroundResult {
                bot_id: bot_id.clone(),
                suggestions: st.suggestions.clone(),
            }),
            BotRpcRequest::ListSuggestions {
                bot_id,
                include_dismissed,
            } => {
                let filtered = st
                    .suggestions
                    .iter()
                    .filter(|s| bot_id.as_ref().is_none_or(|b| &s.bot_id == b))
                    .filter(|s| *include_dismissed || !s.dismissed)
                    .cloned()
                    .collect();
                Ok(BotRpcResponse::Suggestions(filtered))
            }
            BotRpcRequest::DismissSuggestion { suggestion_id, .. } => {
                if let Some(s) = st.suggestions.iter_mut().find(|s| s.id == *suggestion_id) {
                    s.dismissed = true;
                }
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::StartSuggestion {
                bot_id,
                suggestion_id,
            } => {
                st.suggestions.retain(|s| s.id != *suggestion_id);
                Ok(BotRpcResponse::SessionStarted {
                    bot_id: bot_id.clone(),
                    session_id: SessionId(91),
                    pid: 4243,
                })
            }
            BotRpcRequest::ListItems {
                bot_id,
                include_closed,
            } => {
                let filtered = st
                    .items
                    .iter()
                    .filter(|i| bot_id.as_ref().is_none_or(|b| &i.owner_bot_id == b))
                    .filter(|i| *include_closed || i.status.is_open())
                    .cloned()
                    .collect();
                Ok(BotRpcResponse::Items(filtered))
            }
            BotRpcRequest::CreateItem(params) => {
                if let Some(err) = st.fail_create_item.clone() {
                    return Err(err);
                }
                let num = (st.items.len() as u64) + 1;
                let item = BotItem {
                    uid: format!("item-{num}"),
                    number: num,
                    owner_bot_id: params.bot_id.clone(),
                    title: params.title.clone(),
                    description: params.description.clone(),
                    done_means: params.done_means.clone(),
                    status: params.status.unwrap_or(BotItemStatus::Planned),
                    close_note: None,
                    links: params.links.clone(),
                    events: Vec::new(),
                    snoozed_until_ms: None,
                    version: 1,
                    last_activity_at_ms: 1_700_000_000_000,
                    created_at_ms: 1_700_000_000_000,
                    updated_at_ms: 1_700_000_000_000,
                    closed_at_ms: None,
                };
                st.items.push(item.clone());
                Ok(BotRpcResponse::Item(Box::new(item)))
            }
            BotRpcRequest::UpdateItem(params) => {
                let item = st
                    .items
                    .iter_mut()
                    .find(|i| i.uid == params.item_id)
                    .ok_or_else(|| "item not found".to_owned())?;
                if let Some(title) = &params.title {
                    item.title.clone_from(title);
                }
                if let Some(desc) = &params.description {
                    item.description = Some(desc.clone());
                }
                if let Some(done) = &params.done_means {
                    item.done_means = Some(done.clone());
                }
                if let Some(status) = params.status {
                    item.status = status;
                }
                if let Some(note) = &params.close_note {
                    item.close_note = Some(note.clone());
                }
                params
                    .snoozed_until_ms
                    .clone()
                    .apply(&mut item.snoozed_until_ms);
                Ok(BotRpcResponse::Item(Box::new(item.clone())))
            }
            BotRpcRequest::AnswerBotItem {
                item_id,
                answer_text,
                option_index,
                ..
            } => {
                let item = st
                    .items
                    .iter_mut()
                    .find(|i| i.uid == *item_id)
                    .ok_or_else(|| "item not found".to_owned())?;
                let txt = answer_text
                    .clone()
                    .unwrap_or_else(|| format!("Selected option {}", option_index.unwrap_or(0)));
                item.events.push(BotItemEvent {
                    id: (item.events.len() as u64) + 1,
                    item_uid: item.uid.clone(),
                    kind: BotItemEventKind::Commented,
                    actor: BotItemActor::Owner { device: None },
                    text: txt,
                    decisions: Vec::new(),
                    created_at_ms: 1_700_000_000_000,
                });
                if item.status == BotItemStatus::NeedsYou {
                    item.status = BotItemStatus::Working;
                }
                Ok(BotRpcResponse::Item(Box::new(item.clone())))
            }
            BotRpcRequest::AddItemComment { item_id, text, .. } => {
                let item = st
                    .items
                    .iter_mut()
                    .find(|i| i.uid == *item_id)
                    .ok_or_else(|| "item not found".to_owned())?;
                item.events.push(BotItemEvent {
                    id: (item.events.len() as u64) + 1,
                    item_uid: item.uid.clone(),
                    kind: BotItemEventKind::Commented,
                    actor: BotItemActor::Owner { device: None },
                    text: text.clone(),
                    decisions: Vec::new(),
                    created_at_ms: 1_700_000_000_000,
                });
                Ok(BotRpcResponse::Item(Box::new(item.clone())))
            }
            BotRpcRequest::AddItemLink { item_id, link } => {
                let item = st
                    .items
                    .iter_mut()
                    .find(|i| i.uid == *item_id)
                    .ok_or_else(|| "item not found".to_owned())?;
                item.links.push(link.clone());
                Ok(BotRpcResponse::Item(Box::new(item.clone())))
            }
            BotRpcRequest::ListGroups => Ok(BotRpcResponse::Groups(st.groups.clone())),
            BotRpcRequest::CreateGroup {
                name,
                bot_ids,
                description,
            } => {
                let id = format!("grp-{}", st.groups.len() + 1);
                let g = BotGroupChat {
                    id: id.clone(),
                    name: name.clone(),
                    bot_ids: bot_ids.clone(),
                    description: description.clone(),
                    created_at_ms: 1_700_000_000_000,
                    updated_at_ms: 1_700_000_000_000,
                };
                st.groups.push(g.clone());
                st.group_threads.push(BotGroupThread {
                    thread_id: format!("thr-{id}-1"),
                    group_id: id,
                    title: "Main".to_owned(),
                    created_at_ms: 1_700_000_000_000,
                    updated_at_ms: 1_700_000_000_000,
                });
                Ok(BotRpcResponse::Group(g))
            }
            BotRpcRequest::UpdateGroup {
                group_id,
                name,
                description,
                bot_ids,
            } => {
                let g = st
                    .groups
                    .iter_mut()
                    .find(|g| g.id == *group_id)
                    .ok_or_else(|| "group not found".to_owned())?;
                if let Some(n) = name {
                    g.name.clone_from(n);
                }
                if let Some(d) = description {
                    g.description.clone_from(d);
                }
                if let Some(ids) = bot_ids {
                    g.bot_ids.clone_from(ids);
                }
                Ok(BotRpcResponse::Group(g.clone()))
            }
            BotRpcRequest::DeleteGroup { group_id } => {
                st.groups.retain(|g| g.id != *group_id);
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::ReorderGroups { group_ids } => {
                let mut reordered = Vec::new();
                for id in group_ids {
                    if let Some(g) = st.groups.iter().find(|g| &g.id == id) {
                        reordered.push(g.clone());
                    }
                }
                st.groups = reordered;
                Ok(BotRpcResponse::Groups(st.groups.clone()))
            }
            BotRpcRequest::NewGroupThread { group_id, title } => {
                let tid = format!("thr-{}-{}", group_id, st.group_threads.len() + 1);
                let g = st
                    .groups
                    .iter()
                    .find(|g| g.id == *group_id)
                    .cloned()
                    .ok_or_else(|| "group not found".to_owned())?;
                st.group_threads.push(BotGroupThread {
                    thread_id: tid.clone(),
                    group_id: group_id.clone(),
                    title: title.clone().unwrap_or_else(|| "Thread".to_owned()),
                    created_at_ms: 1_700_000_000_000,
                    updated_at_ms: 1_700_000_000_000,
                });
                let threads = st
                    .group_threads
                    .iter()
                    .filter(|t| t.group_id == *group_id)
                    .cloned()
                    .collect();
                let messages = st
                    .group_messages
                    .iter()
                    .filter(|m| m.group_id == *group_id && m.thread_id == tid)
                    .cloned()
                    .collect();
                Ok(BotRpcResponse::GroupView(BotGroupView {
                    group: g,
                    threads,
                    messages,
                    active_run: false,
                }))
            }
            BotRpcRequest::ReadGroup {
                group_id,
                thread_id,
                ..
            } => {
                let g = st
                    .groups
                    .iter()
                    .find(|g| g.id == *group_id)
                    .cloned()
                    .ok_or_else(|| "group not found".to_owned())?;
                let threads: Vec<_> = st
                    .group_threads
                    .iter()
                    .filter(|t| t.group_id == *group_id)
                    .cloned()
                    .collect();
                let active_tid = thread_id
                    .as_deref()
                    .or_else(|| threads.first().map(|t| t.thread_id.as_str()));
                let messages = st
                    .group_messages
                    .iter()
                    .filter(|m| {
                        m.group_id == *group_id && active_tid.is_none_or(|t| m.thread_id == t)
                    })
                    .cloned()
                    .collect();
                Ok(BotRpcResponse::GroupView(BotGroupView {
                    group: g,
                    threads,
                    messages,
                    active_run: false,
                }))
            }
            BotRpcRequest::SendGroupMessage {
                group_id,
                thread_id,
                sender,
                text,
                ..
            } => {
                let tid = thread_id.clone().unwrap_or_else(|| {
                    st.group_threads
                        .iter()
                        .find(|t| t.group_id == *group_id)
                        .map_or_else(|| "thr-1".to_owned(), |t| t.thread_id.clone())
                });
                let msg = BotGroupMessage {
                    message_id: format!("msg-{}", st.group_messages.len() + 1),
                    group_id: group_id.clone(),
                    thread_id: tid,
                    sender_id: sender.clone(),
                    sender_name: sender.clone(),
                    body: text.clone(),
                    mentions: Vec::new(),
                    created_at_ms: 1_700_000_000_000,
                    in_progress: false,
                };
                st.group_messages.push(msg.clone());
                Ok(BotRpcResponse::GroupMessagePosted {
                    message: msg,
                    dispatched_bots: Vec::new(),
                })
            }
            BotRpcRequest::SetConnectorConfig { bot_id, connectors } => {
                if let Some(err) = st.fail_set_connector_config.clone() {
                    return Err(err);
                }
                let bot = st
                    .bots
                    .iter_mut()
                    .find(|b| &b.id == bot_id)
                    .ok_or_else(|| "bot not found".to_owned())?;
                bot.connectors = (**connectors).clone();
                Ok(BotRpcResponse::Bot(Box::new(bot.clone())))
            }
            BotRpcRequest::SetSecret { secret_ref, value } => {
                st.secrets.push((secret_ref.clone(), value.clone()));
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::DeleteSecret { secret_ref } => {
                st.secrets.retain(|(k, _)| k != secret_ref);
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::ReloadConnectors { .. } => {
                if let Some(err) = st.fail_reload_connectors.clone() {
                    return Err(err);
                }
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::TestConnector { source, .. } => {
                Ok(BotRpcResponse::ConnectorTest(BotConnectorTestResult {
                    source: *source,
                    ok: true,
                    account_label: Some("@bot".to_owned()),
                    detail: "connected".to_owned(),
                }))
            }
            BotRpcRequest::RedeemPairingCode { code } => {
                if let Some(req) = st.access_requests.first_mut() {
                    req.status = BotAccessRequestStatus::Approved;
                }
                let _ = code;
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::StartMcpOAuth { bot_id, tool_id }
            | BotRpcRequest::McpOAuthStatus { bot_id, tool_id } => {
                let st_val = BotMcpOAuthState {
                    bot_id: bot_id.clone(),
                    tool_id: tool_id.clone(),
                    status: "connected".to_owned(),
                    auth_url: None,
                    error: None,
                    updated_at_ms: 1_700_000_000_000,
                };
                st.oauth_states
                    .insert((bot_id.clone(), tool_id.clone()), st_val.clone());
                Ok(BotRpcResponse::McpOAuth(st_val))
            }
            BotRpcRequest::CancelMcpOAuth { bot_id, tool_id } => {
                let st_val = BotMcpOAuthState {
                    bot_id: bot_id.clone(),
                    tool_id: tool_id.clone(),
                    status: "canceled".to_owned(),
                    auth_url: None,
                    error: None,
                    updated_at_ms: 1_700_000_000_000,
                };
                st.oauth_states
                    .insert((bot_id.clone(), tool_id.clone()), st_val.clone());
                Ok(BotRpcResponse::McpOAuth(st_val))
            }
            BotRpcRequest::ListComposioConnections { .. } => Ok(
                BotRpcResponse::ComposioConnections(st.composio_connections.clone()),
            ),
            BotRpcRequest::SetDefaultTrackerBot { source, bot_id } => {
                match source {
                    ExternalSource::GitHub => st.default_trackers.github.clone_from(bot_id),
                    ExternalSource::Linear => st.default_trackers.linear.clone_from(bot_id),
                    _ => {}
                }
                Ok(BotRpcResponse::DefaultTrackerBots(
                    st.default_trackers.clone(),
                ))
            }
            BotRpcRequest::GetDefaultTrackerBots => Ok(BotRpcResponse::DefaultTrackerBots(
                st.default_trackers.clone(),
            )),
            BotRpcRequest::ConnectorStatus { bot_id } => {
                let filtered = st
                    .statuses
                    .iter()
                    .filter(|s| bot_id.as_ref().is_none_or(|b| &s.bot_id == b))
                    .cloned()
                    .collect();
                Ok(BotRpcResponse::ConnectorStatuses(filtered))
            }
            BotRpcRequest::TriggerSyncNow { bot_id, source } => Ok(BotRpcResponse::SyncTriggered {
                bot_id: bot_id.clone(),
                sources_synced: source.iter().copied().collect(),
            }),
            BotRpcRequest::ListAccessRequests { bot_id } => {
                let filtered = st
                    .access_requests
                    .iter()
                    .filter(|r| bot_id.as_ref().is_none_or(|b| &r.bot_id == b))
                    .cloned()
                    .collect();
                Ok(BotRpcResponse::AccessRequests(filtered))
            }
            BotRpcRequest::DecideAccessRequest { token, approve } => {
                let req = st
                    .access_requests
                    .iter_mut()
                    .find(|r| r.token == *token)
                    .ok_or_else(|| "request not found".to_owned())?;
                req.status = if *approve {
                    BotAccessRequestStatus::Approved
                } else {
                    BotAccessRequestStatus::Declined
                };
                Ok(BotRpcResponse::AccessRequestDecided(req.clone()))
            }
            BotRpcRequest::StartGoogleOAuth { .. } => Ok(BotRpcResponse::GoogleOAuthStarted {
                auth_url: "https://accounts.google.com/o/oauth2/v2/auth?state=test".to_owned(),
                state: "test".to_owned(),
            }),
            BotRpcRequest::ToolRelayStatus => Ok(BotRpcResponse::ToolRelayStatus(st.relay.clone())),
            BotRpcRequest::ImportExternalBots { dry_run, .. } => {
                let cands = st.candidates.clone();
                let imported = if *dry_run {
                    Vec::new()
                } else {
                    let b = sample_bot("imported-orca", "Imported Orca Bot");
                    st.bots.push(b.clone());
                    vec![b]
                };
                Ok(BotRpcResponse::ImportedExternalBots {
                    candidates: cands,
                    imported,
                })
            }
            BotRpcRequest::ListAgentModels { agent_id } => Ok(st
                .agent_models
                .get(agent_id)
                .map_or(BotRpcResponse::Ok, |m| {
                    BotRpcResponse::AgentModels(m.clone())
                })),
            _ => Ok(BotRpcResponse::Ok),
        }
    })
}

fn open_view(
    cx: &mut TestAppContext,
    state: Arc<Mutex<MockState>>,
) -> (Entity<View>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state);
    let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx));
    vcx.simulate_resize(gpui::size(px(1040.), px(720.)));
    vcx.run_until_parked();
    (view, vcx)
}

#[test]
fn catalog_helpers_cover_24_templates_6_categories_8_runtimes_and_csv() {
    assert_eq!(BUILTIN_BOT_TEMPLATES.len(), 24);
    assert_eq!(filter_templates(None, "").len(), 24);
    let mut sum = 0usize;
    for &cat in BotTemplateCategory::ALL {
        let count = filter_templates(Some(cat), "").len();
        assert!(count >= 2, "category {cat:?} should have templates");
        sum += count;
    }
    assert_eq!(sum, 24);
    assert_eq!(BOT_RUNTIMES.len(), 8);
    assert_eq!(BOT_MCP_APPS.len(), 13);
    assert_eq!(COMPOSIO_CURATED_APPS.len(), 24);

    assert_eq!(
        parse_csv_strings(" acme/a, acme/b\nacme/a , "),
        vec!["acme/a", "acme/b"]
    );
    assert_eq!(format_csv_strings(&["a".into(), "b".into()]), "a, b");
    assert_eq!(
        parse_csv_i64s("123, -100456, bad, 123"),
        vec![123, -100_456]
    );
    assert_eq!(format_csv_i64s(&[123, -100_456]), "123, -100456");
}

#[gpui::test]
fn items_inbox_filters_status_snooze_comments_and_open_chat(cx: &mut TestAppContext) {
    let mut mock = MockState::default();
    mock.bots.push(sample_bot("bot-1", "PR Bot"));
    mock.items
        .push(sample_item("bot-1", "item-1", 1, BotItemStatus::NeedsYou));
    mock.items
        .push(sample_item("bot-1", "item-2", 2, BotItemStatus::Working));
    let state = Arc::new(Mutex::new(mock));
    let (view, vcx) = open_view(cx, state.clone());

    let opened_session = Arc::new(Mutex::new(None));
    let opened_capture = opened_session.clone();
    let badge_seen = Arc::new(Mutex::new(0usize));
    let badge_capture = badge_seen.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, ev: &OpenSession, _| {
            *opened_capture.lock().unwrap() = Some(ev.session_id);
        })
        .detach();
        cx.subscribe(&view, move |_, ev: &BadgeChanged, _| {
            *badge_capture.lock().unwrap() = ev.attention_count;
        })
        .detach();
    });

    assert!(vcx.debug_bounds("bots-item-card-item-1").is_some());
    assert!(vcx.debug_bounds("bots-item-card-item-2").is_some());

    // Filter NeedsYou -> only item-1 shown
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_item_filter(ItemFilter::NeedsYou, cx));
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-item-card-item-1").is_some());
    assert!(vcx.debug_bounds("bots-item-card-item-2").is_none());

    // Mark item-1 Done -> badge drops to 0; add comment; create item; open chat
    let bot_id = BotId::new("bot-1").unwrap();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.update_item_status(bot_id.clone(), "item-1".into(), BotItemStatus::Done, cx);
        });
    });
    vcx.run_until_parked();
    assert_eq!(*badge_seen.lock().unwrap(), 0);

    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_item_filter(ItemFilter::All, cx);
            v.snooze_item(bot_id.clone(), "item-2".into(), Some(9_999_999_999_999), cx);
        });
    });
    vcx.run_until_parked();
    assert_eq!(
        state.lock().unwrap().items[1].snoozed_until_ms,
        Some(9_999_999_999_999)
    );

    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.submit_item_comment("item-2".into(), "Looks great", None, cx);
        });
    });
    vcx.run_until_parked();
    assert!(
        state.lock().unwrap().items[1]
            .events
            .iter()
            .any(|e| e.text == "Looks great")
    );

    let chat_btn = vcx.debug_bounds("bots-open-chat").unwrap();
    vcx.simulate_click(chat_btn.center(), Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(*opened_session.lock().unwrap(), Some(SessionId(77)));
}

#[gpui::test]
fn soul_memory_guard_blocks_silent_deletion_and_allows_forget_tidy_look_around(
    cx: &mut TestAppContext,
) {
    let mut mock = MockState::default();
    mock.bots.push(sample_bot("bot-1", "Memory Bot"));
    mock.soul = "# Memory Bot\n\nReviews carefully.\n".to_owned();
    mock.memory =
        "# Memory\n\n- 2026-11-20 Always check tests\n- 2026-11-19 Prefer small PRs\n".to_owned();
    mock.suggestions.push(BotSuggestion {
        id: "sug-1".to_owned(),
        bot_id: BotId::new("bot-1").unwrap(),
        title: "Review flaky test".to_owned(),
        why: "Failed twice on CI".to_owned(),
        ask: "Want me to investigate?".to_owned(),
        link: Some("https://github.com/acme/repo/issues/9".to_owned()),
        source: Some(ExternalSource::GitHub),
        dismissed: false,
        created_at_ms: 1_700_000_000_000,
    });
    let state = Arc::new(Mutex::new(mock));
    let (view, vcx) = open_view(cx, state.clone());
    let bot_id = BotId::new("bot-1").unwrap();

    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_detail_tab(DetailTab::SoulMemory, cx));
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-tab-soul-memory-body").is_some());

    // Attempt to save MEMORY.md with a line silently deleted -> blocked by MemoryDeletionViolation!
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.save_memory(
                bot_id.clone(),
                "# Memory\n\n- 2026-11-20 Always check tests\n".to_owned(),
                cx,
            );
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("bots-memory-deletion-violation").is_some(),
        "silent deletion must display MemoryDeletionViolation banner"
    );

    // Explicit Forget removes the fact cleanly via ForgetMemoryFact
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.forget_memory_fact(bot_id.clone(), "Prefer small PRs".to_owned(), cx);
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-memory-deletion-violation").is_none());
    assert!(!state.lock().unwrap().memory.contains("Prefer small PRs"));

    // Run Memory Tidy-up & Run Look Around
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.run_memory_tidy(bot_id.clone(), cx));
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.run_look_around(bot_id.clone(), cx));
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-suggestion-0").is_some());
}

#[gpui::test]
fn connectors_slack_manifest_composio_access_and_tools_relay_and_escape(cx: &mut TestAppContext) {
    let mut mock = MockState::default();
    mock.bots.push(sample_bot("bot-1", "Ops Bot"));
    mock.statuses.push(BotConnectorStatus {
        bot_id: BotId::new("bot-1").unwrap(),
        source: ExternalSource::Slack,
        enabled: true,
        connected: true,
        state: ConnectorHealthState::Connected,
        account_label: Some("ops-ws".to_owned()),
        last_sync_ms: None,
        last_error: None,
        error_category: None,
    });
    mock.access_requests.push(BotAccessRequest {
        token: "tok-approve-me".to_owned(),
        bot_id: BotId::new("bot-1").unwrap(),
        source: ExternalSource::Telegram,
        sender_id: "u-99".to_owned(),
        sender_name: "Alice".to_owned(),
        channel_id: "chat-1".to_owned(),
        status: BotAccessRequestStatus::Pending,
        created_at: 1_700_000_000_000,
        decided_at: None,
    });
    mock.relay = BotToolRelayStatus {
        running: true,
        listen_addr: Some("127.0.0.1:49152".to_owned()),
        endpoint_url: Some("http://127.0.0.1:49152/bot-tools".to_owned()),
        connected_tools: vec![BotToolServerStatus {
            id: "sentry".to_owned(),
            name: "Sentry".to_owned(),
            endpoint_url: "https://mcp.sentry.dev/mcp".to_owned(),
            connected: true,
            tool_count: 5,
            last_error: None,
        }],
        last_error: None,
    };
    mock.candidates.push(ExternalBotCandidate {
        app: ExternalBotSourceApp::Orca,
        agent: "claude".to_owned(),
        name: "Migrated Bot".to_owned(),
        path: "/tmp/orca/bots/migrated".to_owned(),
        soul_preview: Some("Helps with triage".to_owned()),
    });
    let state = Arc::new(Mutex::new(mock));
    let (view, vcx) = open_view(cx, state.clone());

    let closed = Arc::new(Mutex::new(false));
    let closed_cap = closed.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, _: &Close, _| {
            *closed_cap.lock().unwrap() = true;
        })
        .detach();
    });

    // Tab 3: Connectors & Access
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_detail_tab(DetailTab::Connectors, cx));
    });
    vcx.run_until_parked();
    for sel in [
        "bots-conn-telegram",
        "bots-conn-slack",
        "bots-conn-discord",
        "bots-conn-google",
        "bots-conn-github",
        "bots-conn-linear",
        "bots-conn-composio",
        "bots-access-req-0",
    ] {
        assert!(vcx.debug_bounds(sel).is_some(), "missing {sel}");
    }

    // Copy Slack Socket Mode manifest YAML
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.copy_slack_manifest("Ops Bot", cx));
    });
    vcx.run_until_parked();
    let expected_yaml = generate_slack_socket_mode_manifest("Ops Bot");
    assert_eq!(
        vcx.read_from_clipboard().and_then(|c| c.text()),
        Some(expected_yaml)
    );

    // Approve access request
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.decide_access_request("tok-approve-me".to_owned(), true, cx);
        });
    });
    vcx.run_until_parked();
    assert_eq!(
        state.lock().unwrap().access_requests[0].status,
        BotAccessRequestStatus::Approved
    );

    // Tab 4: Tools & Sharing (all 13 MCP apps + ToolRelay status)
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_detail_tab(DetailTab::ToolsSharing, cx));
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-tool-relay-card").is_some());
    for sel in [
        "bots-mcp-app-vercel",
        "bots-mcp-app-sentry",
        "bots-mcp-app-datadog",
        "bots-mcp-app-posthog",
        "bots-mcp-app-cloudflare",
        "bots-mcp-app-supabase",
        "bots-mcp-app-neon",
        "bots-mcp-app-netlify",
        "bots-mcp-app-gitlab",
        "bots-mcp-app-context7",
        "bots-mcp-app-atlassian",
        "bots-mcp-app-notion",
        "bots-mcp-app-stripe",
    ] {
        assert!(vcx.debug_bounds(sel).is_some(), "missing MCP app {sel}");
    }
    assert_eq!(BOT_MCP_APPS[9].auth_kind, BotMcpAuthKind::None);

    // Import screen + External scan + Escape back to Detail + Escape emits Close
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.open_import(cx);
            v.run_external_import(true, cx);
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-import-view").is_some());

    vcx.update(|_, cx| view.update(cx, View::handle_escape));
    vcx.run_until_parked();
    assert_eq!(view.read_with(vcx, |v, _| v.screen()), Screen::Detail);
    assert!(!*closed.lock().unwrap());

    vcx.update(|_, cx| view.update(cx, View::handle_escape));
    vcx.run_until_parked();
    assert!(*closed.lock().unwrap());
}

#[gpui::test]
fn concurrency_slots_error_preservation_ime_and_dirty_guard(cx: &mut TestAppContext) {
    let mut mock = MockState::default();
    mock.bots.push(sample_bot("bot-1", "Bot One"));
    mock.bots.push(sample_bot("bot-2", "Bot Two"));
    mock.soul = "Original Soul\n".to_owned();
    mock.memory = "# Memory\n\n- Keep me\n".to_owned();
    mock.items
        .push(sample_item("bot-1", "item-1", 1, BotItemStatus::NeedsYou));
    let state = Arc::new(Mutex::new(mock));
    let (view, vcx) = open_view(cx, state.clone());
    let bot1 = BotId::new("bot-1").unwrap();
    let bot2 = BotId::new("bot-2").unwrap();

    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_detail_tab(DetailTab::SoulMemory, cx));
    });
    vcx.run_until_parked();

    // Inject GetSoul + GetMemory errors -> must keep loaded editor contents & show Retry.
    state.lock().unwrap().fail_get_soul = Some("soul down".to_owned());
    state.lock().unwrap().fail_get_memory = Some("memory down".to_owned());
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.load_selected_bot_detail(bot1.clone(), cx));
    });
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("bots-error-retry").is_some());
    vcx.update(|window, cx| {
        let v = view.read(cx);
        assert!(v.error.is_some());
        let soul_val = v
            .soul_memory
            .soul_input
            .as_ref()
            .map(|i| i.read(cx).value().to_string())
            .unwrap_or_default();
        let mem_val = v
            .soul_memory
            .memory_input
            .as_ref()
            .map(|i| i.read(cx).value().to_string())
            .unwrap_or_default();
        assert_eq!(soul_val, "Original Soul\n");
        assert!(mem_val.contains("- Keep me"));

        // Clear failure, click Retry via load, then test dirty switch guard.
        {
            let mut st = state.lock().unwrap();
            st.fail_get_soul = None;
            st.fail_get_memory = None;
        }
        view.update(cx, |v, cx| {
            assert!(!v.input_is_composing(window, cx));
            v.reload(cx);
        });
    });
    vcx.run_until_parked();
    assert!(view.read_with(vcx, |v, _| v.error.is_none()));

    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            if let Some(inp) = &v.soul_memory.soul_input {
                inp.update(cx, |s, cx| s.set_value("Unsaved edit", window, cx));
            }
            v.select_bot(bot2.clone(), cx);
            assert_eq!(v.pending_switch_bot, Some(bot2.clone()));
            assert_eq!(v.selected_bot_id, Some(bot1.clone()));
            v.confirm_select_bot(bot2.clone(), cx);
            assert_eq!(v.selected_bot_id, Some(bot2.clone()));
            assert!(v.pending_switch_bot.is_none());
            v.confirm_select_bot(bot1.clone(), cx);
        });
    });
    vcx.run_until_parked();

    // Independent task slots allow concurrent actions without stranding busy flags.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.run_memory_tidy(bot1.clone(), cx);
            v.update_item_status(bot1.clone(), "item-1".to_owned(), BotItemStatus::Done, cx);
            v.start_bot_chat(bot1.clone(), cx);
            v.toggle_pause_bot(bot1.clone(), true, cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert!(!v.soul_memory.tidy_running);
        assert!(!v.items.busy);
        assert!(!v.chats.opening);
        assert!(!v.busy);
        assert!(v.bots().iter().find(|b| b.id == bot1).unwrap().paused);
        assert_eq!(state.lock().unwrap().items[0].status, BotItemStatus::Done);
    });

    // Empty rejection + input draft preservation on CreateItem failure.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_detail_tab(DetailTab::Items, cx));
    });
    vcx.run_until_parked();
    state.lock().unwrap().fail_create_item = Some("disk full".to_owned());
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.create_bot_item(bot1.clone(), "   ", None, Some(window), cx);
            assert!(v.error.is_some());
            if let Some(inp) = &v.items.new_title_input {
                inp.update(cx, |s, cx| s.set_value("Keep draft", window, cx));
            }
            v.create_bot_item(bot1.clone(), "Keep draft", None, Some(window), cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert!(v.error.is_some());
        let preserved = v
            .items
            .new_title_input
            .as_ref()
            .map(|i| i.read(cx).value().to_string())
            .unwrap_or_default();
        assert_eq!(preserved, "Keep draft");
    });
}

#[gpui::test]
fn v2_groups_settings_memory_undo_decisions_and_connectors(cx: &mut TestAppContext) {
    let bot1 = BotId::new("bot-1").unwrap();
    let bot2 = BotId::new("bot-2").unwrap();
    let mut mock = MockState::default();
    mock.bots.push(sample_bot("bot-1", "Ops Bot"));
    mock.bots.push(sample_bot("bot-2", "Review Bot"));
    mock.soul = "# Ops\n".to_owned();
    mock.memory = "# Memory\n\n- Old fact A\n- Fact B\n".to_owned();
    mock.topics
        .insert("deploy".to_owned(), "# Deploy\nRun checks".to_owned());
    mock.suggestions.push(BotSuggestion {
        id: "sug-1".to_owned(),
        bot_id: bot1.clone(),
        title: "Clean stale PR".to_owned(),
        why: "Inactive 14 days".to_owned(),
        ask: "Close it?".to_owned(),
        link: None,
        source: Some(ExternalSource::GitHub),
        dismissed: false,
        created_at_ms: 1_700_000_000_000,
    });
    let mut decision_item = sample_item("bot-1", "item-d", 1, BotItemStatus::NeedsYou);
    decision_item
        .links
        .push(BotItemLink::Dispatch("88".to_owned()));
    decision_item.events.push(BotItemEvent {
        id: 2,
        item_uid: "item-d".to_owned(),
        kind: BotItemEventKind::Status,
        actor: BotItemActor::Bot {
            bot_id: bot1.clone(),
            session_id: None,
        },
        text: "Need decision".to_owned(),
        decisions: vec![BotItemDecision {
            question: "Ship now?".to_owned(),
            options: vec!["Go".to_owned(), "Wait".to_owned()],
            recommended: Some(0),
        }],
        created_at_ms: 1_700_000_000_100,
    });
    mock.items.push(decision_item);
    mock.sessions.push(BotSessionSummary {
        session_id: SessionId(55),
        bot_id: bot2.clone(),
        link_id: None,
        pid: 9001,
        live: true,
        working: false,
        label: "Existing Review".to_owned(),
        created_at_ms: 1_700_000_000_000,
    });

    let state = Arc::new(Mutex::new(mock));
    let (view, vcx) = open_view(cx, state.clone());
    let opened = Arc::new(Mutex::new(Vec::<SessionId>::new()));
    let opened_cap = opened.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, ev: &OpenSession, _| {
            opened_cap.lock().unwrap().push(ev.session_id);
        })
        .detach();
    });

    // Decision quick-reply + open chat guard (paused blocked, live session focuses without duplicate RPC)
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.answer_item_decision("item-d".to_owned(), None, Some(0), None, cx);
            v.start_bot_chat(bot2.clone(), cx);
            v.toggle_pause_bot(bot1.clone(), true, cx);
        });
    });
    vcx.run_until_parked();
    assert_eq!(
        state.lock().unwrap().items[0].status,
        BotItemStatus::Working
    );
    assert_eq!(*opened.lock().unwrap(), vec![SessionId(55)]);
    assert!(
        !state
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|r| matches!(r, BotRpcRequest::StartBotSession { bot_id, .. } if *bot_id == bot2))
    );
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.start_bot_chat(bot1.clone(), cx));
    });
    vcx.run_until_parked();
    assert!(view.read_with(vcx, |v, _| v.error.is_some()));

    // Settings tab: edit identity + schedule + save
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_detail_tab(DetailTab::Settings, cx));
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-tab-settings-body").is_some());
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            if let Some(inp) = &v.settings.name_input {
                inp.update(cx, |s, cx| s.set_value("Ops Bot v2", window, cx));
            }
            v.settings.effort = "high".to_owned();
            v.settings.permission_mode = Permission::Yolo;
            if let Some(sn) = &v.settings.sched_name_input {
                sn.update(cx, |s, cx| s.set_value("Daily triage", window, cx));
            }
            if let Some(sc) = &v.settings.sched_cron_input {
                sc.update(cx, |s, cx| s.set_value("0 9 * * *", window, cx));
            }
            if let Some(sp) = &v.settings.sched_prompt_input {
                sp.update(cx, |s, cx| s.set_value("Check inbox", window, cx));
            }
            v.add_schedule(bot1.clone(), cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.save_bot_settings(bot1.clone(), cx));
    });
    vcx.run_until_parked();
    {
        let st = state.lock().unwrap();
        assert_eq!(st.bots[0].name, "Ops Bot v2");
        assert_eq!(st.bots[0].permission_mode, Permission::Yolo);
        assert_eq!(st.bots[0].schedules.len(), 1);
    }

    // Soul & Memory: fact edit -> undo -> topic load/save -> dismiss suggestion
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_detail_tab(DetailTab::SoulMemory, cx);
            v.edit_memory_fact(bot1.clone(), "- Old fact A".to_owned(), "- New fact A", cx);
        });
    });
    vcx.run_until_parked();
    assert!(state.lock().unwrap().memory.contains("- New fact A"));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.undo_memory_change(bot1.clone(), "hist-1".to_owned(), cx);
        });
    });
    vcx.run_until_parked();
    assert!(state.lock().unwrap().memory.contains("- Old fact A"));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.select_memory_topic(bot1.clone(), "deploy", cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            assert_eq!(v.soul_memory.selected_topic.as_deref(), Some("deploy"));
            v.save_memory_topic(bot1.clone(), "ops", "# Ops\n".to_owned(), cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.dismiss_suggestion(bot1.clone(), "sug-1".to_owned(), cx);
        });
    });
    vcx.run_until_parked();
    assert!(state.lock().unwrap().topics.contains_key("ops"));
    assert!(state.lock().unwrap().suggestions[0].dismissed);

    // Groups screen: create group -> thread -> @mention message -> stop
    vcx.update(|_, cx| {
        view.update(cx, View::open_groups);
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-groups-view").is_some());
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            if let Some(inp) = &v.groups.name_input {
                inp.update(cx, |s, cx| s.set_value("Triage Room", window, cx));
            }
            v.groups.member_bot_ids = vec![bot1.clone(), bot2.clone()];
            v.submit_create_or_update_group(cx);
        });
    });
    vcx.run_until_parked();
    let gid = view.read_with(vcx, |v, _| v.groups.groups[0].id.clone());
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            if let Some(t) = &v.groups.thread_title_input {
                t.update(cx, |s, cx| s.set_value("Incident", window, cx));
            }
            v.create_group_thread(gid.clone(), cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            if let Some(c) = &v.groups.composer_input {
                c.update(cx, |s, cx| s.set_value("@bot-1 check", window, cx));
            }
            v.send_group_message(gid.clone(), cx);
        });
    });
    vcx.run_until_parked();
    assert!(
        state
            .lock()
            .unwrap()
            .group_messages
            .iter()
            .any(|m| m.body == "@bot-1 check")
    );

    // Connectors rollback + reload error propagation + pairing + tracker + MCP OAuth
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.screen = Screen::Detail;
            v.set_detail_tab(DetailTab::Connectors, cx);
        });
    });
    vcx.run_until_parked();
    state.lock().unwrap().fail_reload_connectors = Some("relay timeout".to_owned());
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.reload_connectors(bot1.clone(), cx));
    });
    vcx.run_until_parked();
    assert!(view.read_with(vcx, |v, _| {
        v.error
            .as_ref()
            .is_some_and(|e| e.contains("relay timeout"))
    }));

    state.lock().unwrap().fail_reload_connectors = None;
    state.lock().unwrap().fail_set_connector_config = Some("db read-only".to_owned());
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            let conn = v.selected_bot().unwrap().connectors.clone();
            v.save_connector_secret(bot1.clone(), "tg:tok".to_owned(), "sec-123", conn, None, cx);
        });
    });
    vcx.run_until_parked();
    assert!(
        state.lock().unwrap().secrets.is_empty(),
        "SetConnectorConfig failure must roll back SetSecret with DeleteSecret"
    );

    state.lock().unwrap().fail_set_connector_config = None;
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.test_connector(bot1.clone(), ExternalSource::Telegram, cx);
        });
    });
    vcx.run_until_parked();
    assert!(view.read_with(vcx, |v, _| v.notice.is_some()));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.redeem_pairing_code("PAIR-42", None, cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_default_tracker_bot(ExternalSource::GitHub, Some(bot1.clone()), cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_detail_tab(DetailTab::ToolsSharing, cx);
            v.start_mcp_oauth(bot1.clone(), "vercel".to_owned(), cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert_eq!(v.connectors.default_tracker_bots.github, Some(bot1.clone()));
        assert_eq!(
            v.tools
                .mcp_oauth_states
                .get(&(bot1, "vercel".to_owned()))
                .map(|s| s.status.as_str()),
            Some("connected")
        );
    });
}

#[test]
fn sidebar_snapshot_counts_attention_per_bot_and_offers_models_per_runtime() {
    let bots = vec![sample_bot("bot-a", "A"), sample_bot("bot-b", "B")];
    let items = vec![
        sample_item("bot-a", "i1", 1, BotItemStatus::NeedsYou),
        sample_item("bot-a", "i2", 2, BotItemStatus::Working),
        sample_item("bot-gone", "i3", 3, BotItemStatus::NeedsYou),
    ];
    let snap = sidebar_snapshot(&bots, &items, &[], &[]);
    let names: Vec<_> = snap
        .bots
        .iter()
        .map(|b| (b.name.as_str(), b.attention))
        .collect();
    assert_eq!(names, vec![("A", 1), ("B", 0)]);
    assert_eq!(
        snap.attention, 2,
        "the badge total keeps items of bots not listed"
    );

    for rt in BOT_RUNTIMES {
        let models = runtime_models(rt.id);
        assert!(
            models
                .iter()
                .all(|m| !m.id.is_empty() && m.id != "default" && !m.label.is_empty())
        );
    }
    // The composer's list: "Default" is the picker's own row, Fable is 5.2.
    let claude = runtime_models("claude");
    assert!(claude.iter().any(|m| m.id == "opus"));
    assert!(
        claude
            .iter()
            .any(|m| m.id == "fable" && m.label == "Fable 5.2")
    );
    // agy ids change per account and release; only `agy models` names them.
    assert!(runtime_models("agy").is_empty());
    assert!(runtime_models("pi").is_empty());
}

#[gpui::test]
fn new_bot_dialog_saves_the_mascot_and_clears_unchecked_template_skills(cx: &mut TestAppContext) {
    let state = Arc::new(Mutex::new(MockState::default()));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state.clone());
    let tpl = krill_proto::bots::find_bot_template("pr-review").unwrap();
    assert!(!tpl.skills.is_empty());
    let (dialog, vcx) = cx.add_window_view(|window, cx| {
        super::new_bot::NewBot::new(
            rpc,
            "My Mac".into(),
            true,
            Vec::new(),
            Some(tpl),
            window,
            cx,
        )
    });
    vcx.update(|_, cx| {
        dialog.update(cx, |d, cx| {
            for skill in tpl.skills {
                d.toggle_skill(skill, cx);
            }
            d.submit(cx);
        });
    });
    vcx.run_until_parked();
    let reqs = state.lock().unwrap().requests.clone();
    assert!(reqs.iter().any(|r| matches!(
        r,
        BotRpcRequest::CreateBotV2(p)
            if p.base.template_id.as_deref() == Some("pr-review")
                && p.base.avatar.as_deref() == Some(tpl.mascot)
                && p.base.name == tpl.name
    )));
    // krilld would otherwise fill the template's skills back in.
    assert!(reqs.iter().any(|r| matches!(
        r,
        BotRpcRequest::UpdateBotExtras(p) if p.skills.as_deref() == Some(&[][..])
    )));
}

#[gpui::test]
fn new_bot_dialog_lists_the_agents_own_models_and_launches_the_pick(cx: &mut TestAppContext) {
    use krill_proto::bots::{BotAgentModel, BotAgentModels};
    let model = |id: &str, label: &str| BotAgentModel {
        id: id.to_owned(),
        label: label.to_owned(),
    };
    let mut mock = MockState::default();
    mock.agent_models.insert(
        "agy".to_owned(),
        BotAgentModels {
            models: vec![
                model("gemini-4-neon-low", "Gemini 4 Neon (Low)"),
                model("gemini-4-neon-high", "Gemini 4 Neon (High)"),
            ],
            reason: None,
        },
    );
    // Claude Code reports only configured models; its native aliases stay listed.
    mock.agent_models.insert(
        "claude".to_owned(),
        BotAgentModels {
            models: vec![model("opus[1m]", "opus[1m]")],
            reason: None,
        },
    );
    mock.agent_models.insert(
        "grok".to_owned(),
        BotAgentModels {
            models: Vec::new(),
            reason: Some("grok was not found on this computer.".to_owned()),
        },
    );
    let state = Arc::new(Mutex::new(mock));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state.clone());
    let (dialog, vcx) = cx.add_window_view(|window, cx| {
        super::new_bot::NewBot::new(rpc, "My Mac".into(), false, Vec::new(), None, window, cx)
    });
    let ids = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|_, cx| {
            dialog
                .read(cx)
                .model_choices()
                .into_iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
        })
    };

    vcx.update(|_, cx| dialog.update(cx, |d, cx| d.set_runtime("agy", cx)));
    // Cold: nothing guessed for agy, and the picker says it is loading.
    assert!(ids(vcx).is_empty());
    assert!(vcx.update(|_, cx| dialog.read(cx).model_notice().is_some()));
    vcx.run_until_parked();
    assert_eq!(ids(vcx), ["gemini-4-neon-low", "gemini-4-neon-high"]);
    assert_eq!(vcx.update(|_, cx| dialog.read(cx).model_notice()), None);

    vcx.update(|_, cx| dialog.update(cx, |d, cx| d.set_runtime("claude", cx)));
    vcx.run_until_parked();
    let claude = ids(vcx);
    assert!(claude.contains(&"fable".to_owned()) && !claude.contains(&"opus[1m]".to_owned()));

    // A daemon that predates the request keeps the built-in list, without a notice.
    vcx.update(|_, cx| dialog.update(cx, |d, cx| d.set_runtime("codex", cx)));
    vcx.run_until_parked();
    assert!(ids(vcx).contains(&"gpt-5.4".to_owned()));
    assert_eq!(vcx.update(|_, cx| dialog.read(cx).model_notice()), None);

    vcx.update(|_, cx| dialog.update(cx, |d, cx| d.set_runtime("grok", cx)));
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, cx| dialog.read(cx).model_notice())
            .as_deref(),
        Some("grok was not found on this computer.")
    );

    vcx.update(|_, cx| {
        dialog.update(cx, |d, cx| {
            d.set_runtime("agy", cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        dialog.update(cx, |d, cx| {
            d.set_model(Some("gemini-4-neon-high".to_owned()), cx);
            d.submit(cx);
        });
    });
    vcx.run_until_parked();
    let reqs = state.lock().unwrap().requests.clone();
    assert!(reqs.iter().any(|r| matches!(
        r,
        BotRpcRequest::CreateBotV2(p)
            if p.base.agent_id == "agy" && p.base.model.as_deref() == Some("gemini-4-neon-high")
    )));
}

#[gpui::test]
fn saving_bot_settings_leaves_an_orca_background_alone(cx: &mut TestAppContext) {
    let mut mock = MockState::default();
    let mut bot = sample_bot("bot-1", "Lavender bot");
    bot.avatar = Some("B1".to_owned());
    bot.avatar_color = Some("#c9b8e8".to_owned());
    mock.bots.push(bot);
    let state = Arc::new(Mutex::new(mock));
    let (view, vcx) = open_view(cx, state.clone());
    let bot_id = BotId::new("bot-1").unwrap();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.select_bot(bot_id.clone(), cx);
            v.set_detail_tab(DetailTab::Settings, cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| view.update(cx, |v, cx| v.save_bot_settings(bot_id.clone(), cx)));
    vcx.run_until_parked();
    let st = state.lock().unwrap();
    let extras = st
        .requests
        .iter()
        .find_map(|r| match r {
            BotRpcRequest::UpdateBotExtras(p) => Some(p.clone()),
            _ => None,
        })
        .expect("settings save sends extras");
    assert_eq!(
        extras.avatar_color,
        krill_proto::bots::FieldPatch::Unchanged
    );
    assert_eq!(st.bots[0].avatar_color.as_deref(), Some("#c9b8e8"));
}

#[test]
fn home_groups_items_as_orca_does() {
    use super::home::{Group, group_of, grouped, short_age, snooze_presets};
    let now = 1_795_341_600_000; // Sun 2026-11-22 10:00 UTC
    let at = |uid: &str, n: u64, status: BotItemStatus, ago_min: u64| {
        let mut item = sample_item("bot-1", uid, n, status);
        item.last_activity_at_ms = now - ago_min * 60_000;
        item
    };
    let needs = at("a", 1, BotItemStatus::NeedsYou, 5);
    let working = at("b", 2, BotItemStatus::Working, 10);
    let mut snoozed = at("c", 3, BotItemStatus::Planned, 1);
    snoozed.snoozed_until_ms = Some(now + 60_000);
    let stale = at("d", 4, BotItemStatus::NeedsYou, 6 * 24 * 60);
    let done = at("e", 5, BotItemStatus::Done, 1);
    let newer_done = at("f", 6, BotItemStatus::Canceled, 0);
    assert_eq!(group_of(&needs, false, now), Group::NeedsYou);
    assert_eq!(group_of(&snoozed, false, now), Group::Snoozed);
    assert_eq!(group_of(&stale, false, now), Group::Stale);
    // A live worker keeps an old In progress item out of Stale.
    let old_working = at("g", 7, BotItemStatus::Working, 6 * 24 * 60);
    assert_eq!(group_of(&old_working, false, now), Group::Stale);
    assert_eq!(group_of(&old_working, true, now), Group::Working);
    let items = [needs, working, snoozed, stale, done, newer_done];
    let groups = grouped(&items, |_| false, now);
    let uids =
        |g: Group| -> Vec<&str> { groups[g as usize].iter().map(|i| i.uid.as_str()).collect() };
    assert_eq!(uids(Group::NeedsYou), ["a"]);
    assert_eq!(uids(Group::Working), ["b"]);
    assert_eq!(uids(Group::Closed), ["f", "e"], "latest change first");

    assert_eq!(short_age(now - 30_000, now), "now");
    assert_eq!(short_age(now - 6 * 60_000, now), "6m");
    assert_eq!(short_age(now - 22 * 3_600_000, now), "22h");
    assert_eq!(short_age(now - 50 * 3_600_000, now), "2d");

    // Sunday 10:00: later is 6 PM, then Monday 9 AM twice.
    let at_ms = |p: &[(String, u64); 3]| p.iter().map(|(_, at)| *at).collect::<Vec<_>>();
    assert_eq!(
        at_ms(&snooze_presets(now, 0)),
        [1_795_370_400_000, 1_795_424_400_000, 1_795_424_400_000]
    );
    // Monday 18:30: three hours out (rounded up to the hour), Tuesday, next Monday.
    assert_eq!(
        at_ms(&snooze_presets(1_795_458_600_000, 0)),
        [1_795_471_200_000, 1_795_510_800_000, 1_796_029_200_000]
    );
}

#[gpui::test]
fn home_panel_groups_filters_and_opens_an_item(cx: &mut TestAppContext) {
    let now = super::now_unix_ms();
    let mut mock = MockState::default();
    mock.bots.push(sample_bot("bot-1", "PR Bot"));
    mock.bots.push(sample_bot("bot-2", "Quiet Bot"));
    let mut add = |uid: &str, n: u64, status: BotItemStatus| {
        let mut item = sample_item("bot-1", uid, n, status);
        item.last_activity_at_ms = now;
        mock.items.push(item);
    };
    add("ask", 3, BotItemStatus::NeedsYou);
    add("run", 7, BotItemStatus::Working);
    add("plan", 8, BotItemStatus::Planned);
    add("shipped", 5, BotItemStatus::Done);
    let ask = mock.items.iter_mut().find(|i| i.uid == "ask").unwrap();
    ask.events.push(BotItemEvent {
        id: 2,
        item_uid: "ask".to_owned(),
        kind: BotItemEventKind::Status,
        actor: BotItemActor::Bot {
            bot_id: BotId::new("bot-1").unwrap(),
            session_id: None,
        },
        text: "status -> needs_you".to_owned(),
        decisions: vec![BotItemDecision {
            question: "Ship it?".to_owned(),
            options: vec!["Yes".to_owned(), "No".to_owned()],
            recommended: Some(0),
        }],
        created_at_ms: now,
    });
    let state = Arc::new(Mutex::new(mock));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state.clone());
    let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
    vcx.simulate_resize(gpui::size(px(340.), px(900.)));
    vcx.run_until_parked();
    let show = |vcx: &mut VisualTestContext, id: &str| {
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_in_panel(&BotId::new(id).unwrap(), DetailTab::Items, cx);
            });
        });
        vcx.run_until_parked();
    };
    show(vcx, "bot-1");
    let click = |vcx: &mut VisualTestContext, sel: &'static str| {
        let at = vcx.debug_bounds(sel).unwrap_or_else(|| panic!("{sel}"));
        vcx.simulate_click(at.center(), Modifiers::default());
        vcx.run_until_parked();
    };

    // Needs you and In progress open; Planned and Closed folded to their headers.
    let ask_row = vcx
        .debug_bounds("bot-home-item-ask")
        .expect("needs you row");
    let run_row = vcx
        .debug_bounds("bot-home-item-run")
        .expect("in progress row");
    assert!(ask_row.top() < run_row.top());
    assert!(vcx.debug_bounds("bot-home-group-planned").is_some());
    assert!(vcx.debug_bounds("bot-home-item-plan").is_none());
    assert!(vcx.debug_bounds("bot-home-item-shipped").is_none());
    assert!(vcx.debug_bounds("bot-home-automation-tidy").is_some());
    assert!(vcx.debug_bounds("bot-home-automation-look").is_some());
    assert!(vcx.debug_bounds("bot-home-automations-empty").is_some());
    // No always-open add form, no status chips.
    assert!(vcx.debug_bounds("bots-item-add-btn").is_none());
    assert!(vcx.debug_bounds("bots-items-filters").is_none());

    click(vcx, "bot-home-group-closed");
    assert!(vcx.debug_bounds("bot-home-item-shipped").is_some());

    // Needs you only.
    vcx.update(|_, cx| {
        let f = view
            .read(cx)
            .home_filter()
            .expect("filter shows with items");
        assert_eq!(f.needs_you, 1);
        view.update(cx, |v, cx| v.set_needs_you_only(true, cx));
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bot-home-item-ask").is_some());
    assert!(vcx.debug_bounds("bot-home-item-run").is_none());
    click(vcx, "bot-home-filter-chip-clear");
    assert!(vcx.debug_bounds("bot-home-item-run").is_some());

    // A row opens its item in the list's place; a quick reply answers its question.
    click(vcx, "bot-home-item-ask");
    assert!(vcx.debug_bounds("bot-item-pane").is_some());
    assert!(vcx.debug_bounds("bot-home-items").is_none());
    assert!(vcx.debug_bounds("bot-item-done-means").is_some());
    assert!(vcx.debug_bounds("bot-item-comment-box").is_some());
    vcx.update(|_, cx| assert!(view.read(cx).home_item_open()));
    click(vcx, "bot-item-reply-2-0-0");
    assert!(state.lock().unwrap().requests.iter().any(|r| matches!(
        r,
        BotRpcRequest::AnswerBotItem {
            item_id,
            option_index: Some(0),
            ..
        } if item_id == "ask"
    )));
    click(vcx, "bot-item-back");
    assert!(vcx.debug_bounds("bot-home-items").is_some());

    // A bot with no items: Orca's empty state.
    show(vcx, "bot-2");
    assert!(vcx.debug_bounds("bot-home-empty").is_some());
    vcx.update(|_, cx| assert!(view.read(cx).home_filter().is_none()));
}
