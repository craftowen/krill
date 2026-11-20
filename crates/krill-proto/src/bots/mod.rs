// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Krill Bots protocol, home management, catalog, sanitized packaging, and RPC contract.

pub mod catalog;
pub mod home;
pub mod package;
pub mod rpc;
pub mod types;

pub use catalog::{
    BOT_MCP_APPS, BUILTIN_BOT_SKILLS, BUILTIN_BOT_TEMPLATES, BotMcpAppCatalogEntry,
    BotMcpAppCategory, BotTemplate, BotTemplateCategory, BotTemplateEvent, BotTemplateSchedule,
    COMPOSIO_CURATED_APPS, ComposioAppCatalogEntry, bot_templates_by_category, builtin_bot_skills,
    builtin_bot_templates, find_bot_mcp_app, find_bot_skill, find_bot_template, find_composio_app,
    generate_slack_socket_mode_manifest,
};
pub use home::{
    BOT_FIRST_RUN_RULES, BOT_MEMORY_INDEX_LINE_CAP, BOT_MEMORY_SEED, BOT_MEMORY_SOURCE_SEPARATOR,
    BOT_QUIET_MARKER, MemoryDeletionViolation, MemoryFact, MemorySnapshot, append_memory_fact,
    assert_bot_owns_home, build_launch_preamble, cap_memory_index, count_memory_lines,
    fence_quoted_text, forget_memory_fact, init_bot_home, parse_memory_facts, render_bot_agents_md,
    render_bot_claude_md, restore_deleted_memory_facts, soul_description, starter_soul,
    validate_memory_edit,
};
pub use package::{
    BOT_PACKAGE_SCHEMA_VERSION, BotPackageData, BotPackageExportOptions, MAX_AVATAR_PNG_BYTES,
    MAX_ZIP_ARCHIVE_BYTES, MAX_ZIP_ENTRIES, MAX_ZIP_ENTRY_BYTES, MAX_ZIP_TOTAL_BYTES, PackageError,
    SanitizedBotManifest, SanitizedMcpTool, crc32, export_bot_home_zip, export_bot_zip,
    import_bot_zip, read_store_zip, strip_png_metadata, validate_bot_package_path,
    validate_zip_entry_path, write_store_zip,
};
pub use rpc::{
    BOT_REPLY, BOT_REQ, BotRpcRequest, BotRpcResponse, CreateBotItemParams, CreateBotParams,
    FieldPatch, MAX_RPC_ZIP_BYTES, UpdateBotItemParams, UpdateBotParams,
};
pub use types::{
    ActsFor, BOT_DERIVED_SLUG_MAX_BYTES, BOT_DESCRIPTION_MAX_BYTES, BOT_ID_MAX_BYTES,
    BOT_ITEM_COMMENT_MAX_BYTES, BOT_ITEM_DESCRIPTION_MAX_BYTES, BOT_ITEM_TITLE_MAX_BYTES,
    BOT_LOOK_AROUND_FOCUS_MAX_BYTES, BOT_NAME_MAX_BYTES, BOT_SLUG_MAX_BYTES, BotAccessRequest,
    BotAccessRequestStatus, BotConfig, BotConnectorConfig, BotConnectorStatus, BotErrorCategory,
    BotErrorDetail, BotGroupChat, BotId, BotItem, BotItemActor, BotItemDecision, BotItemEvent,
    BotItemEventKind, BotItemLink, BotItemStatus, BotLookAroundConfig, BotMcpAuthKind,
    BotMcpToolConfig, BotMember, BotMemoryTidyConfig, BotNotifications, BotScheduleConfig, BotSlug,
    BotSuggestion, BotToolRelayStatus, BotToolServerStatus, BotValidationError, ComposioConfig,
    ConnectorHealthState, DEFAULT_CONNECTOR_POLL_INTERVAL_SECS, DEFAULT_LOOK_AROUND_INTERVAL_SECS,
    DEFAULT_LOOK_AROUND_MAX_SUGGESTIONS, DEFAULT_MEMORY_TIDY_INTERVAL_SECS, DiscordConfig,
    ExternalBotCandidate, ExternalBotSourceApp, ExternalConversationLink, ExternalSource,
    GitHubTrackerConfig, GoogleConfig, GoogleServicesConfig, LinearTrackerConfig, SlackConfig,
    TelegramConfig, bot_slug_from_name, is_windows_reserved_slug, validate_bot_id,
    validate_bot_name, validate_bot_slug,
};

#[cfg(test)]
mod tests;
