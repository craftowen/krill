// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Versioned IPC RPC request/response protocol for Krill Bots (Phases 1-3).
//!
//! Negotiated with [`crate::ipc::FEAT_BOTS`] (`1 << 40`).
//! Supports both dedicated client-channel frames (`BOT_REQ = 0x48`, `BOT_REPLY = 0x68`)
//! and control-channel multiplexing (`control::Request::Bot` / `control::Reply::Bot`).
//! Append new enum variants only at the end to preserve postcard wire compatibility.

use std::cell::Cell;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::SessionId;
use crate::control::bounded;
use crate::screen::Msg;

use super::package::BotPackageExportOptions;
use super::types::{
    BotAccessRequest, BotAvatarData, BotConfig, BotConnectorConfig, BotConnectorStatus,
    BotConnectorTestResult, BotEventRuleConfig, BotGroupChat, BotGroupMessage,
    BotGroupReplyAudience, BotGroupReplyMode, BotGroupRoomView, BotGroupSummary, BotGroupView,
    BotId, BotItem, BotItemDecision, BotItemLink, BotItemStatus, BotItemsPage, BotItemsStale,
    BotLookAroundConfig, BotMcpOAuthState, BotMcpToolConfig, BotMember, BotMemoryHistoryEntry,
    BotMemoryTidyConfig, BotMemoryTidyStatus, BotMemoryTopicInfo, BotNotifications, BotOffer,
    BotScheduleConfig, BotSessionSummary, BotSettingsExtras, BotSlug, BotSuggestion,
    BotToolRelayStatus, ComposioConnectionInfo, DefaultTrackerBots, ExternalBotCandidate,
    ExternalBotSourceApp, ExternalSource, LookAroundEvery, TidyEvery,
};

/// Dedicated client-to-daemon frame kind for Krill Bots RPC.
pub const BOT_REQ: u8 = 0x48;
/// Dedicated daemon-to-client frame kind for Krill Bots RPC.
pub const BOT_REPLY: u8 = 0x68;

/// Maximum `.zip` payload transferred inside a single IPC frame (below `ipc::MAX_FRAME` = 1 MiB).
pub const MAX_RPC_ZIP_BYTES: usize = 768 * 1024;
/// Maximum PNG avatar payload transferred inside a single IPC frame (512 KiB, leaving headroom
/// for Store-mode ZIP export within `MAX_RPC_ZIP_BYTES` and Mobile base64 within `ipc::MAX_FRAME`).
pub const MAX_RPC_AVATAR_PNG_BYTES: usize = super::MAX_AVATAR_PNG_BYTES;
/// Maximum number of client capability tokens allowed on [`BotRpcRequest::WithCapabilities`].
pub const MAX_BOT_CAPABILITIES: usize = 64;
/// Maximum UTF-8 byte length of a single client capability token.
pub const MAX_BOT_CAPABILITY_LEN: usize = 128;

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

/// Extended parameters for creating a bot (`CreateBotV2`) with explicit slug, color, skills, and events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBotV2Params {
    pub base: CreateBotParams,
    #[serde(default)]
    pub slug: Option<BotSlug>,
    #[serde(default)]
    pub avatar_color: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub events: Vec<BotEventRuleConfig>,
}

impl From<CreateBotParams> for CreateBotV2Params {
    fn from(base: CreateBotParams) -> Self {
        Self {
            base,
            slug: None,
            avatar_color: None,
            skills: Vec::new(),
            events: Vec::new(),
        }
    }
}

/// Partial update parameters (`UpdateBotExtras`) for avatar color, skills, and event rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateBotExtrasParams {
    pub bot_id: BotId,
    #[serde(default)]
    pub avatar_color: FieldPatch<String>,
    #[serde(default)]
    pub skills: Option<Vec<String>>,
    #[serde(default)]
    pub events: Option<Vec<BotEventRuleConfig>>,
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
///
/// On binary postcard wire formats, `waiting_reason` is omitted from the frozen 6-field
/// layout and carried as an optional trailing wire field by [`BotRpcRequest::encode_wire_body`]
/// / [`BotRpcRequest::decode_wire_body`] to preserve N-1 compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateBotItemParams {
    pub bot_id: BotId,
    pub title: String,
    pub description: Option<String>,
    pub done_means: Option<String>,
    pub status: Option<BotItemStatus>,
    pub links: Vec<BotItemLink>,
    pub waiting_reason: Option<String>,
}

impl Serialize for CreateBotItemParams {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct WireV0317<'a> {
            bot_id: &'a BotId,
            title: &'a str,
            description: &'a Option<String>,
            done_means: &'a Option<String>,
            status: Option<BotItemStatus>,
            links: &'a [BotItemLink],
        }
        #[derive(Serialize)]
        struct Human<'a> {
            bot_id: &'a BotId,
            title: &'a str,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            description: &'a Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            done_means: &'a Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            status: Option<BotItemStatus>,
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            links: &'a Vec<BotItemLink>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            waiting_reason: &'a Option<String>,
        }
        if serializer.is_human_readable() {
            Human {
                bot_id: &self.bot_id,
                title: &self.title,
                description: &self.description,
                done_means: &self.done_means,
                status: self.status,
                links: &self.links,
                waiting_reason: &self.waiting_reason,
            }
            .serialize(serializer)
        } else {
            WireV0317 {
                bot_id: &self.bot_id,
                title: &self.title,
                description: &self.description,
                done_means: &self.done_means,
                status: self.status,
                links: &self.links,
            }
            .serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for CreateBotItemParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct WireV0317 {
            bot_id: BotId,
            title: String,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            done_means: Option<String>,
            #[serde(default)]
            status: Option<BotItemStatus>,
            #[serde(default)]
            links: Vec<BotItemLink>,
        }
        #[derive(Deserialize)]
        struct Human {
            bot_id: BotId,
            title: String,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            done_means: Option<String>,
            #[serde(default)]
            status: Option<BotItemStatus>,
            #[serde(default)]
            links: Vec<BotItemLink>,
            #[serde(default)]
            waiting_reason: Option<String>,
        }
        if deserializer.is_human_readable() {
            let h = Human::deserialize(deserializer)?;
            Ok(Self {
                bot_id: h.bot_id,
                title: h.title,
                description: h.description,
                done_means: h.done_means,
                status: h.status,
                links: h.links,
                waiting_reason: h.waiting_reason,
            })
        } else {
            let w = WireV0317::deserialize(deserializer)?;
            Ok(Self {
                bot_id: w.bot_id,
                title: w.title,
                description: w.description,
                done_means: w.done_means,
                status: w.status,
                links: w.links,
                waiting_reason: None,
            })
        }
    }
}

