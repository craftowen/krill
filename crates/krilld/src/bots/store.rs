// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Persistent bot registry, home directory lifecycle, `SOUL.md`/`MEMORY.md` guard,
//! conversation links, access requests, group chats, `.zip` packaging, and external bot migration.
//!
//! Layout under `<state_dir>/bots/`:
//! - `bots.json` (`0o600`): index of configured [`BotConfig`] entries.
//! - `<slug>/`: bot home managed by [`krill_proto::bots::home::init_bot_home`].
//! - `links.json` (`0o600`): [`ExternalConversationLink`] mappings.
//! - `access_requests.json` (`0o600`): [`BotAccessRequest`] ledger.
//! - `groups.json` (`0o600`): [`BotGroupChat`] definitions.
//! - `suggestions.json` (`0o600`): cached [`BotSuggestion`] records from Look Around.
//! - `imported_sources.json` (`0o600`): idempotency ledger for external bot migrations.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use krill_proto::SessionId;
use krill_proto::bots::{
    ActsFor, BOT_MEMORY_SEED, BotAccessRequest, BotAccessRequestStatus, BotAvatarData, BotConfig,
    BotConnectorConfig, BotEventRuleConfig, BotGroupChat, BotId, BotItemsStale, BotMcpToolConfig,
    BotMember, BotMemoryHistoryEntry, BotMemoryTopicInfo, BotOffer, BotPackageExportOptions,
    BotScheduleConfig, BotSettingsExtras, BotSlug, BotSuggestion, CreateBotParams,
    CreateBotV2Params, DefaultTrackerBots, ExternalBotCandidate, ExternalBotSourceApp,
    ExternalConversationLink, ExternalSource, FieldPatch, LookAroundEvery, MAX_AVATAR_PNG_BYTES,
    MAX_RPC_ZIP_BYTES, MemoryDeletionViolation, TidyEvery, UpdateBotExtrasParams, UpdateBotParams,
    append_memory_fact, assert_bot_owns_home, bot_slug_from_name, default_mascot,
    discover_external_bots as proto_discover_external_bots, export_bot_home_zip, find_bot_mcp_app,
    find_bot_template, forget_memory_fact, import_bot_zip, init_bot_home, materialize_bot_skills,
    soul_description, strip_png_metadata, validate_bot_name, validate_bot_slug,
    validate_memory_edit,
};
use krill_proto::os::fs::DirBuilderExt as _;
use krill_proto::os::{canonicalize, write_atomic_0600};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::bots::connectors::AccessDecision;
use crate::bots::memory_history;
use crate::bots::secrets::ensure_no_symlink_path;
use crate::session::lock;

const BOT_OFFERS_FILE: &str = "offers.json";
const BOTS_INDEX_FILE: &str = "bots.json";
/// Settings Orca keeps on the bot that the wire `BotConfig` can't carry (see `BotSettingsExtras`).
const SETTINGS_EXTRAS_FILE: &str = "bot-settings.json";
const LINKS_FILE: &str = "links.json";
const ACCESS_REQUESTS_FILE: &str = "access_requests.json";
const ACCESS_META_FILE: &str = "access_meta.json";
const GROUPS_FILE: &str = "groups.json";
const SUGGESTIONS_FILE: &str = "suggestions.json";
const IMPORTED_SOURCES_FILE: &str = "imported_sources.json";
const DEFAULT_TRACKERS_FILE: &str = "default-trackers.json";
const EVENTS_FILE: &str = "events.json";
const AVATAR_FILE: &str = "avatar.png";
const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_TEXT_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PENDING_ACCESS_PER_BOT: usize = 50;
const ACCESS_NOTIFY_THROTTLE_MS: u64 = 60_000;
const PAIRING_CODE_CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const PAIRING_ATTEMPT_WINDOW_MS: u64 = 60_000;
const PAIRING_MAX_FAILURES_PER_WINDOW: usize = 10;

static ID_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AccessMetaEntry {
    pairing_code: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    last_notified_ms: u64,
}

/// Error returned when a guarded `MEMORY.md` update fails or I/O fails.
#[derive(Debug)]
pub enum MemoryUpdateError {
    DeletionBlocked(MemoryDeletionViolation),
    Io(io::Error),
}

impl fmt::Display for MemoryUpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeletionBlocked(v) => write!(f, "MemoryDeletionViolation: {v}"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for MemoryUpdateError {}

impl From<io::Error> for MemoryUpdateError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// Persistent store for Krill Bots under `<state_dir>/bots/`.
pub struct BotStore {
    root: PathBuf,
    io_lock: Mutex<()>,
    pairing_failures: Mutex<Vec<u64>>,
}

impl fmt::Debug for BotStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotStore").finish_non_exhaustive()
    }
}

impl BotStore {
    /// Creates a lazy store handle pointing at `<state_dir>/bots`. Does zero I/O.
    #[must_use]
    pub fn new(bots_root: PathBuf) -> Self {
        Self {
            root: bots_root,
            io_lock: Mutex::new(()),
            pairing_failures: Mutex::new(Vec::new()),
        }
    }

    /// Returns `true` if `bots.json` already exists on disk (cheap stat for lazy activation).
    #[must_use]
    pub fn index_exists_at(state_dir: &Path) -> bool {
        state_dir.join("bots").join(BOTS_INDEX_FILE).exists()
    }

    /// Returns the `<state_dir>/bots` root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the home directory path `<bots_root>/<slug>` for `bot`.
    #[must_use]
    pub fn bot_home(&self, bot: &BotConfig) -> PathBuf {
        self.root.join(bot.slug.as_str())
    }

    /// Lists all persisted bots.
    pub fn list_bots(&self) -> io::Result<Vec<BotConfig>> {
        let _guard = lock(&self.io_lock);
        self.read_bots()
    }

    /// Gets a bot by exact [`BotId`].
    pub fn get_bot(&self, bot_id: &BotId) -> io::Result<Option<BotConfig>> {
        let bots = self.list_bots()?;
        Ok(bots.into_iter().find(|b| &b.id == bot_id))
    }

    /// Resolves a bot by ID, slug, `bot:<slug>`/`bot:<name>`, or case-insensitive display name.
    pub fn resolve_bot(&self, query: &str) -> io::Result<Option<BotConfig>> {
        let raw = query.trim();
        let needle = raw.strip_prefix("bot:").unwrap_or(raw).trim();
        if needle.is_empty() {
            return Ok(None);
        }
        let bots = self.list_bots()?;
        if let Some(found) = bots.iter().find(|b| b.id.as_str() == needle) {
            return Ok(Some(found.clone()));
        }
        if let Some(found) = bots
            .iter()
            .find(|b| b.slug.as_str().eq_ignore_ascii_case(needle))
        {
            return Ok(Some(found.clone()));
        }
        Ok(bots
            .into_iter()
            .find(|b| b.name.eq_ignore_ascii_case(needle)))
    }

    /// Creates a new bot, applies optional built-in template defaults, initializes its home, and saves.
    pub fn create_bot(&self, params: CreateBotParams, now_ms: u64) -> io::Result<BotConfig> {
        self.create_bot_v2(CreateBotV2Params::from(params), now_ms)
    }

