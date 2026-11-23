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
    imported_sources: Vec<(String, BotId)>,
    suggestions: Vec<BotSuggestion>,
    oauth_states: BTreeMap<(BotId, String), BotMcpOAuthState>,
    composio_connections: Vec<ComposioConnectionInfo>,
    default_trackers: DefaultTrackerBots,
    extras: krill_proto::bots::BotSettingsExtras,
    fail_get_soul: Option<String>,
    fail_get_memory: Option<String>,
    fail_create_item: Option<String>,
    fail_reload_connectors: Option<String>,
    fail_set_connector_config: Option<String>,
    /// `ListAgentModels` replies by agent; a missing agent answers like a daemon that predates it.
    agent_models: BTreeMap<String, krill_proto::bots::BotAgentModels>,
    upcoming: Option<krill_proto::bots::BotUpcoming>,
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

fn mock_create_bot_v2(st: &mut MockState, v2: &krill_proto::bots::CreateBotV2Params) -> BotConfig {
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
    cfg
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
                let cfg = mock_create_bot_v2(&mut st, v2);
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
            BotRpcRequest::GetBotSettingsExtras { .. } => {
                Ok(BotRpcResponse::BotSettingsExtras(st.extras))
            }
            BotRpcRequest::UpdateBotSettingsExtras {
                items_stale,
                look_around_every,
                tidy_every,
                ..
            } => {
                if let Some(v) = items_stale {
                    st.extras.items_stale = *v;
                }
                if let Some(v) = look_around_every {
                    st.extras.look_around_every = *v;
                }
                if let Some(v) = tidy_every {
                    st.extras.tidy_every = *v;
                }
                Ok(BotRpcResponse::BotSettingsExtras(st.extras))
            }
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
            BotRpcRequest::ListImportedSources => {
                Ok(BotRpcResponse::ImportedSources(st.imported_sources.clone()))
            }
            BotRpcRequest::ImportExternalBotCopy { path, params, .. } => {
                let cfg = mock_create_bot_v2(&mut st, params);
                if !st.imported_sources.iter().any(|(p, _)| p == path) {
                    st.imported_sources.push((path.clone(), cfg.id.clone()));
                }
                Ok(BotRpcResponse::Bot(Box::new(cfg)))
            }
            BotRpcRequest::ListAgentModels { agent_id } => Ok(st
                .agent_models
                .get(agent_id)
                .map_or(BotRpcResponse::Ok, |m| {
                    BotRpcResponse::AgentModels(m.clone())
                })),
            BotRpcRequest::GetBotUpcoming { .. } => Ok(st
                .upcoming
                .clone()
                .map_or(BotRpcResponse::Ok, BotRpcResponse::Upcoming)),
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

    // Import modal + Escape closes the dialog directly when no Pick step is waiting behind Export.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.open_import(cx);
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bots-import-view").is_some());
    assert!(vcx.debug_bounds("bot-import-hero").is_some());

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

    // Group items panel: merges member bot items and switches chat/bot filters
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.show_group_in_panel("grp-triage", &[bot1.clone(), bot2.clone()], &[], cx);
            v.set_group_chat_scope(super::GroupChatScope::AllTheirWork, cx);
        });
    });
    vcx.run_until_parked();
    assert!(view.read_with(vcx, |v, _| {
        v.group_panel_active()
            && v.group_items_filter().chat_scope == super::GroupChatScope::AllTheirWork
    }));

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
            vec![super::new_bot::HostChoice {
                id: crate::hosts::HostId::Local,
                label: "My Mac".into(),
                rpc,
            }],
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
        // A remote host offers every runtime, installed here or not.
        super::new_bot::NewBot::new(
            vec![super::new_bot::HostChoice {
                id: crate::hosts::HostId::Remote("iroh:studio".into()),
                label: "Studio".into(),
                rpc,
            }],
            Vec::new(),
            None,
            window,
            cx,
        )
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
    vcx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.set_name_for_test("Agy Bot", window, cx);
            d.set_model(Some("gemini-4-neon-high".to_owned()), cx);
            d.set_effort(Some("high".to_owned()), cx);
            d.set_permission(Some(Permission::Yolo), cx);
            d.submit(cx);
        });
    });
    vcx.run_until_parked();
    let reqs = state.lock().unwrap().requests.clone();
    assert!(reqs.iter().any(|r| matches!(
        r,
        BotRpcRequest::CreateBotV2(p)
            if p.base.agent_id == "agy"
                && p.base.model.as_deref() == Some("gemini-4-neon-high")
                && p.base.effort.as_deref() == Some("high")
                && p.base.permission_mode == Some(Permission::Yolo)
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
    let sent_extras = st
        .requests
        .iter()
        .any(|r| matches!(r, BotRpcRequest::UpdateBotExtras(_)));
    assert!(
        !sent_extras,
        "unchanged avatar_color should skip redundant UpdateBotExtras RPC"
    );
    assert_eq!(st.bots[0].avatar_color.as_deref(), Some("#c9b8e8"));
}

#[test]
fn home_groups_items_as_orca_does() {
    use super::home::{Group, group_of, grouped, short_age, snooze_presets};
    use crate::i18n::catalog::bots as text;
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
    assert_eq!(group_of(&needs, false, Some(5), now), Group::NeedsYou);
    assert_eq!(group_of(&snoozed, false, Some(5), now), Group::Snoozed);
    assert_eq!(group_of(&stale, false, Some(5), now), Group::Stale);
    // A live worker keeps an old In progress item out of Stale.
    let old_working = at("g", 7, BotItemStatus::Working, 6 * 24 * 60);
    assert_eq!(group_of(&old_working, false, Some(5), now), Group::Stale);
    assert_eq!(group_of(&old_working, true, Some(5), now), Group::Working);
    // Settings › Items: a longer wait keeps it out of Stale, and Never keeps everything out.
    assert_eq!(group_of(&stale, false, Some(7), now), Group::NeedsYou);
    assert_eq!(group_of(&stale, false, None, now), Group::NeedsYou);
    let items = [needs, working, snoozed, stale, done, newer_done];
    let groups = grouped(&items, |_| false, Some(5), now);
    let uids =
        |g: Group| -> Vec<&str> { groups[g as usize].iter().map(|i| i.uid.as_str()).collect() };
    assert_eq!(uids(Group::NeedsYou), ["a"]);
    assert_eq!(uids(Group::Working), ["b"]);
    assert_eq!(uids(Group::Closed), ["f", "e"], "latest change first");

    assert_eq!(short_age(now - 30_000, now), text::SHORT_AGE_NOW.get());
    assert_eq!(
        short_age(now - 6 * 60_000, now),
        text::SHORT_AGE_MIN.format(&[("n", "6")])
    );
    assert_eq!(
        short_age(now - 22 * 3_600_000, now),
        text::SHORT_AGE_HOUR.format(&[("n", "22")])
    );
    assert_eq!(
        short_age(now - 50 * 3_600_000, now),
        text::SHORT_AGE_DAY.format(&[("n", "2")])
    );

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
    assert!(vcx.debug_bounds("bot-home-automation-tidy-up").is_some());
    assert!(
        vcx.debug_bounds("bot-home-automation-look-around")
            .is_some()
    );
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

#[test]
fn unsupported_daemon_errors_read_back_and_load_errors_show_once() {
    use super::rpc::{join_errors, unsupported_error, unsupported_version};
    assert_eq!(
        unsupported_version(&unsupported_error("0.2.10+dev.00ab")),
        Some("0.2.10")
    );
    // Builds before the version rode along said only this.
    assert_eq!(
        unsupported_version("krilld does not support FEAT_BOTS"),
        Some("")
    );
    assert_eq!(unsupported_version("connection refused"), None);
    let errors = ["a", "b", "a", "a", "b"].map(str::to_owned);
    assert_eq!(join_errors(&errors), "a | b");
}

/// A daemon too old for bots: every request fails the way `call_daemon_bot` does.
fn old_daemon_rpc(calls: Arc<Mutex<usize>>) -> BotRpcCall {
    Arc::new(move |_| {
        *calls.lock().unwrap() += 1;
        Err(super::rpc::unsupported_error("0.2.10+dev.00ab"))
    })
}

#[gpui::test]
fn a_host_without_bots_is_explained_instead_of_erroring(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let calls = Arc::new(Mutex::new(0));
    let rpc = old_daemon_rpc(calls.clone());
    let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx));
    vcx.simulate_resize(gpui::size(px(1040.), px(720.)));
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert_eq!(v.error, None, "an old daemon is not an error");
        assert_eq!(v.unsupported.as_deref(), Some("0.2.10"));
    });
    assert!(vcx.debug_bounds("bots-unsupported").is_some());
    assert!(vcx.debug_bounds("bots-empty").is_none());
    assert!(vcx.debug_bounds("bots-error-retry").is_none());

    // Opening a bot from the sidebar must not fire a dozen doomed detail calls.
    let before = *calls.lock().unwrap();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.select_bot(BotId::new("bot-1").unwrap(), cx));
    });
    vcx.run_until_parked();
    assert_eq!(*calls.lock().unwrap(), before);
    vcx.update(|_, cx| assert_eq!(view.read(cx).error, None));
}

