// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Core domain types and validation for Krill Bots.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Maximum UTF-8 byte length for a bot display name.
pub const BOT_NAME_MAX_BYTES: usize = 64;
/// Maximum ASCII byte length for a bot ID.
pub const BOT_ID_MAX_BYTES: usize = 64;
/// Maximum ASCII byte length for a bot filesystem slug.
pub const BOT_SLUG_MAX_BYTES: usize = 64;
/// Default derived slug length limit (matches Orca's folder limit).
pub const BOT_DERIVED_SLUG_MAX_BYTES: usize = 40;
/// Maximum description UTF-8 byte length.
pub const BOT_DESCRIPTION_MAX_BYTES: usize = 1024;
/// Maximum look-around focus UTF-8 byte length.
pub const BOT_LOOK_AROUND_FOCUS_MAX_BYTES: usize = 500;
/// Default look-around interval (4 hours in seconds).
pub const DEFAULT_LOOK_AROUND_INTERVAL_SECS: u64 = 4 * 60 * 60;
/// Default maximum suggestions produced per look-around run.
pub const DEFAULT_LOOK_AROUND_MAX_SUGGESTIONS: u32 = 5;
/// Default nightly memory tidy interval (24 hours in seconds).
pub const DEFAULT_MEMORY_TIDY_INTERVAL_SECS: u64 = 24 * 60 * 60;
/// Default connector poll interval (60 seconds).
pub const DEFAULT_CONNECTOR_POLL_INTERVAL_SECS: u64 = 60;
/// Maximum bot item title length.
pub const BOT_ITEM_TITLE_MAX_BYTES: usize = 200;
/// Maximum bot item description length.
pub const BOT_ITEM_DESCRIPTION_MAX_BYTES: usize = 20_000;
/// Maximum bot item comment length.
pub const BOT_ITEM_COMMENT_MAX_BYTES: usize = 8_000;

/// Validation error for bot identifiers, slugs, names, and configs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotValidationError {
    EmptyId,
    IdTooLong(usize),
    InvalidIdChars,
    EmptySlug,
    SlugTooLong(usize),
    InvalidSlugChars,
    ReservedSlug(String),
    EmptyName,
    NameTooLong(usize),
    InvalidNameChars,
    EmptyAgentId,
    InvalidAgentId,
    DescriptionTooLong(usize),
    InvalidField { field: &'static str, reason: String },
}

impl fmt::Display for BotValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId => f.write_str("bot id must not be empty"),
            Self::IdTooLong(n) => {
                write!(f, "bot id exceeds {BOT_ID_MAX_BYTES} bytes ({n} bytes)")
            }
            Self::InvalidIdChars => {
                f.write_str("bot id must use ASCII letters, digits, '-' or '_'")
            }
            Self::EmptySlug => f.write_str("bot slug must not be empty"),
            Self::SlugTooLong(n) => {
                write!(f, "bot slug exceeds {BOT_SLUG_MAX_BYTES} bytes ({n} bytes)")
            }
            Self::InvalidSlugChars => f.write_str(
                "bot slug must be lowercase ASCII alphanumeric segments separated by single '-'",
            ),
            Self::ReservedSlug(slug) => {
                write!(f, "bot slug '{slug}' is reserved on Windows filesystems")
            }
            Self::EmptyName => f.write_str("bot name must not be empty"),
            Self::NameTooLong(n) => {
                write!(f, "bot name exceeds {BOT_NAME_MAX_BYTES} bytes ({n} bytes)")
            }
            Self::InvalidNameChars => f.write_str("bot name must not contain control characters"),
            Self::EmptyAgentId => f.write_str("bot agent_id must not be empty"),
            Self::InvalidAgentId => {
                f.write_str("bot agent_id must not contain whitespace or control characters")
            }
            Self::DescriptionTooLong(n) => write!(
                f,
                "bot description exceeds {BOT_DESCRIPTION_MAX_BYTES} bytes ({n} bytes)"
            ),
            Self::InvalidField { field, reason } => write!(f, "invalid {field}: {reason}"),
        }
    }
}

impl std::error::Error for BotValidationError {}

