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

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use krill_proto::SessionId;
use krill_proto::bots::{
    ActsFor, BOT_MEMORY_SEED, BotAccessRequest, BotAccessRequestStatus, BotConfig,
    BotConnectorConfig, BotGroupChat, BotId, BotMcpToolConfig, BotMember, BotPackageExportOptions,
    BotScheduleConfig, BotSlug, BotSuggestion, CreateBotParams, ExternalBotCandidate,
    ExternalBotSourceApp, ExternalConversationLink, ExternalSource, MAX_RPC_ZIP_BYTES,
    MemoryDeletionViolation, UpdateBotParams, append_memory_fact, assert_bot_owns_home,
    bot_slug_from_name, export_bot_home_zip, find_bot_mcp_app, find_bot_template,
    forget_memory_fact, import_bot_zip, init_bot_home, soul_description, validate_bot_name,
    validate_bot_slug, validate_memory_edit,
};
use krill_proto::os::fs::DirBuilderExt as _;
use krill_proto::os::{canonicalize, home_dir, write_atomic_0600};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::{Serialize, de::DeserializeOwned};

use crate::bots::connectors::AccessDecision;
use crate::session::lock;

const BOTS_INDEX_FILE: &str = "bots.json";
const LINKS_FILE: &str = "links.json";
const ACCESS_REQUESTS_FILE: &str = "access_requests.json";
const GROUPS_FILE: &str = "groups.json";
const SUGGESTIONS_FILE: &str = "suggestions.json";
const IMPORTED_SOURCES_FILE: &str = "imported_sources.json";
const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_TEXT_FILE_BYTES: usize = 2 * 1024 * 1024;