    /// Creates a new bot using v2 extended parameters (`slug`, `avatar_color`, `skills`, `events`).
    pub fn create_bot_v2(&self, params: CreateBotV2Params, now_ms: u64) -> io::Result<BotConfig> {
        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_bots()?;
        let base = params.base;
        let template = base.template_id.as_deref().and_then(find_bot_template);

        let name = validate_bot_name(&base.name)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        let slug = match params.slug {
            Some(requested) => {
                validate_bot_slug(requested.as_str())
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
                if bots.iter().any(|b| b.slug == requested) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("bot slug already exists: {}", requested.as_str()),
                    ));
                }
                requested
            }
            None => allocate_unique_slug(&bots, &bot_slug_from_name(&name))?,
        };
        let id = allocate_unique_id(&bots, slug.as_str())?;

        let agent_id = if base.agent_id.trim().is_empty() {
            "claude".to_owned()
        } else {
            krill_proto::agent::canonical_id(base.agent_id.trim()).to_owned()
        };

        let mut cfg = BotConfig::new(id, &name, &agent_id, now_ms)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        cfg.slug = slug.clone();
        slug.as_str().clone_into(&mut cfg.folder);
        cfg.emoji = base.emoji;
        cfg.avatar = base
            .avatar
            .or_else(|| template.map(|t| t.mascot.to_owned()));
        // Orca gives a bot made without a picture a random mascot.
        if cfg.avatar.is_none() && cfg.emoji.is_none() {
            cfg.avatar = Some(default_mascot(&format!("{}{now_ms}", cfg.id.as_str())).to_owned());
        }
        cfg.avatar_color = params
            .avatar_color
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty());
        cfg.model = base.model;
        cfg.effort = base.effort;
        if let Some(perm) = base.permission_mode {
            cfg.permission_mode = perm;
        }
        cfg.workspace = base
            .workspace
            .map(|w| w.trim().to_owned())
            .filter(|w| !w.is_empty());
        cfg.template_id.clone_from(&base.template_id);

        let initial_soul = base
            .initial_soul
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| template.map(|t| t.default_soul));

        cfg.description = if !base.description.trim().is_empty() {
            base.description.trim().to_owned()
        } else if let Some(t) = template {
            t.description.to_owned()
        } else {
            initial_soul.and_then(soul_description).unwrap_or_default()
        };

        if let Some(t) = template {
            cfg.skills = t.skills.iter().map(|s| (*s).to_owned()).collect();
            cfg.schedules = t
                .schedules
                .iter()
                .map(|s| BotScheduleConfig {
                    id: s.id.to_owned(),
                    name: s.what.to_owned(),
                    cron: s.cron.to_owned(),
                    prompt: s.prompt.to_owned(),
                    enabled: false,
                    timezone: None,
                })
                .collect();
            for app_id in t.mcp_apps {
                if let Some(app) = find_bot_mcp_app(app_id) {
                    cfg.mcp_tools.push(BotMcpToolConfig {
                        id: app.id.to_owned(),
                        name: app.name.to_owned(),
                        endpoint_url: Some(app.endpoint_url.to_owned()),
                        command: Vec::new(),
                        auth_kind: app.auth_kind,
                        header_names: app
                            .header_name
                            .map(|h| vec![h.to_owned()])
                            .unwrap_or_default(),
                        secret_ref: Some(format!("{}:mcp:{}", cfg.id.as_str(), app.id)),
                        enabled: false,
                    });
                }
            }
        }
        if !params.skills.is_empty() {
            cfg.skills = normalize_skill_list(&params.skills)?;
        }

        let events = if !params.events.is_empty() {
            validate_events(&params.events)?;
            params.events
        } else if let Some(t) = template {
            t.events
                .iter()
                .map(|e| BotEventRuleConfig {
                    id: e.id.to_owned(),
                    source: e.source.to_owned(),
                    event: e.event.to_owned(),
                    prompt: e.prompt.to_owned(),
                    enabled: false,
                })
                .collect()
        } else {
            Vec::new()
        };

        cfg.validate()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;

        let home_path = self.bot_home(&cfg);
        let home_existed = home_path.exists();
        ensure_bot_subdirs_safe(&home_path)?;

        let result = (|| -> io::Result<()> {
            let home = init_bot_home(&self.root, &cfg, initial_soul)?;
            ensure_bot_subdirs_safe(&home)?;
            materialize_bot_skills(&home, &cfg.skills)?;
            if !events.is_empty() {
                write_bot_events_sidecar(&home, &events)?;
            }
            bots.push(cfg.clone());
            self.write_json(BOTS_INDEX_FILE, &bots)?;
            Ok(())
        })();

        if let Err(err) = result {
            if !home_existed {
                fs::remove_dir_all(&home_path).ok();
            }
            return Err(err);
        }
        Ok(cfg)
    }

    /// Updates v2 extras (`avatar_color`, `skills`, `events`) on an existing bot.
    pub fn update_bot_extras(
        &self,
        params: UpdateBotExtrasParams,
        now_ms: u64,
    ) -> io::Result<BotConfig> {
        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_bots()?;
        let bot = bots
            .iter_mut()
            .find(|b| b.id == params.bot_id)
            .ok_or_else(|| not_found_bot(&params.bot_id))?;

        let prev_home = self.root.join(&bot.folder);
        ensure_bot_subdirs_safe(&prev_home)?;

        match params.avatar_color {
            FieldPatch::Unchanged => {}
            FieldPatch::Clear => bot.avatar_color = None,
            FieldPatch::Set(c) => {
                let trimmed = c.trim().to_owned();
                bot.avatar_color = (!trimmed.is_empty()).then_some(trimmed);
            }
        }
        if let Some(skills) = params.skills {
            bot.skills = normalize_skill_list(&skills)?;
        }
        if let Some(ref events) = params.events {
            validate_events(events)?;
        }
        bot.updated_at_ms = now_ms;
        bot.validate()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;

        let updated = bot.clone();
        let home = init_bot_home(&self.root, &updated, None)?;
        ensure_bot_subdirs_safe(&home)?;
        materialize_bot_skills(&home, &updated.skills)?;
        if let Some(events) = params.events {
            write_bot_events_sidecar(&home, &events)?;
        }
        self.write_json(BOTS_INDEX_FILE, &bots)?;
        Ok(updated)
    }

    /// Reads the `events.json` sidecar rules for `bot_id`.
    pub fn get_events(&self, bot_id: &BotId) -> io::Result<Vec<BotEventRuleConfig>> {
        let bot = self.get_bot(bot_id)?.ok_or_else(|| not_found_bot(bot_id))?;
        let home = self.bot_home(&bot);
        ensure_bot_subdirs_safe(&home)?;
        let path = home.join(EVENTS_FILE);
        ensure_no_symlink_path(&path)?;
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        if bytes.len() > MAX_JSON_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bot events file exceeds size limit",
            ));
        }
        serde_json::from_slice(&bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    /// Uploads and sanitizes a PNG avatar (`strip_png_metadata`, `0o600`) for `bot_id`.
    pub fn upload_avatar(
        &self,
        bot_id: &BotId,
        png_bytes: &[u8],
        now_ms: u64,
    ) -> io::Result<BotAvatarData> {
        if png_bytes.is_empty() || png_bytes.len() > MAX_AVATAR_PNG_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "avatar PNG size is invalid",
            ));
        }
        let cleaned = strip_png_metadata(png_bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;

        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_bots()?;
        let bot = bots
            .iter_mut()
            .find(|b| &b.id == bot_id)
            .ok_or_else(|| not_found_bot(bot_id))?;

        let home = init_bot_home(&self.root, bot, None)?;
        ensure_bot_subdirs_safe(&home)?;
        let avatar_path = home.join(AVATAR_FILE);
        ensure_no_symlink_path(&avatar_path)?;
        write_atomic_0600(&avatar_path, &cleaned)?;

        bot.avatar = Some(AVATAR_FILE.to_owned());
        bot.updated_at_ms = now_ms;
        let updated = bot.clone();
        init_bot_home(&self.root, &updated, None)?;
        self.write_json(BOTS_INDEX_FILE, &bots)?;

        Ok(BotAvatarData {
            bot_id: bot_id.clone(),
            mime: "image/png".to_owned(),
            png_bytes: cleaned,
        })
    }

    /// Reads the sanitized PNG avatar for `bot_id`.
    pub fn get_avatar(&self, bot_id: &BotId) -> io::Result<BotAvatarData> {
        let bot = self.get_bot(bot_id)?.ok_or_else(|| not_found_bot(bot_id))?;
        let home = self.bot_home(&bot);
        ensure_bot_subdirs_safe(&home)?;
        let avatar_path = home.join(AVATAR_FILE);
        ensure_no_symlink_path(&avatar_path)?;
        let meta = fs::symlink_metadata(&avatar_path)?;
        if usize::try_from(meta.len()).unwrap_or(usize::MAX) > MAX_AVATAR_PNG_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "avatar PNG exceeds size limit",
            ));
        }
        let bytes = fs::read(&avatar_path)?;
        Ok(BotAvatarData {
            bot_id: bot_id.clone(),
            mime: "image/png".to_owned(),
            png_bytes: bytes,
        })
    }

    /// Gets the global default bots configured for GitHub / Linear tracker webhooks/polling.
    pub fn get_default_tracker_bots(&self) -> io::Result<DefaultTrackerBots> {
        let _guard = lock(&self.io_lock);
        self.read_json_or_default(DEFAULT_TRACKERS_FILE)
    }

    /// Sets or clears the default bot for a tracker `source` (`GitHub` or `Linear`).
    pub fn set_default_tracker_bot(
        &self,
        source: ExternalSource,
        bot_id: Option<BotId>,
    ) -> io::Result<DefaultTrackerBots> {
        if !matches!(source, ExternalSource::GitHub | ExternalSource::Linear) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "default tracker bot only supports GitHub or Linear",
            ));
        }
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_bots()?;
        if let Some(ref bid) = bot_id
            && !bots.iter().any(|b| &b.id == bid)
        {
            return Err(not_found_bot(bid));
        }
        let mut defaults: DefaultTrackerBots = self.read_json_or_default(DEFAULT_TRACKERS_FILE)?;
        match source {
            ExternalSource::GitHub => defaults.github = bot_id,
            ExternalSource::Linear => defaults.linear = bot_id,
            _ => {}
        }
        self.write_json(DEFAULT_TRACKERS_FILE, &defaults)?;
        Ok(defaults)
    }

    /// Applies partial updates to an existing bot and refreshes its managed home files.
    pub fn update_bot(&self, params: UpdateBotParams, now_ms: u64) -> io::Result<BotConfig> {
        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_bots()?;
        let bot = bots
            .iter_mut()
            .find(|b| b.id == params.bot_id)
            .ok_or_else(|| not_found_bot(&params.bot_id))?;

        if let Some(name) = params.name {
            bot.name = validate_bot_name(&name)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        }
        if let Some(description) = params.description {
            bot.description = description;
        }
        params.emoji.apply(&mut bot.emoji);
        params.avatar.apply(&mut bot.avatar);
        let mut agent_changed = false;
        if let Some(agent_id) = params.agent_id {
            let next = krill_proto::agent::canonical_id(agent_id.trim());
            let prev = krill_proto::agent::canonical_id(&bot.agent_id);
            agent_changed = !next.is_empty() && next != prev;
            next.clone_into(&mut bot.agent_id);
        }
        if agent_changed {
            // Clear stale provider-specific model/effort unless the caller explicitly sets new ones.
            if matches!(params.model, FieldPatch::Unchanged) {
                bot.model = None;
            }
            if matches!(params.effort, FieldPatch::Unchanged) {
                bot.effort = None;
            }
        }
        params.model.apply(&mut bot.model);
        params.effort.apply(&mut bot.effort);
        if let Some(perm) = params.permission_mode {
            bot.permission_mode = perm;
        }
        params.workspace.apply(&mut bot.workspace);
        if let Some(paused) = params.paused {
            bot.paused = paused;
        }
        if let Some(completed) = params.first_run_completed {
            bot.first_run_completed = completed;
        }
        if let Some(notifications) = params.notifications {
            bot.notifications = notifications;
        }
        if let Some(schedules) = params.schedules {
            bot.schedules = schedules;
        }
        if let Some(look_around) = params.look_around {
            bot.look_around = look_around;
        }
        if let Some(memory_tidy) = params.memory_tidy {
            bot.memory_tidy = memory_tidy;
        }
        if let Some(mcp_tools) = params.mcp_tools {
            bot.mcp_tools = mcp_tools;
        }
        bot.updated_at_ms = now_ms;
        bot.validate()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;

        let updated = bot.clone();
        init_bot_home(&self.root, &updated, None)?;
        self.write_json(BOTS_INDEX_FILE, &bots)?;
        Ok(updated)
    }

    /// Replaces the connector configuration on a bot and refreshes its home `bot.json`.
    pub fn set_connector_config(
        &self,
        bot_id: &BotId,
        connectors: BotConnectorConfig,
        now_ms: u64,
    ) -> io::Result<BotConfig> {
        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_bots()?;
        let bot = bots
            .iter_mut()
            .find(|b| &b.id == bot_id)
            .ok_or_else(|| not_found_bot(bot_id))?;
        bot.connectors = connectors;
        bot.updated_at_ms = now_ms;
        bot.validate()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        let updated = bot.clone();
        init_bot_home(&self.root, &updated, None)?;
        self.write_json(BOTS_INDEX_FILE, &bots)?;
        Ok(updated)
    }

    /// Marks `first_run_completed = true` if not already completed. Returns `true` if changed.
    pub fn mark_first_run_completed(&self, bot_id: &BotId, now_ms: u64) -> io::Result<bool> {
        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_bots()?;
        let Some(bot) = bots.iter_mut().find(|b| &b.id == bot_id) else {
            return Ok(false);
        };
        if bot.first_run_completed {
            return Ok(false);
        }
        bot.first_run_completed = true;
        bot.updated_at_ms = now_ms;
        let updated = bot.clone();
        init_bot_home(&self.root, &updated, None)?;
        self.write_json(BOTS_INDEX_FILE, &bots)?;
        Ok(true)
    }

    /// Deletes a bot from the registry, cleans up links/access requests/groups, and optionally removes its home.
    pub fn delete_bot(&self, bot_id: &BotId, delete_home: bool) -> io::Result<BotConfig> {
        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_bots()?;
        let Some(pos) = bots.iter().position(|b| &b.id == bot_id) else {
            return Err(not_found_bot(bot_id));
        };
        let removed = bots.remove(pos);
        self.write_json(BOTS_INDEX_FILE, &bots)?;

        let mut links: Vec<ExternalConversationLink> = self.read_json_or_default(LINKS_FILE)?;
        let prev_links = links.len();
        links.retain(|l| &l.bot_id != bot_id);
        if links.len() != prev_links {
            self.write_json(LINKS_FILE, &links)?;
        }

        let mut reqs: Vec<BotAccessRequest> = self.read_json_or_default(ACCESS_REQUESTS_FILE)?;
        let prev_reqs = reqs.len();
        reqs.retain(|r| &r.bot_id != bot_id);
        if reqs.len() != prev_reqs {
            self.write_json(ACCESS_REQUESTS_FILE, &reqs)?;
        }

        let mut groups: Vec<BotGroupChat> = self.read_json_or_default(GROUPS_FILE)?;
        let mut groups_changed = false;
        for group in &mut groups {
            let before = group.bot_ids.len();
            group.bot_ids.retain(|id| id != bot_id);
            if group.bot_ids.len() != before {
                groups_changed = true;
            }
        }
        if groups_changed {
            self.write_json(GROUPS_FILE, &groups)?;
        }

        let mut suggestions: Vec<BotSuggestion> = self.read_json_or_default(SUGGESTIONS_FILE)?;
        let prev_sugg = suggestions.len();
        suggestions.retain(|s| &s.bot_id != bot_id);
        if suggestions.len() != prev_sugg {
            self.write_json(SUGGESTIONS_FILE, &suggestions)?;
        }

        let mut defaults: DefaultTrackerBots = self.read_json_or_default(DEFAULT_TRACKERS_FILE)?;
        let mut defaults_changed = false;
        if defaults.github.as_ref() == Some(bot_id) {
            defaults.github = None;
            defaults_changed = true;
        }
        if defaults.linear.as_ref() == Some(bot_id) {
            defaults.linear = None;
            defaults_changed = true;
        }
        if defaults_changed {
            self.write_json(DEFAULT_TRACKERS_FILE, &defaults)?;
        }

        if delete_home {
            let home = self.bot_home(&removed);
            if home.exists() {
                if fs::symlink_metadata(&home)?.file_type().is_symlink() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "refusing to delete symlinked bot home",
                    ));
                }
                assert_bot_owns_home(&home, removed.id.as_str())?;
                fs::remove_dir_all(&home)?;
            }
        }

        Ok(removed)
    }

    // ---------- Soul & Memory ----------

    /// Reads `<home>/SOUL.md` for `bot_id`.
    pub fn get_soul(&self, bot_id: &BotId) -> io::Result<String> {
        let bot = self.get_bot(bot_id)?.ok_or_else(|| not_found_bot(bot_id))?;
        let home = init_bot_home(&self.root, &bot, None)?;
        read_text_bounded(&home.join("SOUL.md"))
    }

    /// Writes `<home>/SOUL.md` atomically (`0o600`) and updates `description` when empty.
    pub fn set_soul(&self, bot_id: &BotId, content: &str, now_ms: u64) -> io::Result<()> {
        if content.len() > MAX_TEXT_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SOUL.md exceeds maximum allowed size",
            ));
        }
        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_bots()?;
        let bot = bots
            .iter_mut()
            .find(|b| &b.id == bot_id)
            .ok_or_else(|| not_found_bot(bot_id))?;
        let home = init_bot_home(&self.root, bot, None)?;
        write_atomic_0600(&home.join("SOUL.md"), content.as_bytes())?;
        if let Some(desc) = soul_description(content)
            && bot.description.is_empty()
        {
            bot.description = desc;
            bot.updated_at_ms = now_ms;
            let snapshot = bot.clone();
            init_bot_home(&self.root, &snapshot, None)?;
            self.write_json(BOTS_INDEX_FILE, &bots)?;
        }
        Ok(())
    }

    /// Reads `<home>/MEMORY.md` for `bot_id`.
    pub fn get_memory(&self, bot_id: &BotId) -> io::Result<String> {
        let bot = self.get_bot(bot_id)?.ok_or_else(|| not_found_bot(bot_id))?;
        let home = init_bot_home(&self.root, &bot, None)?;
        let path = home.join("MEMORY.md");
        if !path.exists() {
            return Ok(BOT_MEMORY_SEED.to_owned());
        }
        read_text_bounded(&path)
    }

    /// Updates `<home>/MEMORY.md` with deletion guard enforcement unless `allow_deletions` is `true`.
    pub fn update_memory(
        &self,
        bot_id: &BotId,
        content: &str,
        allow_deletions: bool,
    ) -> Result<String, MemoryUpdateError> {
        let now_ms = current_unix_ms();
        self.apply_memory_with_history(
            bot_id,
            content,
            allow_deletions,
            "owner",
            "update",
            now_ms,
        )?;
        Ok(content.to_owned())
    }

    /// Updates `<home>/MEMORY.md` recording an attribution entry in `.history.jsonl`.
    pub(crate) fn apply_memory_with_history(
        &self,
        bot_id: &BotId,
        content: &str,
        allow_deletions: bool,
        actor: &str,
        action: &str,
        now_ms: u64,
    ) -> Result<Option<BotMemoryHistoryEntry>, MemoryUpdateError> {
        if content.len() > MAX_TEXT_FILE_BYTES {
            return Err(MemoryUpdateError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MEMORY.md exceeds maximum allowed size",
            )));
        }
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_bots()?;
        let bot = bots
            .iter()
            .find(|b| &b.id == bot_id)
            .ok_or_else(|| not_found_bot(bot_id))?;
        let home = init_bot_home(&self.root, bot, None)?;
        ensure_bot_subdirs_safe(&home)?;
        let mem_path = home.join("MEMORY.md");
        ensure_no_symlink_path(&mem_path)?;
        let old = if mem_path.exists() {
            read_text_bounded(&mem_path)?
        } else {
            BOT_MEMORY_SEED.to_owned()
        };
        if !allow_deletions {
            validate_memory_edit(&old, content).map_err(MemoryUpdateError::DeletionBlocked)?;
        }
        write_atomic_0600(&mem_path, content.as_bytes())?;
        let entry = memory_history::record_change(
            &home,
            bot_id,
            actor,
            action,
            &old,
            content,
            now_ms,
            old != content,
        )?;
        Ok(entry)
    }

    /// Lists `MEMORY.md` change history entries for `bot_id` (newest first).
    pub fn get_memory_history(
        &self,
        bot_id: &BotId,
        limit: Option<u32>,
    ) -> io::Result<Vec<BotMemoryHistoryEntry>> {
        let bot = self.get_bot(bot_id)?.ok_or_else(|| not_found_bot(bot_id))?;
        let home = self.bot_home(&bot);
        ensure_bot_subdirs_safe(&home)?;
        memory_history::list_history(&home, limit)
    }

    /// Records a bot's proposal of a new bot (Orca `bots.createOffer`). A name an existing bot
    /// already has is refused, as Orca does, so the card never offers a duplicate.
    pub fn offer_bot(
        &self,
        bot_id: &BotId,
        session: Option<u64>,
        name: &str,
        purpose: &str,
        now_ms: u64,
    ) -> io::Result<BotOffer> {
        let (name, purpose) = (name.trim(), purpose.trim());
        if name.is_empty() || name.chars().count() > 64 || purpose.chars().count() > 2_000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a proposal needs a name (up to 64 characters) and a purpose (up to 2000)",
            ));
        }
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
        if !bots.iter().any(|b| &b.id == bot_id) {
            return Err(not_found_bot(bot_id));
        }
        if bots.iter().any(|b| b.name.eq_ignore_ascii_case(name)) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "Your owner already has a bot named {name}; no card was shown. Pick another name."
                ),
            ));
        }
        let mut offers: Vec<BotOffer> = self.read_json_or_default(BOT_OFFERS_FILE)?;
        let offer = BotOffer {
            offer_id: format!("offer-{now_ms}-{}", offers.len() + 1),
            bot_id: bot_id.clone(),
            session_id: session,
            name: name.to_owned(),
            purpose: purpose.to_owned(),
            offered_at_ms: now_ms,
            created: None,
            dismissed: false,
        };
        offers.push(offer.clone());
        // ponytail: keeps the newest 200 proposals across bots; per-bot caps if one bot floods.
        let excess = offers.len().saturating_sub(200);
        offers.drain(..excess);
        self.write_json(BOT_OFFERS_FILE, &offers)?;
        Ok(offer)
    }

    /// `bot_id`'s proposals, oldest first.
    pub fn list_bot_offers(&self, bot_id: &BotId) -> io::Result<Vec<BotOffer>> {
        let offers: Vec<BotOffer> = self.read_json_or_default(BOT_OFFERS_FILE)?;
        Ok(offers.into_iter().filter(|o| &o.bot_id == bot_id).collect())
    }

    /// The owner answered a proposal: made `created`, or Not now.
    pub fn resolve_bot_offer(&self, offer_id: &str, created: Option<BotId>) -> io::Result<()> {
        let _guard = lock(&self.io_lock);
        let mut offers: Vec<BotOffer> = self.read_json_or_default(BOT_OFFERS_FILE)?;
        let offer = offers
            .iter_mut()
            .find(|o| o.offer_id == offer_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such bot proposal"))?;
        offer.dismissed = created.is_none();
        offer.created = created;
        self.write_json(BOT_OFFERS_FILE, &offers)
    }

    /// Records the `MEMORY.md` edit a bot's turn in chat `session` made itself (Orca's turn memory receipt).
    pub fn record_turn_memory(
        &self,
        bot_id: &BotId,
        session: u64,
        before: &str,
        after: &str,
        now_ms: u64,
    ) -> io::Result<Option<BotMemoryHistoryEntry>> {
        let bot = self.get_bot(bot_id)?.ok_or_else(|| not_found_bot(bot_id))?;
        let home = self.bot_home(&bot);
        ensure_bot_subdirs_safe(&home)?;
        memory_history::record_turn_change(&home, bot_id, session, before, after, now_ms)
    }

    /// Rewrites a single fact line in `MEMORY.md` and records history.
    pub fn edit_memory_fact(
        &self,
        bot_id: &BotId,
        old_fact: &str,
        new_fact: &str,
        actor: &str,
        now_ms: u64,
    ) -> io::Result<String> {
        let current = self.get_memory(bot_id)?;
        let rebuilt = memory_history::rewrite_single_fact(&current, old_fact, new_fact)?;
        self.apply_memory_with_history(bot_id, &rebuilt, true, actor, "edit_fact", now_ms)
            .map_err(|err| io::Error::other(err.to_string()))?;
        Ok(rebuilt)
    }

    /// Reverts a recorded `.history.jsonl` change on `bot_id`'s `MEMORY.md`.
    pub fn undo_memory_entry(
        &self,
        bot_id: &BotId,
        entry_id: &str,
        actor: &str,
        now_ms: u64,
    ) -> io::Result<String> {
        let bot = self.get_bot(bot_id)?.ok_or_else(|| not_found_bot(bot_id))?;
        let home = self.bot_home(&bot);
        ensure_bot_subdirs_safe(&home)?;
        let entry = memory_history::find_entry(&home, entry_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("memory history entry not found: {entry_id}"),
            )
        })?;
        if !entry.undoable || entry.undone {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "memory change is not undoable or was already undone",
            ));
        }
        let restored = entry.before_content.clone();
        self.apply_memory_with_history(
            bot_id,
            &restored,
            true,
            actor,
            &format!("undo:{entry_id}"),
            now_ms,
        )
        .map_err(|err| io::Error::other(err.to_string()))?;
        memory_history::mark_undone(&home, entry_id)?;
        Ok(restored)
    }

    /// Lists valid topic markdown files under `<home>/memory/<topic>.md`.
    pub fn list_memory_topics(&self, bot_id: &BotId) -> io::Result<Vec<BotMemoryTopicInfo>> {
        let bot = self.get_bot(bot_id)?.ok_or_else(|| not_found_bot(bot_id))?;
        let home = self.bot_home(&bot);
        ensure_bot_subdirs_safe(&home)?;
        memory_history::list_topics(&home)
    }

    /// Reads `<home>/memory/<topic>.md`.
    pub fn get_memory_topic(&self, bot_id: &BotId, topic: &str) -> io::Result<String> {
        let bot = self.get_bot(bot_id)?.ok_or_else(|| not_found_bot(bot_id))?;
        let home = self.bot_home(&bot);
        ensure_bot_subdirs_safe(&home)?;
        memory_history::get_topic(&home, topic)
    }

    /// Writes `<home>/memory/<topic>.md`.
    pub fn set_memory_topic(
        &self,
        bot_id: &BotId,
        topic: &str,
        content: &str,
    ) -> io::Result<BotMemoryTopicInfo> {
        let bot = self.get_bot(bot_id)?.ok_or_else(|| not_found_bot(bot_id))?;
        let home = init_bot_home(&self.root, &bot, None)?;
        ensure_bot_subdirs_safe(&home)?;
        memory_history::set_topic(&home, topic, content)
    }

    /// Explicitly removes matching fact line(s) from `<home>/MEMORY.md`.
    pub fn forget_memory_fact(&self, bot_id: &BotId, fact: &str) -> io::Result<(String, usize)> {
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_bots()?;
        let bot = bots
            .iter()
            .find(|b| &b.id == bot_id)
            .ok_or_else(|| not_found_bot(bot_id))?;
        let home = init_bot_home(&self.root, bot, None)?;
        ensure_bot_subdirs_safe(&home)?;
        let mem_path = home.join("MEMORY.md");
        ensure_no_symlink_path(&mem_path)?;
        let old = if mem_path.exists() {
            read_text_bounded(&mem_path)?
        } else {
            BOT_MEMORY_SEED.to_owned()
        };
        let (updated, removed) = forget_memory_fact(&old, fact);
        if removed > 0 {
            write_atomic_0600(&mem_path, updated.as_bytes())?;
            memory_history::record_change(
                &home,
                bot_id,
                "owner",
                "forget",
                &old,
                &updated,
                current_unix_ms(),
                true,
            )?;
        }
        Ok((updated, removed))
    }

    /// Appends a single dated/sourced fact line to `<home>/MEMORY.md`.
    pub fn append_memory_fact_line(
        &self,
        bot_id: &BotId,
        fact: &str,
        date: Option<&str>,
        source: Option<&str>,
    ) -> io::Result<String> {
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_bots()?;
        let bot = bots
            .iter()
            .find(|b| &b.id == bot_id)
            .ok_or_else(|| not_found_bot(bot_id))?;
        let home = init_bot_home(&self.root, bot, None)?;
        ensure_bot_subdirs_safe(&home)?;
        let mem_path = home.join("MEMORY.md");
        ensure_no_symlink_path(&mem_path)?;
        let old = if mem_path.exists() {
            read_text_bounded(&mem_path)?
        } else {
            BOT_MEMORY_SEED.to_owned()
        };
        let updated = append_memory_fact(&old, fact, date, source);
        write_atomic_0600(&mem_path, updated.as_bytes())?;
        memory_history::record_change(
            &home,
            bot_id,
            "bot",
            "append",
            &old,
            &updated,
            current_unix_ms(),
            true,
        )?;
        Ok(updated)
    }

    // ---------- External Conversation Links ----------

    /// Lists all external conversation links, optionally filtered by `bot_id`.
    pub fn list_links(&self, bot_id: Option<&BotId>) -> io::Result<Vec<ExternalConversationLink>> {
        let _guard = lock(&self.io_lock);
        let links: Vec<ExternalConversationLink> = self.read_json_or_default(LINKS_FILE)?;
        Ok(links
            .into_iter()
            .filter(|l| bot_id.is_none_or(|id| &l.bot_id == id))
            .collect())
    }

    /// Gets a link by ID.
    pub fn get_link(&self, link_id: &str) -> io::Result<Option<ExternalConversationLink>> {
        let _guard = lock(&self.io_lock);
        let links: Vec<ExternalConversationLink> = self.read_json_or_default(LINKS_FILE)?;
        Ok(links.into_iter().find(|l| l.id == link_id))
    }

    /// Looks up or creates a persistent link for `(bot_id, source, workspace_id, channel_id, thread_id)`.
    #[expect(clippy::too_many_arguments, reason = "external conversation key tuple")]
    pub fn lookup_or_create_link(
        &self,
        bot_id: &BotId,
        source: ExternalSource,
        workspace_id: Option<&str>,
        channel_id: &str,
        thread_id: Option<&str>,
        acts_for: ActsFor,
        now_ms: u64,
    ) -> io::Result<ExternalConversationLink> {
        let _guard = lock(&self.io_lock);
        let mut links: Vec<ExternalConversationLink> = self.read_json_or_default(LINKS_FILE)?;
        if let Some(existing) = links.iter_mut().find(|l| {
            &l.bot_id == bot_id
                && l.source == source
                && l.workspace_id.as_deref() == workspace_id
                && l.channel_id == channel_id
                && l.thread_id.as_deref() == thread_id
        }) {
            existing.updated_at_ms = now_ms;
            let found = existing.clone();
            self.write_json(LINKS_FILE, &links)?;
            return Ok(found);
        }
        let seq = ID_SEQ.fetch_add(1, Ordering::Relaxed);
        let link = ExternalConversationLink {
            id: format!("link-{}-{now_ms}-{seq}", source.as_str()),
            bot_id: bot_id.clone(),
            source,
            workspace_id: workspace_id.map(ToOwned::to_owned),
            channel_id: channel_id.to_owned(),
            thread_id: thread_id.map(ToOwned::to_owned),
            session_id: None,
            acts_for,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        links.push(link.clone());
        self.write_json(LINKS_FILE, &links)?;
        Ok(link)
    }

    /// Associates a live Krill [`SessionId`] with an [`ExternalConversationLink`].
    pub fn bind_link_session(
        &self,
        link_id: &str,
        session_id: SessionId,
        now_ms: u64,
    ) -> io::Result<Option<ExternalConversationLink>> {
        let _guard = lock(&self.io_lock);
        let mut links: Vec<ExternalConversationLink> = self.read_json_or_default(LINKS_FILE)?;
        let Some(link) = links.iter_mut().find(|l| l.id == link_id) else {
            return Ok(None);
        };
        link.session_id = Some(session_id);
        link.updated_at_ms = now_ms;
        let updated = link.clone();
        self.write_json(LINKS_FILE, &links)?;
        Ok(Some(updated))
    }

    /// Clears live session bindings on all external links owned by `bot_id` (e.g. after switching
    /// the bot's main agent/provider so incoming messages launch a fresh session on the new runtime).
    pub fn unbind_bot_links(&self, bot_id: &BotId, now_ms: u64) -> io::Result<usize> {
        let _guard = lock(&self.io_lock);
        let mut links: Vec<ExternalConversationLink> = self.read_json_or_default(LINKS_FILE)?;
        let mut cleared = 0_usize;
        for link in &mut links {
            if &link.bot_id == bot_id && link.session_id.is_some() {
                link.session_id = None;
                link.updated_at_ms = now_ms;
                cleared += 1;
            }
        }
        if cleared > 0 {
            self.write_json(LINKS_FILE, &links)?;
        }
        Ok(cleared)
    }

    /// Finds the most recently active link for `bot_id`.
    pub fn latest_link_for_bot(
        &self,
        bot_id: &BotId,
    ) -> io::Result<Option<ExternalConversationLink>> {
        let mut links = self.list_links(Some(bot_id))?;
        links.sort_by_key(|l| std::cmp::Reverse(l.updated_at_ms));
        Ok(links.into_iter().next())
    }

    // ---------- Access Requests ----------

    /// Lists access requests, optionally filtered by `bot_id`.
    pub fn list_access_requests(
        &self,
        bot_id: Option<&BotId>,
    ) -> io::Result<Vec<BotAccessRequest>> {
        let _guard = lock(&self.io_lock);
        let reqs: Vec<BotAccessRequest> = self.read_json_or_default(ACCESS_REQUESTS_FILE)?;
        Ok(reqs
            .into_iter()
            .filter(|r| bot_id.is_none_or(|id| &r.bot_id == id))
            .collect())
    }

    /// Evaluates whether `(source, sender_id, channel_id)` is allowed to message `bot_id`.
    /// If not authorized, creates or returns a pending [`BotAccessRequest`].
    pub fn check_or_request_access(
        &self,
        bot_id: &BotId,
        source: ExternalSource,
        sender_id: &str,
        sender_name: &str,
        channel_id: &str,
        now_ms: u64,
    ) -> io::Result<AccessDecision> {
        self.check_or_request_access_with_workspace(
            bot_id,
            source,
            None,
            sender_id,
            sender_name,
            channel_id,
            now_ms,
        )
    }

    /// Evaluates sender access including an optional `workspace_id` for multi-workspace connectors.
    #[expect(clippy::too_many_arguments, reason = "access decision context tuple")]
    pub fn check_or_request_access_with_workspace(
        &self,
        bot_id: &BotId,
        source: ExternalSource,
        workspace_id: Option<&str>,
        sender_id: &str,
        sender_name: &str,
        channel_id: &str,
        now_ms: u64,
    ) -> io::Result<AccessDecision> {
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_bots()?;
        let bot = bots
            .iter()
            .find(|b| &b.id == bot_id)
            .ok_or_else(|| not_found_bot(bot_id))?;

        if is_sender_allowed_with_workspace(bot, source, workspace_id, sender_id, channel_id) {
            return Ok(AccessDecision::Allowed);
        }

        let mut reqs: Vec<BotAccessRequest> = self.read_json_or_default(ACCESS_REQUESTS_FILE)?;
        let mut meta_map: BTreeMap<String, AccessMetaEntry> =
            self.read_json_or_default(ACCESS_META_FILE)?;
        if let Some(existing) = reqs
            .iter()
            .find(|r| &r.bot_id == bot_id && r.source == source && r.sender_id == sender_id)
        {
            return Ok(match existing.status {
                BotAccessRequestStatus::Approved => {
                    if let Some(ws) = workspace_id {
                        let stored_ws = meta_map
                            .get(&existing.token)
                            .and_then(|m| m.workspace_id.as_deref());
                        if stored_ws.is_some_and(|w| w != ws) {
                            return Ok(AccessDecision::Denied(
                                "sender is approved for a different workspace".to_owned(),
                            ));
                        }
                    }
                    AccessDecision::Allowed
                }
                BotAccessRequestStatus::Pending => {
                    if let Some(meta) = meta_map.get_mut(&existing.token)
                        && now_ms.saturating_sub(meta.last_notified_ms) >= ACCESS_NOTIFY_THROTTLE_MS
                    {
                        meta.last_notified_ms = now_ms;
                        self.write_json(ACCESS_META_FILE, &meta_map).ok();
                    }
                    AccessDecision::Pending(existing.clone())
                }
                BotAccessRequestStatus::Declined => {
                    AccessDecision::Denied("sender access request was declined".to_owned())
                }
            });
        }

        let pending_for_bot = reqs
            .iter()
            .filter(|r| &r.bot_id == bot_id && r.status == BotAccessRequestStatus::Pending)
            .count();
        if pending_for_bot >= MAX_PENDING_ACCESS_PER_BOT {
            return Ok(AccessDecision::Denied(
                "pending access request queue is full for this bot".to_owned(),
            ));
        }

        let token = generate_random_hex(16)?;
        let pairing_code = generate_pairing_code(8)?;
        let req = BotAccessRequest {
            token: token.clone(),
            bot_id: bot_id.clone(),
            source,
            sender_id: sender_id.to_owned(),
            sender_name: sender_name.to_owned(),
            channel_id: channel_id.to_owned(),
            status: BotAccessRequestStatus::Pending,
            created_at: now_ms,
            decided_at: None,
        };
        meta_map.insert(
            token,
            AccessMetaEntry {
                pairing_code,
                workspace_id: workspace_id.map(ToOwned::to_owned),
                last_notified_ms: now_ms,
            },
        );
        reqs.push(req.clone());
        self.write_json(ACCESS_REQUESTS_FILE, &reqs)?;
        self.write_json(ACCESS_META_FILE, &meta_map)?;
        Ok(AccessDecision::Pending(req))
    }

    /// Returns the human pairing code associated with `token`, if recorded.
    pub fn pairing_code_for_token(&self, token: &str) -> io::Result<Option<String>> {
        let _guard = lock(&self.io_lock);
        let meta_map: BTreeMap<String, AccessMetaEntry> =
            self.read_json_or_default(ACCESS_META_FILE)?;
        Ok(meta_map.get(token).map(|m| m.pairing_code.clone()))
    }

    /// Approves or declines a pending access request by `token`.
    /// Approving adds the sender as a [`BotMember`] on the bot config.
    pub fn decide_access_request(
        &self,
        token: &str,
        approve: bool,
        now_ms: u64,
    ) -> io::Result<BotAccessRequest> {
        let _guard = lock(&self.io_lock);
        let mut reqs: Vec<BotAccessRequest> = self.read_json_or_default(ACCESS_REQUESTS_FILE)?;
        let meta_map: BTreeMap<String, AccessMetaEntry> =
            self.read_json_or_default(ACCESS_META_FILE)?;
        let workspace_id = meta_map.get(token).and_then(|m| m.workspace_id.clone());
        let req = reqs.iter_mut().find(|r| r.token == token).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "access request token not found")
        })?;
        req.status = if approve {
            BotAccessRequestStatus::Approved
        } else {
            BotAccessRequestStatus::Declined
        };
        req.decided_at = Some(now_ms);
        let decided = req.clone();
        self.write_json(ACCESS_REQUESTS_FILE, &reqs)?;

        if approve {
            let mut bots: Vec<BotConfig> = self.read_bots()?;
            if let Some(bot) = bots.iter_mut().find(|b| b.id == decided.bot_id) {
                let already = bot
                    .members
                    .iter()
                    .any(|m| m.source == decided.source && m.id == decided.sender_id);
                if !already {
                    bot.members.push(BotMember {
                        id: decided.sender_id.clone(),
                        name: decided.sender_name.clone(),
                        source: decided.source,
                        workspace_id,
                    });
                    bot.updated_at_ms = now_ms;
                    let snapshot = bot.clone();
                    init_bot_home(&self.root, &snapshot, None)?;
                    self.write_json(BOTS_INDEX_FILE, &bots)?;
                }
            }
        }

        Ok(decided)
    }

    /// Redeems a pending pairing code (or exact request token) with rate-limit throttling.
    pub fn redeem_pairing_code(&self, code: &str, now_ms: u64) -> io::Result<BotAccessRequest> {
        let trimmed = code.trim();
        if trimmed.is_empty() || trimmed.len() > 128 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid pairing code",
            ));
        }
        {
            let mut failures = lock(&self.pairing_failures);
            failures.retain(|&t| now_ms.saturating_sub(t) < PAIRING_ATTEMPT_WINDOW_MS);
            if failures.len() >= PAIRING_MAX_FAILURES_PER_WINDOW {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "too many failed pairing code attempts; try again later",
                ));
            }
        }

        let token = {
            let _guard = lock(&self.io_lock);
            let reqs: Vec<BotAccessRequest> = self.read_json_or_default(ACCESS_REQUESTS_FILE)?;
            let meta_map: BTreeMap<String, AccessMetaEntry> =
                self.read_json_or_default(ACCESS_META_FILE)?;
            reqs.iter()
                .find(|r| {
                    r.status == BotAccessRequestStatus::Pending
                        && (r.token == trimmed
                            || meta_map
                                .get(&r.token)
                                .is_some_and(|m| m.pairing_code.eq_ignore_ascii_case(trimmed)))
                })
                .map(|r| r.token.clone())
        };

        let Some(matched_token) = token else {
            lock(&self.pairing_failures).push(now_ms);
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "pairing code not found or already used",
            ));
        };

        self.decide_access_request(&matched_token, true, now_ms)
    }

    // ---------- Group Chats ----------

    /// Lists all multi-bot group chats.
    pub fn list_groups(&self) -> io::Result<Vec<BotGroupChat>> {
        let _guard = lock(&self.io_lock);
        self.read_json_or_default(GROUPS_FILE)
    }

    /// Creates a new multi-bot group chat after validating all member `bot_ids` exist.
    pub fn create_group(
        &self,
        name: &str,
        bot_ids: Vec<BotId>,
        description: String,
        now_ms: u64,
    ) -> io::Result<BotGroupChat> {
        let name = validate_bot_name(name)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_bots()?;
        let mut unique_ids = Vec::new();
        for bid in bot_ids {
            if !bots.iter().any(|b| b.id == bid) {
                return Err(not_found_bot(&bid));
            }
            if !unique_ids.contains(&bid) {
                unique_ids.push(bid);
            }
        }
        if unique_ids.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "group must contain at least one bot",
            ));
        }

        let mut groups: Vec<BotGroupChat> = self.read_json_or_default(GROUPS_FILE)?;
        let seq = ID_SEQ.fetch_add(1, Ordering::Relaxed);
        let group = BotGroupChat {
            id: format!("grp-{}-{seq}", bot_slug_from_name(&name).as_str()),
            name,
            bot_ids: unique_ids,
            description,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        groups.push(group.clone());
        self.write_json(GROUPS_FILE, &groups)?;
        Ok(group)
    }

    /// Finds a group by ID or case-insensitive name.
    pub fn find_group(&self, query: &str) -> io::Result<Option<BotGroupChat>> {
        let trimmed = query.trim().strip_prefix("group:").unwrap_or(query.trim());
        let groups = self.list_groups()?;
        Ok(groups
            .into_iter()
            .find(|g| g.id == trimmed || g.name.eq_ignore_ascii_case(trimmed)))
    }

    /// Gets a group by exact ID.
    pub fn get_group(&self, group_id: &str) -> io::Result<Option<BotGroupChat>> {
        let groups = self.list_groups()?;
        Ok(groups.into_iter().find(|g| g.id == group_id))
    }

    /// Updates a group chat's name, description, and/or member bot list.
    pub fn update_group(
        &self,
        group_id: &str,
        name: Option<&str>,
        description: Option<String>,
        bot_ids: Option<Vec<BotId>>,
        now_ms: u64,
    ) -> io::Result<BotGroupChat> {
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_bots()?;
        let mut groups: Vec<BotGroupChat> = self.read_json_or_default(GROUPS_FILE)?;
        let group = groups
            .iter_mut()
            .find(|g| g.id == group_id)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("group not found: {group_id}"),
                )
            })?;
        if let Some(new_name) = name {
            group.name = validate_bot_name(new_name)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        }
        if let Some(desc) = description {
            group.description = desc;
        }
        if let Some(ids) = bot_ids {
            let mut unique_ids = Vec::new();
            for bid in ids {
                if !bots.iter().any(|b| b.id == bid) {
                    return Err(not_found_bot(&bid));
                }
                if !unique_ids.contains(&bid) {
                    unique_ids.push(bid);
                }
            }
            if unique_ids.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "group must contain at least one bot",
                ));
            }
            group.bot_ids = unique_ids;
        }
        group.updated_at_ms = now_ms;
        let updated = group.clone();
        self.write_json(GROUPS_FILE, &groups)?;
        Ok(updated)
    }

    /// Deletes a group chat by ID.
    pub fn delete_group(&self, group_id: &str) -> io::Result<()> {
        let _guard = lock(&self.io_lock);
        let mut groups: Vec<BotGroupChat> = self.read_json_or_default(GROUPS_FILE)?;
        let before = groups.len();
        groups.retain(|g| g.id != group_id);
        if groups.len() == before {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("group not found: {group_id}"),
            ));
        }
        self.write_json(GROUPS_FILE, &groups)
    }

    /// Reorders group chats matching `group_ids` first while keeping any unlisted groups after.
    pub fn reorder_groups(&self, group_ids: &[String]) -> io::Result<Vec<BotGroupChat>> {
        let _guard = lock(&self.io_lock);
        let groups: Vec<BotGroupChat> = self.read_json_or_default(GROUPS_FILE)?;
        let mut by_id: BTreeMap<String, BotGroupChat> =
            groups.into_iter().map(|g| (g.id.clone(), g)).collect();
        let mut reordered = Vec::with_capacity(by_id.len());
        for gid in group_ids {
            let Some(group) = by_id.remove(gid) else {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("group not found in reorder list: {gid}"),
                ));
            };
            reordered.push(group);
        }
        reordered.extend(by_id.into_values());
        self.write_json(GROUPS_FILE, &reordered)?;
        Ok(reordered)
    }

    // ---------- Look-Around Suggestions ----------

    /// Loads persisted suggestions for `bot_id`.
    pub fn load_suggestions(&self, bot_id: &BotId) -> io::Result<Vec<BotSuggestion>> {
        let _guard = lock(&self.io_lock);
        let all: Vec<BotSuggestion> = self.read_json_or_default(SUGGESTIONS_FILE)?;
        Ok(all.into_iter().filter(|s| &s.bot_id == bot_id).collect())
    }

    /// Saves the latest suggestions for `bot_id` (replacing previous suggestions for that bot).
    pub fn save_suggestions(&self, bot_id: &BotId, next: &[BotSuggestion]) -> io::Result<()> {
        let _guard = lock(&self.io_lock);
        let mut all: Vec<BotSuggestion> = self.read_json_or_default(SUGGESTIONS_FILE)?;
        all.retain(|s| &s.bot_id != bot_id);
        all.extend_from_slice(next);
        self.write_json(SUGGESTIONS_FILE, &all)
    }

    // ---------- Export / Import Zip ----------

    /// Exports a sanitized `.zip` archive of `bot_id` ready for IPC transfer.
    pub fn export_bot_zip(
        &self,
        bot_id: &BotId,
        options: BotPackageExportOptions,
    ) -> io::Result<(String, Vec<u8>)> {
        let bot = self.get_bot(bot_id)?.ok_or_else(|| not_found_bot(bot_id))?;
        let home = init_bot_home(&self.root, &bot, None)?;
        let bytes = export_bot_home_zip(&home, &bot, options)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
        if bytes.len() > MAX_RPC_ZIP_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "exported bot archive ({} bytes) exceeds IPC limit ({MAX_RPC_ZIP_BYTES} bytes)",
                    bytes.len()
                ),
            ));
        }
        let filename = format!("{}.zip", bot.slug.as_str());
        Ok((filename, bytes))
    }

    /// Imports a sanitized `.zip` bot package into the local bot store.
    pub fn import_bot_zip(
        &self,
        zip_bytes: &[u8],
        name_override: Option<&str>,
        agent_override: Option<&str>,
        now_ms: u64,
    ) -> io::Result<BotConfig> {
        self.import_bot_zip_v2(zip_bytes, None, name_override, agent_override, now_ms)
    }

    /// Imports a sanitized `.zip` bot package supporting explicit `slug_override` distinct from `name_override`.
    pub fn import_bot_zip_v2(
        &self,
        zip_bytes: &[u8],
        slug_override: Option<&str>,
        name_override: Option<&str>,
        agent_override: Option<&str>,
        now_ms: u64,
    ) -> io::Result<BotConfig> {
        let pkg = import_bot_zip(zip_bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;

        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_bots()?;

        let final_name = validate_bot_name(name_override.unwrap_or(&pkg.manifest.name))
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        let slug = if let Some(raw_slug) = slug_override.map(str::trim).filter(|s| !s.is_empty()) {
            validate_bot_slug(raw_slug)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
            let parsed = BotSlug::new(raw_slug)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
            if bots.iter().any(|b| b.slug == parsed) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("bot slug already exists: {}", parsed.as_str()),
                ));
            }
            parsed
        } else {
            let base_slug = if name_override.is_some() {
                bot_slug_from_name(&final_name)
            } else {
                pkg.manifest.slug.clone()
            };
            allocate_unique_slug(&bots, &base_slug)?
        };
        let id = allocate_unique_id(&bots, slug.as_str())?;
        let raw_agent_id = agent_override
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&pkg.manifest.agent_id);
        let agent_id = krill_proto::agent::canonical_id(raw_agent_id.trim()).to_owned();

        let mut cfg = BotConfig::new(id, &final_name, &agent_id, now_ms)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        cfg.slug = slug.clone();
        slug.as_str().clone_into(&mut cfg.folder);
        cfg.description = pkg.manifest.description;
        cfg.emoji = pkg.manifest.emoji;
        cfg.avatar = pkg.manifest.avatar;
        cfg.avatar_color = pkg.manifest.avatar_color;
        cfg.model = pkg.manifest.model;
        cfg.effort = pkg.manifest.effort;
        cfg.permission_mode = pkg.manifest.permission_mode;
        cfg.template_id = pkg.manifest.template_id;
        cfg.notifications = pkg.manifest.notifications;
        cfg.schedules = pkg.manifest.schedules;
        cfg.look_around = pkg.manifest.look_around;
        cfg.memory_tidy = pkg.manifest.memory_tidy;
        cfg.skills = pkg.manifest.skills;
        cfg.mcp_tools = pkg
            .manifest
            .mcp_tools
            .into_iter()
            .map(|t| BotMcpToolConfig {
                id: t.id.clone(),
                name: t.name,
                endpoint_url: t.endpoint_url,
                command: Vec::new(),
                auth_kind: t.auth_kind,
                header_names: t.header_names,
                secret_ref: Some(format!("{}:mcp:{}", cfg.id.as_str(), t.id)),
                enabled: false,
            })
            .collect();
        cfg.validate()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;

        let home_path = self.bot_home(&cfg);
        let home_existed = home_path.exists();
        ensure_bot_subdirs_safe(&home_path)?;

        let result = (|| -> io::Result<()> {
            let home = init_bot_home(&self.root, &cfg, Some(&pkg.soul))?;
            ensure_bot_subdirs_safe(&home)?;
            write_atomic_0600(&home.join("SOUL.md"), pkg.soul.as_bytes())?;
            materialize_bot_skills(&home, &cfg.skills)?;

            if let Some(mem) = &pkg.memory_md {
                let mem_path = home.join("MEMORY.md");
                ensure_no_symlink_path(&mem_path)?;
                write_atomic_0600(&mem_path, mem.as_bytes())?;
            }
            for (topic_file, content) in &pkg.memory_topics {
                let dest = home.join("memory").join(topic_file);
                ensure_no_symlink_path(&dest)?;
                write_atomic_0600(&dest, content.as_bytes())?;
            }
            for (rel_skill, content) in &pkg.skills {
                let dest = home.join("skills").join(rel_skill);
                if let Some(parent) = dest.parent() {
                    ensure_no_symlink_path(parent)?;
                }
                ensure_no_symlink_path(&dest)?;
                write_atomic_0600(&dest, content.as_bytes())?;
            }
            if let Some(png) = &pkg.avatar_png {
                let clean_png = strip_png_metadata(png)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
                let avatar_path = home.join(AVATAR_FILE);
                ensure_no_symlink_path(&avatar_path)?;
                write_atomic_0600(&avatar_path, &clean_png)?;
            }

            bots.push(cfg.clone());
            self.write_json(BOTS_INDEX_FILE, &bots)?;
            Ok(())
        })();

        if let Err(err) = result {
            if !home_existed {
                fs::remove_dir_all(&home_path).ok();
            }
            return Err(err);
        }
        Ok(cfg)
    }

    // ---------- External Bot Discovery & Idempotent Migration ----------

    /// Scans the host (or `custom_path`) for Orca, `OpenClaw`, Hermes, and `OpenBot` bots,
    /// and idempotently migrates them when `dry_run == false`.
    pub fn import_external_bots(
        &self,
        source: Option<ExternalBotSourceApp>,
        custom_path: Option<&Path>,
        dry_run: bool,
        now_ms: u64,
    ) -> io::Result<(Vec<ExternalBotCandidate>, Vec<BotConfig>)> {
        let candidates = proto_discover_external_bots(source, custom_path);
        if dry_run || candidates.is_empty() {
            return Ok((candidates, Vec::new()));
        }

        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_bots()?;
        let mut imported_ledger: BTreeMap<String, String> =
            self.read_json_or_default(IMPORTED_SOURCES_FILE)?;
        let mut imported = Vec::new();

        for cand in &candidates {
            if let Some(cfg) = self.import_external_candidate(
                cand,
                None,
                false,
                &mut bots,
                &mut imported_ledger,
                now_ms,
            )? {
                imported.push(cfg);
            }
        }

        if !imported.is_empty() {
            self.write_json(BOTS_INDEX_FILE, &bots)?;
            self.write_json(IMPORTED_SOURCES_FILE, &imported_ledger)?;
        }

        Ok((candidates, imported))
    }

    /// Returns active `(canonical_source_path, bot_id)` pairs from `imported_sources.json`.
    pub fn list_imported_sources(&self) -> io::Result<Vec<(String, BotId)>> {
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
        let ledger: BTreeMap<String, String> = self.read_json_or_default(IMPORTED_SOURCES_FILE)?;
        Ok(ledger
            .into_iter()
            .filter_map(|(path, id)| {
                bots.iter()
                    .find(|b| b.id.as_str() == id)
                    .map(|b| (path, b.id.clone()))
            })
            .collect())
    }

    /// Imports a single discovered external bot directory, allowing a new copy even when an earlier
    /// bot came from the same source, and applying the owner's chosen `params`.
    pub fn import_external_bot_copy(
        &self,
        source: Option<ExternalBotSourceApp>,
        path: &str,
        params: CreateBotV2Params,
        now_ms: u64,
    ) -> io::Result<BotConfig> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "import source path cannot be empty",
            ));
        }
        let target_path = Path::new(trimmed);
        let canonical_target = canonicalize(target_path)
            .map_or_else(|_| trimmed.to_owned(), |p| p.to_string_lossy().into_owned());
        let candidates = proto_discover_external_bots(source, Some(target_path));
        let cand = candidates
            .iter()
            .find(|c| {
                let c_canon = canonicalize(Path::new(&c.path))
                    .map_or_else(|_| c.path.clone(), |p| p.to_string_lossy().into_owned());
                c_canon == canonical_target
            })
            .or_else(|| candidates.first())
            .cloned()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no external bot found at {trimmed}"),
                )
            })?;

        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
        let mut imported_ledger: BTreeMap<String, String> =
            self.read_json_or_default(IMPORTED_SOURCES_FILE)?;
        let cfg = self
            .import_external_candidate(
                &cand,
                Some(params),
                true,
                &mut bots,
                &mut imported_ledger,
                now_ms,
            )?
            .ok_or_else(|| io::Error::other("import produced no bot"))?;
        self.write_json(BOTS_INDEX_FILE, &bots)?;
        self.write_json(IMPORTED_SOURCES_FILE, &imported_ledger)?;
        Ok(cfg)
    }

    fn import_external_candidate(
        &self,
        cand: &ExternalBotCandidate,
        params: Option<CreateBotV2Params>,
        force_copy: bool,
        bots: &mut Vec<BotConfig>,
        imported_ledger: &mut BTreeMap<String, String>,
        now_ms: u64,
    ) -> io::Result<Option<BotConfig>> {
        let src_dir = Path::new(&cand.path);
        let canonical_key = canonicalize(src_dir)
            .map_or_else(|_| cand.path.clone(), |p| p.to_string_lossy().into_owned());
        if !force_copy
            && let Some(existing_id) = imported_ledger.get(&canonical_key)
            && bots.iter().any(|b| b.id.as_str() == existing_id)
        {
            return Ok(None);
        }

        let manifest = read_external_manifest(src_dir);
        if !force_copy {
            if let Some(ref ext_id) = manifest.id
                && bots.iter().any(|b| b.id.as_str() == ext_id)
            {
                imported_ledger.insert(canonical_key, ext_id.clone());
                return Ok(None);
            }
            if let Some(ref ext_slug) = manifest.slug
                && let Some(existing) = bots.iter().find(|b| b.slug.as_str() == ext_slug)
            {
                imported_ledger.insert(canonical_key, existing.id.as_str().to_owned());
                return Ok(None);
            }
        }

        let soul = read_external_soul(src_dir, &cand.name);
        let memory = read_external_memory(src_dir);
        let requested_name = params
            .as_ref()
            .map(|p| p.base.name.as_str())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(&cand.name);
        let clean_name =
            validate_bot_name(requested_name).unwrap_or_else(|_| "Imported Bot".to_owned());
        let base_slug = manifest
            .slug
            .as_deref()
            .filter(|_| !force_copy)
            .and_then(|s| BotSlug::new(s).ok())
            .unwrap_or_else(|| bot_slug_from_name(&clean_name));
        let slug = allocate_unique_slug(bots, &base_slug)?;
        let id = manifest
            .id
            .as_deref()
            .filter(|_| !force_copy)
            .and_then(|raw| BotId::new(raw).ok())
            .filter(|bid| !bots.iter().any(|b| &b.id == bid))
            .map_or_else(|| allocate_unique_id(bots, slug.as_str()), Ok)?;
        let raw_agent = params
            .as_ref()
            .map(|p| p.base.agent_id.trim())
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| cand.agent.trim());
        let agent_id = if raw_agent.is_empty() {
            "claude".to_owned()
        } else {
            krill_proto::agent::canonical_id(raw_agent).to_owned()
        };

        let mut cfg = BotConfig::new(id, &clean_name, &agent_id, now_ms)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        cfg.slug = slug.clone();
        slug.as_str().clone_into(&mut cfg.folder);
        cfg.description = manifest
            .description
            .unwrap_or_else(|| soul_description(&soul).unwrap_or_default());
        cfg.emoji = manifest.emoji;
        cfg.model = manifest.model;
        cfg.avatar_color = manifest.avatar_color;
        cfg.members = manifest.members;
        cfg.skills = manifest.skill_projects;

        if let Some(p) = params {
            if p.base.model.is_some() {
                cfg.model = p.base.model;
            }
            if p.base.effort.is_some() {
                cfg.effort = p.base.effort;
            }
            if let Some(perm) = p.base.permission_mode {
                cfg.permission_mode = perm;
            }
            if p.base.avatar.is_some() {
                cfg.avatar = p.base.avatar;
            }
            if let Some(color) = p
                .avatar_color
                .map(|c| c.trim().to_owned())
                .filter(|c| !c.is_empty())
            {
                cfg.avatar_color = Some(color);
            }
            if let Some(ws) = p
                .base
                .workspace
                .map(|w| w.trim().to_owned())
                .filter(|w| !w.is_empty())
            {
                cfg.workspace = Some(ws);
            }
            if !p.base.description.trim().is_empty() {
                p.base.description.trim().clone_into(&mut cfg.description);
            }
            if !p.skills.is_empty() {
                cfg.skills = normalize_skill_list(&p.skills)?;
            }
        }

        let home_path = self.bot_home(&cfg);
        let home_existed = home_path.exists();
        let result = (|| -> io::Result<()> {
            let home = init_bot_home(&self.root, &cfg, Some(&soul))?;
            ensure_bot_subdirs_safe(&home)?;
            write_atomic_0600(&home.join("SOUL.md"), soul.as_bytes())?;
            if let Some(mem) = memory {
                write_atomic_0600(&home.join("MEMORY.md"), mem.as_bytes())?;
            }
            copy_external_markdown_dir(&src_dir.join("memory"), &home.join("memory"))?;
            copy_external_skills_dir(&src_dir.join("skills"), &home.join("skills"))?;
            copy_external_skill_projects(src_dir, &cfg.skills, &home.join("skills"))?;
            materialize_bot_skills(&home, &cfg.skills)?;

            if cfg.avatar.is_none() {
                let ext_avatar = src_dir.join(AVATAR_FILE);
                if fs::symlink_metadata(&ext_avatar)
                    .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
                    && usize::try_from(fs::symlink_metadata(&ext_avatar)?.len())
                        .unwrap_or(usize::MAX)
                        <= MAX_AVATAR_PNG_BYTES
                    && let Ok(raw_png) = fs::read(&ext_avatar)
                    && let Ok(clean_png) = strip_png_metadata(&raw_png)
                {
                    let dst_avatar = home.join(AVATAR_FILE);
                    ensure_no_symlink_path(&dst_avatar)?;
                    write_atomic_0600(&dst_avatar, &clean_png)?;
                    cfg.avatar = Some(AVATAR_FILE.to_owned());
                    init_bot_home(&self.root, &cfg, None)?;
                }
            }
            Ok(())
        })();

        if let Err(err) = result {
            if !home_existed {
                fs::remove_dir_all(&home_path).ok();
            }
            return Err(err);
        }

        let recorded_active = imported_ledger
            .get(&canonical_key)
            .is_some_and(|existing| bots.iter().any(|b| b.id.as_str() == existing));
        if !recorded_active {
            imported_ledger.insert(canonical_key, cfg.id.as_str().to_owned());
        }
        bots.push(cfg.clone());
        Ok(Some(cfg))
    }

    /// The bot's Items, Look around and Memory upkeep presets; defaults until changed.
    pub fn settings_extras(&self, bot_id: &BotId) -> io::Result<BotSettingsExtras> {
        let _guard = lock(&self.io_lock);
        let all: BTreeMap<String, BotSettingsExtras> =
            self.read_json_or_default(SETTINGS_EXTRAS_FILE)?;
        Ok(all.get(bot_id.as_str()).copied().unwrap_or_default())
    }

    /// Changes the presets that are `Some` and returns the bot's presets after it.
    pub fn update_settings_extras(
        &self,
        bot_id: &BotId,
        items_stale: Option<BotItemsStale>,
        look_around_every: Option<LookAroundEvery>,
        tidy_every: Option<TidyEvery>,
    ) -> io::Result<BotSettingsExtras> {
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
        if !bots.iter().any(|b| &b.id == bot_id) {
            return Err(not_found_bot(bot_id));
        }
        let mut all: BTreeMap<String, BotSettingsExtras> =
            self.read_json_or_default(SETTINGS_EXTRAS_FILE)?;
        // Deleted bots' presets go with them.
        all.retain(|id, _| bots.iter().any(|b| b.id.as_str() == id));
        let extras = all.entry(bot_id.as_str().to_owned()).or_default();
        if let Some(v) = items_stale {
            extras.items_stale = v;
        }
        if let Some(v) = look_around_every {
            extras.look_around_every = v;
        }
        if let Some(v) = tidy_every {
            extras.tidy_every = v;
        }
        let out = *extras;
        self.write_json(SETTINGS_EXTRAS_FILE, &all)?;
        Ok(out)
    }

    /// Takes `member` off the bot's Who can ask; people join only by an approved request.
    pub fn remove_member(
        &self,
        bot_id: &BotId,
        member: &BotMember,
        now_ms: u64,
    ) -> io::Result<BotConfig> {
        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
        let bot = bots
            .iter_mut()
            .find(|b| &b.id == bot_id)
            .ok_or_else(|| not_found_bot(bot_id))?;
        bot.members.retain(|m| m != member);
        bot.updated_at_ms = now_ms;
        let updated = bot.clone();
        init_bot_home(&self.root, &updated, None)?;
        self.write_json(BOTS_INDEX_FILE, &bots)?;
        Ok(updated)
    }

    // ---------- Internal JSON Helpers ----------

    /// `bots.json`, with a mascot for a bot saved without any picture (Orca never shows one
    /// bare); the next save keeps it.
    fn read_bots(&self) -> io::Result<Vec<BotConfig>> {
        let mut bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
        for bot in &mut bots {
            let bare = |v: &Option<String>| v.as_deref().is_none_or(|s| s.trim().is_empty());
            if bare(&bot.avatar) && bare(&bot.emoji) {
                bot.avatar = Some(default_mascot(bot.id.as_str()).to_owned());
            }
        }
        Ok(bots)
    }

    fn read_json_or_default<T: DeserializeOwned + Default>(&self, filename: &str) -> io::Result<T> {
        ensure_no_symlink_path(&self.root)?;
        let path = self.root.join(filename);
        ensure_no_symlink_path(&path)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
            Err(err) => return Err(err),
        };
        if bytes.len() > MAX_JSON_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bot metadata file exceeds size limit",
            ));
        }
        serde_json::from_slice(&bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    fn write_json<T: Serialize>(&self, filename: &str, value: &T) -> io::Result<()> {
        ensure_no_symlink_path(&self.root)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)?;
        let path = self.root.join(filename);
        ensure_no_symlink_path(&path)?;
        let mut bytes = serde_json::to_vec_pretty(value)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        bytes.push(b'\n');
        write_atomic_0600(&path, &bytes)
    }
}