#[gpui::test]
fn a_failed_bot_detail_shows_each_error_once_with_the_host_name(cx: &mut TestAppContext) {
    let state = Arc::new(Mutex::new(MockState::default()));
    state
        .lock()
        .unwrap()
        .bots
        .push(sample_bot("bot-1", "Helper"));
    let (view, vcx) = open_view(cx, state.clone());
    {
        let mut st = state.lock().unwrap();
        st.fail_get_soul = Some("connection reset".to_owned());
        st.fail_get_memory = Some("connection reset".to_owned());
    }
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_host_target(
                crate::hosts::HostId::Local,
                None,
                vec![
                    (crate::hosts::HostId::Local, "My Mac".to_owned(), None),
                    (
                        crate::hosts::HostId::Remote("iroh:studio".into()),
                        "Studio".to_owned(),
                        None,
                    ),
                ],
                cx,
            );
            v.select_bot(BotId::new("bot-1").unwrap(), cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        assert_eq!(
            view.read(cx).error.as_deref(),
            Some("My Mac: connection reset")
        );
    });
}

#[gpui::test]
fn new_bot_makes_the_bot_on_the_picked_computer_and_greys_out_old_ones(cx: &mut TestAppContext) {
    use super::new_bot::{Event, HostChoice, NewBot};
    use crate::hosts::HostId;
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let here = Arc::new(Mutex::new(MockState::default()));
    let studio = Arc::new(Mutex::new(MockState::default()));
    let old_calls = Arc::new(Mutex::new(0));
    let studio_id = HostId::Remote("iroh:studio".into());
    let old_id = HostId::Remote("iroh:old".into());
    let hosts = vec![
        HostChoice {
            id: HostId::Local,
            label: "My Mac".into(),
            rpc: make_mock_rpc(here.clone()),
        },
        HostChoice {
            id: old_id.clone(),
            label: "Old".into(),
            rpc: old_daemon_rpc(old_calls),
        },
        HostChoice {
            id: studio_id.clone(),
            label: "Studio".into(),
            rpc: make_mock_rpc(studio.clone()),
        },
    ];
    let (dialog, vcx) =
        cx.add_window_view(|window, cx| NewBot::new(hosts, Vec::new(), None, window, cx));
    let created: Arc<Mutex<Vec<HostId>>> = Arc::default();
    let seen = created.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&dialog, move |_, ev: &Event, _| {
            if let Event::Created { host, .. } = ev {
                seen.lock().unwrap().push(host.clone());
            }
        })
        .detach();
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            // Krill on "Old" predates bots: it can't be picked.
            d.select_host(&old_id, cx);
            assert_eq!(d.host(), &HostId::Local);
            d.select_host(&studio_id, cx);
            assert_eq!(d.host(), &studio_id);
            d.set_name_for_test("Studio Bot", window, cx);
            d.submit(cx);
        });
    });
    vcx.run_until_parked();
    let made_on_studio = studio
        .lock()
        .unwrap()
        .requests
        .iter()
        .any(|r| matches!(r, BotRpcRequest::CreateBotV2(_)));
    let made_here = here
        .lock()
        .unwrap()
        .requests
        .iter()
        .any(|r| matches!(r, BotRpcRequest::CreateBotV2(_)));
    assert!(made_on_studio, "the bot is made on the picked computer");
    assert!(!made_here);
    assert_eq!(*created.lock().unwrap(), vec![studio_id]);
}

#[gpui::test]
fn new_bot_moves_off_a_default_computer_that_cannot_run_bots(cx: &mut TestAppContext) {
    use super::new_bot::{HostChoice, NewBot};
    use crate::hosts::HostId;
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let old_id = HostId::Remote("iroh:old".into());
    let hosts = vec![
        HostChoice {
            id: old_id.clone(),
            label: "Old".into(),
            rpc: old_daemon_rpc(Arc::default()),
        },
        HostChoice {
            id: HostId::Local,
            label: "My Mac".into(),
            rpc: make_mock_rpc(Arc::default()),
        },
    ];
    let (dialog, vcx) =
        cx.add_window_view(|window, cx| NewBot::new(hosts, Vec::new(), None, window, cx));
    vcx.run_until_parked();
    vcx.update(|_, cx| assert_eq!(dialog.read(cx).host(), &HostId::Local));
}