/// Parameters for updating a tracked bot item.
///
/// On binary postcard wire formats, `waiting_reason` is omitted from the frozen 9-field
/// layout and carried as an optional trailing wire field by [`BotRpcRequest::encode_wire_body`]
/// / [`BotRpcRequest::decode_wire_body`] to preserve N-1 compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateBotItemParams {
    pub item_id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub done_means: Option<String>,
    pub status: Option<BotItemStatus>,
    pub comment: Option<String>,
    pub decisions: Vec<BotItemDecision>,
    pub close_note: Option<String>,
    pub snoozed_until_ms: FieldPatch<u64>,
    pub waiting_reason: Option<String>,
}

impl Serialize for UpdateBotItemParams {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct WireV0317<'a> {
            item_id: &'a str,
            title: &'a Option<String>,
            description: &'a Option<String>,
            done_means: &'a Option<String>,
            status: Option<BotItemStatus>,
            comment: &'a Option<String>,
            decisions: &'a [BotItemDecision],
            close_note: &'a Option<String>,
            snoozed_until_ms: &'a FieldPatch<u64>,
        }
        #[derive(Serialize)]
        struct Human<'a> {
            item_id: &'a str,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            title: &'a Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            description: &'a Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            done_means: &'a Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            status: Option<BotItemStatus>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            comment: &'a Option<String>,
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            decisions: &'a Vec<BotItemDecision>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            close_note: &'a Option<String>,
            #[serde(default)]
            snoozed_until_ms: &'a FieldPatch<u64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            waiting_reason: &'a Option<String>,
        }
        if serializer.is_human_readable() {
            Human {
                item_id: &self.item_id,
                title: &self.title,
                description: &self.description,
                done_means: &self.done_means,
                status: self.status,
                comment: &self.comment,
                decisions: &self.decisions,
                close_note: &self.close_note,
                snoozed_until_ms: &self.snoozed_until_ms,
                waiting_reason: &self.waiting_reason,
            }
            .serialize(serializer)
        } else {
            WireV0317 {
                item_id: &self.item_id,
                title: &self.title,
                description: &self.description,
                done_means: &self.done_means,
                status: self.status,
                comment: &self.comment,
                decisions: &self.decisions,
                close_note: &self.close_note,
                snoozed_until_ms: &self.snoozed_until_ms,
            }
            .serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for UpdateBotItemParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct WireV0317 {
            item_id: String,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            done_means: Option<String>,
            #[serde(default)]
            status: Option<BotItemStatus>,
            #[serde(default)]
            comment: Option<String>,
            #[serde(default)]
            decisions: Vec<BotItemDecision>,
            #[serde(default)]
            close_note: Option<String>,
            #[serde(default)]
            snoozed_until_ms: FieldPatch<u64>,
        }
        #[derive(Deserialize)]
        struct Human {
            item_id: String,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            done_means: Option<String>,
            #[serde(default)]
            status: Option<BotItemStatus>,
            #[serde(default)]
            comment: Option<String>,
            #[serde(default)]
            decisions: Vec<BotItemDecision>,
            #[serde(default)]
            close_note: Option<String>,
            #[serde(default)]
            snoozed_until_ms: FieldPatch<u64>,
            #[serde(default)]
            waiting_reason: Option<String>,
        }
        if deserializer.is_human_readable() {
            let h = Human::deserialize(deserializer)?;
            Ok(Self {
                item_id: h.item_id,
                title: h.title,
                description: h.description,
                done_means: h.done_means,
                status: h.status,
                comment: h.comment,
                decisions: h.decisions,
                close_note: h.close_note,
                snoozed_until_ms: h.snoozed_until_ms,
                waiting_reason: h.waiting_reason,
            })
        } else {
            let w = WireV0317::deserialize(deserializer)?;
            Ok(Self {
                item_id: w.item_id,
                title: w.title,
                description: w.description,
                done_means: w.done_means,
                status: w.status,
                comment: w.comment,
                decisions: w.decisions,
                close_note: w.close_note,
                snoozed_until_ms: w.snoozed_until_ms,
                waiting_reason: None,
            })
        }
    }
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
    CreateBotV2(Box<CreateBotV2Params>),
    UpdateBotExtras(Box<UpdateBotExtrasParams>),
    ImportBotZipV2 {
        #[serde(deserialize_with = "rpc_zip_bytes")]
        zip_bytes: Vec<u8>,
        #[serde(default)]
        name_override: Option<String>,
        #[serde(default)]
        slug_override: Option<BotSlug>,
        #[serde(default)]
        agent_override: Option<String>,
    },
    UploadBotAvatar {
        bot_id: BotId,
        #[serde(deserialize_with = "rpc_avatar_bytes")]
        png_bytes: Vec<u8>,
    },
    GetBotAvatar {
        bot_id: BotId,
    },
    ListItemsPage {
        #[serde(default)]
        bot_id: Option<BotId>,
        include_closed: bool,
        include_snoozed: bool,
        #[serde(default)]
        limit: Option<u32>,
        #[serde(default)]
        cursor: Option<String>,
    },
    GetBotItem {
        item_id: String,
    },
    AnswerBotItem {
        item_id: String,
        #[serde(default)]
        answer_text: Option<String>,
        #[serde(default)]
        option_index: Option<u32>,
        #[serde(default)]
        decision_id: Option<String>,
        #[serde(default)]
        request_id: Option<String>,
    },
    MarkBotItemsOpened {
        bot_id: BotId,
        #[serde(default)]
        up_to_event_id: Option<String>,
    },
    GetMemoryHistory {
        bot_id: BotId,
        #[serde(default)]
        limit: Option<u32>,
    },
    EditMemoryFact {
        bot_id: BotId,
        old_fact: String,
        new_fact: String,
    },
    UndoMemoryChange {
        bot_id: BotId,
        entry_id: String,
    },
    GetMemoryTidyStatus {
        bot_id: BotId,
    },
    UndoMemoryTidy {
        bot_id: BotId,
        #[serde(default)]
        tidy_id: Option<String>,
    },
    ListMemoryTopics {
        bot_id: BotId,
    },
    GetMemoryTopic {
        bot_id: BotId,
        topic: String,
    },
    SetMemoryTopic {
        bot_id: BotId,
        topic: String,
        content: String,
    },
    ListSuggestions {
        #[serde(default)]
        bot_id: Option<BotId>,
        include_dismissed: bool,
    },
    DismissSuggestion {
        bot_id: BotId,
        suggestion_id: String,
    },
    StartSuggestion {
        bot_id: BotId,
        suggestion_id: String,
    },
    MarkUpNextSeen {
        #[serde(default)]
        bot_id: Option<BotId>,
    },
    ListBotSessions {
        #[serde(default)]
        bot_id: Option<BotId>,
    },
    ResetBotChat {
        bot_id: BotId,
        #[serde(default)]
        session_id: Option<SessionId>,
    },
    StopBotSession {
        bot_id: BotId,
        session_id: SessionId,
    },
    RetryBotKickoff {
        bot_id: BotId,
    },
    TakeOverWorkspace {
        bot_id: BotId,
        session_id: SessionId,
    },
    SendBotRunAgain {
        bot_id: BotId,
        session_id: SessionId,
    },
    UpdateGroup {
        group_id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        bot_ids: Option<Vec<BotId>>,
    },
    DeleteGroup {
        group_id: String,
    },
    ReorderGroups {
        group_ids: Vec<String>,
    },
    NewGroupThread {
        group_id: String,
        #[serde(default)]
        title: Option<String>,
    },
    ReadGroup {
        group_id: String,
        #[serde(default)]
        thread_id: Option<String>,
        #[serde(default)]
        limit: Option<u32>,
    },
    SendGroupMessage {
        group_id: String,
        #[serde(default)]
        thread_id: Option<String>,
        sender: String,
        text: String,
        #[serde(default)]
        reply_mode: Option<BotGroupReplyMode>,
    },
    StopGroup {
        group_id: String,
        #[serde(default)]
        thread_id: Option<String>,
    },
    TestConnector {
        bot_id: BotId,
        source: ExternalSource,
    },
    RedeemPairingCode {
        code: String,
    },
    StartMcpOAuth {
        bot_id: BotId,
        tool_id: String,
    },
    McpOAuthStatus {
        bot_id: BotId,
        tool_id: String,
    },
    CancelMcpOAuth {
        bot_id: BotId,
        tool_id: String,
    },
    ListComposioConnections {
        bot_id: BotId,
    },
    StartComposioConnect {
        bot_id: BotId,
        app_id: String,
    },
    DisconnectComposioAccount {
        bot_id: BotId,
        connection_id: String,
    },
    SetDefaultTrackerBot {
        source: ExternalSource,
        #[serde(default)]
        bot_id: Option<BotId>,
    },
    GetDefaultTrackerBots,
    /// Models a new bot on this computer can launch `agent_id` with, from the same source the
    /// chat composer's model picker uses.
    ListAgentModels {
        agent_id: String,
    },
    /// When the bot's automations ran last and run next, for its Home (Orca's Automations card
    /// and the header's "Idle · next 8:00 PM · Look around").
    GetBotUpcoming {
        bot_id: BotId,
    },
    /// Runs one of the bot's schedules now, as its cron would.
    RunScheduleNow {
        bot_id: BotId,
        schedule_id: String,
    },
    /// Settings › Items, Look around and Memory upkeep presets (Orca's `itemsStale`, `every`).
    GetBotSettingsExtras {
        bot_id: BotId,
    },
    /// Changes only the fields that are `Some`; answers with the bot's extras after it.
    UpdateBotSettingsExtras {
        bot_id: BotId,
        #[serde(default)]
        items_stale: Option<BotItemsStale>,
        #[serde(default)]
        look_around_every: Option<LookAroundEvery>,
        #[serde(default)]
        tidy_every: Option<TidyEvery>,
    },
    /// Settings › Who can ask: takes one person off the bot; people join only by an approved
    /// access request. Answers with the bot.
    RemoveBotMember {
        bot_id: BotId,
        member: BotMember,
    },
    /// A bot proposes a new bot (Orca `bots.createOffer`): its chat shows a card, and nothing is
    /// created until the owner presses Create bot.
    OfferBot {
        bot_id: BotId,
        name: String,
        purpose: String,
    },
    /// The bot's proposals, newest last (its chat's create offer cards).
    ListBotOffers {
        bot_id: BotId,
    },
    /// The owner answered a proposal: `created` names the bot made from it, `None` is Not now.
    ResolveBotOffer {
        offer_id: String,
        #[serde(default)]
        created: Option<BotId>,
    },
    /// Lists external bot source paths already imported into active Krill bots.
    ListImportedSources,
    /// Imports a single discovered external candidate even if an earlier bot came from the same
    /// folder, applying the owner's chosen name, mascot/color, and agent settings.
    ImportExternalBotCopy {
        source: Option<ExternalBotSourceApp>,
        path: String,
        params: Box<CreateBotV2Params>,
    },
    /// Lists multi-bot groups with reply audience, threads, latest message, and active turns.
    ListGroupSummaries,
    /// Reads a paginated multi-bot group room page with active turns and persisted reply audience.
    ReadGroupRoom {
        group_id: String,
        #[serde(default)]
        thread_id: Option<String>,
        #[serde(default)]
        before_message_id: Option<String>,
        #[serde(default)]
        limit: Option<u32>,
    },
    /// Updates the default reply audience for a group chat room.
    SetGroupReplyAudience {
        group_id: String,
        audience: BotGroupReplyAudience,
    },
    /// Posts a message to a group room using an explicit [`BotGroupReplyAudience`].
    SendGroupMessageWithAudience {
        group_id: String,
        #[serde(default)]
        thread_id: Option<String>,
        sender: String,
        text: String,
        audience: BotGroupReplyAudience,
    },
    /// Stops either one member bot or all active bots in a group thread.
    StopGroupMember {
        group_id: String,
        #[serde(default)]
        thread_id: Option<String>,
        #[serde(default)]
        bot_id: Option<BotId>,
    },
    /// Wraps a bot RPC request with client capability strings (e.g. `bots.item-waiting.v1`)
    /// so the daemon can preserve or downgrade newer item states cleanly.
    /// Maximum nesting depth is 1; nested `WithCapabilities` wrappers are rejected on decode.
    WithCapabilities {
        #[serde(default, deserialize_with = "rpc_capabilities")]
        capabilities: Vec<String>,
        #[serde(deserialize_with = "rpc_inner_request")]
        request: Box<BotRpcRequest>,
    },
}