fn not_found_bot(bot_id: &BotId) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("bot not found: {}", bot_id.as_str()),
    )
}

fn allocate_unique_slug(existing: &[BotConfig], base: &BotSlug) -> io::Result<BotSlug> {
    if !existing.iter().any(|b| &b.slug == base) {
        return Ok(base.clone());
    }
    for n in 2..=9999_u32 {
        let candidate = format!("{}-{n}", base.as_str());
        if validate_bot_slug(&candidate).is_ok()
            && let Ok(slug) = BotSlug::new(&candidate)
            && !existing.iter().any(|b| b.slug == slug)
        {
            return Ok(slug);
        }
    }
    Err(io::Error::other("unable to allocate unique bot slug"))
}

fn allocate_unique_id(existing: &[BotConfig], base_slug: &str) -> io::Result<BotId> {
    let first = format!("bot-{base_slug}");
    if let Ok(id) = BotId::new(&first)
        && !existing.iter().any(|b| b.id == id)
    {
        return Ok(id);
    }
    for n in 2..=9999_u32 {
        let candidate = format!("bot-{base_slug}-{n}");
        if let Ok(id) = BotId::new(&candidate)
            && !existing.iter().any(|b| b.id == id)
        {
            return Ok(id);
        }
    }
    Err(io::Error::other("unable to allocate unique bot id"))
}