#[gpui::test]
fn home_panel_selects_rows_steps_items_and_lists_asks(cx: &mut TestAppContext) {
    let now = super::now_unix_ms();
    let mut mock = MockState::default();
    let mut bot = sample_bot("bot-1", "PR Bot");
    bot.schedules.push(krill_proto::bots::BotScheduleConfig {
        id: "s1".to_owned(),
        name: "Mentions triage".to_owned(),
        cron: "0 9 * * *".to_owned(),
        prompt: "triage".to_owned(),
        enabled: true,
        timezone: None,
    });
    mock.bots.push(bot);
    for (uid, n, status) in [
        ("ask", 3, BotItemStatus::NeedsYou),
        ("run", 7, BotItemStatus::Working),
        ("run2", 9, BotItemStatus::Working),
    ] {
        let mut item = sample_item("bot-1", uid, n, status);
        item.last_activity_at_ms = now;
        item.links.clear();
        mock.items.push(item);
    }
    // A bot's comment after the owner last looked: the row is unseen.
    let run = mock.items.iter_mut().find(|i| i.uid == "run").unwrap();
    run.events.push(BotItemEvent {
        id: 9,
        item_uid: "run".to_owned(),
        kind: BotItemEventKind::Commented,
        actor: BotItemActor::Bot {
            bot_id: BotId::new("bot-1").unwrap(),
            session_id: Some(SessionId(41)),
        },
        text: "Working on it".to_owned(),
        decisions: vec![],
        created_at_ms: now,
    });
    mock.sessions.push(BotSessionSummary {
        session_id: SessionId(41),
        bot_id: BotId::new("bot-1").unwrap(),
        link_id: None,
        pid: 1,
        live: true,
        working: false,
        label: String::new(),
        created_at_ms: now,
    });
    mock.upcoming = Some(krill_proto::bots::BotUpcoming {
        look_around_next_ms: Some(now + 3_600_000),
        schedules: vec![krill_proto::bots::BotScheduleUpcoming {
            id: "s1".to_owned(),
            running: true,
            ..Default::default()
        }],
        items_seen_event_id: 5,
        ..Default::default()
    });
    let state = Arc::new(Mutex::new(mock));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state.clone());
    let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
    vcx.simulate_resize(gpui::size(px(360.), px(900.)));
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.show_in_panel(&BotId::new("bot-1").unwrap(), DetailTab::Items, cx);
        });
    });
    vcx.run_until_parked();
    let click = |vcx: &mut VisualTestContext, sel: &'static str| {
        let at = vcx.debug_bounds(sel).unwrap_or_else(|| panic!("{sel}"));
        vcx.simulate_click(at.center(), Modifiers::default());
        vcx.run_until_parked();
    };

    // The header says the running automation; the row spins.
    vcx.update(|_, cx| {
        let (line, _, running) = view.read(cx).home_quiet_line().unwrap();
        assert!(running);
        assert!(line.contains("Mentions triage"), "{line}");
    });
    assert!(
        vcx.debug_bounds("bot-home-automation-automation-s1")
            .is_some()
    );
    assert!(vcx.debug_bounds("bot-home-automations-new").is_some());
    // The row the bot wrote on since the owner looked keeps its dot.
    assert!(vcx.debug_bounds("bot-home-item-unseen").is_some());
    assert!(
        state
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|r| matches!(r, BotRpcRequest::MarkBotItemsOpened { .. }))
    );

    // Checking two rows shows the pill; Move to Done writes both.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.items.selected.insert("run".to_owned());
            v.items.selected.insert("run2".to_owned());
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bot-home-selection-pill").is_some());
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_items_status(
                BotId::new("bot-1").unwrap(),
                &["run".to_owned(), "run2".to_owned()],
                BotItemStatus::Done,
                cx,
            );
        });
    });
    vcx.run_until_parked();
    let done = state
        .lock()
        .unwrap()
        .items
        .iter()
        .filter(|i| i.status == BotItemStatus::Done)
        .count();
    assert_eq!(done, 2);
    assert!(vcx.debug_bounds("bot-home-selection-pill").is_none());

    // Pick a time: the dialog's custom grid snoozes the item.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.open_snooze_pick(BotId::new("bot-1").unwrap(), vec!["ask".to_owned()], cx);
        });
    });
    vcx.run_until_parked();
    click(vcx, "bot-snooze-custom");
    assert!(vcx.debug_bounds("bot-snooze-time").is_some());
    click(vcx, "bot-snooze-submit");
    assert!(vcx.debug_bounds("bot-snooze-pick").is_none());
    assert!(
        state
            .lock()
            .unwrap()
            .items
            .iter()
            .any(|i| i.uid == "ask" && i.snoozed_until_ms.is_some())
    );
}

#[gpui::test]
fn home_item_steps_shows_its_chat_and_steps_several_questions(cx: &mut TestAppContext) {
    let now = super::now_unix_ms();
    let mut mock = MockState::default();
    mock.bots.push(sample_bot("bot-1", "PR Bot"));
    for (uid, n) in [("a", 1), ("b", 2)] {
        let mut item = sample_item("bot-1", uid, n, BotItemStatus::NeedsYou);
        item.last_activity_at_ms = now - n;
        item.links.clear();
        mock.items.push(item);
    }
    let a = mock.items.iter_mut().find(|i| i.uid == "a").unwrap();
    a.events.push(BotItemEvent {
        id: 2,
        item_uid: "a".to_owned(),
        kind: BotItemEventKind::Status,
        actor: BotItemActor::Bot {
            bot_id: BotId::new("bot-1").unwrap(),
            session_id: None,
        },
        text: "status -> needs_you".to_owned(),
        decisions: ["Ship it?", "Tell the team?"]
            .map(|q| BotItemDecision {
                question: q.to_owned(),
                options: vec!["Yes".to_owned(), "No".to_owned()],
                recommended: Some(0),
            })
            .to_vec(),
        created_at_ms: now,
    });
    a.events.push(BotItemEvent {
        id: 3,
        item_uid: "a".to_owned(),
        kind: BotItemEventKind::Edited,
        actor: BotItemActor::Owner { device: None },
        text: r#"{"fields":{"title":{"from":"Old","to":"Task #1"}}}"#.to_owned(),
        decisions: vec![],
        created_at_ms: now,
    });
    let state = Arc::new(Mutex::new(mock));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state.clone());
    let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
    vcx.simulate_resize(gpui::size(px(360.), px(900.)));
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.show_in_panel(&BotId::new("bot-1").unwrap(), DetailTab::Items, cx);
            v.open_home_item("a".to_owned(), cx);
        });
    });
    vcx.run_until_parked();
    let click = |vcx: &mut VisualTestContext, sel: &'static str| {
        let at = vcx.debug_bounds(sel).unwrap_or_else(|| panic!("{sel}"));
        vcx.simulate_click(at.center(), Modifiers::default());
        vcx.run_until_parked();
    };
    assert!(vcx.debug_bounds("bot-item-chat").is_some());
    assert!(vcx.debug_bounds("bot-item-stepper").is_some());
    assert!(vcx.debug_bounds("bot-item-replying-to").is_some());
    // The edit opens to its before and after.
    click(vcx, "bot-item-change-3:title");

    // Two questions, one line at a time: the first reply holds, the second sends both.
    click(vcx, "bot-item-step-reply-0-0");
    click(vcx, "bot-item-step-reply-1-1");
    assert!(state.lock().unwrap().requests.iter().any(|r| matches!(
        r,
        BotRpcRequest::AnswerBotItem { answer_text: Some(t), .. }
            if t == "Ship it? → Yes\nTell the team? → No"
    )));

    // Answered, it moved to In progress below #2: ↑ steps back to it.
    click(vcx, "bot-item-prev");
    vcx.update(|_, cx| assert_eq!(view.read(cx).items.open_uid.as_deref(), Some("b")));
}

use crate::i18n::catalog::bots as text;