impl BotRpcRequest {
    /// Returns `true` for read-only requests safe for `Role::Observer` clients.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        let mut current = self;
        while let Self::WithCapabilities { request, .. } = current {
            current = request;
        }
        matches!(
            current,
            Self::ListBots
                | Self::GetBot { .. }
                | Self::GetSoul { .. }
                | Self::GetMemory { .. }
                | Self::ListItems { .. }
                | Self::ListGroups
                | Self::ConnectorStatus { .. }
                | Self::ToolRelayStatus
                | Self::ImportExternalBots { dry_run: true, .. }
                | Self::GetBotAvatar { .. }
                | Self::ListItemsPage { .. }
                | Self::GetBotItem { .. }
                | Self::GetMemoryHistory { .. }
                | Self::GetMemoryTidyStatus { .. }
                | Self::ListMemoryTopics { .. }
                | Self::GetMemoryTopic { .. }
                | Self::ListSuggestions { .. }
                | Self::ListBotSessions { .. }
                | Self::ReadGroup { .. }
                | Self::McpOAuthStatus { .. }
                | Self::ListComposioConnections { .. }
                | Self::GetDefaultTrackerBots
                | Self::ListAgentModels { .. }
                | Self::GetBotUpcoming { .. }
                | Self::GetBotSettingsExtras { .. }
                | Self::ListBotOffers { .. }
                | Self::ListImportedSources
                | Self::ListGroupSummaries
                | Self::ReadGroupRoom { .. }
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
            Self::CreateBotV2(p) => f.debug_tuple("CreateBotV2").field(p).finish(),
            Self::UpdateBotExtras(p) => f.debug_tuple("UpdateBotExtras").field(p).finish(),
            Self::ImportBotZipV2 {
                zip_bytes,
                name_override,
                slug_override,
                agent_override,
            } => f
                .debug_struct("ImportBotZipV2")
                .field("zip_bytes", &zip_bytes.len())
                .field("name_override", name_override)
                .field("slug_override", slug_override)
                .field("agent_override", agent_override)
                .finish(),
            Self::UploadBotAvatar { bot_id, png_bytes } => f
                .debug_struct("UploadBotAvatar")
                .field("bot_id", bot_id)
                .field("bytes", &png_bytes.len())
                .finish(),
            Self::GetBotAvatar { bot_id } => f
                .debug_struct("GetBotAvatar")
                .field("bot_id", bot_id)
                .finish(),
            Self::ListItemsPage {
                bot_id,
                include_closed,
                include_snoozed,
                limit,
                cursor,
            } => f
                .debug_struct("ListItemsPage")
                .field("bot_id", bot_id)
                .field("include_closed", include_closed)
                .field("include_snoozed", include_snoozed)
                .field("limit", limit)
                .field("cursor", cursor)
                .finish(),
            Self::GetBotItem { item_id } => f
                .debug_struct("GetBotItem")
                .field("item_id", item_id)
                .finish(),
            Self::AnswerBotItem {
                item_id,
                option_index,
                decision_id,
                request_id,
                ..
            } => f
                .debug_struct("AnswerBotItem")
                .field("item_id", item_id)
                .field("option_index", option_index)
                .field("decision_id", decision_id)
                .field("request_id", request_id)
                .finish_non_exhaustive(),
            Self::MarkBotItemsOpened {
                bot_id,
                up_to_event_id,
            } => f
                .debug_struct("MarkBotItemsOpened")
                .field("bot_id", bot_id)
                .field("up_to_event_id", up_to_event_id)
                .finish(),
            Self::GetMemoryHistory { bot_id, limit } => f
                .debug_struct("GetMemoryHistory")
                .field("bot_id", bot_id)
                .field("limit", limit)
                .finish(),
            Self::EditMemoryFact { bot_id, .. } => f
                .debug_struct("EditMemoryFact")
                .field("bot_id", bot_id)
                .finish_non_exhaustive(),
            Self::UndoMemoryChange { bot_id, entry_id } => f
                .debug_struct("UndoMemoryChange")
                .field("bot_id", bot_id)
                .field("entry_id", entry_id)
                .finish(),
            Self::GetMemoryTidyStatus { bot_id } => f
                .debug_struct("GetMemoryTidyStatus")
                .field("bot_id", bot_id)
                .finish(),
            Self::UndoMemoryTidy { bot_id, tidy_id } => f
                .debug_struct("UndoMemoryTidy")
                .field("bot_id", bot_id)
                .field("tidy_id", tidy_id)
                .finish(),
            Self::ListMemoryTopics { bot_id } => f
                .debug_struct("ListMemoryTopics")
                .field("bot_id", bot_id)
                .finish(),
            Self::GetMemoryTopic { bot_id, topic } => f
                .debug_struct("GetMemoryTopic")
                .field("bot_id", bot_id)
                .field("topic", topic)
                .finish(),
            Self::SetMemoryTopic {
                bot_id,
                topic,
                content,
            } => f
                .debug_struct("SetMemoryTopic")
                .field("bot_id", bot_id)
                .field("topic", topic)
                .field("bytes", &content.len())
                .finish(),
            Self::ListSuggestions {
                bot_id,
                include_dismissed,
            } => f
                .debug_struct("ListSuggestions")
                .field("bot_id", bot_id)
                .field("include_dismissed", include_dismissed)
                .finish(),
            Self::DismissSuggestion {
                bot_id,
                suggestion_id,
            } => f
                .debug_struct("DismissSuggestion")
                .field("bot_id", bot_id)
                .field("suggestion_id", suggestion_id)
                .finish(),
            Self::StartSuggestion {
                bot_id,
                suggestion_id,
            } => f
                .debug_struct("StartSuggestion")
                .field("bot_id", bot_id)
                .field("suggestion_id", suggestion_id)
                .finish(),
            Self::MarkUpNextSeen { bot_id } => f
                .debug_struct("MarkUpNextSeen")
                .field("bot_id", bot_id)
                .finish(),
            Self::ListBotSessions { bot_id } => f
                .debug_struct("ListBotSessions")
                .field("bot_id", bot_id)
                .finish(),
            Self::ResetBotChat { bot_id, session_id } => f
                .debug_struct("ResetBotChat")
                .field("bot_id", bot_id)
                .field("session_id", session_id)
                .finish(),
            Self::StopBotSession { bot_id, session_id } => f
                .debug_struct("StopBotSession")
                .field("bot_id", bot_id)
                .field("session_id", session_id)
                .finish(),
            Self::RetryBotKickoff { bot_id } => f
                .debug_struct("RetryBotKickoff")
                .field("bot_id", bot_id)
                .finish(),
            Self::TakeOverWorkspace { bot_id, session_id } => f
                .debug_struct("TakeOverWorkspace")
                .field("bot_id", bot_id)
                .field("session_id", session_id)
                .finish(),
            Self::SendBotRunAgain { bot_id, session_id } => f
                .debug_struct("SendBotRunAgain")
                .field("bot_id", bot_id)
                .field("session_id", session_id)
                .finish(),
            Self::UpdateGroup {
                group_id,
                name,
                bot_ids,
                ..
            } => f
                .debug_struct("UpdateGroup")
                .field("group_id", group_id)
                .field("name", name)
                .field("bot_ids", bot_ids)
                .finish_non_exhaustive(),
            Self::DeleteGroup { group_id } => f
                .debug_struct("DeleteGroup")
                .field("group_id", group_id)
                .finish(),
            Self::ReorderGroups { group_ids } => f
                .debug_struct("ReorderGroups")
                .field("group_ids", group_ids)
                .finish(),
            Self::NewGroupThread { group_id, title } => f
                .debug_struct("NewGroupThread")
                .field("group_id", group_id)
                .field("title", title)
                .finish(),
            Self::ReadGroup {
                group_id,
                thread_id,
                limit,
            } => f
                .debug_struct("ReadGroup")
                .field("group_id", group_id)
                .field("thread_id", thread_id)
                .field("limit", limit)
                .finish(),
            Self::SendGroupMessage {
                group_id,
                thread_id,
                sender,
                reply_mode,
                ..
            } => f
                .debug_struct("SendGroupMessage")
                .field("group_id", group_id)
                .field("thread_id", thread_id)
                .field("sender", sender)
                .field("reply_mode", reply_mode)
                .finish_non_exhaustive(),
            Self::StopGroup {
                group_id,
                thread_id,
            } => f
                .debug_struct("StopGroup")
                .field("group_id", group_id)
                .field("thread_id", thread_id)
                .finish(),
            Self::TestConnector { bot_id, source } => f
                .debug_struct("TestConnector")
                .field("bot_id", bot_id)
                .field("source", source)
                .finish(),
            // Never print secret pairing codes in Debug logs.
            Self::RedeemPairingCode { .. } => f
                .debug_struct("RedeemPairingCode")
                .field("code", &"<redacted>")
                .finish(),
            Self::StartMcpOAuth { bot_id, tool_id } => f
                .debug_struct("StartMcpOAuth")
                .field("bot_id", bot_id)
                .field("tool_id", tool_id)
                .finish(),
            Self::McpOAuthStatus { bot_id, tool_id } => f
                .debug_struct("McpOAuthStatus")
                .field("bot_id", bot_id)
                .field("tool_id", tool_id)
                .finish(),
            Self::CancelMcpOAuth { bot_id, tool_id } => f
                .debug_struct("CancelMcpOAuth")
                .field("bot_id", bot_id)
                .field("tool_id", tool_id)
                .finish(),
            Self::ListComposioConnections { bot_id } => f
                .debug_struct("ListComposioConnections")
                .field("bot_id", bot_id)
                .finish(),
            Self::StartComposioConnect { bot_id, app_id } => f
                .debug_struct("StartComposioConnect")
                .field("bot_id", bot_id)
                .field("app_id", app_id)
                .finish(),
            Self::DisconnectComposioAccount {
                bot_id,
                connection_id,
            } => f
                .debug_struct("DisconnectComposioAccount")
                .field("bot_id", bot_id)
                .field("connection_id", connection_id)
                .finish(),
            Self::SetDefaultTrackerBot { source, bot_id } => f
                .debug_struct("SetDefaultTrackerBot")
                .field("source", source)
                .field("bot_id", bot_id)
                .finish(),
            Self::GetDefaultTrackerBots => f.write_str("GetDefaultTrackerBots"),
            Self::ListAgentModels { agent_id } => f
                .debug_struct("ListAgentModels")
                .field("agent_id", agent_id)
                .finish(),
            Self::GetBotUpcoming { bot_id } => f
                .debug_struct("GetBotUpcoming")
                .field("bot_id", bot_id)
                .finish(),
            Self::RunScheduleNow {
                bot_id,
                schedule_id,
            } => f
                .debug_struct("RunScheduleNow")
                .field("bot_id", bot_id)
                .field("schedule_id", schedule_id)
                .finish(),
            Self::GetBotSettingsExtras { bot_id } => f
                .debug_struct("GetBotSettingsExtras")
                .field("bot_id", bot_id)
                .finish(),
            Self::UpdateBotSettingsExtras {
                bot_id,
                items_stale,
                look_around_every,
                tidy_every,
            } => f
                .debug_struct("UpdateBotSettingsExtras")
                .field("bot_id", bot_id)
                .field("items_stale", items_stale)
                .field("look_around_every", look_around_every)
                .field("tidy_every", tidy_every)
                .finish(),
            Self::RemoveBotMember { bot_id, member } => f
                .debug_struct("RemoveBotMember")
                .field("bot_id", bot_id)
                .field("member", &member.id)
                .finish(),
            Self::OfferBot {
                bot_id,
                name,
                purpose,
            } => f
                .debug_struct("OfferBot")
                .field("bot_id", bot_id)
                .field("name", name)
                .field("purpose_bytes", &purpose.len())
                .finish(),
            Self::ListBotOffers { bot_id } => f
                .debug_struct("ListBotOffers")
                .field("bot_id", bot_id)
                .finish(),
            Self::ResolveBotOffer { offer_id, created } => f
                .debug_struct("ResolveBotOffer")
                .field("offer_id", offer_id)
                .field("created", created)
                .finish(),
            Self::ListImportedSources => f.write_str("ListImportedSources"),
            Self::ImportExternalBotCopy {
                source,
                path,
                params,
            } => f
                .debug_struct("ImportExternalBotCopy")
                .field("source", source)
                .field("path", path)
                .field("name", &params.base.name)
                .finish(),
            Self::ListGroupSummaries => f.write_str("ListGroupSummaries"),
            Self::ReadGroupRoom {
                group_id,
                thread_id,
                before_message_id,
                limit,
            } => f
                .debug_struct("ReadGroupRoom")
                .field("group_id", group_id)
                .field("thread_id", thread_id)
                .field("before_message_id", before_message_id)
                .field("limit", limit)
                .finish(),
            Self::SetGroupReplyAudience { group_id, audience } => f
                .debug_struct("SetGroupReplyAudience")
                .field("group_id", group_id)
                .field("audience", audience)
                .finish(),
            Self::SendGroupMessageWithAudience {
                group_id,
                thread_id,
                sender,
                audience,
                ..
            } => f
                .debug_struct("SendGroupMessageWithAudience")
                .field("group_id", group_id)
                .field("thread_id", thread_id)
                .field("sender", sender)
                .field("audience", audience)
                .finish_non_exhaustive(),
            Self::StopGroupMember {
                group_id,
                thread_id,
                bot_id,
            } => f
                .debug_struct("StopGroupMember")
                .field("group_id", group_id)
                .field("thread_id", thread_id)
                .field("bot_id", bot_id)
                .finish(),
            Self::WithCapabilities {
                capabilities,
                request,
            } => f
                .debug_struct("WithCapabilities")
                .field("capabilities", capabilities)
                .field("request", request)
                .finish(),
        }
    }
}