/// Returns `true` if `slug` is a reserved Windows device filename.
#[must_use]
pub fn is_windows_reserved_slug(slug: &str) -> bool {
    if matches!(slug, "con" | "prn" | "aux" | "nul") {
        return true;
    }
    for prefix in ["com", "lpt"] {
        if let Some(rest) = slug.strip_prefix(prefix)
            && rest.len() == 1
            && rest.bytes().all(|b| b.is_ascii_digit())
        {
            return true;
        }
    }
    false
}

/// Validates a bot identifier (`1..=64` ASCII alphanumeric, `-`, `_`, starting with alphanumeric).
pub fn validate_bot_id(raw: &str) -> Result<String, BotValidationError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(BotValidationError::EmptyId);
    }
    if trimmed.len() > BOT_ID_MAX_BYTES {
        return Err(BotValidationError::IdTooLong(trimmed.len()));
    }
    let valid_start = trimmed
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphanumeric());
    let valid_all = trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    if !valid_start || !valid_all {
        return Err(BotValidationError::InvalidIdChars);
    }
    Ok(trimmed.to_owned())
}

/// Validates a filesystem-safe bot slug: lowercase ASCII `[a-z0-9]` segments separated by single `-`.
/// Rejects path traversal (`..`, `/`, `\`), control bytes, and Windows reserved device names.
pub fn validate_bot_slug(raw: &str) -> Result<String, BotValidationError> {
    if raw.is_empty() {
        return Err(BotValidationError::EmptySlug);
    }
    if raw.len() > BOT_SLUG_MAX_BYTES {
        return Err(BotValidationError::SlugTooLong(raw.len()));
    }
    let mut prev_dash = true;
    for b in raw.bytes() {
        if b.is_ascii_lowercase() || b.is_ascii_digit() {
            prev_dash = false;
        } else if b == b'-' && !prev_dash {
            prev_dash = true;
        } else {
            return Err(BotValidationError::InvalidSlugChars);
        }
    }
    if prev_dash {
        return Err(BotValidationError::InvalidSlugChars);
    }
    if is_windows_reserved_slug(raw) {
        return Err(BotValidationError::ReservedSlug(raw.to_owned()));
    }
    Ok(raw.to_owned())
}

/// Derives a filesystem-safe [`BotSlug`] from an arbitrary human bot name.
#[must_use]
pub fn bot_slug_from_name(name: &str) -> BotSlug {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > BOT_DERIVED_SLUG_MAX_BYTES {
        let mut truncated = out
            .get(..BOT_DERIVED_SLUG_MAX_BYTES)
            .unwrap_or_default()
            .to_owned();
        while truncated.ends_with('-') {
            truncated.pop();
        }
        out = truncated;
    }
    if out.is_empty() {
        return BotSlug("bot".to_owned());
    }
    if is_windows_reserved_slug(&out) {
        out.push_str("-bot");
    }
    BotSlug(out)
}

/// Validates a bot display name (`1..=64` bytes, no control characters).
pub fn validate_bot_name(raw: &str) -> Result<String, BotValidationError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(BotValidationError::EmptyName);
    }
    if trimmed.len() > BOT_NAME_MAX_BYTES {
        return Err(BotValidationError::NameTooLong(trimmed.len()));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(BotValidationError::InvalidNameChars);
    }
    Ok(trimmed.to_owned())
}

/// Validated unique identifier for a bot.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BotId(String);

impl BotId {
    pub fn new(raw: impl AsRef<str>) -> Result<Self, BotValidationError> {
        validate_bot_id(raw.as_ref()).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for BotId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(&raw).map_err(serde::de::Error::custom)
    }
}