fn generate_random_hex(len_bytes: usize) -> io::Result<String> {
    let mut buf = vec![0u8; len_bytes];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| io::Error::other("RNG failure"))?;
    Ok(crate::mobile::devices::hex(&buf))
}

fn read_text_bounded(path: &Path) -> io::Result<String> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to read symlink inside bot home",
        ));
    }
    if usize::try_from(meta.len()).unwrap_or(usize::MAX) > MAX_TEXT_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bot file exceeds maximum size",
        ));
    }
    fs::read_to_string(path)
}

fn is_sender_allowed_with_workspace(
    bot: &BotConfig,
    source: ExternalSource,
    workspace_id: Option<&str>,
    sender_id: &str,
    channel_id: &str,
) -> bool {
    if bot.members.iter().any(|m| {
        m.source == source
            && m.id == sender_id
            && match (&m.workspace_id, workspace_id) {
                (Some(expected), Some(actual)) => expected == actual,
                _ => true,
            }
    }) {
        return true;
    }
    match source {
        ExternalSource::Telegram => {
            if bot.connectors.telegram.acts_for == ActsFor::Anyone {
                return true;
            }
            let uid_ok = sender_id
                .parse::<i64>()
                .is_ok_and(|id| bot.connectors.telegram.allowed_user_ids.contains(&id));
            let cid_ok = channel_id
                .parse::<i64>()
                .is_ok_and(|id| bot.connectors.telegram.allowed_chat_ids.contains(&id));
            uid_ok || cid_ok
        }
        ExternalSource::Slack => {
            if bot.connectors.slack.acts_for == ActsFor::Anyone {
                return true;
            }
            bot.connectors
                .slack
                .allowed_user_ids
                .iter()
                .any(|u| u == sender_id)
                || bot
                    .connectors
                    .slack
                    .allowed_channel_ids
                    .iter()
                    .any(|c| c == channel_id)
        }
        ExternalSource::Discord => {
            if bot.connectors.discord.acts_for == ActsFor::Anyone {
                return true;
            }
            bot.connectors
                .discord
                .allowed_user_ids
                .iter()
                .any(|u| u == sender_id)
                || bot
                    .connectors
                    .discord
                    .allowed_channel_ids
                    .iter()
                    .any(|c| c == channel_id)
        }
        ExternalSource::Google
        | ExternalSource::GitHub
        | ExternalSource::Linear
        | ExternalSource::Composio => true,
    }
}

