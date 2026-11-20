// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Versioned IPC RPC request/response protocol for Krill Bots (Phases 1-3).
//!
//! Negotiated with [`crate::ipc::FEAT_BOTS`] (`1 << 40`).
//! Supports both dedicated client-channel frames (`BOT_REQ = 0x48`, `BOT_REPLY = 0x68`)
//! and control-channel multiplexing (`control::Request::Bot` / `control::Reply::Bot`).
//! Append new enum variants only at the end to preserve postcard wire compatibility.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::SessionId;
use crate::control::bounded;
use crate::screen::Msg;

use super::package::BotPackageExportOptions;
use super::types::{
    BotAccessRequest, BotConfig, BotConnectorConfig, BotConnectorStatus, BotGroupChat, BotId,
    BotItem, BotItemDecision, BotItemLink, BotItemStatus, BotLookAroundConfig, BotMcpToolConfig,
    BotMemoryTidyConfig, BotNotifications, BotScheduleConfig, BotSuggestion, BotToolRelayStatus,
    ExternalBotCandidate, ExternalBotSourceApp, ExternalSource,
};

/// Dedicated client-to-daemon frame kind for Krill Bots RPC.
pub const BOT_REQ: u8 = 0x48;
/// Dedicated daemon-to-client frame kind for Krill Bots RPC.
pub const BOT_REPLY: u8 = 0x68;

/// Maximum `.zip` payload transferred inside a single IPC frame (below `ipc::MAX_FRAME` = 1 MiB).
pub const MAX_RPC_ZIP_BYTES: usize = 768 * 1024;

/// Parameters for creating a new bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBotParams {
    pub name: String,
    pub agent_id: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<crate::agent::Permission>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub initial_soul: Option<String>,
}

/// Three-state patch for nullable fields in update RPC payloads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldPatch<T> {
    #[default]
    Unchanged,
    Set(T),
    Clear,
}

impl<T> FieldPatch<T> {
    #[must_use]
    pub const fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }

    /// Applies this patch to an existing `Option<T>` field.
    pub fn apply(self, target: &mut Option<T>) {
        match self {
            Self::Unchanged => {}
            Self::Set(value) => *target = Some(value),
            Self::Clear => *target = None,
        }
    }
}

/// Partial update parameters for an existing bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateBotParams {
    pub bot_id: BotId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub emoji: FieldPatch<String>,
    #[serde(default)]
    pub avatar: FieldPatch<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub model: FieldPatch<String>,
    #[serde(default)]
    pub effort: FieldPatch<String>,
    #[serde(default)]
    pub permission_mode: Option<crate::agent::Permission>,
    #[serde(default)]
    pub workspace: FieldPatch<String>,
    #[serde(default)]
    pub paused: Option<bool>,
    #[serde(default)]
    pub first_run_completed: Option<bool>,
    #[serde(default)]
    pub notifications: Option<BotNotifications>,
    #[serde(default)]
    pub schedules: Option<Vec<BotScheduleConfig>>,
    #[serde(default)]
    pub look_around: Option<BotLookAroundConfig>,
    #[serde(default)]
    pub memory_tidy: Option<BotMemoryTidyConfig>,
    #[serde(default)]
    pub mcp_tools: Option<Vec<BotMcpToolConfig>>,
}

/// Parameters for creating a tracked bot item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBotItemParams {
    pub bot_id: BotId,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub done_means: Option<String>,
    #[serde(default)]
    pub status: Option<BotItemStatus>,
    #[serde(default)]
    pub links: Vec<BotItemLink>,
}

/// Parameters for updating a tracked bot item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateBotItemParams {
    pub item_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub done_means: Option<String>,
    #[serde(default)]
    pub status: Option<BotItemStatus>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub decisions: Vec<BotItemDecision>,
    #[serde(default)]
    pub close_note: Option<String>,
    #[serde(default)]
    pub snoozed_until_ms: FieldPatch<u64>,
}