/// Orca's settings view: each root row opens its own page, the crumb names it, Back returns to
/// the list; Settings › Items reaches Home's Stale group.
#[gpui::test]
fn settings_rows_open_their_own_pages(cx: &mut TestAppContext) {
    let now = super::now_unix_ms();
    let mut mock = MockState::default();
    mock.bots.push(sample_bot("bot-1", "Helper"));
    let mut old = sample_item("bot-1", "old", 1, BotItemStatus::NeedsYou);
    old.last_activity_at_ms = now - 6 * 24 * 3_600_000;
    mock.items.push(old);
    let state = Arc::new(Mutex::new(mock));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state.clone());
    let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
    vcx.simulate_resize(gpui::size(px(340.), px(1400.)));
    vcx.run_until_parked();
    let bot = BotId::new("bot-1").unwrap();
    let show = |vcx: &mut VisualTestContext, tab: DetailTab| {
        vcx.update(|_, cx| view.update(cx, |v, cx| _ = v.show_in_panel(&bot, tab, cx)));
        vcx.run_until_parked();
    };
    let click = |vcx: &mut VisualTestContext, sel: &'static str| {
        let at = vcx.debug_bounds(sel).unwrap_or_else(|| panic!("{sel}"));
        vcx.simulate_click(at.center(), Modifiers::default());
        vcx.run_until_parked();
    };
    // Five days by default: the six-day-old item is Stale.
    show(vcx, DetailTab::Items);
    assert!(vcx.debug_bounds("bot-home-group-stale").is_some());

    show(vcx, DetailTab::Settings);
    assert!(vcx.debug_bounds("bot-settings-root").is_some());
    for (row, page, title) in [
        (
            "bot-settings-agent",
            "bot-settings-agent-page",
            text::SV_AGENT_MODEL.get(),
        ),
        (
            "bot-settings-permissions",
            "bot-settings-permissions-page",
            text::SV_PERMISSIONS.get(),
        ),
        (
            "bot-settings-computer",
            "bot-settings-computer-page",
            text::SV_COMPUTER.get(),
        ),
        (
            "bot-settings-look-around-open",
            "bot-settings-look-around-page",
            text::SV_LOOK_AROUND.get(),
        ),
        (
            "bot-settings-memory-upkeep-open",
            "bot-settings-tidy-up-page",
            text::SV_MEMORY_UPKEEP.get(),
        ),
        (
            "bot-settings-schedules",
            "bot-settings-schedules-page",
            text::SV_SCHEDULES.get(),
        ),
        (
            "bot-settings-soul",
            "bot-settings-soul-page",
            text::SV_SOUL.get(),
        ),
        (
            "bot-settings-memory",
            "bot-settings-memory-page",
            text::SV_MEMORY.get(),
        ),
        (
            "bot-settings-who-can-ask",
            "bot-settings-who-can-ask-page",
            text::SV_WHO_CAN_ASK.get(),
        ),
        (
            "bot-settings-tools",
            "bot-settings-tools-page",
            text::SV_TOOLS.get(),
        ),
        (
            "bot-settings-skills",
            "bot-settings-skills-page",
            text::SV_SKILLS.get(),
        ),
        (
            "bot-settings-sharing",
            "bot-settings-sharing-page",
            text::SV_SHARING.get(),
        ),
    ] {
        click(vcx, row);
        assert!(vcx.debug_bounds(page).is_some(), "{row} opens {page}");
        assert!(vcx.debug_bounds("bot-settings-root").is_none());
        vcx.update(|_, cx| assert_eq!(view.read(cx).settings_subpage(), Some(title)));
        vcx.update(|_, cx| view.update(cx, View::close_settings_subpage));
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("bot-settings-root").is_some(),
            "back from {page}"
        );
    }

    // Memory: What it knows, then History.
    click(vcx, "bot-settings-memory");
    click(vcx, "bot-memory-tab-history");
    vcx.update(|_, cx| assert!(view.read(cx).settings.memory_history_tab));
    vcx.update(|_, cx| view.update(cx, View::close_settings_subpage));
    vcx.run_until_parked();

    // Settings › Items › Never: nothing goes Stale on Home.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.update_settings_extras(
                bot.clone(),
                super::settings_root::Extras {
                    items_stale: Some(krill_proto::bots::BotItemsStale::Never),
                    ..Default::default()
                },
                cx,
            );
        });
    });
    vcx.run_until_parked();
    assert!(state.lock().unwrap().requests.iter().any(|r| matches!(
        r,
        BotRpcRequest::UpdateBotSettingsExtras {
            items_stale: Some(krill_proto::bots::BotItemsStale::Never),
            ..
        }
    )));
    show(vcx, DetailTab::Items);
    assert!(vcx.debug_bounds("bot-home-group-stale").is_none());
}

#[gpui::test]
fn import_dialog_supports_picture_picker_runs_on_copy_update_and_filter_threshold(
    cx: &mut TestAppContext,
) {
    let mut mock = MockState::default();
    let existing = sample_bot("imported-scout", "Scout Old");
    mock.bots.push(existing.clone());
    for i in 1..=11 {
        mock.candidates.push(ExternalBotCandidate {
            app: if i % 2 == 1 {
                ExternalBotSourceApp::OpenClaw
            } else {
                ExternalBotSourceApp::Hermes
            },
            agent: "claude".to_owned(),
            name: format!("Agent {i}"),
            path: format!("/tmp/external-bot-{i}"),
            soul_preview: Some(format!("External bot #{i}")),
        });
    }
    mock.imported_sources
        .push(("/tmp/external-bot-1".to_owned(), existing.id.clone()));

    let state = Arc::new(Mutex::new(mock));
    let (view, vcx) = open_view(cx, state.clone());
    let closed = Arc::new(Mutex::new(false));
    let closed_flag = closed.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, ev: &Close, _| {
            let _ = ev;
            *closed_flag.lock().unwrap() = true;
        })
        .detach();
    });

    vcx.update(|_, cx| view.update(cx, View::open_import));
    vcx.run_until_parked();

    // Candidate 1 was already imported (Update mode hides BotRunsOnPicker) and filter input appears past FILTER_FROM (10).
    assert!(vcx.debug_bounds("bots-import-view").is_some());
    assert!(vcx.debug_bounds("bot-import-filter").is_some());
    assert!(vcx.debug_bounds("bot-import-update").is_some());
    assert!(vcx.debug_bounds("import-bot-runs-on").is_none());

    // Typing a new name switches to Copy mode, revealing BotRunsOnPicker + "Create".
    vcx.simulate_input("Agent Copy");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bot-import-create").is_some());
    assert!(vcx.debug_bounds("import-bot-runs-on").is_some());
    let pic_btn = vcx.debug_bounds("import-bot-picture-btn").unwrap();
    vcx.simulate_click(pic_btn.center(), Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("import-bot-carousel").is_some());
    let ink = vcx.debug_bounds("import-bot-swatch-1f2126").unwrap();
    vcx.simulate_click(ink.center(), Modifiers::default());
    vcx.run_until_parked();
    vcx.simulate_click(pic_btn.center(), Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("import-bot-carousel").is_none());

    // Filter narrows the list below the scroll fold and reveals "Another app..." right away.
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            if let Some(filter) = &v.import_dialog.filter_input {
                filter.update(cx, |s, cx| s.set_value("Agent 11", window, cx));
            }
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bot-import-pick-Agent 11").is_some());
    assert!(vcx.debug_bounds("bot-import-pick-Agent 2").is_none());

    // Switching to "Another app..." goes to Export; first Escape returns to Pick; second closes.
    let other_app = vcx.debug_bounds("bot-import-other-app").unwrap();
    vcx.simulate_click(other_app.center(), Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bot-import-choose-folder").is_some());

    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.import_dialog.folder = Some(std::path::PathBuf::from("/tmp/exported-helper"));
            v.import_dialog.typed = None;
            if let Some(inp) = &v.import_dialog.name_input {
                inp.update(cx, |s, cx| s.set_value("", window, cx));
            }
            cx.notify();
        });
    });
    vcx.run_until_parked();
    let suggestion = vcx
        .debug_bounds("bot-import-suggestion")
        .expect("export folder suggestion visible");
    vcx.simulate_click(suggestion.center(), Modifiers::default());
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        assert_eq!(
            view.read(cx).import_dialog.typed.as_deref(),
            Some("Exported helper")
        );
    });

    vcx.update(|_, cx| view.update(cx, View::handle_escape));
    vcx.run_until_parked();
    assert!(!*closed.lock().unwrap());
    assert_eq!(
        view.read_with(vcx, |v, _| v.import_dialog.view),
        super::import_dialog::ImportView::Pick
    );

    // Back in Pick, submit creates an imported copy via ImportExternalBotCopy.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.import_dialog.typed = Some("Scout Two".to_owned());
            v.create_imported_bot(cx);
        });
    });
    vcx.run_until_parked();
    let reqs = state.lock().unwrap().requests.clone();
    assert!(reqs.iter().any(|r| matches!(
        r,
        BotRpcRequest::ImportExternalBotCopy { path, params, .. }
            if path == "/tmp/external-bot-1"
                && params.base.name == "Scout Two"
                && params.avatar_color.as_deref() == Some("#1f2126")
    )));
}