/// Redacts pairing tokens from [`BotAccessRequest`] records for read-only Observer peers.
#[must_use]
pub fn redact_access_requests_for_observer(reqs: Vec<BotAccessRequest>) -> Vec<BotAccessRequest> {
    reqs.into_iter()
        .map(|mut r| {
            r.token.clear();
            r
        })
        .collect()
}

/// Discovers bots from Orca, `OpenClaw`, Hermes, and `OpenBot` using `krill_proto::bots::external`.
#[must_use]
pub fn discover_external_bots(
    source_filter: Option<ExternalBotSourceApp>,
    custom_path: Option<&Path>,
) -> Vec<ExternalBotCandidate> {
    proto_discover_external_bots(source_filter, custom_path)
}

#[derive(Default)]
struct ExternalManifest {
    id: Option<String>,
    slug: Option<String>,
    description: Option<String>,
    emoji: Option<String>,
    model: Option<String>,
    avatar_color: Option<String>,
    members: Vec<BotMember>,
    skill_projects: Vec<String>,
}

fn read_external_manifest(dir: &Path) -> ExternalManifest {
    let mut out = ExternalManifest::default();
    for file in ["bot.json", "agent.json"] {
        let Ok(raw) = read_text_bounded(&dir.join(file)) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        out.id = val
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        out.slug = val
            .get("slug")
            .and_then(serde_json::Value::as_str)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        out.description = val
            .get("description")
            .and_then(serde_json::Value::as_str)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        out.emoji = val
            .get("emoji")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        out.model = val
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        out.avatar_color = val
            .get("avatarColor")
            .or_else(|| val.get("avatar_color"))
            .and_then(serde_json::Value::as_str)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if let Some(arr) = val.get("members").and_then(serde_json::Value::as_array) {
            for item in arr {
                if let Ok(member) = serde_json::from_value::<BotMember>(item.clone())
                    && !member.id.trim().is_empty()
                {
                    out.members.push(member);
                }
            }
        }
        if let Some(arr) = val
            .get("skillProjects")
            .or_else(|| val.get("skills"))
            .and_then(serde_json::Value::as_array)
        {
            for item in arr {
                if let Some(skill) = item.as_str()
                    && validate_bot_slug(skill.trim()).is_ok()
                {
                    out.skill_projects.push(skill.trim().to_owned());
                }
            }
        }
        break;
    }
    out
}