/// Versioned extensible RPC request enum covering all Phase 1-3 Krill Bot operations.
/// Variants must only be appended at the end for postcard compatibility.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotRpcRequest {
    ListBots,
    GetBot {
        bot_id: BotId,
    },
    CreateBot(Box<CreateBotParams>),
    UpdateBot(Box<UpdateBotParams>),
    DeleteBot {
        bot_id: BotId,
        delete_home: bool,
    },
    StartBotSession {
        bot_id: BotId,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        prompt: Option<String>,
    },
    GetSoul {
        bot_id: BotId,
    },
    SetSoul {
        bot_id: BotId,
        content: String,
    },
    GetMemory {
        bot_id: BotId,
    },
    UpdateMemory {
        bot_id: BotId,
        content: String,
        allow_deletions: bool,
    },
    ForgetMemoryFact {
        bot_id: BotId,
        fact: String,
    },
    RunMemoryTidy {
        bot_id: BotId,
    },
    RunLookAround {
        bot_id: BotId,
    },
    ListItems {
        #[serde(default)]
        bot_id: Option<BotId>,
        include_closed: bool,
    },
    CreateItem(Box<CreateBotItemParams>),
    UpdateItem(Box<UpdateBotItemParams>),
    AddItemComment {
        item_id: String,
        author: String,
        text: String,
    },
    AddItemLink {
        item_id: String,
        link: BotItemLink,
    },
    ListGroups,
    CreateGroup {
        name: String,
        bot_ids: Vec<BotId>,
        description: String,
    },
    SendBotMail {
        from: String,
        to_bot: String,
        subject: String,
        body: String,
        #[serde(default)]
        thread_id: Option<String>,
    },
    PostBotReply {
        bot_id: BotId,
        #[serde(default)]
        link_id: Option<String>,
        text: String,
        #[serde(default)]
        files: Vec<String>,
        #[serde(default)]
        reaction: Option<String>,
    },
    SetConnectorConfig {
        bot_id: BotId,
        connectors: Box<BotConnectorConfig>,
    },
    SetSecret {
        secret_ref: String,
        value: String,
    },
    DeleteSecret {
        secret_ref: String,
    },
    ConnectorStatus {
        #[serde(default)]
        bot_id: Option<BotId>,
    },
    ReloadConnectors {
        #[serde(default)]
        bot_id: Option<BotId>,
    },
    ListAccessRequests {
        #[serde(default)]
        bot_id: Option<BotId>,
    },
    DecideAccessRequest {
        token: String,
        approve: bool,
    },
    StartGoogleOAuth {
        bot_id: BotId,
    },
    TriggerSyncNow {
        bot_id: BotId,
        #[serde(default)]
        source: Option<ExternalSource>,
    },
    ToolRelayStatus,
    ExportBotZip {
        bot_id: BotId,
        options: BotPackageExportOptions,
    },
    ImportBotZip {
        #[serde(deserialize_with = "rpc_zip_bytes")]
        zip_bytes: Vec<u8>,
        #[serde(default)]
        name_override: Option<String>,
        #[serde(default)]
        agent_override: Option<String>,
    },
    ImportExternalBots {
        #[serde(default)]
        source: Option<ExternalBotSourceApp>,
        #[serde(default)]
        path: Option<String>,
        dry_run: bool,
    },
}

impl BotRpcRequest {
    /// Returns `true` for read-only requests safe for `Role::Observer` clients.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        matches!(
            self,
            Self::ListBots
                | Self::GetBot { .. }
                | Self::GetSoul { .. }
                | Self::GetMemory { .. }
                | Self::ListItems { .. }
                | Self::ListGroups
                | Self::ConnectorStatus { .. }
                | Self::ListAccessRequests { .. }
                | Self::ToolRelayStatus
                | Self::ImportExternalBots { dry_run: true, .. }
        )
    }
}