/// One model a bot can launch with: `id` is what the agent CLI's `--model` accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotAgentModel {
    pub id: String,
    pub label: String,
}

/// Reply to [`BotRpcRequest::ListAgentModels`]. An empty `models` means "nothing discovered":
/// the client keeps its built-in list, and `reason` says why when the agent reported one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotAgentModels {
    pub models: Vec<BotAgentModel>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Reply to [`BotRpcRequest::GetBotUpcoming`]: each automation's last and next run (Unix ms).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotUpcoming {
    #[serde(default)]
    pub look_around_last_ms: Option<u64>,
    /// Ahead of now; `None` before its first look (it runs once the bot is used).
    #[serde(default)]
    pub look_around_next_ms: Option<u64>,
    #[serde(default)]
    pub tidy_last_ms: Option<u64>,
    #[serde(default)]
    pub tidy_next_ms: Option<u64>,
    #[serde(default)]
    pub schedules: Vec<BotScheduleUpcoming>,
    /// Suggestions made after this were not seen yet (Suggestions' "N new").
    #[serde(default)]
    pub up_next_seen_ms: u64,
    /// Item events after this were not seen yet (a row's dot, Closed's "N new").
    #[serde(default)]
    pub items_seen_event_id: u64,
}

/// One schedule's runs in [`BotUpcoming`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotScheduleUpcoming {
    pub id: String,
    #[serde(default)]
    pub next_ms: Option<u64>,
    #[serde(default)]
    pub last_ms: Option<u64>,
    /// Its conversation is on a turn right now.
    #[serde(default)]
    pub running: bool,
    /// Last start/run error sentence, if the most recent attempt could not start or failed.
    #[serde(default)]
    pub last_error: Option<String>,
    /// Structured refusal for the most recent failed attempt so each reader can word it locally.
    #[serde(default)]
    pub last_refusal: Option<super::BotSessionRefusal>,
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
    BotAvatar(BotAvatarData),
    ItemsPage(BotItemsPage),
    MemoryHistory(Vec<BotMemoryHistoryEntry>),
    MemoryTidyStatus(BotMemoryTidyStatus),
    MemoryTopics(Vec<BotMemoryTopicInfo>),
    Suggestions(Vec<BotSuggestion>),
    BotSessions(Vec<BotSessionSummary>),
    GroupView(BotGroupView),
    GroupMessagePosted {
        message: BotGroupMessage,
        dispatched_bots: Vec<BotId>,
    },
    ConnectorTest(BotConnectorTestResult),
    McpOAuth(BotMcpOAuthState),
    ComposioConnections(Vec<ComposioConnectionInfo>),
    ComposioConnectStarted {
        connection_id: String,
        redirect_url: String,
    },
    DefaultTrackerBots(DefaultTrackerBots),
    AgentModels(BotAgentModels),
    Upcoming(BotUpcoming),
    BotSettingsExtras(BotSettingsExtras),
    BotOffers(Vec<BotOffer>),
    ImportedSources(Vec<(String, BotId)>),
    GroupSummaries(Vec<BotGroupSummary>),
    GroupRoom(BotGroupRoomView),
    GroupReplyAudienceUpdated {
        group_id: String,
        audience: BotGroupReplyAudience,
    },
    /// Structured refusal when a bot chat or schedule run could not start (`b10792b6`).
    SessionRefused(Box<super::BotSessionRefusal>),
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
            Self::BotAvatar(data) => f.debug_tuple("BotAvatar").field(data).finish(),
            Self::ItemsPage(page) => f
                .debug_struct("ItemsPage")
                .field("items", &page.items.len())
                .field("next_cursor", &page.next_cursor)
                .field("total_count", &page.total_count)
                .field("unseen_count", &page.unseen_count)
                .finish(),
            Self::MemoryHistory(entries) => f
                .debug_tuple("MemoryHistory")
                .field(&entries.len())
                .finish(),
            Self::MemoryTidyStatus(status) => {
                f.debug_tuple("MemoryTidyStatus").field(status).finish()
            }
            Self::MemoryTopics(topics) => {
                f.debug_tuple("MemoryTopics").field(&topics.len()).finish()
            }
            Self::Suggestions(suggestions) => f
                .debug_tuple("Suggestions")
                .field(&suggestions.len())
                .finish(),
            Self::BotSessions(sessions) => {
                f.debug_tuple("BotSessions").field(&sessions.len()).finish()
            }
            Self::GroupView(view) => f
                .debug_struct("GroupView")
                .field("group_id", &view.group.id)
                .field("threads", &view.threads.len())
                .field("messages", &view.messages.len())
                .field("active_run", &view.active_run)
                .finish(),
            Self::GroupMessagePosted {
                message,
                dispatched_bots,
            } => f
                .debug_struct("GroupMessagePosted")
                .field("message_id", &message.message_id)
                .field("group_id", &message.group_id)
                .field("thread_id", &message.thread_id)
                .field("dispatched_bots", dispatched_bots)
                .finish(),
            Self::ConnectorTest(res) => f.debug_tuple("ConnectorTest").field(res).finish(),
            Self::McpOAuth(state) => f.debug_tuple("McpOAuth").field(state).finish(),
            Self::ComposioConnections(conns) => f
                .debug_tuple("ComposioConnections")
                .field(&conns.len())
                .finish(),
            // Never print OAuth/connect redirect URLs in Debug logs.
            Self::ComposioConnectStarted { connection_id, .. } => f
                .debug_struct("ComposioConnectStarted")
                .field("connection_id", connection_id)
                .field("redirect_url", &"<redacted>")
                .finish(),
            Self::DefaultTrackerBots(defaults) => {
                f.debug_tuple("DefaultTrackerBots").field(defaults).finish()
            }
            Self::AgentModels(models) => f.debug_tuple("AgentModels").field(models).finish(),
            Self::Upcoming(up) => f.debug_tuple("Upcoming").field(up).finish(),
            Self::BotSettingsExtras(extras) => {
                f.debug_tuple("BotSettingsExtras").field(extras).finish()
            }
            Self::BotOffers(offers) => f.debug_tuple("BotOffers").field(offers).finish(),
            Self::ImportedSources(sources) => f
                .debug_tuple("ImportedSources")
                .field(&sources.len())
                .finish(),
            Self::GroupSummaries(groups) => f
                .debug_tuple("GroupSummaries")
                .field(&groups.len())
                .finish(),
            Self::GroupRoom(room) => f
                .debug_struct("GroupRoom")
                .field("group_id", &room.group.id)
                .field("active_thread_id", &room.active_thread_id)
                .field("messages", &room.messages.len())
                .field("has_earlier", &room.has_earlier)
                .finish(),
            Self::GroupReplyAudienceUpdated { group_id, audience } => f
                .debug_struct("GroupReplyAudienceUpdated")
                .field("group_id", group_id)
                .field("audience", audience)
                .finish(),
            Self::SessionRefused(refusal) => {
                f.debug_tuple("SessionRefused").field(refusal).finish()
            }
        }
    }
}