fn ensure_bot_subdirs_safe(home: &Path) -> io::Result<()> {
    ensure_no_symlink_path(home)?;
    ensure_no_symlink_path(&home.join("memory"))?;
    ensure_no_symlink_path(&home.join("skills"))?;
    ensure_no_symlink_path(&home.join("SOUL.md"))?;
    ensure_no_symlink_path(&home.join("MEMORY.md"))?;
    ensure_no_symlink_path(&home.join(AVATAR_FILE))?;
    ensure_no_symlink_path(&home.join(EVENTS_FILE))?;
    Ok(())
}

fn write_bot_events_sidecar(home: &Path, events: &[BotEventRuleConfig]) -> io::Result<()> {
    ensure_no_symlink_path(home)?;
    let path = home.join(EVENTS_FILE);
    ensure_no_symlink_path(&path)?;
    let mut bytes = serde_json::to_vec_pretty(events)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    bytes.push(b'\n');
    write_atomic_0600(&path, &bytes)
}

fn normalize_skill_list(raw: &[String]) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    for s in raw {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            continue;
        }
        validate_bot_slug(trimmed)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        if !out.iter().any(|existing: &String| existing == trimmed) {
            out.push(trimmed.to_owned());
        }
    }
    Ok(out)
}

fn validate_events(events: &[BotEventRuleConfig]) -> io::Result<()> {
    for ev in events {
        if ev.id.trim().is_empty()
            || ev.source.trim().is_empty()
            || ev.event.trim().is_empty()
            || ev.prompt.trim().is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bot event rule requires id, source, event, and prompt",
            ));
        }
    }
    Ok(())
}