impl fmt::Debug for BotRpcRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ListBots => f.write_str("ListBots"),
            Self::GetBot { bot_id } => f.debug_struct("GetBot").field("bot_id", bot_id).finish(),
            Self::CreateBot(p) => f.debug_tuple("CreateBot").field(p).finish(),
            Self::UpdateBot(p) => f.debug_tuple("UpdateBot").field(p).finish(),
            Self::DeleteBot {
                bot_id,
                delete_home,
            } => f
                .debug_struct("DeleteBot")
                .field("bot_id", bot_id)
                .field("delete_home", delete_home)
                .finish(),
            Self::StartBotSession { bot_id, cwd, .. } => f
                .debug_struct("StartBotSession")
                .field("bot_id", bot_id)
                .field("cwd", cwd)
                .finish_non_exhaustive(),
            Self::GetSoul { bot_id } => f.debug_struct("GetSoul").field("bot_id", bot_id).finish(),
            Self::SetSoul { bot_id, content } => f
                .debug_struct("SetSoul")
                .field("bot_id", bot_id)
                .field("bytes", &content.len())
                .finish(),
            Self::GetMemory { bot_id } => {
                f.debug_struct("GetMemory").field("bot_id", bot_id).finish()
            }
            Self::UpdateMemory {
                bot_id,
                content,
                allow_deletions,
            } => f
                .debug_struct("UpdateMemory")
                .field("bot_id", bot_id)
                .field("bytes", &content.len())
                .field("allow_deletions", allow_deletions)
                .finish(),
            Self::ForgetMemoryFact { bot_id, .. } => f
                .debug_struct("ForgetMemoryFact")
                .field("bot_id", bot_id)
                .finish_non_exhaustive(),
            Self::RunMemoryTidy { bot_id } => f
                .debug_struct("RunMemoryTidy")
                .field("bot_id", bot_id)
                .finish(),
            Self::RunLookAround { bot_id } => f
                .debug_struct("RunLookAround")
                .field("bot_id", bot_id)
                .finish(),
            Self::ListItems {
                bot_id,
                include_closed,
            } => f
                .debug_struct("ListItems")
                .field("bot_id", bot_id)
                .field("include_closed", include_closed)
                .finish(),
            Self::CreateItem(p) => f.debug_tuple("CreateItem").field(p).finish(),
            Self::UpdateItem(p) => f.debug_tuple("UpdateItem").field(p).finish(),
            Self::AddItemComment {
                item_id, author, ..
            } => f
                .debug_struct("AddItemComment")
                .field("item_id", item_id)
                .field("author", author)
                .finish_non_exhaustive(),
            Self::AddItemLink { item_id, link } => f
                .debug_struct("AddItemLink")
                .field("item_id", item_id)
                .field("link", link)
                .finish(),
            Self::ListGroups => f.write_str("ListGroups"),
            Self::CreateGroup { name, bot_ids, .. } => f
                .debug_struct("CreateGroup")
                .field("name", name)
                .field("bot_ids", bot_ids)
                .finish_non_exhaustive(),
            Self::SendBotMail {
                from,
                to_bot,
                subject,
                ..
            } => f
                .debug_struct("SendBotMail")
                .field("from", from)
                .field("to_bot", to_bot)
                .field("subject", subject)
                .finish_non_exhaustive(),
            Self::PostBotReply {
                bot_id,
                link_id,
                files,
                reaction,
                ..
            } => f
                .debug_struct("PostBotReply")
                .field("bot_id", bot_id)
                .field("link_id", link_id)
                .field("files", &files.len())
                .field("reaction", reaction)
                .finish_non_exhaustive(),
            Self::SetConnectorConfig { bot_id, connectors } => f
                .debug_struct("SetConnectorConfig")
                .field("bot_id", bot_id)
                .field("connectors", connectors)
                .finish(),
            // Never print secret values in Debug logs.
            Self::SetSecret { secret_ref, .. } => f
                .debug_struct("SetSecret")
                .field("secret_ref", secret_ref)
                .field("value", &"<redacted>")
                .finish(),
            Self::DeleteSecret { secret_ref } => f
                .debug_struct("DeleteSecret")
                .field("secret_ref", secret_ref)
                .finish(),
            Self::ConnectorStatus { bot_id } => f
                .debug_struct("ConnectorStatus")
                .field("bot_id", bot_id)
                .finish(),
            Self::ReloadConnectors { bot_id } => f
                .debug_struct("ReloadConnectors")
                .field("bot_id", bot_id)
                .finish(),
            Self::ListAccessRequests { bot_id } => f
                .debug_struct("ListAccessRequests")
                .field("bot_id", bot_id)
                .finish(),
            Self::DecideAccessRequest { approve, .. } => f
                .debug_struct("DecideAccessRequest")
                .field("token", &"<redacted>")
                .field("approve", approve)
                .finish(),
            Self::StartGoogleOAuth { bot_id } => f
                .debug_struct("StartGoogleOAuth")
                .field("bot_id", bot_id)
                .finish(),
            Self::TriggerSyncNow { bot_id, source } => f
                .debug_struct("TriggerSyncNow")
                .field("bot_id", bot_id)
                .field("source", source)
                .finish(),
            Self::ToolRelayStatus => f.write_str("ToolRelayStatus"),
            Self::ExportBotZip { bot_id, options } => f
                .debug_struct("ExportBotZip")
                .field("bot_id", bot_id)
                .field("options", options)
                .finish(),
            Self::ImportBotZip {
                zip_bytes,
                name_override,
                agent_override,
            } => f
                .debug_struct("ImportBotZip")
                .field("zip_bytes", &zip_bytes.len())
                .field("name_override", name_override)
                .field("agent_override", agent_override)
                .finish(),
            Self::ImportExternalBots {
                source,
                path,
                dry_run,
            } => f
                .debug_struct("ImportExternalBots")
                .field("source", source)
                .field("path", path)
                .field("dry_run", dry_run)
                .finish(),
        }
    }
}