#[gpui::test]
fn new_bot_import_nudge_opens_import_and_dismisses_cleanly(cx: &mut TestAppContext) {
    use super::new_bot::{Event, HostChoice, NewBot};
    use crate::hosts::HostId;
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::settings::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        crate::settings::edit(cx, |s| s.bot_import_nudge_dismissed = false);
    });
    let rpc = make_mock_rpc(Arc::default());
    let (dialog, vcx) = cx.add_window_view(|window, cx| {
        NewBot::new(
            vec![HostChoice {
                id: HostId::Local,
                label: "My Mac".into(),
                rpc,
            }],
            Vec::new(),
            None,
            window,
            cx,
        )
    });
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let ev_ref = events.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&dialog, move |_, ev: &Event, _| {
            if matches!(ev, Event::OpenImport) {
                ev_ref.lock().unwrap().push("open_import".to_owned());
            }
        })
        .detach();
        dialog.update(cx, |d, cx| {
            d.set_nudge_candidates_for_test(
                &[ExternalBotCandidate {
                    app: ExternalBotSourceApp::OpenClaw,
                    agent: "claude".to_owned(),
                    name: "Claw".to_owned(),
                    path: "/tmp/openclaw-agent".to_owned(),
                    soul_preview: None,
                }],
                cx,
            );
        });
    });
    vcx.run_until_parked();

    let action = vcx
        .debug_bounds("new-bot-import-nudge-action")
        .expect("nudge action rendered");
    vcx.simulate_click(action.center(), Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(events.lock().unwrap().as_slice(), ["open_import"]);

    let dismiss = vcx
        .debug_bounds("new-bot-import-nudge-dismiss")
        .expect("nudge dismiss rendered");
    vcx.simulate_click(dismiss.center(), Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("new-bot-import-nudge").is_none());
    vcx.update(|_, cx| {
        assert!(crate::settings::current(cx).bot_import_nudge_dismissed);
        crate::settings::edit(cx, |s| s.bot_import_nudge_dismissed = false);
    });
}

#[gpui::test]
fn new_bot_proposal_fields_template_chips_and_avatar_gen_work(cx: &mut TestAppContext) {
    use super::avatar_gen::{
        AvatarAgentStatusKind, AvatarGenerationCapability, AvatarGenerationFailure,
        AvatarImageAgent, AvatarImageAgentStatus,
    };
    use super::new_bot::{HostChoice, NewBot};
    use super::picture_picker::{GenRunState, PictureFace};
    use crate::hosts::HostId;
    use krill_proto::bots::{BotProposal, HandedWorkspace};

    let state = Arc::new(Mutex::new(MockState::default()));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state.clone());
    let proposal = BotProposal {
        name: "Release Marshal".to_owned(),
        purpose: "Coordinates releases and watches CI failures.".to_owned(),
        watches: Some("PRs labeled release".to_owned()),
        does: Some("Summarizes blockers".to_owned()),
        reports: Some("Daily in #release".to_owned()),
        automations: Vec::new(),
        template_id: None,
        mascot: Some("krill".to_owned()),
        worktrees: vec![HandedWorkspace {
            id: "wt-1".to_owned(),
            name: "bot-create-group".to_owned(),
        }],
    };
    let (dialog, vcx) = cx.add_window_view(|window, cx| {
        NewBot::new_with_proposal(
            vec![HostChoice {
                id: HostId::Local,
                label: "My Mac".into(),
                rpc: rpc.clone(),
            }],
            Vec::new(),
            None,
            Some(proposal),
            None,
            window,
            cx,
        )
    });
    vcx.run_until_parked();

    for sel in [
        "new-bot-watches",
        "new-bot-does",
        "new-bot-reports",
        "new-bot-takes-over",
    ] {
        assert!(
            vcx.debug_bounds(sel).is_some(),
            "missing proposal field {sel}"
        );
    }

    // Verify AvatarGenerateFace capability + real-person safety guard.
    vcx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.picture.set_capability_for_test(
                AvatarGenerationCapability {
                    agents: vec![AvatarImageAgentStatus {
                        agent: AvatarImageAgent::Codex,
                        status: AvatarAgentStatusKind::Ready,
                    }],
                },
                cx,
            );
            assert!(d.picture.faces().contains(&PictureFace::Generate));
            d.set_gen_description_for_test("portrait of a real person", window, cx);
            d.picture.start_generation(NewBot::picture_mut, cx);
            assert!(matches!(
                d.picture.gen_run,
                GenRunState::Error {
                    failure: AvatarGenerationFailure::RealPerson,
                    ..
                }
            ));
            d.submit(cx);
        });
    });
    vcx.run_until_parked();

    let reqs = state.lock().unwrap().requests.clone();
    assert!(reqs.iter().any(|r| matches!(
        r,
        BotRpcRequest::CreateBotV2(p)
            if p.base.name == "Release Marshal"
                && p.base.description.contains("Coordinates releases")
                && p.base.initial_soul.as_deref().is_some_and(|soul| {
                    soul.contains("Watches") && soul.contains("Does") && soul.contains("Reports")
                })
    )));

    // Also verify interactive template chips popover (`pr-review`).
    let tpl = krill_proto::bots::find_bot_template("pr-review").unwrap();
    let (tpl_dialog, vcx2) = cx.add_window_view(|window, cx| {
        NewBot::new(
            vec![HostChoice {
                id: HostId::Local,
                label: "My Mac".into(),
                rpc,
            }],
            Vec::new(),
            Some(tpl),
            window,
            cx,
        )
    });
    let _ = &tpl_dialog;
    vcx2.run_until_parked();
    assert!(vcx2.debug_bounds("new-bot-template-parts").is_some());
    let skills_chip = vcx2.debug_bounds("new-bot-chip-skills").unwrap();
    vcx2.simulate_click(skills_chip.center(), Modifiers::default());
    vcx2.run_until_parked();
    assert_eq!(tpl.skills, ["pr-review-checklist"]);
    assert!(
        vcx2.debug_bounds("new-bot-skill-switch-pr-review-checklist")
            .is_some()
    );
}

#[test]
fn settings_roundtrip_preserves_new_bot_nudge_and_avatar_agent() {
    let mut s = crate::settings::Settings::default();
    assert!(!s.bot_import_nudge_dismissed);
    assert_eq!(s.avatar_generation_agent, None);
    s.bot_import_nudge_dismissed = true;
    s.avatar_generation_agent = Some("codex".to_owned());
    let json = serde_json::to_string(&s).unwrap();
    let back: crate::settings::Settings = serde_json::from_str(&json).unwrap();
    assert!(back.bot_import_nudge_dismissed);
    assert_eq!(back.avatar_generation_agent.as_deref(), Some("codex"));
}

#[test]
fn agy_effort_options_and_alias_helpers_cover_all_cli_levels() {
    use super::rpc::{effort_label, effort_options_for_runtime, runtime_display_label};

    let agy_efforts = effort_options_for_runtime("agy");
    assert_eq!(agy_efforts, &["", "low", "medium", "high", "xhigh", "max"]);
    assert_eq!(effort_options_for_runtime("antigravity"), agy_efforts);
    assert_eq!(
        runtime_display_label("antigravity"),
        runtime_display_label("agy")
    );
    for level in ["", "low", "medium", "high", "xhigh", "max"] {
        assert!(!effort_label(level).is_empty(), "missing label for {level}");
    }
    let (_, builtin_efforts) = crate::agent_models::builtin("agy");
    assert_eq!(builtin_efforts, &["low", "medium", "high", "xhigh", "max"]);
}