/// Postcard variant discriminant for [`BotRpcResponse::Items`] (0-indexed variant 10).
pub(crate) const ITEMS_RESPONSE_TAG: u8 = 10;
/// Postcard variant discriminant for [`BotRpcResponse::Item`] (0-indexed variant 11).
pub(crate) const ITEM_RESPONSE_TAG: u8 = 11;
/// Postcard variant discriminant for [`BotRpcResponse::ItemsPage`] (0-indexed variant 25).
pub(crate) const ITEMS_PAGE_RESPONSE_TAG: u8 = 25;

fn item_has_waiting_meta(item: &BotItem) -> bool {
    item.waiting_reason.is_some() || item.waiting_since_ms.is_some()
}

fn append_waiting_meta(body: &mut Vec<u8>, item: &BotItem) -> Result<(), crate::ipc::Error> {
    body.extend(
        postcard::to_stdvec(&(&item.waiting_reason, item.waiting_since_ms))
            .map_err(crate::ipc::Error::Codec)?,
    );
    Ok(())
}

fn decode_waiting_meta<'a>(
    tail: &'a [u8],
    item: &mut BotItem,
) -> Result<&'a [u8], crate::ipc::Error> {
    let ((reason, since), next) = postcard::take_from_bytes::<(Option<String>, Option<u64>)>(tail)
        .map_err(crate::ipc::Error::Codec)?;
    item.waiting_reason = reason;
    item.waiting_since_ms = since;
    Ok(next)
}