/// Versioned extensible RPC response enum paired with [`BotRpcRequest`].
/// Variants must only be appended at the end for postcard compatibility.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotRpcResponse {
    Ok,
    Error(String),
    Bots(Vec<BotConfig>),
    Bot(Box<BotConfig>),
    Deleted {
        bot_id: BotId,
    },
    SessionStarted {
        bot_id: BotId,
        session_id: SessionId,
        pid: u32,
    },
    Text(String),
    MemoryUpdated {
        content: String,
        #[serde(default)]
        restored_facts: Vec<String>,
        removed_count: usize,
    },
    MemoryTidyResult {
        bot_id: BotId,
        summary: String,
        changes: u32,
    },
    LookAroundResult {
        bot_id: BotId,
        suggestions: Vec<BotSuggestion>,
    },
    Items(Vec<BotItem>),
    Item(Box<BotItem>),
    Groups(Vec<BotGroupChat>),
    Group(BotGroupChat),
    MailSent {
        message_id: String,
    },
    ReplyPosted {
        #[serde(default)]
        external_ref: Option<String>,
    },
    ConnectorStatuses(Vec<BotConnectorStatus>),
    AccessRequests(Vec<BotAccessRequest>),
    AccessRequestDecided(BotAccessRequest),
    GoogleOAuthStarted {
        auth_url: String,
        state: String,
    },
    SyncTriggered {
        bot_id: BotId,
        sources_synced: Vec<ExternalSource>,
    },
    ToolRelayStatus(BotToolRelayStatus),
    ExportedZip {
        filename: String,
        #[serde(deserialize_with = "rpc_zip_bytes")]
        zip_bytes: Vec<u8>,
    },
    ImportedExternalBots {
        candidates: Vec<ExternalBotCandidate>,
        imported: Vec<BotConfig>,
    },
}