impl FromStr for BotId {
    type Err = BotValidationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl fmt::Display for BotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for BotId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Validated filesystem-safe folder slug for a bot home (`<bots_root>/<slug>/`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BotSlug(String);

impl BotSlug {
    pub fn new(raw: impl AsRef<str>) -> Result<Self, BotValidationError> {
        validate_bot_slug(raw.as_ref()).map(Self)
    }

    #[must_use]
    pub fn from_name(name: &str) -> Self {
        bot_slug_from_name(name)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for BotSlug {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(&raw).map_err(serde::de::Error::custom)
    }
}

impl FromStr for BotSlug {
    type Err = BotValidationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl fmt::Display for BotSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for BotSlug {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Notification switches for a bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotNotifications {
    #[serde(default = "default_true")]
    pub needs_permission: bool,
    #[serde(default = "default_true")]
    pub task_complete: bool,
    #[serde(default = "default_true")]
    pub could_not_finish: bool,
}

impl Default for BotNotifications {
    fn default() -> Self {
        Self {
            needs_permission: true,
            task_complete: true,
            could_not_finish: true,
        }
    }
}

/// A recurring cron schedule attached to a bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotScheduleConfig {
    pub id: String,
    pub name: String,
    pub cron: String,
    pub prompt: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub timezone: Option<String>,
}

/// Periodic proactive "Look around" configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotLookAroundConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_look_around_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_look_around_max_suggestions")]
    pub max_suggestions: u32,
    #[serde(default = "default_true")]
    pub weekdays_only: bool,
    #[serde(default)]
    pub focus: String,
}

/// Orca's Look around presets: local hours a look runs at; nothing runs at night.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LookAroundEvery {
    #[default]
    #[serde(rename = "every-4h")]
    Every4h,
    #[serde(rename = "every-2h")]
    Every2h,
    #[serde(rename = "twice-daily")]
    TwiceDaily,
    #[serde(rename = "mornings")]
    Mornings,
}

impl LookAroundEvery {
    /// Settings' order.
    pub const ALL: [Self; 4] = [
        Self::Every4h,
        Self::Every2h,
        Self::TwiceDaily,
        Self::Mornings,
    ];

    /// Local hours each preset looks at.
    #[must_use]
    pub const fn hours(self) -> &'static [u32] {
        match self {
            Self::Every4h => &[8, 12, 16, 20],
            Self::Every2h => &[8, 10, 12, 14, 16, 18, 20],
            Self::TwiceDaily => &[8, 13],
            Self::Mornings => &[8],
        }
    }
}

/// Orca's Memory upkeep presets: each night or once a week, after 3 AM local time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TidyEvery {
    #[default]
    Night,
    Week,
}

/// Orca's "Mark items stale after": days an untouched item waits before Home moves it to Stale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotItemsStale {
    #[serde(rename = "3")]
    Days3,
    #[default]
    #[serde(rename = "5")]
    Days5,
    #[serde(rename = "7")]
    Days7,
    #[serde(rename = "14")]
    Days14,
    #[serde(rename = "never")]
    Never,
}

impl BotItemsStale {
    /// Settings' order.
    pub const ALL: [Self; 5] = [
        Self::Days3,
        Self::Days5,
        Self::Days7,
        Self::Days14,
        Self::Never,
    ];

    /// The days, or `None` for Never.
    #[must_use]
    pub const fn days(self) -> Option<u64> {
        match self {
            Self::Days3 => Some(3),
            Self::Days5 => Some(5),
            Self::Days7 => Some(7),
            Self::Days14 => Some(14),
            Self::Never => None,
        }
    }
}

/// Settings Orca keeps on the bot that Krill's postcard `BotConfig` can't grow without breaking
/// N-1 peers: they travel on their own RPC (`GetBotSettingsExtras`) and live in their own file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotSettingsExtras {
    #[serde(default)]
    pub items_stale: BotItemsStale,
    #[serde(default)]
    pub look_around_every: LookAroundEvery,
    #[serde(default)]
    pub tidy_every: TidyEvery,
}

impl Default for BotLookAroundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: DEFAULT_LOOK_AROUND_INTERVAL_SECS,
            max_suggestions: DEFAULT_LOOK_AROUND_MAX_SUGGESTIONS,
            weekdays_only: true,
            focus: String::new(),
        }
    }
}

/// Periodic memory consolidation ("Tidy up") configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotMemoryTidyConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_memory_tidy_interval")]
    pub interval_secs: u64,
}

impl Default for BotMemoryTidyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: DEFAULT_MEMORY_TIDY_INTERVAL_SECS,
        }
    }
}