impl BotRpcResponse {
    /// Encodes this response into postcard body bytes, preserving frozen v0.3.17/v0.3.23
    /// layouts while appending trailing extensions (`waiting_reason`/`waiting_since_ms` on
    /// `Item`/`Items`/`ItemsPage`) only when set.
    pub(crate) fn encode_wire_body(&self) -> Result<Vec<u8>, crate::ipc::Error> {
        let mut body = postcard::to_stdvec(self).map_err(crate::ipc::Error::Codec)?;
        match self {
            Self::Item(item) if item_has_waiting_meta(item) => {
                append_waiting_meta(&mut body, item)?;
            }
            Self::Items(items) if items.iter().any(item_has_waiting_meta) => {
                for item in items {
                    append_waiting_meta(&mut body, item)?;
                }
            }
            Self::ItemsPage(page) if page.items.iter().any(item_has_waiting_meta) => {
                for item in &page.items {
                    append_waiting_meta(&mut body, item)?;
                }
            }
            _ => {}
        }
        Ok(body)
    }

    /// Decodes postcard body bytes produced by either this build or N-1 releases.
    pub(crate) fn decode_wire_body(body: &[u8]) -> Result<Self, crate::ipc::Error> {
        if matches!(
            body.first().copied(),
            Some(ITEMS_RESPONSE_TAG | ITEM_RESPONSE_TAG | ITEMS_PAGE_RESPONSE_TAG)
        ) {
            let (mut resp, mut tail) =
                postcard::take_from_bytes::<Self>(body).map_err(crate::ipc::Error::Codec)?;
            if !tail.is_empty() {
                match &mut resp {
                    Self::Item(item) => {
                        let _ = decode_waiting_meta(tail, item)?;
                    }
                    Self::Items(items) => {
                        for item in items {
                            tail = decode_waiting_meta(tail, item)?;
                        }
                    }
                    Self::ItemsPage(page) => {
                        for item in &mut page.items {
                            tail = decode_waiting_meta(tail, item)?;
                        }
                    }
                    _ => {}
                }
            }
            return Ok(resp);
        }
        postcard::from_bytes(body).map_err(crate::ipc::Error::Codec)
    }
}