#[gpui::test]
fn new_bot_picker_supports_agy_effort_and_clears_effort_on_suffixed_model(cx: &mut TestAppContext) {
    use super::new_bot::{HostChoice, NewBot};
    use crate::hosts::HostId;

    let state = Arc::new(Mutex::new(MockState::default()));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state);
    let (dialog, vcx) = cx.add_window_view(|window, cx| {
        NewBot::new(
            vec![HostChoice {
                id: HostId::Local,
                label: "My Mac".into(),
                rpc,
            }],
            Vec::new(),
            None,
            window,
            cx,
        )
    });
    vcx.run_until_parked();

    vcx.update(|_, cx| {
        dialog.update(cx, |d, cx| {
            d.set_runtime("agy", cx);
            assert!(
                d.setup().supports_effort(),
                "agy must support effort in picker"
            );
            d.set_effort(Some("xhigh".to_owned()), cx);
            assert_eq!(d.setup().effort.as_deref(), Some("xhigh"));
            d.set_model(Some("gemini-3.8-flash-low".to_owned()), cx);
            assert_eq!(d.setup().model.as_deref(), Some("gemini-3.8-flash-low"));
            assert_eq!(
                d.setup().effort,
                None,
                "selecting an effort-suffixed agy model must clear separate effort"
            );
        });
    });
}

#[gpui::test]
fn switching_bot_provider_in_settings_refreshes_models_and_clears_old_sessions(
    cx: &mut TestAppContext,
) {
    use krill_proto::bots::{BotAgentModel, BotAgentModels};

    let mut mock = MockState::default();
    let mut bot = sample_bot("switch-bot", "Switch Bot");
    bot.agent_id = "claude".to_owned();
    bot.model = Some("opus".to_owned());
    bot.effort = Some("high".to_owned());
    let bot_id = bot.id.clone();
    mock.bots.push(bot);
    mock.sessions.push(BotSessionSummary {
        session_id: SessionId(55),
        bot_id: bot_id.clone(),
        link_id: None,
        pid: 1234,
        live: true,
        working: false,
        label: "Switch Bot".to_owned(),
        created_at_ms: 1_000,
    });
    mock.agent_models.insert(
        "agy".to_owned(),
        BotAgentModels {
            models: vec![
                BotAgentModel {
                    id: "gemini-3.8-flash-low".to_owned(),
                    label: "Gemini 3.8 Flash (Low)".to_owned(),
                },
                BotAgentModel {
                    id: "gemini-4-neon-high".to_owned(),
                    label: "Gemini 4 Neon (High)".to_owned(),
                },
            ],
            reason: None,
        },
    );
    let state = Arc::new(Mutex::new(mock));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state.clone());
    let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
    vcx.run_until_parked();

    let sessions_emitted = Arc::new(Mutex::new(Vec::<usize>::new()));
    let emitted_ref = sessions_emitted.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, ev: &super::SessionsChanged, _| {
            emitted_ref.lock().unwrap().push(ev.sessions.len());
        })
        .detach();
        view.update(cx, |v, cx| {
            v.show_in_panel(&bot_id, DetailTab::Settings, cx);
        });
    });
    vcx.run_until_parked();

    let params = krill_proto::bots::UpdateBotParams {
        agent_id: Some("agy".to_owned()),
        model: krill_proto::bots::FieldPatch::Clear,
        effort: krill_proto::bots::FieldPatch::Clear,
        ..super::settings_root::patch(&state.lock().unwrap().bots[0])
    };
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.patch_bot(params, cx));
    });
    vcx.run_until_parked();

    vcx.update(|_, cx| {
        let v = view.read(cx);
        let updated = v.selected_bot().unwrap();
        assert_eq!(updated.agent_id, "agy");
        assert_eq!(updated.model, None);
        assert_eq!(updated.effort, None);
        assert!(
            v.sessions_for_bot(&bot_id).is_empty(),
            "old provider sessions must be dropped from view state"
        );
        let (agent, models) = v.settings.agent_models.as_ref().expect("agy models loaded");
        assert_eq!(agent, "agy");
        assert_eq!(models.models.len(), 2);
        assert_eq!(models.models[0].id, "gemini-3.8-flash-low");
    });
    assert!(
        sessions_emitted.lock().unwrap().contains(&0),
        "SessionsChanged must emit cleared session map"
    );
}

#[test]
fn bot_refusal_text_and_automation_rows_surface_specific_reasons() {
    // Mirrors Orca `bot-presentation.test.ts` (#46838, #46892) and run-card refusal wording.
    use krill_proto::bots::{
        BotScheduleConfig, BotScheduleUpcoming, BotSessionRefusal, BotUpcoming,
    };

    let too_old = BotSessionRefusal::cli_too_old("hermes", "0.20.5", "0.23.0");
    let local_msg = super::rpc::bot_refusal_text(&too_old, Some("this computer"));
    assert!(local_msg.contains("Hermes"), "{local_msg}");
    assert!(local_msg.contains("0.20.5"), "{local_msg}");
    assert!(local_msg.contains("0.23.0"), "{local_msg}");

    // Paired server without a name must never be called "this computer".
    let remote_msg = super::rpc::bot_refusal_text(&too_old, None);
    assert!(remote_msg.contains("Hermes"), "{remote_msg}");
    assert!(
        !remote_msg.contains("this computer") && !remote_msg.contains("이 컴퓨터"),
        "{remote_msg}"
    );

    // call_checked_on promotes SessionRefused into a host-worded Err instead of generic text.
    let refused_rpc: super::rpc::BotRpcCall = Arc::new(move |_| {
        Ok(BotRpcResponse::SessionRefused(Box::new(
            BotSessionRefusal::cli_too_old("hermes", "0.20.5", "0.23.0"),
        )))
    });
    let err = super::rpc::call_checked_on(
        &refused_rpc,
        &BotRpcRequest::StartBotSession {
            bot_id: BotId::new("refused-bot").unwrap(),
            cwd: None,
            prompt: None,
        },
        Some("Mac Studio"),
    )
    .unwrap_err();
    assert!(err.contains("Hermes"), "{err}");
    assert!(err.contains("Mac Studio"), "{err}");
    assert!(err.contains("0.20.5"), "{err}");
    assert!(err.contains("0.23.0"), "{err}");

    // Schedule run cards carry `last_refusal` so Home Automation rows mark them failed with reason.
    let mut bot = sample_bot("sched-refused", "Nightly Bot");
    bot.schedules.push(BotScheduleConfig {
        id: "nightly".to_owned(),
        name: "Nightly triage".to_owned(),
        cron: "0 9 * * *".to_owned(),
        prompt: "Triage issues".to_owned(),
        enabled: true,
        timezone: None,
    });
    let up = BotUpcoming {
        schedules: vec![BotScheduleUpcoming {
            id: "nightly".to_owned(),
            next_ms: Some(200_000),
            last_ms: None,
            running: false,
            last_error: Some(
                "Your Hermes CLI on this computer is 0.20.5. Chats need 0.23.0 or newer."
                    .to_owned(),
            ),
            last_refusal: Some(too_old),
        }],
        ..BotUpcoming::default()
    };
    let rows = super::home_upcoming::automation_rows_on(
        &bot,
        Some(&up),
        false,
        None,
        Some("Build Server"),
        100_000,
    );
    let sched_row = rows
        .iter()
        .find(|r| r.id == "nightly")
        .expect("schedule row");
    let failed = sched_row.failed.as_deref().expect("failed reason");
    assert!(failed.contains("Hermes"), "{failed}");
    assert!(failed.contains("Build Server"), "{failed}");
    assert!(failed.contains("0.20.5"), "{failed}");
    assert!(failed.contains("0.23.0"), "{failed}");
}