fn generate_pairing_code(len: usize) -> io::Result<String> {
    let mut buf = vec![0u8; len];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| io::Error::other("RNG failure"))?;
    let mut code = String::with_capacity(len);
    for b in buf {
        let idx = usize::from(b) % PAIRING_CODE_CHARS.len();
        if let Some(&ch) = PAIRING_CODE_CHARS.get(idx) {
            code.push(char::from(ch));
        }
    }
    Ok(code)
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn copy_external_skill_projects(
    src_root: &Path,
    skill_projects: &[String],
    dst_skills: &Path,
) -> io::Result<()> {
    for name in skill_projects {
        if validate_bot_slug(name).is_err() {
            continue;
        }
        let candidate_dir = src_root.join(name);
        let candidate_md = candidate_dir.join("SKILL.md");
        if fs::symlink_metadata(&candidate_dir)
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
            && let Ok(text) = read_text_bounded(&candidate_md)
        {
            let skill_dst_dir = dst_skills.join(name);
            ensure_no_symlink_path(&skill_dst_dir)?;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&skill_dst_dir)?;
            let dst_file = skill_dst_dir.join("SKILL.md");
            ensure_no_symlink_path(&dst_file)?;
            write_atomic_0600(&dst_file, text.as_bytes())?;
        }
    }
    Ok(())
}

fn read_external_soul(dir: &Path, fallback_name: &str) -> String {
    for candidate in ["SOUL.md", "IDENTITY.md", "SYSTEM.md"] {
        if let Ok(text) = read_text_bounded(&dir.join(candidate))
            && !text.trim().is_empty()
        {
            return text;
        }
    }
    krill_proto::bots::starter_soul(fallback_name)
}

fn read_external_memory(dir: &Path) -> Option<String> {
    for candidate in ["MEMORY.md", "memory.md"] {
        if let Ok(text) = read_text_bounded(&dir.join(candidate))
            && !text.trim().is_empty()
        {
            return Some(text);
        }
    }
    None
}

fn copy_external_markdown_dir(src: &Path, dst: &Path) -> io::Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dst)?;
    let Ok(entries) = fs::read_dir(src) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
            && path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            && let Some(name) = path.file_name()
            && let Ok(text) = read_text_bounded(&path)
        {
            write_atomic_0600(&dst.join(name), text.as_bytes())?;
        }
    }
    Ok(())
}