/// Authentication method for a curated or user-configured MCP tool server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotMcpAuthKind {
    /// Server responds with OAuth browser sign-in challenge (401 + `WWW-Authenticate`).
    BrowserOAuth,
    /// Static API key / bearer token passed via an HTTP header stored in keychain.
    BearerHeader,
    /// Public / unauthenticated server.
    #[default]
    None,
}

/// An MCP server configured on a bot. Secret header values live only in OS keychain (`secret_ref`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotMcpToolConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub endpoint_url: Option<String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub auth_kind: BotMcpAuthKind,
    #[serde(default)]
    pub header_names: Vec<String>,
    #[serde(default)]
    pub secret_ref: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Who a connector or conversation is acting on behalf of.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActsFor {
    #[default]
    Owner,
    Anyone,
    Member(String),
}

/// External connector / integration sources supported by Krill Bots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSource {
    Telegram,
    Slack,
    Discord,
    Google,
    GitHub,
    Linear,
    Composio,
}

impl ExternalSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Slack => "slack",
            Self::Discord => "discord",
            Self::Google => "google",
            Self::GitHub => "github",
            Self::Linear => "linear",
            Self::Composio => "composio",
        }
    }
}

impl fmt::Display for ExternalSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A person allowed to interact with a bot on an external platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotMember {
    pub id: String,
    pub name: String,
    pub source: ExternalSource,
    #[serde(default)]
    pub workspace_id: Option<String>,
}

/// Telegram Bot API long-polling connector configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub secret_ref: Option<String>,
    #[serde(default)]
    pub allowed_chat_ids: Vec<i64>,
    #[serde(default)]
    pub allowed_user_ids: Vec<i64>,
    #[serde(default)]
    pub acts_for: ActsFor,
}

/// Local Slack Socket Mode connector configuration (zero cloud server required).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub app_token_secret_ref: Option<String>,
    #[serde(default)]
    pub bot_token_secret_ref: Option<String>,
    #[serde(default)]
    pub allowed_channel_ids: Vec<String>,
    #[serde(default)]
    pub allowed_user_ids: Vec<String>,
    #[serde(default = "default_true")]
    pub mirror_desktop: bool,
    #[serde(default)]
    pub acts_for: ActsFor,
}

/// Discord Gateway WebSocket connector configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token_secret_ref: Option<String>,
    #[serde(default)]
    pub allowed_guild_ids: Vec<String>,
    #[serde(default)]
    pub allowed_channel_ids: Vec<String>,
    #[serde(default)]
    pub allowed_user_ids: Vec<String>,
    #[serde(default)]
    pub acts_for: ActsFor,
}

/// Per-service switches for Google Workspace integration.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each Google Workspace service toggles independently"
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleServicesConfig {
    #[serde(default)]
    pub gmail: bool,
    #[serde(default)]
    pub calendar: bool,
    #[serde(default)]
    pub drive: bool,
    #[serde(default)]
    pub docs: bool,
    #[serde(default)]
    pub sheets: bool,
    #[serde(default)]
    pub contacts: bool,
}

/// Local loopback OAuth Google Workspace connector configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub oauth_client_secret_ref: Option<String>,
    #[serde(default)]
    pub token_secret_ref: Option<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub services: GoogleServicesConfig,
}

impl Default for GoogleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            oauth_client_secret_ref: None,
            token_secret_ref: None,
            poll_interval_secs: DEFAULT_CONNECTOR_POLL_INTERVAL_SECS,
            services: GoogleServicesConfig::default(),
        }
    }
}

/// GitHub issue/PR/CI tracker polling connector configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubTrackerConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token_secret_ref: Option<String>,
    #[serde(default = "default_true")]
    pub use_gh_cli: bool,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
}

impl Default for GitHubTrackerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token_secret_ref: None,
            use_gh_cli: true,
            repos: Vec::new(),
            poll_interval_secs: DEFAULT_CONNECTOR_POLL_INTERVAL_SECS,
        }
    }
}

/// Linear GraphQL issue tracker polling connector configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearTrackerConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub api_key_secret_ref: Option<String>,
    #[serde(default)]
    pub team_keys: Vec<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
}