impl fmt::Debug for BotRpcResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok => f.write_str("Ok"),
            Self::Error(msg) => f.debug_tuple("Error").field(msg).finish(),
            Self::Bots(bots) => f.debug_tuple("Bots").field(&bots.len()).finish(),
            Self::Bot(bot) => f.debug_tuple("Bot").field(&bot.id).finish(),
            Self::Deleted { bot_id } => f.debug_struct("Deleted").field("bot_id", bot_id).finish(),
            Self::SessionStarted {
                bot_id,
                session_id,
                pid,
            } => f
                .debug_struct("SessionStarted")
                .field("bot_id", bot_id)
                .field("session_id", session_id)
                .field("pid", pid)
                .finish(),
            Self::Text(t) => f.debug_struct("Text").field("bytes", &t.len()).finish(),
            Self::MemoryUpdated {
                restored_facts,
                removed_count,
                ..
            } => f
                .debug_struct("MemoryUpdated")
                .field("restored_facts", &restored_facts.len())
                .field("removed_count", removed_count)
                .finish_non_exhaustive(),
            Self::MemoryTidyResult {
                bot_id, changes, ..
            } => f
                .debug_struct("MemoryTidyResult")
                .field("bot_id", bot_id)
                .field("changes", changes)
                .finish_non_exhaustive(),
            Self::LookAroundResult {
                bot_id,
                suggestions,
            } => f
                .debug_struct("LookAroundResult")
                .field("bot_id", bot_id)
                .field("suggestions", &suggestions.len())
                .finish(),
            Self::Items(items) => f.debug_tuple("Items").field(&items.len()).finish(),
            Self::Item(item) => f.debug_tuple("Item").field(&item.uid).finish(),
            Self::Groups(groups) => f.debug_tuple("Groups").field(&groups.len()).finish(),
            Self::Group(group) => f.debug_tuple("Group").field(&group.id).finish(),
            Self::MailSent { message_id } => f
                .debug_struct("MailSent")
                .field("message_id", message_id)
                .finish(),
            Self::ReplyPosted { external_ref } => f
                .debug_struct("ReplyPosted")
                .field("external_ref", external_ref)
                .finish(),
            Self::ConnectorStatuses(list) => f
                .debug_tuple("ConnectorStatuses")
                .field(&list.len())
                .finish(),
            Self::AccessRequests(list) => {
                f.debug_tuple("AccessRequests").field(&list.len()).finish()
            }
            Self::AccessRequestDecided(req) => f
                .debug_struct("AccessRequestDecided")
                .field("bot_id", &req.bot_id)
                .field("status", &req.status)
                .finish(),
            Self::GoogleOAuthStarted { .. } => f
                .debug_struct("GoogleOAuthStarted")
                .field("auth_url", &"<redacted>")
                .field("state", &"<redacted>")
                .finish(),
            Self::SyncTriggered {
                bot_id,
                sources_synced,
            } => f
                .debug_struct("SyncTriggered")
                .field("bot_id", bot_id)
                .field("sources_synced", sources_synced)
                .finish(),
            Self::ToolRelayStatus(status) => {
                f.debug_tuple("ToolRelayStatus").field(status).finish()
            }
            Self::ExportedZip {
                filename,
                zip_bytes,
            } => f
                .debug_struct("ExportedZip")
                .field("filename", filename)
                .field("bytes", &zip_bytes.len())
                .finish(),
            Self::ImportedExternalBots {
                candidates,
                imported,
            } => f
                .debug_struct("ImportedExternalBots")
                .field("candidates", &candidates.len())
                .field("imported", &imported.len())
                .finish(),
        }
    }
}

impl Msg for BotRpcRequest {
    const KIND: u8 = BOT_REQ;
}

impl Msg for BotRpcResponse {
    const KIND: u8 = BOT_REPLY;
}

fn rpc_zip_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    bounded::<D, u8, MAX_RPC_ZIP_BYTES>(d)
}