#[gpui::test]
fn u01_panel_soul_settings_syncs_input_when_switching_bots(cx: &mut TestAppContext) {
    let mut mock = MockState::default();
    let bot_a = sample_bot("bot-a", "Bot A");
    let bot_b = sample_bot("bot-b", "Bot B");
    let id_a = bot_a.id.clone();
    let id_b = bot_b.id.clone();
    mock.bots.push(bot_a);
    mock.bots.push(bot_b);
    mock.soul = "Soul of A".to_owned();
    let state = Arc::new(Mutex::new(mock));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state.clone());
    let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
    vcx.simulate_resize(gpui::size(px(1040.), px(720.)));
    vcx.run_until_parked();

    // Open Bot A Settings > Soul so soul_input & memory_input are created.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.show_in_panel(&id_a, DetailTab::Settings, cx);
            v.settings.page = super::settings_root::SettingsPage::Soul;
            cx.notify();
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let v = view.read(cx);
        let inp = v
            .soul_memory
            .soul_input
            .as_ref()
            .expect("soul input created");
        assert_eq!(inp.read(cx).value().as_ref(), "Soul of A");
    });

    // Switch to Bot B Settings > Soul: existing soul_input MUST sync to Bot B's SOUL.
    state.lock().unwrap().soul = "Soul of B".to_owned();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.show_in_panel(&id_b, DetailTab::Settings, cx);
            v.settings.page = super::settings_root::SettingsPage::Soul;
            cx.notify();
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let v = view.read(cx);
        let inp = v
            .soul_memory
            .soul_input
            .as_ref()
            .expect("soul input reused");
        assert_eq!(
            inp.read(cx).value().as_ref(),
            "Soul of B",
            "switching bots in panel Settings > Soul must sync existing input to new bot's SOUL"
        );
    });
}

#[gpui::test]
fn u03_u04_group_panel_upsert_updates_backing_and_panel_return_reloads_single_bot(
    cx: &mut TestAppContext,
) {
    let mut mock = MockState::default();
    let bot_a = sample_bot("bot-a", "Bot A");
    let bot_b = sample_bot("bot-b", "Bot B");
    let id_a = bot_a.id.clone();
    let id_b = bot_b.id.clone();
    mock.bots.push(bot_a);
    mock.bots.push(bot_b);
    let item_a = sample_item("bot-a", "uid-a1", 1, BotItemStatus::NeedsYou);
    let item_b = sample_item("bot-b", "uid-b1", 2, BotItemStatus::Planned);
    mock.items.push(item_a.clone());
    mock.items.push(item_b.clone());
    let state = Arc::new(Mutex::new(mock));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state);
    let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
    vcx.run_until_parked();

    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.show_in_panel(&id_a, DetailTab::Items, cx);
        });
    });
    vcx.run_until_parked();

    // Enter group panel covering both bot A and bot B.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.show_group_in_panel("grp-1", &[id_a.clone(), id_b.clone()], &[], cx);
        });
    });
    vcx.run_until_parked();

    // U-03: Upserting an updated item must update group_panel.all_items and NOT duplicate
    // bot_b's item into all_open_items under bot_a.
    let mut done_a = item_a;
    done_a.status = BotItemStatus::Done;
    done_a.version += 1;
    vcx.update(|_, cx| {
        view.update(cx, |v, _| {
            v.upsert_item(done_a.clone(), &id_a);
            let in_group = v
                .group_panel
                .all_items
                .iter()
                .find(|i| i.uid == "uid-a1")
                .expect("updated item in group backing");
            assert_eq!(in_group.status, BotItemStatus::Done);
            let b_copies = v
                .all_open_items
                .iter()
                .filter(|i| i.uid == "uid-b1")
                .count();
            assert_eq!(
                b_copies, 1,
                "sync_open_items_for_bot must not duplicate other bots' items"
            );
        });
    });

    // U-04: Returning to bot_a (same selected_bot_id!) must exit group panel and reload bot_a's
    // single-bot items so bot_b's item does not leak.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.show_in_panel(&id_a, DetailTab::Items, cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert!(!v.group_panel.active);
        assert!(
            v.items.items.iter().all(|i| i.owner_bot_id == id_a),
            "returning from group panel to same bot must not leak other bots' items"
        );
    });
}

#[gpui::test]
fn u16_u26_panel_open_import_emits_event_and_silent_mark_seen_sets_no_notice(
    cx: &mut TestAppContext,
) {
    let mut mock = MockState::default();
    let bot = sample_bot("bot-1", "Bot One");
    let bot_id = bot.id.clone();
    mock.bots.push(bot);
    let state = Arc::new(Mutex::new(mock));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state);
    let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
    vcx.run_until_parked();

    let import_requested = Arc::new(Mutex::new(false));
    let flag = import_requested.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, _: &super::RequestImport, _| {
            *flag.lock().unwrap() = true;
        })
        .detach();
        view.update(cx, |v, cx| {
            v.open_import(cx);
            assert_eq!(
                v.screen(),
                Screen::Detail,
                "Panel mode must not switch internal screen to Import"
            );
            v.mark_bot_items_opened_silently(bot_id.clone(), cx);
            assert!(
                !v.items.busy,
                "silent mark_bot_items_opened must not set items.busy"
            );
        });
    });
    vcx.run_until_parked();

    assert!(
        *import_requested.lock().unwrap(),
        "Panel mode open_import must emit RequestImport"
    );
    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert!(
            v.notice.is_none(),
            "silent mark_bot_items_opened must not flash a Saved notice banner"
        );
    });
}

#[test]
fn u47_find_imported_bot_matches_direct_and_precomputed_canonical_without_fs() {
    let bot = sample_bot("imp-1", "Imported Bot");
    let sources = vec![("/real/path/agent".to_owned(), bot.id.clone())];
    assert_eq!(
        super::pickers::find_imported_bot(
            std::slice::from_ref(&bot),
            &sources,
            "/symlink/agent",
            Some("/real/path/agent"),
        )
        .map(|b| &b.id),
        Some(&bot.id)
    );
    assert!(
        super::pickers::find_imported_bot(
            std::slice::from_ref(&bot),
            &sources,
            "/symlink/agent",
            None,
        )
        .is_none()
    );
}