static ID_SEQ: AtomicU64 = AtomicU64::new(1);

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
        self.read_json_or_default(BOTS_INDEX_FILE)
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
        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
        let template = params.template_id.as_deref().and_then(find_bot_template);

        let name = validate_bot_name(&params.name)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        let base_slug = bot_slug_from_name(&name);
        let slug = allocate_unique_slug(&bots, &base_slug)?;
        let id = allocate_unique_id(&bots, slug.as_str())?;

        let agent_id = if params.agent_id.trim().is_empty() {
            "claude".to_owned()
        } else {
            params.agent_id.trim().to_owned()
        };

        let mut cfg = BotConfig::new(id, &name, &agent_id, now_ms)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        cfg.slug = slug.clone();
        slug.as_str().clone_into(&mut cfg.folder);
        cfg.emoji = params.emoji;
        cfg.avatar = params
            .avatar
            .or_else(|| template.map(|t| t.mascot.to_owned()));
        cfg.model = params.model;
        cfg.effort = params.effort;
        if let Some(perm) = params.permission_mode {
            cfg.permission_mode = perm;
        }
        cfg.workspace = params
            .workspace
            .map(|w| w.trim().to_owned())
            .filter(|w| !w.is_empty());
        cfg.template_id = params.template_id.clone();

        let initial_soul = params
            .initial_soul
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| template.map(|t| t.default_soul));

        cfg.description = if !params.description.trim().is_empty() {
            params.description.trim().to_owned()
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

        cfg.validate()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;

        init_bot_home(&self.root, &cfg, initial_soul)?;
        bots.push(cfg.clone());
        self.write_json(BOTS_INDEX_FILE, &bots)?;
        Ok(cfg)
    }

    /// Applies partial updates to an existing bot and refreshes its managed home files.
    pub fn update_bot(&self, params: UpdateBotParams, now_ms: u64) -> io::Result<BotConfig> {
        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
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
        if let Some(agent_id) = params.agent_id {
            agent_id.trim().clone_into(&mut bot.agent_id);
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
        let mut bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
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
        let mut bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
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
        let mut bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
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
        let mut bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
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
        if content.len() > MAX_TEXT_FILE_BYTES {
            return Err(MemoryUpdateError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MEMORY.md exceeds maximum allowed size",
            )));
        }
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
        let bot = bots
            .iter()
            .find(|b| &b.id == bot_id)
            .ok_or_else(|| not_found_bot(bot_id))?;
        let home = init_bot_home(&self.root, bot, None)?;
        let mem_path = home.join("MEMORY.md");
        let old = if mem_path.exists() {
            read_text_bounded(&mem_path)?
        } else {
            BOT_MEMORY_SEED.to_owned()
        };
        if !allow_deletions {
            validate_memory_edit(&old, content).map_err(MemoryUpdateError::DeletionBlocked)?;
        }
        write_atomic_0600(&mem_path, content.as_bytes())?;
        Ok(content.to_owned())
    }

    /// Explicitly removes matching fact line(s) from `<home>/MEMORY.md`.
    pub fn forget_memory_fact(&self, bot_id: &BotId, fact: &str) -> io::Result<(String, usize)> {
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
        let bot = bots
            .iter()
            .find(|b| &b.id == bot_id)
            .ok_or_else(|| not_found_bot(bot_id))?;
        let home = init_bot_home(&self.root, bot, None)?;
        let mem_path = home.join("MEMORY.md");
        let old = if mem_path.exists() {
            read_text_bounded(&mem_path)?
        } else {
            BOT_MEMORY_SEED.to_owned()
        };
        let (updated, removed) = forget_memory_fact(&old, fact);
        if removed > 0 {
            write_atomic_0600(&mem_path, updated.as_bytes())?;
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
        let bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
        let bot = bots
            .iter()
            .find(|b| &b.id == bot_id)
            .ok_or_else(|| not_found_bot(bot_id))?;
        let home = init_bot_home(&self.root, bot, None)?;
        let mem_path = home.join("MEMORY.md");
        let old = if mem_path.exists() {
            read_text_bounded(&mem_path)?
        } else {
            BOT_MEMORY_SEED.to_owned()
        };
        let updated = append_memory_fact(&old, fact, date, source);
        write_atomic_0600(&mem_path, updated.as_bytes())?;
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
        let _guard = lock(&self.io_lock);
        let bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
        let bot = bots
            .iter()
            .find(|b| &b.id == bot_id)
            .ok_or_else(|| not_found_bot(bot_id))?;

        if is_sender_allowed(bot, source, sender_id, channel_id) {
            return Ok(AccessDecision::Allowed);
        }

        let mut reqs: Vec<BotAccessRequest> = self.read_json_or_default(ACCESS_REQUESTS_FILE)?;
        if let Some(existing) = reqs
            .iter()
            .find(|r| &r.bot_id == bot_id && r.source == source && r.sender_id == sender_id)
        {
            return Ok(match existing.status {
                BotAccessRequestStatus::Approved => AccessDecision::Allowed,
                BotAccessRequestStatus::Pending => AccessDecision::Pending(existing.clone()),
                BotAccessRequestStatus::Declined => {
                    AccessDecision::Denied("sender access request was declined".to_owned())
                }
            });
        }

        let token = generate_random_hex(16)?;
        let req = BotAccessRequest {
            token,
            bot_id: bot_id.clone(),
            source,
            sender_id: sender_id.to_owned(),
            sender_name: sender_name.to_owned(),
            channel_id: channel_id.to_owned(),
            status: BotAccessRequestStatus::Pending,
            created_at: now_ms,
            decided_at: None,
        };
        reqs.push(req.clone());
        self.write_json(ACCESS_REQUESTS_FILE, &reqs)?;
        Ok(AccessDecision::Pending(req))
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
            let mut bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
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
                        workspace_id: None,
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
        let bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
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

    // ---------- Look-Around Suggestions ----------

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
        let pkg = import_bot_zip(zip_bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;

        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;

        let final_name = validate_bot_name(name_override.unwrap_or(&pkg.manifest.name))
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
        let base_slug = if name_override.is_some() {
            bot_slug_from_name(&final_name)
        } else {
            pkg.manifest.slug.clone()
        };
        let slug = allocate_unique_slug(&bots, &base_slug)?;
        let id = allocate_unique_id(&bots, slug.as_str())?;
        let agent_id = agent_override
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&pkg.manifest.agent_id)
            .to_owned();

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

        let home = init_bot_home(&self.root, &cfg, Some(&pkg.soul))?;
        write_atomic_0600(&home.join("SOUL.md"), pkg.soul.as_bytes())?;

        if let Some(mem) = &pkg.memory_md {
            write_atomic_0600(&home.join("MEMORY.md"), mem.as_bytes())?;
        }
        for (topic_file, content) in &pkg.memory_topics {
            write_atomic_0600(&home.join("memory").join(topic_file), content.as_bytes())?;
        }
        for (rel_skill, content) in &pkg.skills {
            let dest = home.join("skills").join(rel_skill);
            write_atomic_0600(&dest, content.as_bytes())?;
        }
        if let Some(png) = &pkg.avatar_png {
            write_atomic_0600(&home.join("avatar.png"), png)?;
        }

        bots.push(cfg.clone());
        self.write_json(BOTS_INDEX_FILE, &bots)?;
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
        let candidates = discover_external_bots(source, custom_path);
        if dry_run || candidates.is_empty() {
            return Ok((candidates, Vec::new()));
        }

        let _guard = lock(&self.io_lock);
        let mut bots: Vec<BotConfig> = self.read_json_or_default(BOTS_INDEX_FILE)?;
        let mut imported_ledger: BTreeMap<String, String> =
            self.read_json_or_default(IMPORTED_SOURCES_FILE)?;
        let mut imported = Vec::new();

        for cand in &candidates {
            let canonical_key = canonicalize(Path::new(&cand.path))
                .map_or_else(|_| cand.path.clone(), |p| p.to_string_lossy().into_owned());
            if let Some(existing_id) = imported_ledger.get(&canonical_key)
                && bots.iter().any(|b| b.id.as_str() == existing_id)
            {
                // Idempotent: already imported and still present in Krill.
                continue;
            }

            let src_dir = Path::new(&cand.path);
            let soul = read_external_soul(src_dir, &cand.name);
            let memory = read_external_memory(src_dir);
            let clean_name =
                validate_bot_name(&cand.name).unwrap_or_else(|_| "Imported Bot".to_owned());
            let base_slug = bot_slug_from_name(&clean_name);
            let slug = allocate_unique_slug(&bots, &base_slug)?;
            let id = allocate_unique_id(&bots, slug.as_str())?;
            let agent_id = if cand.agent.trim().is_empty() {
                "claude".to_owned()
            } else {
                cand.agent.trim().to_owned()
            };

            let mut cfg = BotConfig::new(id, &clean_name, &agent_id, now_ms)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
            cfg.slug = slug.clone();
            slug.as_str().clone_into(&mut cfg.folder);
            cfg.description = soul_description(&soul).unwrap_or_default();

            // Copy optional non-secret metadata from Orca/OpenBot `bot.json`.
            if let Ok(raw_manifest) = read_text_bounded(&src_dir.join("bot.json"))
                && let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw_manifest)
            {
                if let Some(desc) = val.get("description").and_then(serde_json::Value::as_str)
                    && !desc.trim().is_empty()
                {
                    desc.trim().clone_into(&mut cfg.description);
                }
                if let Some(emoji) = val.get("emoji").and_then(serde_json::Value::as_str) {
                    cfg.emoji = Some(emoji.to_owned());
                }
                if let Some(model) = val.get("model").and_then(serde_json::Value::as_str) {
                    cfg.model = Some(model.to_owned());
                }
            }

            let home = init_bot_home(&self.root, &cfg, Some(&soul))?;
            write_atomic_0600(&home.join("SOUL.md"), soul.as_bytes())?;
            if let Some(mem) = memory {
                write_atomic_0600(&home.join("MEMORY.md"), mem.as_bytes())?;
            }
            copy_external_markdown_dir(&src_dir.join("memory"), &home.join("memory"))?;
            copy_external_skills_dir(&src_dir.join("skills"), &home.join("skills"))?;

            imported_ledger.insert(canonical_key, cfg.id.as_str().to_owned());
            bots.push(cfg.clone());
            imported.push(cfg);
        }

        if !imported.is_empty() {
            self.write_json(BOTS_INDEX_FILE, &bots)?;
            self.write_json(IMPORTED_SOURCES_FILE, &imported_ledger)?;
        }

        Ok((candidates, imported))
    }

    // ---------- Internal JSON Helpers ----------

    fn read_json_or_default<T: DeserializeOwned + Default>(&self, filename: &str) -> io::Result<T> {
        let path = self.root.join(filename);
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
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)?;
        let mut bytes = serde_json::to_vec_pretty(value)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        bytes.push(b'\n');
        write_atomic_0600(&self.root.join(filename), &bytes)
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