impl Default for LinearTrackerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key_secret_ref: None,
            team_keys: Vec::new(),
            poll_interval_secs: DEFAULT_CONNECTOR_POLL_INTERVAL_SECS,
        }
    }
}

/// Composio third-party app bridge configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposioConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub api_key_secret_ref: Option<String>,
    #[serde(default)]
    pub enabled_apps: Vec<String>,
}

/// All connector configurations owned by a bot (non-secret references only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotConnectorConfig {
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub slack: SlackConfig,
    #[serde(default)]
    pub discord: DiscordConfig,
    #[serde(default)]
    pub google: GoogleConfig,
    #[serde(default)]
    pub github: GitHubTrackerConfig,
    #[serde(default)]
    pub linear: LinearTrackerConfig,
    #[serde(default)]
    pub composio: ComposioConfig,
}

impl BotConnectorConfig {
    /// Returns the list of external sources currently enabled on this bot.
    #[must_use]
    pub fn enabled_sources(&self) -> Vec<ExternalSource> {
        let mut out = Vec::new();
        if self.telegram.enabled {
            out.push(ExternalSource::Telegram);
        }
        if self.slack.enabled {
            out.push(ExternalSource::Slack);
        }
        if self.discord.enabled {
            out.push(ExternalSource::Discord);
        }
        if self.google.enabled {
            out.push(ExternalSource::Google);
        }
        if self.github.enabled {
            out.push(ExternalSource::GitHub);
        }
        if self.linear.enabled {
            out.push(ExternalSource::Linear);
        }
        if self.composio.enabled {
            out.push(ExternalSource::Composio);
        }
        out
    }
}

/// Persistent bot configuration stored in `<bots_root>/<slug>/bot.json` and daemon registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotConfig {
    pub id: BotId,
    pub slug: BotSlug,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub avatar_color: Option<String>,
    pub agent_id: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub permission_mode: crate::agent::Permission,
    /// Home folder name under `<bots_root>` (typically matches `slug`) or custom workspace path.
    pub folder: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub first_run_completed: bool,
    #[serde(default)]
    pub notifications: BotNotifications,
    #[serde(default)]
    pub schedules: Vec<BotScheduleConfig>,
    #[serde(default)]
    pub look_around: BotLookAroundConfig,
    #[serde(default)]
    pub memory_tidy: BotMemoryTidyConfig,
    #[serde(default)]
    pub mcp_tools: Vec<BotMcpToolConfig>,
    #[serde(default)]
    pub connectors: BotConnectorConfig,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub members: Vec<BotMember>,
    #[serde(default)]
    pub created_at_ms: u64,
    #[serde(default)]
    pub updated_at_ms: u64,
}

