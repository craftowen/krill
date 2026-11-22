// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Krill Bots protocol, home management, catalog, sanitized packaging, and RPC contract.

pub mod catalog;
pub mod external;
pub mod home;
pub mod package;
pub mod proposal;
pub mod rpc;
pub mod types;

pub use catalog::{
    BOT_MCP_APPS, BUILTIN_BOT_SKILLS, BUILTIN_BOT_TEMPLATES, BotMcpAppCatalogEntry,
    BotMcpAppCategory, BotTemplate, BotTemplateCategory, BotTemplateEvent, BotTemplateSchedule,
    COMPOSIO_CURATED_APPS, ComposioAppCatalogEntry, bot_templates_by_category, builtin_bot_skills,
    builtin_bot_templates, default_mascot, find_bot_mcp_app, find_bot_skill, find_bot_template,
    find_composio_app, generate_slack_socket_mode_manifest,
};
pub use external::{
    BOT_IMPORT_EXPORT_PROMPT, BOT_IMPORT_REPLY_FILE, BOT_IMPORT_REPLY_MAX, BotImportNudgeInfo,
    bot_import_app_label, bot_import_nudge, bot_name_from_export_folder, discover_external_bots,
    discover_external_bots_filtered, discover_external_bots_in, expand_tilde, map_orca_agent_id,
    tilde_display,
};
pub use home::{
    BOT_FIRST_RUN_RULES, BOT_MEMORY_INDEX_LINE_CAP, BOT_MEMORY_SEED, BOT_MEMORY_SOURCE_SEPARATOR,
    BOT_QUIET_MARKER, MemoryDeletionViolation, MemoryFact, MemorySnapshot, append_memory_fact,
    assert_bot_owns_home, build_launch_preamble, build_launch_preamble_in, cap_memory_index,
    count_memory_lines, fence_quoted_text, forget_memory_fact, init_bot_home,
    materialize_bot_skills, parse_memory_facts, render_bot_agents_md, render_bot_claude_md,
    restore_deleted_memory_facts, soul_description, starter_soul, strip_launch_preamble,
    validate_memory_edit,
};
pub use package::{
    BOT_PACKAGE_SCHEMA_VERSION, BotPackageData, BotPackageExportOptions, MAX_AVATAR_PNG_BYTES,
    MAX_ZIP_ARCHIVE_BYTES, MAX_ZIP_ENTRIES, MAX_ZIP_ENTRY_BYTES, MAX_ZIP_TOTAL_BYTES, PackageError,
    SanitizedBotManifest, SanitizedMcpTool, crc32, export_bot_home_zip, export_bot_zip,
    import_bot_zip, read_store_zip, scan_export_secrets, strip_png_metadata,
    validate_bot_package_path, validate_zip_entry_path, write_store_zip,
};
pub use proposal::{
    BOT_PROPOSAL_PURPOSE_MAX, BotProposal, HANDED_WORKSPACES_MAX, HandedWorkspace,
    bot_made_for_workspaces_kickoff, parse_template_proposal_fields, proposal_identity,
    proposal_soul, template_proposal,
};
pub use rpc::{
    BOT_REPLY, BOT_REQ, BotAgentModel, BotAgentModels, BotRpcRequest, BotRpcResponse,
    BotScheduleUpcoming, BotUpcoming, CreateBotItemParams, CreateBotParams, CreateBotV2Params,
    FieldPatch, MAX_RPC_ZIP_BYTES, UpdateBotExtrasParams, UpdateBotItemParams, UpdateBotParams,
};
pub use types::{
    ActsFor, BOT_DERIVED_SLUG_MAX_BYTES, BOT_DESCRIPTION_MAX_BYTES, BOT_ID_MAX_BYTES,
    BOT_ITEM_COMMENT_MAX_BYTES, BOT_ITEM_DESCRIPTION_MAX_BYTES, BOT_ITEM_TITLE_MAX_BYTES,
    BOT_LOOK_AROUND_FOCUS_MAX_BYTES, BOT_NAME_MAX_BYTES, BOT_SLUG_MAX_BYTES, BotAccessRequest,
    BotAccessRequestStatus, BotAvatarData, BotConfig, BotConnectorConfig, BotConnectorStatus,
    BotConnectorTestResult, BotErrorCategory, BotErrorDetail, BotEventRuleConfig, BotGroupChat,
    BotGroupMessage, BotGroupReplyMode, BotGroupThread, BotGroupView, BotId, BotItem, BotItemActor,
    BotItemDecision, BotItemEvent, BotItemEventKind, BotItemLink, BotItemStatus, BotItemsPage,
    BotItemsStale, BotLookAroundConfig, BotMcpAuthKind, BotMcpOAuthState, BotMcpToolConfig,
    BotMember, BotMemoryHistoryEntry, BotMemoryTidyConfig, BotMemoryTidyStatus, BotMemoryTopicInfo,
    BotNotifications, BotOffer, BotScheduleConfig, BotSessionSummary, BotSettingsExtras, BotSlug,
    BotSuggestion, BotToolRelayStatus, BotToolServerStatus, BotValidationError, ComposioConfig,
    ComposioConnectionInfo, ConnectorHealthState, DEFAULT_CONNECTOR_POLL_INTERVAL_SECS,
    DEFAULT_LOOK_AROUND_INTERVAL_SECS, DEFAULT_LOOK_AROUND_MAX_SUGGESTIONS,
    DEFAULT_MEMORY_TIDY_INTERVAL_SECS, DefaultTrackerBots, DiscordConfig, ExternalBotCandidate,
    ExternalBotSourceApp, ExternalConversationLink, ExternalSource, GitHubTrackerConfig,
    GoogleConfig, GoogleServicesConfig, LinearTrackerConfig, LookAroundEvery, SlackConfig,
    TelegramConfig, TidyEvery, bot_slug_from_name, is_windows_reserved_slug, validate_bot_id,
    validate_bot_name, validate_bot_slug,
};

#[cfg(test)]
mod tests;