fn is_sender_allowed(
    bot: &BotConfig,
    source: ExternalSource,
    sender_id: &str,
    channel_id: &str,
) -> bool {
    if bot
        .members
        .iter()
        .any(|m| m.source == source && m.id == sender_id)
    {
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

/// Discovers bots from Orca, `OpenClaw` (`.openclaw`/`.clawdbot`/`.moltbot`), Hermes, and `OpenBot`.
#[must_use]
pub fn discover_external_bots(
    source_filter: Option<ExternalBotSourceApp>,
    custom_path: Option<&Path>,
) -> Vec<ExternalBotCandidate> {
    let mut out = Vec::new();
    let mut seen_paths = BTreeSet::new();

    if let Some(path) = custom_path {
        let app = source_filter.unwrap_or_else(|| infer_source_app(path));
        scan_external_root(app, path, &mut seen_paths, &mut out);
        return out;
    }

    let Some(home) = home_dir() else {
        return out;
    };

    let targets: &[(ExternalBotSourceApp, &[&str])] = &[
        (ExternalBotSourceApp::Orca, &[".orca/bots"]),
        (
            ExternalBotSourceApp::OpenClaw,
            &[".openclaw", ".clawdbot", ".moltbot"],
        ),
        (ExternalBotSourceApp::Hermes, &[".hermes"]),
        (ExternalBotSourceApp::OpenBot, &[".openbot"]),
    ];

    for &(app, rel_roots) in targets {
        if source_filter.is_some_and(|want| want != app) {
            continue;
        }
        for rel in rel_roots {
            let root = home.join(rel);
            if root.is_dir() {
                scan_external_root(app, &root, &mut seen_paths, &mut out);
            }
        }
    }

    out
}

fn infer_source_app(path: &Path) -> ExternalBotSourceApp {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.contains("orca") {
        ExternalBotSourceApp::Orca
    } else if lower.contains("hermes") {
        ExternalBotSourceApp::Hermes
    } else if lower.contains("openbot") {
        ExternalBotSourceApp::OpenBot
    } else {
        ExternalBotSourceApp::OpenClaw
    }
}

fn scan_external_root(
    app: ExternalBotSourceApp,
    root: &Path,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<ExternalBotCandidate>,
) {
    if is_bot_dir(root) {
        push_candidate(app, root, seen, out);
    }

    // Check known container subdirectories (`bots/`, `agents/`, `Agents/`, `profiles/`, `workspaces/`).
    for sub in ["bots", "agents", "Agents", "profiles", "workspaces"] {
        let container = root.join(sub);
        if container.is_dir() {
            scan_children_as_bots(app, &container, seen, out);
        }
    }

    // For flat bot-collection directories (like `~/.orca/bots/*`), scan direct children too.
    scan_children_as_bots(app, root, seen, out);
}

fn scan_children_as_bots(
    app: ExternalBotSourceApp,
    dir: &Path,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<ExternalBotCandidate>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
            && is_bot_dir(&path)
        {
            push_candidate(app, &path, seen, out);
        }
    }
}

fn is_bot_dir(dir: &Path) -> bool {
    dir.join("SOUL.md").is_file()
        || dir.join("IDENTITY.md").is_file()
        || dir.join("bot.json").is_file()
        || dir.join("agent.json").is_file()
}

fn push_candidate(
    app: ExternalBotSourceApp,
    dir: &Path,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<ExternalBotCandidate>,
) {
    let key = canonicalize(dir).map_or_else(
        |_| dir.to_string_lossy().into_owned(),
        |p| p.to_string_lossy().into_owned(),
    );
    if !seen.insert(key) {
        return;
    }

    let mut name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.trim_start_matches('.').to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Imported Bot".to_owned());
    let mut agent = match app {
        ExternalBotSourceApp::Hermes => "hermes".to_owned(),
        _ => "claude".to_owned(),
    };

    // Never open `openclaw.json` because external installs may store user tokens inside it.
    for manifest_name in ["bot.json", "agent.json"] {
        if let Ok(text) = read_text_bounded(&dir.join(manifest_name))
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(&text)
        {
            if let Some(n) = val.get("name").and_then(serde_json::Value::as_str)
                && !n.trim().is_empty()
            {
                n.trim().clone_into(&mut name);
            }
            if let Some(a) = val
                .get("agent_id")
                .or_else(|| val.get("agent"))
                .and_then(serde_json::Value::as_str)
                && !a.trim().is_empty()
            {
                a.trim().clone_into(&mut agent);
            }
            break;
        }
    }

    let soul_text = read_external_soul(dir, &name);
    let soul_preview = soul_description(&soul_text).or_else(|| {
        let first = soul_text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'));
        first.map(|s| s.chars().take(160).collect())
    });

    out.push(ExternalBotCandidate {
        app,
        agent,
        name,
        path: dir.to_string_lossy().into_owned(),
        soul_preview,
    });
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
}