impl BotConfig {
    /// Creates a validated default configuration for a new bot.
    pub fn new(
        id: BotId,
        name: impl AsRef<str>,
        agent_id: impl AsRef<str>,
        now_ms: u64,
    ) -> Result<Self, BotValidationError> {
        let name = validate_bot_name(name.as_ref())?;
        let slug = BotSlug::from_name(&name);
        let folder = slug.as_str().to_owned();
        let cfg = Self {
            id,
            slug,
            name,
            description: String::new(),
            emoji: None,
            avatar: None,
            avatar_color: None,
            agent_id: agent_id.as_ref().trim().to_owned(),
            model: None,
            effort: None,
            permission_mode: crate::agent::Permission::default(),
            folder,
            workspace: None,
            template_id: None,
            paused: false,
            first_run_completed: false,
            notifications: BotNotifications::default(),
            schedules: Vec::new(),
            look_around: BotLookAroundConfig::default(),
            memory_tidy: BotMemoryTidyConfig::default(),
            mcp_tools: Vec::new(),
            connectors: BotConnectorConfig::default(),
            skills: Vec::new(),
            members: Vec::new(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validates all invariant fields of this bot configuration.
    pub fn validate(&self) -> Result<(), BotValidationError> {
        validate_bot_id(self.id.as_str())?;
        validate_bot_slug(self.slug.as_str())?;
        validate_bot_name(&self.name)?;
        if self.description.len() > BOT_DESCRIPTION_MAX_BYTES {
            return Err(BotValidationError::DescriptionTooLong(
                self.description.len(),
            ));
        }
        if self.agent_id.trim().is_empty() {
            return Err(BotValidationError::EmptyAgentId);
        }
        if self
            .agent_id
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(BotValidationError::InvalidAgentId);
        }
        if self.look_around.focus.len() > BOT_LOOK_AROUND_FOCUS_MAX_BYTES {
            return Err(BotValidationError::InvalidField {
                field: "look_around.focus",
                reason: format!("exceeds {BOT_LOOK_AROUND_FOCUS_MAX_BYTES} bytes"),
            });
        }
        for sched in &self.schedules {
            crate::schedule::Cron::parse(&sched.cron).map_err(|err| {
                BotValidationError::InvalidField {
                    field: "schedules.cron",
                    reason: format!("{}: {err}", sched.id),
                }
            })?;
        }
        Ok(())
    }
}

/// Maps an external platform thread/channel to a local Krill bot session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalConversationLink {
    pub id: String,
    pub bot_id: BotId,
    pub source: ExternalSource,
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub channel_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<crate::SessionId>,
    #[serde(default)]
    pub acts_for: ActsFor,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Decision state for an external sender requesting access to a bot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotAccessRequestStatus {
    #[default]
    Pending,
    Approved,
    Declined,
}

/// Pending or decided access request from an external sender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotAccessRequest {
    pub token: String,
    pub bot_id: BotId,
    pub source: ExternalSource,
    pub sender_id: String,
    pub sender_name: String,
    pub channel_id: String,
    pub status: BotAccessRequestStatus,
    pub created_at: u64,
    #[serde(default)]
    pub decided_at: Option<u64>,
}

/// Typed failure taxonomy adapted from Orca's connector-contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotErrorCategory {
    SignedOut,
    UsageLimit,
    NotSetUp,
    FolderMissing,
    #[default]
    AgentError,
}

impl BotErrorCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SignedOut => "signed_out",
            Self::UsageLimit => "usage_limit",
            Self::NotSetUp => "not_set_up",
            Self::FolderMissing => "folder_missing",
            Self::AgentError => "agent_error",
        }
    }

    #[must_use]
    pub fn from_wire_str(raw: &str) -> Self {
        match raw.trim() {
            "signed_out" => Self::SignedOut,
            "usage_limit" => Self::UsageLimit,
            "not_set_up" => Self::NotSetUp,
            "folder_missing" => Self::FolderMissing,
            _ => Self::AgentError,
        }
    }
}

/// Structured failure detail attached to bot notifications or connector statuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotErrorDetail {
    pub category: BotErrorCategory,
    #[serde(default)]
    pub agent_name: Option<String>,
    pub message: String,
    #[serde(default)]
    pub resets_at_ms: Option<u64>,
}

/// Lifecycle state of a tracked bot deliverable or decision item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotItemStatus {
    #[default]
    Planned,
    Working,
    NeedsYou,
    Done,
    Canceled,
}

impl BotItemStatus {
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Planned | Self::Working | Self::NeedsYou)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Working => "working",
            Self::NeedsYou => "needs_you",
            Self::Done => "done",
            Self::Canceled => "canceled",
        }
    }
}

/// A typed reference attached to a [`BotItem`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotItemLink {
    Dispatch(String),
    Pr(String),
    Url(String),
    File(String),
}

/// Event kinds recorded in a [`BotItem`] audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotItemEventKind {
    Made,
    Edited,
    Status,
    Commented,
    Linked,
    Snoozed,
    Unsnoozed,
}

/// Actor that created a [`BotItemEvent`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotItemActor {
    Owner {
        #[serde(default)]
        device: Option<String>,
    },
    Bot {
        bot_id: BotId,
        #[serde(default)]
        session_id: Option<crate::SessionId>,
    },
    System {
        reason: String,
    },
}

/// Structured quick-reply decision embedded in a `NeedsYou` item ask.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotItemDecision {
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub recommended: Option<usize>,
}