/// Commit 5 (`eefd93a3`): Bot Home and Group panels show the greyed 40px bot/group avatar above
/// folded groups when nothing needs attention and nothing is running, and hide it when paused.
#[gpui::test]
fn bot_home_and_group_all_clear_show_avatar_above_folded_groups_and_hide_when_paused(
    cx: &mut gpui::TestAppContext,
) {
    let mut bot = sample_bot("clear-bot", "Clear Bot");
    bot.avatar = Some("B1".into());
    let now = super::now_unix_ms();
    let planned = krill_proto::bots::BotItem {
        uid: "item-planned".into(),
        number: 1,
        owner_bot_id: bot.id.clone(),
        title: "Backlog cleanup".into(),
        description: None,
        done_means: None,
        status: BotItemStatus::Planned,
        snoozed_until_ms: None,
        close_note: None,
        closed_at_ms: None,
        links: Vec::new(),
        events: Vec::new(),
        version: 1,
        created_at_ms: now,
        updated_at_ms: now,
        last_activity_at_ms: now,
    };
    let state = Arc::new(Mutex::new(MockState {
        bots: vec![bot.clone()],
        items: vec![planned.clone()],
        ..MockState::default()
    }));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state.clone());
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let mut v = View::with_rpc(rpc, None, cx).into_panel();
        v.bots = vec![bot.clone()];
        v.selected_bot_id = Some(bot.id.clone());
        v.items.set_items(vec![planned.clone()]);
        v
    });
    vcx.run_until_parked();

    // All clear avatar appears above the folded Planned group.
    let all_clear = vcx
        .debug_bounds("bot-home-all-clear")
        .expect("BotItemsAllClear should render when only Planned items exist");
    let avatar_bounds = vcx
        .debug_bounds("bot-home-all-clear-avatar")
        .expect("40px greyed bot avatar should render in AllClear");
    let planned_header = vcx
        .debug_bounds("bot-home-group-planned")
        .expect("Planned group header");
    assert!(
        all_clear.top() < planned_header.top(),
        "AllClear must sit above folded groups: all_clear={:?} planned={:?}",
        all_clear.top(),
        planned_header.top()
    );
    assert_eq!(avatar_bounds.size.width, px(40.));
    assert_eq!(avatar_bounds.size.height, px(40.));

    // Pausing the bot hides AllClear.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.bots[0].paused = true;
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("bot-home-all-clear").is_none(),
        "Paused bot must not show AllClear"
    );

    // Group panel AllClear shows 40px group faces when active and hides when all members paused.
    let mut grp_item = planned.clone();
    grp_item.uid = "item-grp-planned".into();
    grp_item.description = Some("grp-clear".into());
    state.lock().unwrap().items = vec![grp_item.clone()];
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.bots[0].paused = false;
            v.group_panel.all_items = vec![grp_item];
            v.show_group_in_panel("grp-clear", &[bot.id.clone()], &[], cx);
            v.clear_group_filters(cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("bot-group-all-clear").is_some(),
        "Group panel should render AllClear with default filters and only Planned items"
    );
    let grp_avatar = vcx
        .debug_bounds("bot-group-all-clear-avatar")
        .expect("group AllClear avatar");
    assert_eq!(grp_avatar.size.width, px(40.));
    assert_eq!(grp_avatar.size.height, px(40.));
}

/// Commit 6 (`d6cc2684`): an agent's long asking comment above an open question is shown whole
/// while waiting, and regular/answered long comments clamp at 8 lines with a Show more toggle.
#[gpui::test]
fn waiting_question_comment_is_shown_whole_and_settled_comment_clamps(
    cx: &mut gpui::TestAppContext,
) {
    use krill_proto::bots::{BotItemActor, BotItemDecision, BotItemEvent, BotItemEventKind};

    let bot = sample_bot("ask-bot", "Ask Bot");
    let long_body = (1..=14)
        .map(|i| format!("Line {i}: detailed explanation of the choice."))
        .collect::<Vec<_>>()
        .join("\n");
    let settled_comment = BotItemEvent {
        id: 1,
        item_uid: "item-ask".into(),
        kind: BotItemEventKind::Commented,
        actor: BotItemActor::Bot {
            bot_id: bot.id.clone(),
            session_id: None,
        },
        text: long_body.clone(),
        decisions: Vec::new(),
        created_at_ms: 1_000,
    };
    let asking_comment = BotItemEvent {
        id: 2,
        item_uid: "item-ask".into(),
        kind: BotItemEventKind::Commented,
        actor: BotItemActor::Bot {
            bot_id: bot.id.clone(),
            session_id: None,
        },
        text: long_body,
        decisions: vec![BotItemDecision {
            question: "Proceed with option A?".into(),
            options: vec!["Yes".into(), "No".into()],
            recommended: Some(0),
        }],
        created_at_ms: 2_000,
    };
    let item = krill_proto::bots::BotItem {
        uid: "item-ask".into(),
        number: 7,
        owner_bot_id: bot.id.clone(),
        title: "Choose migration strategy".into(),
        description: None,
        done_means: None,
        status: BotItemStatus::NeedsYou,
        snoozed_until_ms: None,
        close_note: None,
        closed_at_ms: None,
        links: Vec::new(),
        events: vec![settled_comment, asking_comment],
        version: 1,
        created_at_ms: 1_000,
        updated_at_ms: 2_000,
        last_activity_at_ms: 2_000,
    };
    let state = Arc::new(Mutex::new(MockState {
        bots: vec![bot.clone()],
        items: vec![item.clone()],
        ..MockState::default()
    }));
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let rpc = make_mock_rpc(state);
    let (_view, vcx) = cx.add_window_view(|_, cx| {
        let mut v = View::with_rpc(rpc, None, cx).into_panel();
        v.bots = vec![bot.clone()];
        v.selected_bot_id = Some(bot.id.clone());
        v.items.set_items(vec![item.clone()]);
        v.open_home_item(item.uid.clone(), cx);
        v
    });
    vcx.run_until_parked();

    // Event 2 (waiting open question) is rendered whole with NO Show more toggle.
    assert!(
        vcx.debug_bounds("bot-item-comment-whole").is_some(),
        "Waiting question comment must render whole"
    );
    assert!(
        vcx.debug_bounds("bot-item-comment-toggle-2").is_none(),
        "Waiting question comment must not have a clamp toggle"
    );
    // Event 1 (settled long comment) IS clamped at 8 lines with a toggle button.
    let clamped = vcx
        .debug_bounds("bot-item-comment-clamped")
        .expect("Settled long comment must clamp");
    assert!(
        clamped.size.height <= px(154.),
        "Clamped comment height should be capped around 152px, got {:?}",
        clamped.size.height
    );
    let toggle = vcx
        .debug_bounds("bot-item-comment-toggle-1")
        .expect("Settled long comment should have Show more toggle");
    vcx.simulate_click(toggle.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("bot-item-comment-clamped").is_none(),
        "Clicking Show more must unclamp the settled comment"
    );
}

/// Commit 7 (`22927c89`): Create stays enabled on New bot, reveals inline validation on empty
/// submit without calling `CreateBotV2`, clears error when typed, and waits while a picture loads.
#[gpui::test]
fn new_bot_keeps_create_available_and_validates_inline(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
    });
    let state = Arc::new(Mutex::new(MockState::default()));
    let rpc = make_mock_rpc(state.clone());
    let (dialog, vcx) = cx.add_window_view(|window, cx| {
        super::new_bot::NewBot::new(
            vec![super::new_bot::HostChoice {
                id: crate::hosts::HostId::Local,
                label: "This Mac".into(),
                rpc,
            }],
            Vec::new(),
            None,
            window,
            cx,
        )
    });
    vcx.run_until_parked();

    // No error before first submit attempt.
    assert!(vcx.debug_bounds("new-bot-name-error").is_none());

    // Clicking Create on an empty form shows inline "Add a name" without sending CreateBotV2.
    let create_btn = vcx
        .debug_bounds("new-bot-create")
        .expect("Create button must be rendered");
    vcx.simulate_click(create_btn.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.update(|_, cx| dialog.read(cx).is_submitted()));
    assert!(
        vcx.debug_bounds("new-bot-name-error").is_some(),
        "Empty submit must display inline Add a name error"
    );
    assert!(
        state
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|r| !matches!(r, BotRpcRequest::CreateBotV2(_))),
        "Invalid submit must not call CreateBotV2"
    );

    // Typing a name clears the inline error and allows Create to succeed.
    vcx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.set_name_for_test("Scout", window, cx);
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("new-bot-name-error").is_none(),
        "Typing a name must clear the inline error"
    );

    let create_btn = vcx.debug_bounds("new-bot-create").expect("Create button");
    vcx.simulate_click(create_btn.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        state
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|r| matches!(r, BotRpcRequest::CreateBotV2(p) if p.base.name == "Scout")),
        "Valid submit must call CreateBotV2 with the entered name"
    );
}