fn copy_external_skills_dir(src: &Path, dst: &Path) -> io::Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    let Ok(entries) = fs::read_dir(src) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
            && let Some(skill_name) = path.file_name().and_then(|n| n.to_str())
            && validate_bot_slug(skill_name).is_ok()
        {
            let skill_md = path.join("SKILL.md");
            if let Ok(text) = read_text_bounded(&skill_md) {
                write_atomic_0600(&dst.join(skill_name).join("SKILL.md"), text.as_bytes())?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-bot-store-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_bot_without_a_picture_gets_an_orca_mascot() {
        let root = temp_dir("mascot");
        let store = BotStore::new(root.clone());
        let created = store
            .create_bot(
                CreateBotParams {
                    name: "Shop".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: None,
                },
                1_000,
            )
            .unwrap();
        let mascot = created.avatar.clone().unwrap();
        assert!(krill_proto::bots::catalog::PICKABLE_MASCOTS.contains(&mascot.as_str()));
        // A bot saved bare (before this) reads with a stable mascot of its own.
        let index = root.join(BOTS_INDEX_FILE);
        let raw = fs::read_to_string(&index).unwrap();
        fs::write(&index, raw.replace(&format!("\"{mascot}\""), "null")).unwrap();
        let listed = store.list_bots().unwrap();
        assert_eq!(
            listed[0].avatar.as_deref(),
            Some(default_mascot(created.id.as_str()))
        );
    }

    #[test]
    fn settings_extras_persist_per_bot_and_members_only_leave() {
        let root = temp_dir("extras");
        let store = BotStore::new(root.clone());
        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "Helper".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: None,
                },
                1_000,
            )
            .unwrap();
        assert_eq!(
            store.settings_extras(&bot.id).unwrap(),
            BotSettingsExtras::default()
        );
        let after = store
            .update_settings_extras(&bot.id, Some(BotItemsStale::Never), None, None)
            .unwrap();
        let after = store
            .update_settings_extras(&bot.id, None, Some(LookAroundEvery::Mornings), None)
            .unwrap_or(after);
        assert_eq!(
            after.items_stale,
            BotItemsStale::Never,
            "a later edit keeps it"
        );
        assert_eq!(after.look_around_every, LookAroundEvery::Mornings);
        assert_eq!(store.settings_extras(&bot.id).unwrap(), after);
        let ghost = BotId::new("ghost").unwrap();
        assert!(
            store
                .update_settings_extras(&ghost, None, None, None)
                .is_err()
        );

        let person = BotMember {
            id: "U1".to_owned(),
            name: "Ana".to_owned(),
            source: ExternalSource::Slack,
            workspace_id: None,
        };
        let mut bots: Vec<BotConfig> = store.read_json_or_default(BOTS_INDEX_FILE).unwrap();
        bots[0].members.push(person.clone());
        store.write_json(BOTS_INDEX_FILE, &bots).unwrap();
        let updated = store.remove_member(&bot.id, &person, 2_000).unwrap();
        assert!(updated.members.is_empty());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_bot_proposes_bots_and_the_owner_answers_each_once() {
        let store = BotStore::new(temp_dir("offers"));
        let params = |name: &str| CreateBotParams {
            name: name.to_owned(),
            agent_id: "claude".to_owned(),
            description: String::new(),
            emoji: None,
            avatar: None,
            model: None,
            effort: None,
            permission_mode: None,
            workspace: None,
            template_id: None,
            initial_soul: None,
        };
        let shop = store.create_bot(params("Shop"), 1_000).unwrap();
        // An existing name is refused, so no card offers a duplicate.
        let taken = store.offer_bot(&shop.id, Some(7), "shop", "again", 2_000);
        assert_eq!(taken.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        let offer = store
            .offer_bot(
                &shop.id,
                Some(7),
                " Ledger ",
                "Keeps the spend sheet.",
                3_000,
            )
            .unwrap();
        assert_eq!((offer.name.as_str(), offer.session_id), ("Ledger", Some(7)));
        let listed = store.list_bot_offers(&shop.id).unwrap();
        assert_eq!(listed, vec![offer.clone()]);
        let ledger = store.create_bot(params("Ledger"), 4_000).unwrap();
        store
            .resolve_bot_offer(&offer.offer_id, Some(ledger.id.clone()))
            .unwrap();
        let answered = &store.list_bot_offers(&shop.id).unwrap()[0];
        assert_eq!(answered.created.as_ref(), Some(&ledger.id));
        assert!(!answered.dismissed);
        assert!(store.list_bot_offers(&ledger.id).unwrap().is_empty());
        assert!(store.resolve_bot_offer("missing", None).is_err());
    }

    #[test]
    fn bot_crud_memory_guard_links_access_groups_and_zip_roundtrip() {
        let root = temp_dir("full");
        let store = BotStore::new(root.clone());

        let created = store
            .create_bot(
                CreateBotParams {
                    name: "PR Reviewer".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: Some("R".to_owned()),
                    avatar: None,
                    model: Some("claude-sonnet-4-5".to_owned()),
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: Some("pr-review".to_owned()),
                    initial_soul: None,
                },
                1_000,
            )
            .unwrap();
        assert_eq!(created.slug.as_str(), "pr-reviewer");
        assert!(created.skills.contains(&"pr-review-checklist".to_owned()));

        // Memory append, deletion guard block, and explicit forget.
        let mem = store
            .append_memory_fact_line(
                &created.id,
                "Repo is craftowen/krill",
                Some("2026-11-20"),
                None,
            )
            .unwrap();
        assert!(mem.contains("Repo is craftowen/krill"));
        let err = store
            .update_memory(&created.id, "# Memory\n\n- 2026-11-20 Unrelated\n", false)
            .unwrap_err();
        assert!(matches!(err, MemoryUpdateError::DeletionBlocked(_)));
        let (after_forget, removed) = store
            .forget_memory_fact(&created.id, "Repo is craftowen/krill")
            .unwrap();
        assert_eq!(removed, 1);
        assert!(!after_forget.contains("Repo is craftowen/krill"));

        // Access request & approval adds member.
        let decision = store
            .check_or_request_access(
                &created.id,
                ExternalSource::Telegram,
                "999",
                "Alice",
                "chat-1",
                2_000,
            )
            .unwrap();
        let token = match decision {
            AccessDecision::Pending(req) => Some(req.token),
            _ => None,
        }
        .unwrap();
        let decided = store.decide_access_request(&token, true, 2_500).unwrap();
        assert_eq!(decided.status, BotAccessRequestStatus::Approved);
        assert_eq!(
            store
                .check_or_request_access(
                    &created.id,
                    ExternalSource::Telegram,
                    "999",
                    "Alice",
                    "chat-1",
                    3_000
                )
                .unwrap(),
            AccessDecision::Allowed
        );

        // Groups & Links.
        let group = store
            .create_group(
                "Engineering",
                vec![created.id.clone()],
                "Core review team".to_owned(),
                3_500,
            )
            .unwrap();
        assert_eq!(store.list_groups().unwrap().len(), 1);
        assert_eq!(store.find_group("Engineering").unwrap(), Some(group));

        let link = store
            .lookup_or_create_link(
                &created.id,
                ExternalSource::Slack,
                Some("T1"),
                "C1",
                Some("123.456"),
                ActsFor::Owner,
                4_000,
            )
            .unwrap();
        store
            .bind_link_session(&link.id, SessionId(7), 4_100)
            .unwrap();
        assert_eq!(
            store.get_link(&link.id).unwrap().and_then(|l| l.session_id),
            Some(SessionId(7))
        );

        // Zip export & import.
        let (filename, zip_bytes) = store
            .export_bot_zip(
                &created.id,
                BotPackageExportOptions {
                    include_memory: true,
                    include_avatar: true,
                },
            )
            .unwrap();
        assert_eq!(filename, "pr-reviewer.zip");
        let imported = store
            .import_bot_zip(&zip_bytes, Some("PR Reviewer Copy"), None, 5_000)
            .unwrap();
        assert_eq!(imported.slug.as_str(), "pr-reviewer-copy");

        // Delete bot.
        store.delete_bot(&created.id, true).unwrap();
        assert!(store.get_bot(&created.id).unwrap().is_none());
        assert!(!root.join("pr-reviewer").exists());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn external_bot_migration_is_idempotent() {
        let root = temp_dir("migrate");
        let external = temp_dir("orca-src");
        let orca_bot = external.join("release-captain");
        fs::create_dir_all(orca_bot.join("memory")).unwrap();
        fs::write(
            orca_bot.join("bot.json"),
            r#"{"name":"Release Captain","agent":"codex","description":"Ships releases"}"#,
        )
        .unwrap();
        fs::write(
            orca_bot.join("SOUL.md"),
            "# Release Captain\n\nShips every release cleanly.\n",
        )
        .unwrap();
        fs::write(
            orca_bot.join("MEMORY.md"),
            "# Memory\n\n- 2026-11-10 Always tag releases.\n",
        )
        .unwrap();

        let store = BotStore::new(root.clone());
        let (cands_dry, imp_dry) = store
            .import_external_bots(
                Some(ExternalBotSourceApp::Orca),
                Some(&external),
                true,
                1_000,
            )
            .unwrap();
        assert_eq!(cands_dry.len(), 1);
        assert!(imp_dry.is_empty());

        let (cands1, imp1) = store
            .import_external_bots(
                Some(ExternalBotSourceApp::Orca),
                Some(&external),
                false,
                2_000,
            )
            .unwrap();
        assert_eq!(cands1.len(), 1);
        assert_eq!(imp1.len(), 1);
        assert_eq!(imp1.first().map(|b| b.agent_id.as_str()), Some("codex"));

        // Second run must be idempotent (0 newly imported).
        let (cands2, imp2) = store
            .import_external_bots(
                Some(ExternalBotSourceApp::Orca),
                Some(&external),
                false,
                3_000,
            )
            .unwrap();
        assert_eq!(cands2.len(), 1);
        assert!(imp2.is_empty());

        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&external).ok();
    }

    #[test]
    fn v2_create_extras_avatars_pairing_and_default_trackers() {
        let root = temp_dir("v2-extras");
        let store = BotStore::new(root.clone());

        let created = store
            .create_bot_v2(
                CreateBotV2Params {
                    base: CreateBotParams {
                        name: "Triage Bot".to_owned(),
                        agent_id: "claude".to_owned(),
                        description: String::new(),
                        emoji: None,
                        avatar: None,
                        model: None,
                        effort: None,
                        permission_mode: None,
                        workspace: None,
                        template_id: None,
                        initial_soul: None,
                    },
                    slug: Some(BotSlug::new("custom-triage").unwrap()),
                    avatar_color: Some("#3366ff".to_owned()),
                    skills: vec!["pr-review-checklist".to_owned()],
                    events: vec![BotEventRuleConfig {
                        id: "pr-opened".to_owned(),
                        source: "github".to_owned(),
                        event: "pull_request.opened".to_owned(),
                        prompt: "Review the PR".to_owned(),
                        enabled: true,
                    }],
                },
                1_000,
            )
            .unwrap();
        assert_eq!(created.slug.as_str(), "custom-triage");
        assert_eq!(created.avatar_color.as_deref(), Some("#3366ff"));
        assert!(
            store
                .bot_home(&created)
                .join("skills/pr-review-checklist/SKILL.md")
                .is_file()
        );
        assert_eq!(store.get_events(&created.id).unwrap().len(), 1);

        // Reject duplicate explicit slug.
        assert!(
            store
                .create_bot_v2(
                    CreateBotV2Params {
                        base: CreateBotParams {
                            name: "Another".to_owned(),
                            agent_id: "claude".to_owned(),
                            description: String::new(),
                            emoji: None,
                            avatar: None,
                            model: None,
                            effort: None,
                            permission_mode: None,
                            workspace: None,
                            template_id: None,
                            initial_soul: None,
                        },
                        slug: Some(BotSlug::new("custom-triage").unwrap()),
                        avatar_color: None,
                        skills: Vec::new(),
                        events: Vec::new(),
                    },
                    1_100,
                )
                .is_err()
        );

        // Upload & get PNG avatar (minimal valid 1x1 PNG).
        let png = build_minimal_test_png();
        let saved_avatar = store.upload_avatar(&created.id, &png, 1_200).unwrap();
        assert_eq!(saved_avatar.mime, "image/png");
        let loaded_avatar = store.get_avatar(&created.id).unwrap();
        assert_eq!(loaded_avatar.png_bytes, saved_avatar.png_bytes);

        // Pairing code redemption approves sender with workspace_id.
        let decision = store
            .check_or_request_access_with_workspace(
                &created.id,
                ExternalSource::Slack,
                Some("T_WS_1"),
                "U99",
                "Eve",
                "D1",
                2_000,
            )
            .unwrap();
        let token = match decision {
            AccessDecision::Pending(r) => r.token,
            _ => panic!("expected pending"),
        };
        let pairing_code = store.pairing_code_for_token(&token).unwrap().unwrap();
        let approved = store.redeem_pairing_code(&pairing_code, 2_100).unwrap();
        assert_eq!(approved.status, BotAccessRequestStatus::Approved);
        assert_eq!(
            store
                .check_or_request_access_with_workspace(
                    &created.id,
                    ExternalSource::Slack,
                    Some("T_WS_1"),
                    "U99",
                    "Eve",
                    "D1",
                    2_200,
                )
                .unwrap(),
            AccessDecision::Allowed
        );

        // Default tracker bots.
        let defaults = store
            .set_default_tracker_bot(ExternalSource::GitHub, Some(created.id.clone()))
            .unwrap();
        assert_eq!(defaults.github, Some(created.id.clone()));

        fs::remove_dir_all(&root).ok();
    }

    fn build_minimal_test_png() -> Vec<u8> {
        let mut png = Vec::from(&[137u8, 80, 78, 71, 13, 10, 26, 10][..]);
        let ihdr_data = [0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0];
        push_png_chunk(&mut png, *b"IHDR", &ihdr_data);
        push_png_chunk(
            &mut png,
            *b"IDAT",
            &[0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01],
        );
        push_png_chunk(&mut png, *b"IEND", &[]);
        png
    }

    fn push_png_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&kind);
        out.extend_from_slice(data);
        let mut crc_buf = Vec::with_capacity(4 + data.len());
        crc_buf.extend_from_slice(&kind);
        crc_buf.extend_from_slice(data);
        let crc = krill_proto::bots::crc32(&crc_buf);
        out.extend_from_slice(&crc.to_be_bytes());
    }

    #[test]
    fn canonicalizes_antigravity_agent_alias_on_create_update_and_import() {
        let root = std::env::temp_dir().join(format!("krilld-agy-canon-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
        let store = BotStore::new(root.clone());

        let created = store
            .create_bot(
                CreateBotParams {
                    name: "Agy Alias".to_owned(),
                    agent_id: "Antigravity".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: Some("xhigh".to_owned()),
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: None,
                },
                1_000,
            )
            .unwrap();
        assert_eq!(created.agent_id, "agy");
        assert_eq!(created.effort.as_deref(), Some("xhigh"));

        let updated = store
            .update_bot(
                UpdateBotParams {
                    bot_id: created.id.clone(),
                    name: None,
                    description: None,
                    emoji: FieldPatch::Unchanged,
                    avatar: FieldPatch::Unchanged,
                    agent_id: Some("antigravity".to_owned()),
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
                    mcp_tools: None,
                },
                2_000,
            )
            .unwrap();
        assert_eq!(updated.agent_id, "agy");

        let (_, zip) = store
            .export_bot_zip(&created.id, BotPackageExportOptions::default())
            .unwrap();
        let imported = store
            .import_bot_zip_v2(
                &zip,
                Some("agy-alias-copy"),
                Some("Agy Alias Copy"),
                Some("antigravity"),
                3_000,
            )
            .unwrap();
        assert_eq!(imported.agent_id, "agy");

        fs::remove_dir_all(&root).ok();
    }
}