/// A single immutable event in a [`BotItem`]'s timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotItemEvent {
    pub id: u64,
    pub item_uid: String,
    pub kind: BotItemEventKind,
    pub actor: BotItemActor,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub decisions: Vec<BotItemDecision>,
    pub created_at_ms: u64,
}

/// A deliverable or decision tracked by a bot for its owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotItem {
    pub uid: String,
    pub number: u64,
    pub owner_bot_id: BotId,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub done_means: Option<String>,
    pub status: BotItemStatus,
    #[serde(default)]
    pub close_note: Option<String>,
    #[serde(default)]
    pub links: Vec<BotItemLink>,
    #[serde(default)]
    pub events: Vec<BotItemEvent>,
    #[serde(default)]
    pub snoozed_until_ms: Option<u64>,
    pub version: u64,
    pub last_activity_at_ms: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub closed_at_ms: Option<u64>,
}

/// A named multi-bot group chat room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotGroupChat {
    pub id: String,
    pub name: String,
    pub bot_ids: Vec<BotId>,
    #[serde(default)]
    pub description: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// A proactive next-step suggestion generated by a bot's Look Around run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotSuggestion {
    pub id: String,
    pub bot_id: BotId,
    pub title: String,
    pub why: String,
    pub ask: String,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub source: Option<ExternalSource>,
    #[serde(default)]
    pub dismissed: bool,
    pub created_at_ms: u64,
}

/// Live connection state of a bot's external connector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorHealthState {
    #[default]
    Disabled,
    Disconnected,
    Connecting,
    Connected,
    Error,
}

/// Live status snapshot of a single connector for a bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotConnectorStatus {
    pub bot_id: BotId,
    pub source: ExternalSource,
    pub enabled: bool,
    pub connected: bool,
    pub state: ConnectorHealthState,
    #[serde(default)]
    pub account_label: Option<String>,
    #[serde(default)]
    pub last_sync_ms: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub error_category: Option<BotErrorCategory>,
}

/// Status of an upstream MCP server attached to the local bot tool relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotToolServerStatus {
    pub id: String,
    pub name: String,
    pub endpoint_url: String,
    pub connected: bool,
    pub tool_count: u32,
    #[serde(default)]
    pub last_error: Option<String>,
}

/// Status of Krill's local loopback MCP tool relay server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotToolRelayStatus {
    pub running: bool,
    #[serde(default)]
    pub listen_addr: Option<String>,
    #[serde(default)]
    pub endpoint_url: Option<String>,
    #[serde(default)]
    pub connected_tools: Vec<BotToolServerStatus>,
    #[serde(default)]
    pub last_error: Option<String>,
}

/// External desktop apps from which Krill can discover and import bots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalBotSourceApp {
    OpenClaw,
    Hermes,
    OpenBot,
    Orca,
}

/// A discovered external bot candidate on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalBotCandidate {
    pub app: ExternalBotSourceApp,
    pub agent: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub soul_preview: Option<String>,
}

/// Event-triggered automation rule attached to a bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotEventRuleConfig {
    pub id: String,
    pub source: String,
    pub event: String,
    pub prompt: String,
    #[serde(default)]
    pub enabled: bool,
}

/// Sanitized PNG avatar payload returned by `GetBotAvatar`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotAvatarData {
    pub bot_id: BotId,
    pub mime: String,
    #[serde(deserialize_with = "super::rpc::rpc_avatar_bytes")]
    pub png_bytes: Vec<u8>,
}

impl fmt::Debug for BotAvatarData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotAvatarData")
            .field("bot_id", &self.bot_id)
            .field("mime", &self.mime)
            .field("bytes", &self.png_bytes.len())
            .finish()
    }
}

/// Paginated item list returned by `ListItemsPage`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotItemsPage {
    #[serde(default)]
    pub items: Vec<BotItem>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub total_count: usize,
    #[serde(default)]
    pub unseen_count: usize,
}