/// Postcard variant discriminant for [`BotRpcRequest::CreateItem`] (0-indexed variant 14).
pub(crate) const CREATE_ITEM_REQUEST_TAG: u8 = 14;
/// Postcard variant discriminant for [`BotRpcRequest::UpdateItem`] (0-indexed variant 15).
pub(crate) const UPDATE_ITEM_REQUEST_TAG: u8 = 15;
/// Postcard variant discriminant for [`BotRpcRequest::WithCapabilities`] (0-indexed variant 95).
pub(crate) const WITH_CAPABILITIES_REQUEST_TAG: u8 = 95;

#[derive(Deserialize)]
struct BoundedCapabilities(#[serde(deserialize_with = "rpc_capabilities")] Vec<String>);

impl BotRpcRequest {
    /// Encodes this request into postcard body bytes, preserving frozen N-1 layouts for
    /// `CreateItem` / `UpdateItem` and appending `waiting_reason` as an optional trailing field
    /// only when set.
    pub(crate) fn encode_wire_body(&self) -> Result<Vec<u8>, crate::ipc::Error> {
        let inner = match self {
            Self::WithCapabilities {
                capabilities,
                request,
            } => {
                if matches!(request.as_ref(), Self::WithCapabilities { .. })
                    || capabilities.len() > MAX_BOT_CAPABILITIES
                    || capabilities
                        .iter()
                        .any(|c| c.len() > MAX_BOT_CAPABILITY_LEN)
                {
                    return Err(crate::ipc::Error::Codec(
                        postcard::Error::DeserializeBadEncoding,
                    ));
                }
                request.as_ref()
            }
            other => other,
        };
        let mut body = postcard::to_stdvec(self).map_err(crate::ipc::Error::Codec)?;
        match inner {
            Self::CreateItem(params) if params.waiting_reason.is_some() => {
                body.extend(
                    postcard::to_stdvec(&params.waiting_reason)
                        .map_err(crate::ipc::Error::Codec)?,
                );
            }
            Self::UpdateItem(params) if params.waiting_reason.is_some() => {
                body.extend(
                    postcard::to_stdvec(&params.waiting_reason)
                        .map_err(crate::ipc::Error::Codec)?,
                );
            }
            _ => {}
        }
        Ok(body)
    }

    /// Decodes postcard body bytes produced by either this build or N-1 releases,
    /// enforcing a maximum `WithCapabilities` nesting depth of 1 without recursion.
    pub(crate) fn decode_wire_body(body: &[u8]) -> Result<Self, crate::ipc::Error> {
        if body.first() == Some(&WITH_CAPABILITIES_REQUEST_TAG) {
            let (_, rest) =
                postcard::take_from_bytes::<u8>(body).map_err(crate::ipc::Error::Codec)?;
            let (BoundedCapabilities(capabilities), inner_bytes) =
                postcard::take_from_bytes::<BoundedCapabilities>(rest)
                    .map_err(crate::ipc::Error::Codec)?;
            if inner_bytes.first() == Some(&WITH_CAPABILITIES_REQUEST_TAG) {
                return Err(crate::ipc::Error::Codec(
                    postcard::Error::DeserializeBadEncoding,
                ));
            }
            let inner = Self::decode_unwrapped_wire_body(inner_bytes)?;
            return Ok(Self::WithCapabilities {
                capabilities,
                request: Box::new(inner),
            });
        }
        Self::decode_unwrapped_wire_body(body)
    }

    fn decode_unwrapped_wire_body(body: &[u8]) -> Result<Self, crate::ipc::Error> {
        if body.first() == Some(&WITH_CAPABILITIES_REQUEST_TAG) {
            return Err(crate::ipc::Error::Codec(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        if matches!(
            body.first().copied(),
            Some(CREATE_ITEM_REQUEST_TAG | UPDATE_ITEM_REQUEST_TAG)
        ) {
            let (mut req, tail) =
                postcard::take_from_bytes::<Self>(body).map_err(crate::ipc::Error::Codec)?;
            if !tail.is_empty() {
                let (waiting_reason, rest) = postcard::take_from_bytes::<Option<String>>(tail)
                    .map_err(crate::ipc::Error::Codec)?;
                if !rest.is_empty() {
                    return Err(crate::ipc::Error::Codec(
                        postcard::Error::DeserializeBadEncoding,
                    ));
                }
                match &mut req {
                    Self::CreateItem(params) => params.waiting_reason = waiting_reason,
                    Self::UpdateItem(params) => params.waiting_reason = waiting_reason,
                    _ => {}
                }
            }
            return Ok(req);
        }
        postcard::from_bytes(body).map_err(crate::ipc::Error::Codec)
    }
}

impl Msg for BotRpcRequest {
    const KIND: u8 = BOT_REQ;

    fn encode(&self) -> Result<Vec<u8>, crate::ipc::Error> {
        let body = self.encode_wire_body()?;
        crate::ipc::encode_frame(Self::KIND, &body)
    }

    fn decode(kind: u8, body: &[u8]) -> Result<Self, crate::ipc::Error> {
        if kind != Self::KIND {
            return Err(crate::ipc::Error::UnexpectedKind {
                want: Self::KIND,
                got: kind,
            });
        }
        Self::decode_wire_body(body)
    }
}

impl Msg for BotRpcResponse {
    const KIND: u8 = BOT_REPLY;

    fn encode(&self) -> Result<Vec<u8>, crate::ipc::Error> {
        let body = self.encode_wire_body()?;
        crate::ipc::encode_frame(Self::KIND, &body)
    }

    fn decode(kind: u8, body: &[u8]) -> Result<Self, crate::ipc::Error> {
        if kind != Self::KIND {
            return Err(crate::ipc::Error::UnexpectedKind {
                want: Self::KIND,
                got: kind,
            });
        }
        Self::decode_wire_body(body)
    }
}

thread_local! {
    static IN_WITH_CAPABILITIES: Cell<bool> = const { Cell::new(false) };
}

fn rpc_capabilities<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    struct CapsVisitor;
    impl<'de> serde::de::Visitor<'de> for CapsVisitor {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("bounded list of bot capability strings")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            use serde::de::Error as _;
            if seq.size_hint().is_some_and(|n| n > MAX_BOT_CAPABILITIES) {
                return Err(A::Error::custom("bot capabilities list exceeds limit"));
            }
            let mut out = Vec::new();
            while let Some(cap) = seq.next_element::<String>()? {
                if out.len() >= MAX_BOT_CAPABILITIES {
                    return Err(A::Error::custom("bot capabilities list exceeds limit"));
                }
                if cap.len() > MAX_BOT_CAPABILITY_LEN {
                    return Err(A::Error::custom("bot capability string exceeds byte limit"));
                }
                out.push(cap);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(CapsVisitor)
}

fn rpc_inner_request<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Box<BotRpcRequest>, D::Error> {
    use serde::de::Error as _;
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            IN_WITH_CAPABILITIES.with(|flag| flag.set(false));
        }
    }
    let already_inside = IN_WITH_CAPABILITIES.with(|flag| flag.replace(true));
    if already_inside {
        return Err(D::Error::custom(
            "nested WithCapabilities request is not allowed",
        ));
    }
    let _guard = Guard;
    let inner = Box::<BotRpcRequest>::deserialize(d)?;
    if matches!(inner.as_ref(), BotRpcRequest::WithCapabilities { .. }) {
        return Err(D::Error::custom(
            "nested WithCapabilities request is not allowed",
        ));
    }
    Ok(inner)
}

fn rpc_zip_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    bounded::<D, u8, MAX_RPC_ZIP_BYTES>(d)
}

pub(crate) fn rpc_avatar_bytes<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<u8>, D::Error> {
    bounded::<D, u8, MAX_RPC_AVATAR_PNG_BYTES>(d)
}