/// Single audit diff entry in a bot's `MEMORY.md` change history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotMemoryHistoryEntry {
    pub entry_id: String,
    pub bot_id: BotId,
    pub timestamp_ms: u64,
    pub actor: String,
    pub action: String,
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default)]
    pub before_content: String,
    #[serde(default)]
    pub after_content: String,
    #[serde(default)]
    pub undoable: bool,
    #[serde(default)]
    pub undone: bool,
}

/// Runtime status of automatic and manual `MEMORY.md` tidy-up.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotMemoryTidyStatus {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub interval_secs: u64,
    #[serde(default)]
    pub last_run_ms: Option<u64>,
    #[serde(default)]
    pub last_tidy_id: Option<String>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub auto_disabled: bool,
    #[serde(default)]
    pub undoable: bool,
}

/// Metadata for a topic markdown file stored under `memory/<topic>.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotMemoryTopicInfo {
    pub topic: String,
    pub bytes: u64,
    pub updated_at_ms: u64,
}

/// Live or recently tracked PTY session associated with a bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotSessionSummary {
    pub session_id: crate::SessionId,
    pub bot_id: BotId,
    #[serde(default)]
    pub link_id: Option<String>,
    pub pid: u32,
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub working: bool,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub created_at_ms: u64,
}

/// Reply target selection mode when sending a message in a bot group chat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotGroupReplyMode {
    #[default]
    Mentions,
    All,
    Selected,
}

/// Conversation thread inside a [`BotGroupChat`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotGroupThread {
    pub thread_id: String,
    pub group_id: String,
    #[serde(default)]
    pub title: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Message posted inside a [`BotGroupThread`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotGroupMessage {
    pub message_id: String,
    pub group_id: String,
    pub thread_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub body: String,
    #[serde(default)]
    pub mentions: Vec<BotId>,
    pub created_at_ms: u64,
    #[serde(default)]
    pub in_progress: bool,
}

/// Hydrated view of a bot group chat, its threads, and messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotGroupView {
    pub group: BotGroupChat,
    #[serde(default)]
    pub threads: Vec<BotGroupThread>,
    #[serde(default)]
    pub messages: Vec<BotGroupMessage>,
    #[serde(default)]
    pub active_run: bool,
}

/// OAuth sign-in state for a `BrowserOAuth` MCP tool attached to a bot.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotMcpOAuthState {
    pub bot_id: BotId,
    pub tool_id: String,
    pub status: String,
    #[serde(default)]
    pub auth_url: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub updated_at_ms: u64,
}

impl fmt::Debug for BotMcpOAuthState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotMcpOAuthState")
            .field("bot_id", &self.bot_id)
            .field("tool_id", &self.tool_id)
            .field("status", &self.status)
            .field("auth_url", &self.auth_url.as_ref().map(|_| "<redacted>"))
            .field("error", &self.error)
            .field("updated_at_ms", &self.updated_at_ms)
            .finish()
    }
}

/// Connected third-party app account reported by Composio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposioConnectionInfo {
    pub id: String,
    pub app_id: String,
    pub status: String,
    #[serde(default)]
    pub account_label: Option<String>,
    #[serde(default)]
    pub created_at_ms: Option<u64>,
}

/// Default bot assignments for GitHub and Linear tracker routing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultTrackerBots {
    #[serde(default)]
    pub github: Option<BotId>,
    #[serde(default)]
    pub linear: Option<BotId>,
}

/// Result of a live connector connectivity test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotConnectorTestResult {
    pub source: ExternalSource,
    pub ok: bool,
    #[serde(default)]
    pub account_label: Option<String>,
    #[serde(default)]
    pub detail: String,
}

const fn default_true() -> bool {
    true
}

const fn default_look_around_interval() -> u64 {
    DEFAULT_LOOK_AROUND_INTERVAL_SECS
}

const fn default_look_around_max_suggestions() -> u32 {
    DEFAULT_LOOK_AROUND_MAX_SUGGESTIONS
}

const fn default_memory_tidy_interval() -> u64 {
    DEFAULT_MEMORY_TIDY_INTERVAL_SECS
}

const fn default_poll_interval() -> u64 {
    DEFAULT_CONNECTOR_POLL_INTERVAL_SECS
}
