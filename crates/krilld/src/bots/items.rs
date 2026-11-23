// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Lazily opened SQLite store (`<state_dir>/bots/bot-items.db`) for Krill Bot deliverables,
//! links, and immutable audit events.
//!
//! Zero SQLite connections are opened until the first bot item operation. When opened, uses WAL,
//! `busy_timeout = 5000`, `cache_size = -1000` (~1 MiB), and `foreign_keys = ON`.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use krill_proto::bots::{
    BOT_ITEM_COMMENT_MAX_BYTES, BOT_ITEM_DESCRIPTION_MAX_BYTES, BOT_ITEM_TITLE_MAX_BYTES, BotId,
    BotItem, BotItemActor, BotItemDecision, BotItemEvent, BotItemEventKind, BotItemLink,
    BotItemStatus, BotItemsPage, CreateBotItemParams, FieldPatch, UpdateBotItemParams,
};
use krill_proto::os::fs::DirBuilderExt as _;
use rusqlite::{Connection, OptionalExtension as _, params};

use crate::session::lock;

const CURRENT_DB_VERSION: i64 = 1;
const DEFAULT_PAGE_LIMIT: usize = 50;
const MAX_PAGE_LIMIT: usize = 200;
static ITEM_UID_SEQ: AtomicU64 = AtomicU64::new(1);

/// Status or virtual bucket filter for paginated bot item queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotItemsStatusFilter {
    Status(BotItemStatus),
    Closed,
    Snoozed,
}

impl BotItemsStatusFilter {
    /// Parses a mobile/CLI status filter token (`needs_you`/`needs-you`, `working`, `planned`,
    /// `done`, `canceled`/`cancelled`, `closed`, `snoozed`).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "planned" => Some(Self::Status(BotItemStatus::Planned)),
            "working" => Some(Self::Status(BotItemStatus::Working)),
            "needs_you" | "needs-you" => Some(Self::Status(BotItemStatus::NeedsYou)),
            "done" => Some(Self::Status(BotItemStatus::Done)),
            "canceled" | "cancelled" => Some(Self::Status(BotItemStatus::Canceled)),
            "closed" => Some(Self::Closed),
            "snoozed" => Some(Self::Snoozed),
            _ => None,
        }
    }
}

/// Result of [`BotItemsStore::answer_item`] indicating whether a bot session should wake up.
#[derive(Debug, Clone)]
pub struct AnswerItemOutcome {
    pub item: BotItem,
    pub should_wake_bot: bool,
    pub answer_summary: String,
}

/// Lazily opened SQLite persistence for [`BotItem`], [`BotItemLink`], and [`BotItemEvent`].
pub struct BotItemsStore {
    db_path: PathBuf,
    conn: Mutex<Option<Connection>>,
}

impl fmt::Debug for BotItemsStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let open = self.conn.lock().is_ok_and(|g| g.is_some());
        f.debug_struct("BotItemsStore")
            .field("open", &open)
            .finish_non_exhaustive()
    }
}

impl BotItemsStore {
    /// Creates a lazy handle pointing at `db_path`. Opens zero connections until first query.
    #[must_use]
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            conn: Mutex::new(None),
        }
    }

    /// Returns `true` if a SQLite connection is currently open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        lock(&self.conn).is_some()
    }

    /// Lists items optionally filtered by `bot_id`, `status`, closed status, and snooze window.
    pub fn list_items(
        &self,
        bot_id: Option<&BotId>,
        status: Option<BotItemStatus>,
        include_closed: bool,
        include_snoozed: bool,
        now_ms: u64,
    ) -> io::Result<Vec<BotItem>> {
        if !self.db_path.exists() && !self.is_open() {
            return Ok(Vec::new());
        }
        self.with_conn(|conn| {
            let mut sql = String::from(
                "SELECT uid, number, owner_bot_id, title, description, done_means, status, \
                 close_note, snoozed_until_ms, version, last_activity_at_ms, created_at_ms, \
                 updated_at_ms, closed_at_ms \
                 FROM items WHERE 1=1",
            );
            let mut bind: Vec<rusqlite::types::Value> = Vec::new();
            if let Some(bid) = bot_id {
                sql.push_str(" AND owner_bot_id = ?");
                bind.push(rusqlite::types::Value::Text(bid.as_str().to_owned()));
            }
            if let Some(st) = status {
                sql.push_str(" AND status = ?");
                bind.push(rusqlite::types::Value::Text(st.as_str().to_owned()));
            } else if !include_closed {
                sql.push_str(" AND status NOT IN ('done', 'canceled')");
            }
            if !include_snoozed {
                sql.push_str(" AND (snoozed_until_ms IS NULL OR snoozed_until_ms <= ?)");
                bind.push(rusqlite::types::Value::Integer(u64_to_i64(now_ms)));
            }
            sql.push_str(" ORDER BY last_activity_at_ms DESC, number DESC");

            let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(bind), row_to_item_header)
                .map_err(sql_err)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row.map_err(sql_err)?);
            }
            drop(stmt);
            hydrate_items_batch(conn, items)
        })
    }

    /// Paginated item list with batch-hydrated links/events and per-bot `unseen_count`.
    pub fn list_items_page(
        &self,
        bot_id: Option<&BotId>,
        include_closed: bool,
        include_snoozed: bool,
        limit: Option<u32>,
        cursor: Option<&str>,
        now_ms: u64,
    ) -> io::Result<BotItemsPage> {
        self.list_items_page_filtered(
            bot_id,
            None,
            include_closed,
            include_snoozed,
            limit,
            cursor,
            now_ms,
        )
    }

    /// Paginated item list with optional status/bucket filter (`needs_you`, `closed`, `snoozed`, etc.).
    #[expect(clippy::too_many_arguments, reason = "paginated filtered item query")]
    pub fn list_items_page_filtered(
        &self,
        bot_id: Option<&BotId>,
        status_filter: Option<BotItemsStatusFilter>,
        include_closed: bool,
        include_snoozed: bool,
        limit: Option<u32>,
        cursor: Option<&str>,
        now_ms: u64,
    ) -> io::Result<BotItemsPage> {
        if !self.db_path.exists() && !self.is_open() {
            return Ok(BotItemsPage::default());
        }
        let page_limit = limit
            .and_then(|n| usize::try_from(n).ok())
            .map_or(DEFAULT_PAGE_LIMIT, |n| n.clamp(1, MAX_PAGE_LIMIT));
        let parsed_cursor = cursor
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(parse_items_cursor)
            .transpose()?;

        self.with_conn(|conn| {
            let mut where_clause = String::from(" WHERE 1=1");
            let mut base_bind: Vec<rusqlite::types::Value> = Vec::new();
            if let Some(bid) = bot_id {
                where_clause.push_str(" AND owner_bot_id = ?");
                base_bind.push(rusqlite::types::Value::Text(bid.as_str().to_owned()));
            }
            match status_filter {
                Some(BotItemsStatusFilter::Status(st)) => {
                    where_clause.push_str(" AND status = ?");
                    base_bind.push(rusqlite::types::Value::Text(st.as_str().to_owned()));
                    if st.is_open() && !include_snoozed {
                        where_clause
                            .push_str(" AND (snoozed_until_ms IS NULL OR snoozed_until_ms <= ?)");
                        base_bind.push(rusqlite::types::Value::Integer(u64_to_i64(now_ms)));
                    }
                }
                Some(BotItemsStatusFilter::Closed) => {
                    where_clause.push_str(" AND status IN ('done', 'canceled')");
                }
                Some(BotItemsStatusFilter::Snoozed) => {
                    if !include_closed {
                        where_clause.push_str(" AND status NOT IN ('done', 'canceled')");
                    }
                    where_clause
                        .push_str(" AND snoozed_until_ms IS NOT NULL AND snoozed_until_ms > ?");
                    base_bind.push(rusqlite::types::Value::Integer(u64_to_i64(now_ms)));
                }
                None => {
                    if !include_closed {
                        where_clause.push_str(" AND status NOT IN ('done', 'canceled')");
                    }
                    if !include_snoozed {
                        where_clause
                            .push_str(" AND (snoozed_until_ms IS NULL OR snoozed_until_ms <= ?)");
                        base_bind.push(rusqlite::types::Value::Integer(u64_to_i64(now_ms)));
                    }
                }
            }

            let count_sql = format!("SELECT COUNT(*) FROM items{where_clause}");
            let total_i64: i64 = conn
                .query_row(
                    &count_sql,
                    rusqlite::params_from_iter(base_bind.clone()),
                    |row| row.get(0),
                )
                .map_err(sql_err)?;
            let total_count = usize::try_from(total_i64.max(0)).unwrap_or(0);

            let unseen_sql = format!(
                "SELECT COUNT(*) FROM items i{where_clause} AND EXISTS (\
                    SELECT 1 FROM item_events e \
                    WHERE e.item_uid = i.uid \
                    AND e.id > COALESCE((SELECT last_seen_event_id FROM items_seen s WHERE s.bot_id = i.owner_bot_id), 0)\
                )"
            );
            let unseen_i64: i64 = conn
                .query_row(
                    &unseen_sql,
                    rusqlite::params_from_iter(base_bind.clone()),
                    |row| row.get(0),
                )
                .map_err(sql_err)?;
            let unseen_count = usize::try_from(unseen_i64.max(0)).unwrap_or(0);

            let mut page_sql = format!(
                "SELECT uid, number, owner_bot_id, title, description, done_means, status, \
                 close_note, snoozed_until_ms, version, last_activity_at_ms, created_at_ms, \
                 updated_at_ms, closed_at_ms \
                 FROM items{where_clause}"
            );
            let mut page_bind = base_bind;
            if let Some((cursor_ts, cursor_uid)) = parsed_cursor {
                page_sql.push_str(
                    " AND (last_activity_at_ms < ? OR (last_activity_at_ms = ? AND uid < ?))",
                );
                page_bind.push(rusqlite::types::Value::Integer(cursor_ts));
                page_bind.push(rusqlite::types::Value::Integer(cursor_ts));
                page_bind.push(rusqlite::types::Value::Text(cursor_uid));
            }
            page_sql.push_str(" ORDER BY last_activity_at_ms DESC, uid DESC LIMIT ?");
            page_bind.push(rusqlite::types::Value::Integer(
                i64::try_from(page_limit.saturating_add(1)).unwrap_or(i64::MAX),
            ));

            let mut stmt = conn.prepare(&page_sql).map_err(sql_err)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(page_bind), row_to_item_header)
                .map_err(sql_err)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row.map_err(sql_err)?);
            }
            drop(stmt);

            let next_cursor = if items.len() > page_limit {
                items.truncate(page_limit);
                items
                    .last()
                    .map(|last| format!("{}|{}", last.last_activity_at_ms, last.uid))
            } else {
                None
            };

            let items = hydrate_items_batch(conn, items)?;
            Ok(BotItemsPage {
                items,
                next_cursor,
                total_count,
                unseen_count,
            })
        })
    }

    /// Loads a single item by exact `uid`, `<bot_id>#<number>`, or unambiguous `#<number>`/`<number>`.
    pub fn get_item(&self, item_id: &str) -> io::Result<Option<BotItem>> {
        if !self.db_path.exists() && !self.is_open() {
            return Ok(None);
        }
        self.with_conn(|conn| {
            let Some(uid) = resolve_item_uid(conn, item_id)? else {
                return Ok(None);
            };
            load_item_by_uid(conn, &uid)
        })
    }

    /// Finds an existing item for `bot_id` that already carries `link` (for poller deduplication).
    pub fn find_item_by_link(
        &self,
        bot_id: &BotId,
        link: &BotItemLink,
    ) -> io::Result<Option<BotItem>> {
        if !self.db_path.exists() && !self.is_open() {
            return Ok(None);
        }
        let (kind, target) = encode_link(link);
        self.with_conn(|conn| {
            let uid: Option<String> = conn
                .query_row(
                    "SELECT i.uid FROM items i \
                     INNER JOIN item_links l ON l.item_uid = i.uid \
                     WHERE i.owner_bot_id = ?1 AND l.kind = ?2 AND l.target = ?3 \
                     ORDER BY i.created_at_ms DESC LIMIT 1",
                    params![bot_id.as_str(), kind, target],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql_err)?;
            match uid {
                Some(uid) => load_item_by_uid(conn, &uid),
                None => Ok(None),
            }
        })
    }

    /// Creates a new item with a monotonically increasing per-bot number, initial links, and `Made` event.
    pub fn create_item(
        &self,
        params_in: &CreateBotItemParams,
        actor: &BotItemActor,
        now_ms: u64,
    ) -> io::Result<BotItem> {
        let title = validate_title(&params_in.title)?;
        let description = validate_optional_description(params_in.description.as_deref())?;
        let done_means = validate_optional_description(params_in.done_means.as_deref())?;
        let status = params_in.status.unwrap_or(BotItemStatus::Planned);
        let closed_at_ms = (!status.is_open()).then_some(now_ms);
        let seq = ITEM_UID_SEQ.fetch_add(1, Ordering::Relaxed);
        let uid = format!("item-{now_ms}-{}-{seq}", std::process::id());

        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            let next_number: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(number), 0) + 1 FROM items WHERE owner_bot_id = ?1",
                    params![params_in.bot_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(sql_err)?;
            let initial_mail_state = if status == BotItemStatus::NeedsYou
                && matches!(actor, BotItemActor::Bot { .. })
            {
                Some("owed")
            } else {
                None
            };
            tx.execute(
                "INSERT INTO items \
                 (uid, number, owner_bot_id, title, description, done_means, status, close_note, \
                  snoozed_until_ms, version, last_activity_at_ms, created_at_ms, updated_at_ms, closed_at_ms, \
                  mail_state) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, 1, ?8, ?8, ?8, ?9, ?10)",
                params![
                    uid,
                    next_number,
                    params_in.bot_id.as_str(),
                    title,
                    description,
                    done_means,
                    status.as_str(),
                    u64_to_i64(now_ms),
                    closed_at_ms.map(u64_to_i64),
                    initial_mail_state,
                ],
            )
            .map_err(sql_err)?;

            for link in &params_in.links {
                let (kind, target) = encode_link(link);
                tx.execute(
                    "INSERT OR IGNORE INTO item_links (item_uid, kind, target) VALUES (?1, ?2, ?3)",
                    params![uid, kind, target],
                )
                .map_err(sql_err)?;
            }

            insert_event(
                &tx,
                &uid,
                BotItemEventKind::Made,
                actor,
                &title,
                &[],
                now_ms,
            )?;

            tx.commit().map_err(sql_err)?;
            load_item_by_uid(conn, &uid)?
                .ok_or_else(|| io::Error::other("created bot item vanished"))
        })
    }

    /// Updates an existing item's metadata, status, snooze window, and/or appends a comment event.
    pub fn update_item(
        &self,
        params_in: &UpdateBotItemParams,
        actor: &BotItemActor,
        now_ms: u64,
    ) -> io::Result<BotItem> {
        if let Some(ref comment) = params_in.comment {
            validate_comment(comment)?;
        }
        let next_title = params_in.title.as_deref().map(validate_title).transpose()?;
        let next_desc = match params_in.description.as_deref() {
            Some(d) => Some(validate_optional_description(Some(d))?),
            None => None,
        };
        let next_done = match params_in.done_means.as_deref() {
            Some(d) => Some(validate_optional_description(Some(d))?),
            None => None,
        };
        let next_close_note = match params_in.close_note.as_deref() {
            Some(note) => Some(validate_optional_description(Some(note))?),
            None => None,
        };

        self.with_conn(|conn| {
            let uid = resolve_item_uid(conn, &params_in.item_id)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("bot item not found: {}", params_in.item_id),
                )
            })?;
            let mut item = load_item_by_uid(conn, &uid)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("bot item not found: {}", params_in.item_id),
                )
            })?;

            let mut edited = false;
            // What changed, before and after, for the item's Activity ("changed title ›").
            let mut fields = serde_json::Map::new();
            let mut change = |name: &str, from: &str, to: &str| {
                fields.insert(
                    name.to_owned(),
                    serde_json::json!({ "from": from, "to": to }),
                );
            };
            if let Some(t) = next_title
                && t != item.title
            {
                change("title", &item.title, &t);
                item.title = t;
                edited = true;
            }
            if let Some(d) = next_desc
                && d != item.description
            {
                change(
                    "description",
                    item.description.as_deref().unwrap_or(""),
                    d.as_deref().unwrap_or(""),
                );
                item.description = d;
                edited = true;
            }
            if let Some(dm) = next_done
                && dm != item.done_means
            {
                change(
                    "doneMeans",
                    item.done_means.as_deref().unwrap_or(""),
                    dm.as_deref().unwrap_or(""),
                );
                item.done_means = dm;
                edited = true;
            }
            if let Some(cn) = next_close_note {
                item.close_note = cn;
                edited = true;
            }

            let status_changed = params_in.status.filter(|&s| s != item.status);
            let effective_status = status_changed.unwrap_or(item.status);
            if !effective_status.is_open()
                && matches!(params_in.snoozed_until_ms, FieldPatch::Set(_))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot snooze a closed (Done or Canceled) item",
                ));
            }
            if let Some(new_status) = status_changed {
                item.status = new_status;
                if new_status.is_open() {
                    item.closed_at_ms = None;
                } else {
                    item.closed_at_ms = Some(now_ms);
                }
            }

            let prev_snooze = item.snoozed_until_ms;
            let trimmed_comment = params_in
                .comment
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let owner_comment =
                trimmed_comment.is_some() && matches!(actor, BotItemActor::Owner { .. });
            let auto_unsnooze_on_status = status_changed.is_some_and(|s| {
                matches!(
                    s,
                    BotItemStatus::Done | BotItemStatus::Canceled | BotItemStatus::NeedsYou
                )
            });

            match &params_in.snoozed_until_ms {
                FieldPatch::Unchanged => {
                    if auto_unsnooze_on_status || owner_comment {
                        item.snoozed_until_ms = None;
                    }
                }
                FieldPatch::Clear => item.snoozed_until_ms = None,
                FieldPatch::Set(until) => item.snoozed_until_ms = Some(*until),
            }
            let snooze_event = match (prev_snooze, item.snoozed_until_ms) {
                (_, Some(until)) if prev_snooze != Some(until) => {
                    Some((BotItemEventKind::Snoozed, format!("snoozed until {until}")))
                }
                (Some(_), None) => Some((BotItemEventKind::Unsnoozed, "unsnoozed".to_owned())),
                _ => None,
            };

            item.version = item.version.saturating_add(1);
            item.updated_at_ms = now_ms;
            item.last_activity_at_ms = now_ms;

            let has_decisions = !params_in.decisions.is_empty();
            let next_mail_state: Option<&str> = match status_changed {
                Some(BotItemStatus::NeedsYou)
                    if matches!(actor, BotItemActor::Bot { .. }) || has_decisions =>
                {
                    Some("owed")
                }
                Some(s) if !s.is_open() => Some("closed"),
                None if effective_status == BotItemStatus::NeedsYou && has_decisions => {
                    Some("owed")
                }
                _ => None,
            };

            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            tx.execute(
                "UPDATE items SET title = ?1, description = ?2, done_means = ?3, status = ?4, \
                 close_note = ?5, snoozed_until_ms = ?6, version = ?7, last_activity_at_ms = ?8, \
                 updated_at_ms = ?8, closed_at_ms = ?9, \
                 mail_state = COALESCE(?10, mail_state) \
                 WHERE uid = ?11",
                params![
                    item.title,
                    item.description,
                    item.done_means,
                    item.status.as_str(),
                    item.close_note,
                    item.snoozed_until_ms.map(u64_to_i64),
                    u64_to_i64(item.version),
                    u64_to_i64(now_ms),
                    item.closed_at_ms.map(u64_to_i64),
                    next_mail_state,
                    uid,
                ],
            )
            .map_err(sql_err)?;

            if edited {
                let text = if fields.is_empty() {
                    "updated item".to_owned()
                } else {
                    serde_json::json!({ "fields": fields }).to_string()
                };
                insert_event(
                    &tx,
                    &uid,
                    BotItemEventKind::Edited,
                    actor,
                    &text,
                    &[],
                    now_ms,
                )?;
            }
            if let Some(new_status) = status_changed {
                let status_decisions: &[BotItemDecision] = if trimmed_comment.is_some() {
                    &[]
                } else {
                    &params_in.decisions
                };
                insert_event(
                    &tx,
                    &uid,
                    BotItemEventKind::Status,
                    actor,
                    &format!("status -> {}", new_status.as_str()),
                    status_decisions,
                    now_ms,
                )?;
                if new_status == BotItemStatus::NeedsYou {
                    let row_id = tx.last_insert_rowid();
                    tx.execute(
                        "UPDATE items SET question_event = ?1 WHERE uid = ?2",
                        params![row_id.to_string(), uid],
                    )
                    .map_err(sql_err)?;
                }
            }
            if let Some(comment) = trimmed_comment {
                insert_event(
                    &tx,
                    &uid,
                    BotItemEventKind::Commented,
                    actor,
                    comment,
                    &params_in.decisions,
                    now_ms,
                )?;
                if status_changed.is_none()
                    && has_decisions
                    && item.status == BotItemStatus::NeedsYou
                {
                    let row_id = tx.last_insert_rowid();
                    tx.execute(
                        "UPDATE items SET question_event = ?1, \
                         mail_state = COALESCE(mail_state, 'owed') WHERE uid = ?2",
                        params![row_id.to_string(), uid],
                    )
                    .map_err(sql_err)?;
                }
            } else if status_changed.is_none() && has_decisions {
                let prompt_text = params_in
                    .decisions
                    .first()
                    .map(|d| d.question.trim())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("decision requested");
                insert_event(
                    &tx,
                    &uid,
                    BotItemEventKind::Commented,
                    actor,
                    prompt_text,
                    &params_in.decisions,
                    now_ms,
                )?;
                if item.status == BotItemStatus::NeedsYou {
                    let row_id = tx.last_insert_rowid();
                    tx.execute(
                        "UPDATE items SET question_event = ?1, \
                         mail_state = COALESCE(mail_state, 'owed') WHERE uid = ?2",
                        params![row_id.to_string(), uid],
                    )
                    .map_err(sql_err)?;
                }
            }

            if let Some((kind, text)) = snooze_event {
                insert_event(&tx, &uid, kind, actor, &text, &[], now_ms)?;
            }

            tx.commit().map_err(sql_err)?;
            load_item_by_uid(conn, &uid)?
                .ok_or_else(|| io::Error::other("updated bot item vanished"))
        })
    }

    /// Appends a comment event to an existing item.
    pub fn add_comment(
        &self,
        item_id: &str,
        actor: &BotItemActor,
        text: &str,
        decisions: &[BotItemDecision],
        now_ms: u64,
    ) -> io::Result<BotItem> {
        let cleaned = validate_comment(text)?;
        self.with_conn(|conn| {
            let uid = resolve_item_uid(conn, item_id)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("bot item not found: {item_id}"),
                )
            })?;
            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            let was_snoozed: Option<i64> = tx
                .query_row(
                    "SELECT snoozed_until_ms FROM items WHERE uid = ?1",
                    params![uid],
                    |row| row.get(0),
                )
                .map_err(sql_err)?;
            insert_event(
                &tx,
                &uid,
                BotItemEventKind::Commented,
                actor,
                &cleaned,
                decisions,
                now_ms,
            )?;
            let event_row_id = tx.last_insert_rowid();
            let auto_unsnooze =
                was_snoozed.is_some() && matches!(actor, BotItemActor::Owner { .. });
            if auto_unsnooze {
                insert_event(
                    &tx,
                    &uid,
                    BotItemEventKind::Unsnoozed,
                    actor,
                    "unsnoozed",
                    &[],
                    now_ms,
                )?;
                tx.execute(
                    "UPDATE items SET snoozed_until_ms = NULL, version = version + 1, \
                     last_activity_at_ms = ?1, updated_at_ms = ?1, \
                     others_last_event_id = CASE WHEN ?3 THEN ?2 ELSE others_last_event_id END \
                     WHERE uid = ?4",
                    params![
                        u64_to_i64(now_ms),
                        event_row_id,
                        !matches!(actor, BotItemActor::Bot { .. }),
                        uid
                    ],
                )
                .map_err(sql_err)?;
            } else {
                tx.execute(
                    "UPDATE items SET version = version + 1, last_activity_at_ms = ?1, \
                     updated_at_ms = ?1, \
                     others_last_event_id = CASE WHEN ?3 THEN ?2 ELSE others_last_event_id END \
                     WHERE uid = ?4",
                    params![
                        u64_to_i64(now_ms),
                        event_row_id,
                        !matches!(actor, BotItemActor::Bot { .. }),
                        uid
                    ],
                )
                .map_err(sql_err)?;
            }
            tx.commit().map_err(sql_err)?;
            load_item_by_uid(conn, &uid)?
                .ok_or_else(|| io::Error::other("commented bot item vanished"))
        })
    }

    /// Records an owner decision/answer for `item_id` and returns whether `mail_state == 'owed'`.
    #[expect(clippy::too_many_arguments, reason = "owner item answer payload")]
    pub fn answer_item(
        &self,
        item_id: &str,
        answer_text: Option<&str>,
        option_index: Option<u32>,
        decision_id: Option<&str>,
        request_id: Option<&str>,
        actor: &BotItemActor,
        now_ms: u64,
    ) -> io::Result<AnswerItemOutcome> {
        let req_id_trimmed = request_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let text_trimmed = answer_text
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        if let Some(ref text) = text_trimmed {
            validate_comment(text)?;
        }
        if text_trimmed.is_none() && option_index.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "answer_text or option_index must be provided",
            ));
        }

        self.with_conn(|conn| {
            let uid = resolve_item_uid(conn, item_id)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("bot item not found: {item_id}"),
                )
            })?;
            let item = load_item_by_uid(conn, &uid)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("bot item not found: {item_id}"),
                )
            })?;

            let (mail_state, existing_answer_id, snoozed): (
                Option<String>,
                Option<String>,
                Option<i64>,
            ) = conn
                .query_row(
                    "SELECT mail_state, answer_id, snoozed_until_ms FROM items WHERE uid = ?1",
                    params![uid],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(sql_err)?;

            if let (Some(req_id), Some(prev_ans)) = (&req_id_trimmed, &existing_answer_id)
                && req_id == prev_ans
            {
                return Ok(AnswerItemOutcome {
                    item,
                    should_wake_bot: false,
                    answer_summary: "already answered".to_owned(),
                });
            }

            let active_ask = (item.status == BotItemStatus::NeedsYou)
                .then(|| active_decision_event(&item.events))
                .flatten();
            let multi_ask = active_ask.filter(|ev| ev.decisions.len() > 1);
            let mut collected_multi = multi_ask
                .map(|ask_ev| collect_decision_answers(&ask_ev.decisions, &item.events, ask_ev.id));

            let decision_id_trimmed = decision_id.map(str::trim).filter(|s| !s.is_empty());
            let target_on_multi = multi_ask.and_then(|ask_ev| {
                let existing = collected_multi.as_deref().unwrap_or(&[]);
                resolve_target_decision_on_event(
                    ask_ev,
                    decision_id_trimmed,
                    option_index,
                    existing,
                )
            });
            if multi_ask.is_some()
                && let Some(raw_dec_id) = decision_id_trimmed
                && target_on_multi.is_none()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("decision_id '{raw_dec_id}' does not match an active decision"),
                ));
            }

            let resolved_option = if let Some(idx) = option_index {
                let idx_usize = usize::try_from(idx).unwrap_or(usize::MAX);
                let opt_label = target_on_multi
                    .map(|(_, d)| d)
                    .or_else(|| resolve_target_decision(&item.events, decision_id))
                    .and_then(|d| d.options.get(idx_usize))
                    .cloned();
                match opt_label {
                    Some(label) => Some(label),
                    None if text_trimmed.is_some() => Some(format!("option #{idx}")),
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("decision option index {idx} out of bounds"),
                        ));
                    }
                }
            } else {
                None
            };

            let raw_summary = match (resolved_option, text_trimmed) {
                (Some(opt), Some(txt)) if opt == txt => opt,
                (Some(_), Some(txt)) if txt.contains(" → ") => txt,
                (Some(opt), Some(txt)) => format!("{opt} — {txt}"),
                (Some(opt), None) => opt,
                (None, Some(txt)) => txt,
                (None, None) => {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty answer"));
                }
            };

            let (event_text, all_decisions_answered, final_summary) =
                if let (Some(ask_ev), Some(collected)) = (multi_ask, collected_multi.as_mut()) {
                    let matched_lines =
                        apply_decision_answer_lines(&ask_ev.decisions, &raw_summary, collected);
                    if matched_lines > 0 {
                        let all_done = collected.iter().all(Option::is_some);
                        let summary = if all_done {
                            format_collected_decision_answers(&ask_ev.decisions, collected)
                        } else {
                            raw_summary.clone()
                        };
                        (raw_summary, all_done, summary)
                    } else if let Some((target_idx, target_dec)) = target_on_multi {
                        let q_line = decision_question_line(&target_dec.question);
                        let prefixed = format!("{q_line} → {raw_summary}");
                        let clamped =
                            clamp_utf8_bytes(&prefixed, BOT_ITEM_COMMENT_MAX_BYTES).to_owned();
                        if let Some(slot) = collected.get_mut(target_idx) {
                            *slot = Some(raw_summary);
                        }
                        let all_done = collected.iter().all(Option::is_some);
                        let summary = if all_done {
                            format_collected_decision_answers(&ask_ev.decisions, collected)
                        } else {
                            clamped.clone()
                        };
                        (clamped, all_done, summary)
                    } else {
                        // Freeform prose answer with no explicit decision target resolves the item.
                        (raw_summary.clone(), true, raw_summary)
                    }
                } else {
                    (raw_summary.clone(), true, raw_summary)
                };

            let answer_id = req_id_trimmed.unwrap_or_else(|| {
                let seq = ITEM_UID_SEQ.fetch_add(1, Ordering::Relaxed);
                format!("ans-{now_ms}-{seq}")
            });
            let should_wake_bot = all_decisions_answered && mail_state.as_deref() == Some("owed");
            let next_mail_state: Option<&str> = if all_decisions_answered {
                Some("answered")
            } else {
                mail_state.as_deref()
            };

            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            insert_event(
                &tx,
                &uid,
                BotItemEventKind::Commented,
                actor,
                &event_text,
                &[],
                now_ms,
            )?;
            let comment_row_id = tx.last_insert_rowid();
            let next_status = if item.status == BotItemStatus::NeedsYou && all_decisions_answered {
                insert_event(
                    &tx,
                    &uid,
                    BotItemEventKind::Status,
                    actor,
                    "status -> working",
                    &[],
                    now_ms,
                )?;
                BotItemStatus::Working
            } else {
                item.status
            };
            if snoozed.is_some() {
                insert_event(
                    &tx,
                    &uid,
                    BotItemEventKind::Unsnoozed,
                    actor,
                    "unsnoozed",
                    &[],
                    now_ms,
                )?;
            }
            tx.execute(
                "UPDATE items SET status = ?1, snoozed_until_ms = NULL, \
                 mail_state = ?2, answer_id = ?3, others_last_event_id = ?4, \
                 version = version + 1, last_activity_at_ms = ?5, updated_at_ms = ?5 \
                 WHERE uid = ?6",
                params![
                    next_status.as_str(),
                    next_mail_state,
                    answer_id,
                    comment_row_id,
                    u64_to_i64(now_ms),
                    uid,
                ],
            )
            .map_err(sql_err)?;
            tx.commit().map_err(sql_err)?;

            let updated = load_item_by_uid(conn, &uid)?
                .ok_or_else(|| io::Error::other("answered bot item vanished"))?;
            Ok(AnswerItemOutcome {
                item: updated,
                should_wake_bot,
                answer_summary: final_summary,
            })
        })
    }

    /// Attaches chat/mail routing metadata to an item for downstream reply wakeups.
    pub fn set_mail_metadata(
        &self,
        item_id: &str,
        chat_id: Option<&str>,
        mail_state: Option<&str>,
        request_id: Option<&str>,
    ) -> io::Result<()> {
        self.with_conn(|conn| {
            let uid = resolve_item_uid(conn, item_id)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("bot item not found: {item_id}"),
                )
            })?;
            conn.execute(
                "UPDATE items SET chat_id = COALESCE(?1, chat_id), \
                 mail_state = COALESCE(?2, mail_state), \
                 request_id = COALESCE(?3, request_id) \
                 WHERE uid = ?4",
                params![chat_id, mail_state, request_id, uid],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }

    /// Marks `bot_id`'s items as seen/opened up to `up_to_event_id` (or latest event if `None`).
    pub fn mark_items_opened(
        &self,
        bot_id: &BotId,
        up_to_event_id: Option<&str>,
        now_ms: u64,
    ) -> io::Result<()> {
        self.with_conn(|conn| {
            let target_event_id: i64 = match up_to_event_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(raw) => raw.parse::<i64>().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "up_to_event_id must be a numeric event id",
                    )
                })?,
                None => conn
                    .query_row(
                        "SELECT COALESCE(MAX(e.id), 0) FROM item_events e \
                         INNER JOIN items i ON i.uid = e.item_uid \
                         WHERE i.owner_bot_id = ?1",
                        params![bot_id.as_str()],
                        |row| row.get(0),
                    )
                    .map_err(sql_err)?,
            };

            conn.execute(
                "INSERT INTO items_seen (bot_id, last_seen_event_id, opened_at_ms) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(bot_id) DO UPDATE SET \
                     last_seen_event_id = MAX(items_seen.last_seen_event_id, excluded.last_seen_event_id), \
                     opened_at_ms = excluded.opened_at_ms",
                params![bot_id.as_str(), target_event_id, u64_to_i64(now_ms)],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }

    /// The newest item event the owner has seen for `bot_id` (0 before the first open).
    pub fn items_seen_event_id(&self, bot_id: &BotId) -> io::Result<u64> {
        if !self.db_path.exists() && !self.is_open() {
            return Ok(0);
        }
        self.with_conn(|conn| {
            let id: Option<i64> = conn
                .query_row(
                    "SELECT last_seen_event_id FROM items_seen WHERE bot_id = ?1",
                    params![bot_id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql_err)?;
            Ok(id.and_then(|id| u64::try_from(id).ok()).unwrap_or(0))
        })
    }

    /// Attaches a typed link (`dispatch`/`pr`/`url`/`file`) to an item idempotently.
    pub fn add_link(
        &self,
        item_id: &str,
        link: &BotItemLink,
        actor: &BotItemActor,
        now_ms: u64,
    ) -> io::Result<BotItem> {
        let (kind, target) = encode_link(link);
        if target.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "item link target must not be empty",
            ));
        }
        self.with_conn(|conn| {
            let uid = resolve_item_uid(conn, item_id)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("bot item not found: {item_id}"),
                )
            })?;
            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            let inserted = tx
                .execute(
                    "INSERT OR IGNORE INTO item_links (item_uid, kind, target) VALUES (?1, ?2, ?3)",
                    params![uid, kind, target],
                )
                .map_err(sql_err)?;
            if inserted > 0 {
                insert_event(
                    &tx,
                    &uid,
                    BotItemEventKind::Linked,
                    actor,
                    &format!("{kind}:{target}"),
                    &[],
                    now_ms,
                )?;
                tx.execute(
                    "UPDATE items SET version = version + 1, last_activity_at_ms = ?1, updated_at_ms = ?1 WHERE uid = ?2",
                    params![u64_to_i64(now_ms), uid],
                )
                .map_err(sql_err)?;
            }
            tx.commit().map_err(sql_err)?;
            load_item_by_uid(conn, &uid)?
                .ok_or_else(|| io::Error::other("linked bot item vanished"))
        })
    }

    /// Deletes all items (and cascaded links/events) plus `items_seen` rows owned by `bot_id`.
    pub fn delete_bot_items(&self, bot_id: &BotId) -> io::Result<usize> {
        if !self.db_path.exists() && !self.is_open() {
            return Ok(0);
        }
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            let deleted = tx
                .execute(
                    "DELETE FROM items WHERE owner_bot_id = ?1",
                    params![bot_id.as_str()],
                )
                .map_err(sql_err)?;
            tx.execute(
                "DELETE FROM items_seen WHERE bot_id = ?1",
                params![bot_id.as_str()],
            )
            .map_err(sql_err)?;
            tx.commit().map_err(sql_err)?;
            Ok(deleted)
        })
    }

    fn with_conn<T>(&self, f: impl FnOnce(&mut Connection) -> io::Result<T>) -> io::Result<T> {
        let mut guard = lock(&self.conn);
        if guard.is_none() {
            *guard = Some(open_and_migrate(&self.db_path)?);
        }
        let conn = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("bot items SQLite connection missing"))?;
        f(conn)
    }
}

fn open_and_migrate(path: &PathBuf) -> io::Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let conn = Connection::open(path).map_err(sql_err)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;\n\
         PRAGMA busy_timeout = 5000;\n\
         PRAGMA cache_size = -1000;\n\
         PRAGMA foreign_keys = ON;\n\
         PRAGMA synchronous = NORMAL;\n\
         CREATE TABLE IF NOT EXISTS items (\n\
             uid TEXT PRIMARY KEY,\n\
             number INTEGER NOT NULL,\n\
             owner_bot_id TEXT NOT NULL,\n\
             title TEXT NOT NULL,\n\
             description TEXT,\n\
             done_means TEXT,\n\
             status TEXT NOT NULL,\n\
             close_note TEXT,\n\
             snoozed_until_ms INTEGER,\n\
             version INTEGER NOT NULL,\n\
             last_activity_at_ms INTEGER NOT NULL,\n\
             created_at_ms INTEGER NOT NULL,\n\
             updated_at_ms INTEGER NOT NULL,\n\
             closed_at_ms INTEGER,\n\
             chat_id TEXT,\n\
             question_event TEXT,\n\
             others_last_event_id INTEGER,\n\
             mail_state TEXT,\n\
             request_id TEXT,\n\
             answer_id TEXT,\n\
             UNIQUE(owner_bot_id, number)\n\
         );\n\
         CREATE INDEX IF NOT EXISTS idx_items_bot_status ON items(owner_bot_id, status);\n\
         CREATE TABLE IF NOT EXISTS item_links (\n\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
             item_uid TEXT NOT NULL REFERENCES items(uid) ON DELETE CASCADE,\n\
             kind TEXT NOT NULL,\n\
             target TEXT NOT NULL,\n\
             UNIQUE(item_uid, kind, target)\n\
         );\n\
         CREATE INDEX IF NOT EXISTS idx_item_links_uid ON item_links(item_uid);\n\
         CREATE TABLE IF NOT EXISTS item_events (\n\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
             item_uid TEXT NOT NULL REFERENCES items(uid) ON DELETE CASCADE,\n\
             kind TEXT NOT NULL,\n\
             actor_json TEXT NOT NULL,\n\
             text TEXT NOT NULL,\n\
             decisions_json TEXT NOT NULL,\n\
             created_at_ms INTEGER NOT NULL\n\
         );\n\
         CREATE INDEX IF NOT EXISTS idx_item_events_uid ON item_events(item_uid);\n\
         CREATE TABLE IF NOT EXISTS items_seen (\n\
             bot_id TEXT PRIMARY KEY,\n\
             last_seen_event_id INTEGER NOT NULL DEFAULT 0,\n\
             opened_at_ms INTEGER NOT NULL DEFAULT 0\n\
         );",
    )
    .map_err(sql_err)?;

    let user_version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sql_err)?;
    if user_version < CURRENT_DB_VERSION {
        ensure_items_v1_columns(&conn)?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_items_mail_state ON items(owner_bot_id, mail_state);\n\
             CREATE INDEX IF NOT EXISTS idx_items_request_id ON items(request_id);\n\
             PRAGMA user_version = 1;",
        )
        .map_err(sql_err)?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).ok();
    }

    Ok(conn)
}

fn ensure_items_v1_columns(conn: &Connection) -> io::Result<()> {
    let mut existing = std::collections::HashSet::new();
    let mut stmt = conn.prepare("PRAGMA table_info(items)").map_err(sql_err)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sql_err)?;
    for row in rows {
        existing.insert(row.map_err(sql_err)?);
    }
    drop(stmt);

    for (col, ty) in [
        ("chat_id", "TEXT"),
        ("question_event", "TEXT"),
        ("others_last_event_id", "INTEGER"),
        ("mail_state", "TEXT"),
        ("request_id", "TEXT"),
        ("answer_id", "TEXT"),
    ] {
        if !existing.contains(col) {
            conn.execute(&format!("ALTER TABLE items ADD COLUMN {col} {ty}"), [])
                .map_err(sql_err)?;
        }
    }
    Ok(())
}

fn resolve_item_uid(conn: &Connection, raw: &str) -> io::Result<Option<String>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let exact: Option<String> = conn
        .query_row(
            "SELECT uid FROM items WHERE uid = ?1",
            params![trimmed],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_err)?;
    if exact.is_some() {
        return Ok(exact);
    }
    if let Some((bot_part, num_part)) = trimmed.split_once('#')
        && !bot_part.is_empty()
        && let Ok(num) = num_part.parse::<i64>()
    {
        return conn
            .query_row(
                "SELECT uid FROM items WHERE owner_bot_id = ?1 AND number = ?2",
                params![bot_part, num],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_err);
    }
    let num_str = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if let Ok(num) = num_str.parse::<i64>() {
        let mut stmt = conn
            .prepare("SELECT uid FROM items WHERE number = ?1 ORDER BY updated_at_ms DESC LIMIT 2")
            .map_err(sql_err)?;
        let matches: Vec<String> = stmt
            .query_map(params![num], |row| row.get(0))
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;
        if matches.len() > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "ambiguous item number #{num}: matches multiple bots; qualify with <bot_id>#{num}"
                ),
            ));
        }
        return Ok(matches.into_iter().next());
    }
    Ok(None)
}

fn hydrate_items_batch(conn: &Connection, mut items: Vec<BotItem>) -> io::Result<Vec<BotItem>> {
    if items.is_empty() {
        return Ok(items);
    }
    let mut index_by_uid = HashMap::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        index_by_uid.insert(item.uid.clone(), idx);
    }

    let uids: Vec<String> = items.iter().map(|i| i.uid.clone()).collect();
    for chunk in uids.chunks(400) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let links_sql = format!(
            "SELECT item_uid, kind, target FROM item_links \
             WHERE item_uid IN ({placeholders}) ORDER BY id ASC"
        );
        let mut stmt = conn.prepare(&links_sql).map_err(sql_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                let uid: String = row.get(0)?;
                let kind: String = row.get(1)?;
                let target: String = row.get(2)?;
                Ok((uid, decode_link(&kind, target)))
            })
            .map_err(sql_err)?;
        for row in rows {
            let (uid, link) = row.map_err(sql_err)?;
            if let Some(&idx) = index_by_uid.get(&uid)
                && let Some(item) = items.get_mut(idx)
            {
                item.links.push(link);
            }
        }
        drop(stmt);

        let events_sql = format!(
            "SELECT id, item_uid, kind, actor_json, text, decisions_json, created_at_ms \
             FROM item_events WHERE item_uid IN ({placeholders}) ORDER BY id ASC"
        );
        let mut stmt = conn.prepare(&events_sql).map_err(sql_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                let id: i64 = row.get(0)?;
                let uid: String = row.get(1)?;
                let kind_raw: String = row.get(2)?;
                let actor_json: String = row.get(3)?;
                let text: String = row.get(4)?;
                let decisions_json: String = row.get(5)?;
                let created_at: i64 = row.get(6)?;
                let actor: BotItemActor =
                    serde_json::from_str(&actor_json).unwrap_or(BotItemActor::System {
                        reason: "unknown".to_owned(),
                    });
                let decisions: Vec<BotItemDecision> =
                    serde_json::from_str(&decisions_json).unwrap_or_default();
                Ok((
                    uid.clone(),
                    BotItemEvent {
                        id: i64_to_u64(id),
                        item_uid: uid,
                        kind: parse_event_kind(&kind_raw),
                        actor,
                        text,
                        decisions,
                        created_at_ms: i64_to_u64(created_at),
                    },
                ))
            })
            .map_err(sql_err)?;
        for row in rows {
            let (uid, event) = row.map_err(sql_err)?;
            if let Some(&idx) = index_by_uid.get(&uid)
                && let Some(item) = items.get_mut(idx)
            {
                item.events.push(event);
            }
        }
    }
    Ok(items)
}

fn parse_items_cursor(raw: &str) -> io::Result<(i64, String)> {
    let (ts_part, uid_part) = raw.split_once('|').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid items pagination cursor",
        )
    })?;
    let ts: i64 = ts_part.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid items cursor timestamp",
        )
    })?;
    if uid_part.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid items cursor uid",
        ));
    }
    Ok((ts, uid_part.to_owned()))
}

fn raw_decision_question_line(question: &str) -> &str {
    question
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| question.trim())
}

fn decision_question_line(question: &str) -> String {
    let first = raw_decision_question_line(question);
    let stripped = first.trim_start_matches(|c: char| {
        matches!(c, '#' | '>' | '-' | '*' | '+') || c.is_whitespace()
    });
    stripped
        .replace("**", "")
        .replace("__", "")
        .replace('`', "")
        .trim()
        .to_owned()
}

fn active_decision_event(events: &[BotItemEvent]) -> Option<&BotItemEvent> {
    let mut current: Option<&BotItemEvent> = None;
    let mut prev_had_decisions = false;
    for ev in events {
        if ev.kind == BotItemEventKind::Status {
            let lower = ev.text.to_ascii_lowercase();
            if lower.contains("-> done") || lower.contains("-> canceled") {
                current = None;
                prev_had_decisions = false;
                continue;
            }
            if lower.contains("-> needs_you") {
                if !ev.decisions.is_empty() {
                    current = Some(ev);
                    prev_had_decisions = true;
                } else if !prev_had_decisions {
                    current = None;
                }
                continue;
            }
        }
        if ev.decisions.is_empty() {
            prev_had_decisions = false;
        } else {
            current = Some(ev);
            prev_had_decisions = true;
        }
    }
    current
}

fn apply_decision_answer_lines(
    decisions: &[BotItemDecision],
    text: &str,
    answers: &mut [Option<String>],
) -> usize {
    let mut matched = 0_usize;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let mut best_len = 0_usize;
        let mut best_matches: Vec<(usize, String)> = Vec::new();
        for (idx, dec) in decisions.iter().enumerate() {
            let norm = decision_question_line(&dec.question);
            let raw = raw_decision_question_line(&dec.question);
            let mut best_for_dec: Option<String> = None;
            for cand in [norm.as_str(), raw] {
                if cand.is_empty() {
                    continue;
                }
                let prefix = format!("{cand} →");
                if line.starts_with(&prefix)
                    && best_for_dec
                        .as_ref()
                        .is_none_or(|prev| prefix.len() > prev.len())
                {
                    best_for_dec = Some(prefix);
                }
            }
            if let Some(prefix) = best_for_dec {
                match prefix.len().cmp(&best_len) {
                    std::cmp::Ordering::Greater => {
                        best_len = prefix.len();
                        best_matches.clear();
                        best_matches.push((idx, prefix));
                    }
                    std::cmp::Ordering::Equal => {
                        best_matches.push((idx, prefix));
                    }
                    std::cmp::Ordering::Less => {}
                }
            }
        }
        let Some((target_idx, matched_prefix)) = best_matches
            .iter()
            .find(|(idx, _)| answers.get(*idx).is_some_and(Option::is_none))
            .or_else(|| best_matches.first())
        else {
            continue;
        };
        let val = line
            .strip_prefix(matched_prefix.as_str())
            .unwrap_or("")
            .trim();
        if let Some(slot) = answers.get_mut(*target_idx) {
            *slot = Some(val.to_owned());
            matched += 1;
        }
    }
    matched
}

fn collect_decision_answers(
    decisions: &[BotItemDecision],
    events: &[BotItemEvent],
    ask_event_id: u64,
) -> Vec<Option<String>> {
    let mut answers = vec![None; decisions.len()];
    for ev in events {
        if ev.id <= ask_event_id
            || ev.kind != BotItemEventKind::Commented
            || !matches!(ev.actor, BotItemActor::Owner { .. })
        {
            continue;
        }
        apply_decision_answer_lines(decisions, &ev.text, &mut answers);
    }
    answers
}

fn format_collected_decision_answers(
    decisions: &[BotItemDecision],
    answers: &[Option<String>],
) -> String {
    decisions
        .iter()
        .enumerate()
        .filter_map(|(idx, dec)| {
            let ans = answers.get(idx).and_then(Option::as_deref)?;
            Some(format!("{} → {ans}", decision_question_line(&dec.question)))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn resolve_target_decision_on_event<'a>(
    ask_ev: &'a BotItemEvent,
    decision_id: Option<&str>,
    option_index: Option<u32>,
    existing_answers: &[Option<String>],
) -> Option<(usize, &'a BotItemDecision)> {
    let trimmed = decision_id.map(str::trim).filter(|s| !s.is_empty());
    if let Some(raw) = trimmed {
        if let Some((ev_part, dec_part)) = raw.split_once(':')
            && let (Ok(ev_id), Ok(dec_idx)) = (ev_part.parse::<u64>(), dec_part.parse::<usize>())
        {
            if ev_id == ask_ev.id {
                return ask_ev.decisions.get(dec_idx).map(|d| (dec_idx, d));
            }
            return None;
        }
        if let Ok(dec_idx) = raw.parse::<usize>() {
            return ask_ev.decisions.get(dec_idx).map(|d| (dec_idx, d));
        }
        let mut matches = ask_ev.decisions.iter().enumerate().filter(|(_, d)| {
            let q = d.question.trim();
            q == raw
                || raw_decision_question_line(&d.question) == raw
                || decision_question_line(&d.question) == raw
        });
        let first = matches.next()?;
        if existing_answers.get(first.0).is_some_and(Option::is_some) {
            for candidate in matches {
                if existing_answers
                    .get(candidate.0)
                    .is_some_and(Option::is_none)
                {
                    return Some(candidate);
                }
            }
        }
        return Some(first);
    }
    if option_index.is_some() {
        let idx = existing_answers
            .iter()
            .position(Option::is_none)
            .unwrap_or(0);
        return ask_ev.decisions.get(idx).map(|d| (idx, d));
    }
    None
}

fn resolve_target_decision<'a>(
    events: &'a [BotItemEvent],
    decision_id: Option<&str>,
) -> Option<&'a BotItemDecision> {
    let trimmed = decision_id.map(str::trim).filter(|s| !s.is_empty());
    if let Some(raw) = trimmed {
        if let Some((ev_part, dec_part)) = raw.split_once(':')
            && let (Ok(ev_id), Ok(dec_idx)) = (ev_part.parse::<u64>(), dec_part.parse::<usize>())
        {
            return events
                .iter()
                .find(|e| e.id == ev_id)
                .and_then(|e| e.decisions.get(dec_idx));
        }
        let latest = events
            .iter()
            .rev()
            .find(|e| !e.decisions.is_empty())
            .map(|e| e.decisions.as_slice())?;
        if let Ok(dec_idx) = raw.parse::<usize>() {
            return latest.get(dec_idx);
        }
        return latest.iter().find(|d| {
            let q = d.question.trim();
            q == raw
                || raw_decision_question_line(&d.question) == raw
                || decision_question_line(&d.question) == raw
        });
    }
    events
        .iter()
        .rev()
        .find(|e| !e.decisions.is_empty())
        .and_then(|e| e.decisions.first())
}

fn load_item_by_uid(conn: &Connection, uid: &str) -> io::Result<Option<BotItem>> {
    let header = conn
        .query_row(
            "SELECT uid, number, owner_bot_id, title, description, done_means, status, \
             close_note, snoozed_until_ms, version, last_activity_at_ms, created_at_ms, \
             updated_at_ms, closed_at_ms \
             FROM items WHERE uid = ?1",
            params![uid],
            row_to_item_header,
        )
        .optional()
        .map_err(sql_err)?;
    let Some(mut item) = header else {
        return Ok(None);
    };
    item.links = load_links(conn, uid)?;
    item.events = load_events(conn, uid)?;
    Ok(Some(item))
}

fn row_to_item_header(row: &rusqlite::Row<'_>) -> rusqlite::Result<BotItem> {
    let uid: String = row.get(0)?;
    let number: i64 = row.get(1)?;
    let owner_raw: String = row.get(2)?;
    let title: String = row.get(3)?;
    let description: Option<String> = row.get(4)?;
    let done_means: Option<String> = row.get(5)?;
    let status_raw: String = row.get(6)?;
    let close_note: Option<String> = row.get(7)?;
    let snoozed: Option<i64> = row.get(8)?;
    let version: i64 = row.get(9)?;
    let last_activity: i64 = row.get(10)?;
    let created: i64 = row.get(11)?;
    let updated: i64 = row.get(12)?;
    let closed: Option<i64> = row.get(13)?;

    let owner_bot_id = BotId::new(&owner_raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(err))
    })?;

    Ok(BotItem {
        uid,
        number: i64_to_u64(number),
        owner_bot_id,
        title,
        description,
        done_means,
        status: parse_status(&status_raw),
        close_note,
        links: Vec::new(),
        events: Vec::new(),
        snoozed_until_ms: snoozed.map(i64_to_u64),
        version: i64_to_u64(version),
        last_activity_at_ms: i64_to_u64(last_activity),
        created_at_ms: i64_to_u64(created),
        updated_at_ms: i64_to_u64(updated),
        closed_at_ms: closed.map(i64_to_u64),
    })
}

fn load_links(conn: &Connection, uid: &str) -> io::Result<Vec<BotItemLink>> {
    let mut stmt = conn
        .prepare("SELECT kind, target FROM item_links WHERE item_uid = ?1 ORDER BY id ASC")
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![uid], |row| {
            let kind: String = row.get(0)?;
            let target: String = row.get(1)?;
            Ok(decode_link(&kind, target))
        })
        .map_err(sql_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(sql_err)?);
    }
    Ok(out)
}

fn load_events(conn: &Connection, uid: &str) -> io::Result<Vec<BotItemEvent>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, actor_json, text, decisions_json, created_at_ms \
             FROM item_events WHERE item_uid = ?1 ORDER BY id ASC",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![uid], |row| {
            let id: i64 = row.get(0)?;
            let kind_raw: String = row.get(1)?;
            let actor_json: String = row.get(2)?;
            let text: String = row.get(3)?;
            let decisions_json: String = row.get(4)?;
            let created_at: i64 = row.get(5)?;
            let actor: BotItemActor =
                serde_json::from_str(&actor_json).unwrap_or(BotItemActor::System {
                    reason: "unknown".to_owned(),
                });
            let decisions: Vec<BotItemDecision> =
                serde_json::from_str(&decisions_json).unwrap_or_default();
            Ok(BotItemEvent {
                id: i64_to_u64(id),
                item_uid: uid.to_owned(),
                kind: parse_event_kind(&kind_raw),
                actor,
                text,
                decisions,
                created_at_ms: i64_to_u64(created_at),
            })
        })
        .map_err(sql_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(sql_err)?);
    }
    Ok(out)
}

fn insert_event(
    conn: &Connection,
    uid: &str,
    kind: BotItemEventKind,
    actor: &BotItemActor,
    text: &str,
    decisions: &[BotItemDecision],
    now_ms: u64,
) -> io::Result<()> {
    let actor_json = serde_json::to_string(actor).map_err(io::Error::other)?;
    let decisions_json = serde_json::to_string(decisions).map_err(io::Error::other)?;
    conn.execute(
        "INSERT INTO item_events (item_uid, kind, actor_json, text, decisions_json, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            uid,
            event_kind_str(kind),
            actor_json,
            text,
            decisions_json,
            u64_to_i64(now_ms),
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

fn encode_link(link: &BotItemLink) -> (&'static str, &str) {
    match link {
        BotItemLink::Dispatch(v) => ("dispatch", v.as_str()),
        BotItemLink::Pr(v) => ("pr", v.as_str()),
        BotItemLink::Url(v) => ("url", v.as_str()),
        BotItemLink::File(v) => ("file", v.as_str()),
    }
}

fn decode_link(kind: &str, target: String) -> BotItemLink {
    match kind {
        "dispatch" => BotItemLink::Dispatch(target),
        "pr" => BotItemLink::Pr(target),
        "file" => BotItemLink::File(target),
        _ => BotItemLink::Url(target),
    }
}

fn parse_status(raw: &str) -> BotItemStatus {
    match raw {
        "working" => BotItemStatus::Working,
        "needs_you" | "needs-you" => BotItemStatus::NeedsYou,
        "done" => BotItemStatus::Done,
        "canceled" => BotItemStatus::Canceled,
        _ => BotItemStatus::Planned,
    }
}

const fn event_kind_str(kind: BotItemEventKind) -> &'static str {
    match kind {
        BotItemEventKind::Made => "made",
        BotItemEventKind::Edited => "edited",
        BotItemEventKind::Status => "status",
        BotItemEventKind::Commented => "commented",
        BotItemEventKind::Linked => "linked",
        BotItemEventKind::Snoozed => "snoozed",
        BotItemEventKind::Unsnoozed => "unsnoozed",
    }
}

fn parse_event_kind(raw: &str) -> BotItemEventKind {
    match raw {
        "edited" => BotItemEventKind::Edited,
        "status" => BotItemEventKind::Status,
        "commented" => BotItemEventKind::Commented,
        "linked" => BotItemEventKind::Linked,
        "snoozed" => BotItemEventKind::Snoozed,
        "unsnoozed" => BotItemEventKind::Unsnoozed,
        _ => BotItemEventKind::Made,
    }
}

/// Clamps `raw` to at most `max_bytes` UTF-8 bytes on a valid character boundary.
#[must_use]
pub(crate) fn clamp_utf8_bytes(raw: &str, max_bytes: usize) -> &str {
    if raw.len() <= max_bytes {
        return raw;
    }
    let mut end = max_bytes;
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }
    raw.get(..end).unwrap_or("")
}

/// Clamps `raw` to at most `max_chars` Unicode scalar values AND `max_bytes` UTF-8 bytes.
#[must_use]
pub(crate) fn clamp_utf8_bytes_and_chars(raw: &str, max_chars: usize, max_bytes: usize) -> String {
    let mut end = 0_usize;
    for (count, (idx, ch)) in raw.char_indices().enumerate() {
        let next_end = idx + ch.len_utf8();
        if count >= max_chars || next_end > max_bytes {
            break;
        }
        end = next_end;
    }
    raw.get(..end).unwrap_or("").trim().to_owned()
}

/// Clamps an item title to [`BOT_ITEM_TITLE_MAX_BYTES`] UTF-8 bytes on a valid char boundary.
#[must_use]
pub(crate) fn clamp_item_title(raw: &str) -> String {
    let trimmed = raw.trim();
    let clamped = clamp_utf8_bytes(trimmed, BOT_ITEM_TITLE_MAX_BYTES).trim_end();
    if clamped.is_empty() {
        "Untitled".to_owned()
    } else {
        clamped.to_owned()
    }
}

/// Clamps an optional item description to [`BOT_ITEM_DESCRIPTION_MAX_BYTES`] UTF-8 bytes.
#[must_use]
pub(crate) fn clamp_item_description(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let clamped = clamp_utf8_bytes(trimmed, BOT_ITEM_DESCRIPTION_MAX_BYTES).trim_end();
    (!clamped.is_empty()).then(|| clamped.to_owned())
}

fn validate_title(raw: &str) -> io::Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "item title must not be empty",
        ));
    }
    if trimmed.len() > BOT_ITEM_TITLE_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("item title exceeds {BOT_ITEM_TITLE_MAX_BYTES} bytes"),
        ));
    }
    Ok(trimmed.to_owned())
}

fn validate_optional_description(raw: Option<&str>) -> io::Result<Option<String>> {
    let Some(s) = raw else {
        return Ok(None);
    };
    if s.len() > BOT_ITEM_DESCRIPTION_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("item description exceeds {BOT_ITEM_DESCRIPTION_MAX_BYTES} bytes"),
        ));
    }
    let trimmed = s.trim();
    Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
}

fn validate_comment(raw: &str) -> io::Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "comment must not be empty",
        ));
    }
    if trimmed.len() > BOT_ITEM_COMMENT_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("comment exceeds {BOT_ITEM_COMMENT_MAX_BYTES} bytes"),
        ));
    }
    Ok(trimmed.to_owned())
}

fn u64_to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn i64_to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn sql_err(err: rusqlite::Error) -> io::Error {
    io::Error::other(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::FieldPatch;
    use std::sync::atomic::AtomicU64;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_db(label: &str) -> (PathBuf, PathBuf) {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-bot-items-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("bot-items.db");
        (dir, db)
    }

    #[test]
    fn items_store_is_lazy_and_supports_full_crud_links_comments_snooze() {
        let (dir, db) = temp_db("crud");
        let store = BotItemsStore::new(db.clone());
        let bot = BotId::new("bot-pr").unwrap();
        let actor = BotItemActor::Owner { device: None };

        // Strictly lazy before any write.
        assert!(!store.is_open());
        assert!(
            store
                .list_items(Some(&bot), None, true, true, 1_000)
                .unwrap()
                .is_empty()
        );
        assert!(!store.is_open());
        assert!(!db.exists());

        let created = store
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot.clone(),
                    title: "Review PR #42".to_owned(),
                    description: Some("Check timeout handling".to_owned()),
                    done_means: Some("Approved or changes requested".to_owned()),
                    status: Some(BotItemStatus::NeedsYou),
                    links: vec![BotItemLink::Pr(
                        "https://github.com/craftowen/krill/pull/42".to_owned(),
                    )],
                },
                &actor,
                1_000,
            )
            .unwrap();
        assert!(store.is_open());
        assert_eq!(created.number, 1);
        assert_eq!(created.status, BotItemStatus::NeedsYou);
        assert_eq!(created.links.len(), 1);
        assert_eq!(created.events.len(), 1);

        // Deduplication lookup by PR link works.
        let dup = store
            .find_item_by_link(
                &bot,
                &BotItemLink::Pr("https://github.com/craftowen/krill/pull/42".to_owned()),
            )
            .unwrap();
        assert_eq!(
            dup.as_ref().map(|i| i.uid.as_str()),
            Some(created.uid.as_str())
        );

        // Add comment & extra link.
        let commented = store
            .add_comment(&created.uid, &actor, "Looks risky", &[], 2_000)
            .unwrap();
        assert_eq!(commented.events.len(), 2);

        let linked = store
            .add_link(
                "#1",
                &BotItemLink::File("crates/krilld/src/lib.rs".to_owned()),
                &actor,
                2_500,
            )
            .unwrap();
        assert_eq!(linked.links.len(), 2);

        // Snooze hides item when include_snoozed = false.
        let snoozed = store
            .update_item(
                &UpdateBotItemParams {
                    item_id: created.uid.clone(),
                    title: None,
                    description: None,
                    done_means: None,
                    status: None,
                    comment: None,
                    decisions: Vec::new(),
                    close_note: None,
                    snoozed_until_ms: FieldPatch::Set(10_000),
                },
                &actor,
                3_000,
            )
            .unwrap();
        assert_eq!(snoozed.snoozed_until_ms, Some(10_000));
        assert!(
            store
                .list_items(Some(&bot), None, false, false, 5_000)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .list_items(Some(&bot), None, false, true, 5_000)
                .unwrap()
                .len(),
            1
        );

        // Close item & verify open-only filter hides it.
        let closed = store
            .update_item(
                &UpdateBotItemParams {
                    item_id: created.uid.clone(),
                    title: None,
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::Done),
                    comment: Some("Merged cleanly".to_owned()),
                    decisions: Vec::new(),
                    close_note: Some("Done".to_owned()),
                    snoozed_until_ms: FieldPatch::Clear,
                },
                &actor,
                6_000,
            )
            .unwrap();
        assert_eq!(closed.status, BotItemStatus::Done);
        assert_eq!(closed.closed_at_ms, Some(6_000));
        assert!(
            store
                .list_items(Some(&bot), None, false, true, 7_000)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .list_items(Some(&bot), None, true, true, 7_000)
                .unwrap()
                .len(),
            1
        );

        assert_eq!(store.delete_bot_items(&bot).unwrap(), 1);
        assert!(
            store
                .list_items(Some(&bot), None, true, true, 8_000)
                .unwrap()
                .is_empty()
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_edit_records_each_field_before_and_after() {
        let (dir, db) = temp_db("edit-fields");
        let store = BotItemsStore::new(db);
        let bot = BotId::new("bot-a").unwrap();
        let owner = BotItemActor::Owner { device: None };
        let item = store
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot,
                    title: "Old".to_owned(),
                    description: None,
                    done_means: None,
                    status: None,
                    links: vec![],
                },
                &owner,
                1_000,
            )
            .unwrap();
        let edited = store
            .update_item(
                &UpdateBotItemParams {
                    item_id: item.uid,
                    title: Some("New".to_owned()),
                    description: Some("Why".to_owned()),
                    done_means: None,
                    status: None,
                    comment: None,
                    decisions: vec![],
                    close_note: None,
                    snoozed_until_ms: FieldPatch::Unchanged,
                },
                &owner,
                2_000,
            )
            .unwrap();
        let event = edited
            .events
            .iter()
            .find(|e| e.kind == BotItemEventKind::Edited)
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&event.text).unwrap();
        assert_eq!(v["fields"]["title"]["from"], "Old");
        assert_eq!(v["fields"]["title"]["to"], "New");
        assert_eq!(v["fields"]["description"]["to"], "Why");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn v2_migration_snooze_guards_ambiguity_pagination_and_answer_wake() {
        let (dir, db) = temp_db("v2");
        let store = BotItemsStore::new(db);
        let bot_a = BotId::new("bot-a").unwrap();
        let bot_b = BotId::new("bot-b").unwrap();
        let bot_actor = BotItemActor::Bot {
            bot_id: bot_a.clone(),
            session_id: Some(krill_proto::SessionId(10)),
        };
        let owner = BotItemActor::Owner { device: None };

        let item_a = store
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot_a.clone(),
                    title: "Deploy prod?".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    links: vec![],
                },
                &bot_actor,
                1_000,
            )
            .unwrap();
        let _item_b = store
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot_b.clone(),
                    title: "Other bot #1".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::Planned),
                    links: vec![],
                },
                &owner,
                1_100,
            )
            .unwrap();

        // Ambiguous "#1" across multiple bots must fail explicitly.
        assert!(store.get_item("#1").is_err());
        assert!(store.get_item("bot-a#1").unwrap().is_some());

        // Updating an already-NeedsYou item with decisions and no comment still persists decisions.
        let decision_only = store
            .update_item(
                &UpdateBotItemParams {
                    item_id: item_a.uid.clone(),
                    title: None,
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    comment: None,
                    decisions: vec![BotItemDecision {
                        question: "Approve?".to_owned(),
                        options: vec!["Yes".to_owned(), "No".to_owned()],
                        recommended: Some(0),
                        multi: false,
                    }],
                    close_note: None,
                    snoozed_until_ms: FieldPatch::Unchanged,
                },
                &owner,
                1_500,
            )
            .unwrap();
        assert!(
            decision_only
                .events
                .iter()
                .any(|e| !e.decisions.is_empty() && e.text == "Approve?")
        );

        // Combining status change + comment emits both Status and Commented events.
        let with_decision = store
            .update_item(
                &UpdateBotItemParams {
                    item_id: item_a.uid.clone(),
                    title: Some("Deploy prod v2?".to_owned()),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    comment: Some("Pick target env".to_owned()),
                    decisions: vec![BotItemDecision {
                        question: "Env?".to_owned(),
                        options: vec!["Staging".to_owned(), "Prod".to_owned()],
                        recommended: Some(0),
                        multi: false,
                    }],
                    close_note: None,
                    snoozed_until_ms: FieldPatch::Set(99_000),
                },
                &bot_actor,
                2_000,
            )
            .unwrap();
        assert!(
            with_decision
                .events
                .iter()
                .any(|e| e.kind == BotItemEventKind::Edited)
        );
        assert!(
            with_decision
                .events
                .iter()
                .any(|e| e.kind == BotItemEventKind::Commented)
        );

        // Snooze on Done item is rejected.
        assert!(
            store
                .update_item(
                    &UpdateBotItemParams {
                        item_id: item_a.uid.clone(),
                        title: None,
                        description: None,
                        done_means: None,
                        status: Some(BotItemStatus::Done),
                        comment: None,
                        decisions: Vec::new(),
                        close_note: None,
                        snoozed_until_ms: FieldPatch::Set(120_000),
                    },
                    &owner,
                    2_100,
                )
                .is_err()
        );

        // AnswerBotItem resolves option index, clears owed mail state, unsnoozes, and requests wake.
        let outcome = store
            .answer_item(
                &item_a.uid,
                Some("Ship it"),
                Some(1),
                None,
                Some("req-1"),
                &owner,
                3_000,
            )
            .unwrap();
        assert!(outcome.should_wake_bot);
        assert_eq!(outcome.answer_summary, "Prod — Ship it");
        assert!(outcome.item.snoozed_until_ms.is_none());
        assert_eq!(outcome.item.status, BotItemStatus::Working);

        // Replaying same request_id is idempotent and does not double-wake.
        let replay = store
            .answer_item(
                &item_a.uid,
                Some("Ship it"),
                Some(1),
                None,
                Some("req-1"),
                &owner,
                3_100,
            )
            .unwrap();
        assert!(!replay.should_wake_bot);

        // Pagination + unseen tracking.
        let page1 = store
            .list_items_page(None, true, true, Some(1), None, 4_000)
            .unwrap();
        assert_eq!(page1.items.len(), 1);
        assert_eq!(page1.total_count, 2);
        assert!(page1.unseen_count >= 1);
        assert!(page1.next_cursor.is_some());

        store.mark_items_opened(&bot_a, None, 4_100).unwrap();
        let after_seen = store
            .list_items_page(Some(&bot_a), true, true, Some(10), None, 4_200)
            .unwrap();
        assert_eq!(after_seen.unseen_count, 0);

        // D-32: auto-unsnooze on owner comment updates others_last_event_id, and delete_bot_items
        // removes items_seen rows too.
        let snoozed = store
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot_a.clone(),
                    title: "Snoozed item".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::Working),
                    links: Vec::new(),
                },
                &owner,
                5_000,
            )
            .unwrap();
        store
            .update_item(
                &UpdateBotItemParams {
                    item_id: snoozed.uid.clone(),
                    title: None,
                    description: None,
                    done_means: None,
                    status: None,
                    comment: None,
                    decisions: Vec::new(),
                    close_note: None,
                    snoozed_until_ms: FieldPatch::Set(999_999),
                },
                &owner,
                5_100,
            )
            .unwrap();
        let unsnoozed = store
            .add_comment(&snoozed.uid, &owner, "Wake up now", &[], 5_200)
            .unwrap();
        assert!(unsnoozed.snoozed_until_ms.is_none());
        let others_event_id: Option<i64> = store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT others_last_event_id FROM items WHERE uid = ?1",
                    params![snoozed.uid],
                    |row| row.get(0),
                )
                .map_err(sql_err)
            })
            .unwrap();
        assert!(
            others_event_id.is_some(),
            "auto_unsnooze comment must update others_last_event_id"
        );

        // M-07: filtered pagination by status (needs_you / closed / snoozed).
        let needs_you_item = store
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot_a.clone(),
                    title: "Approve release".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    links: Vec::new(),
                },
                &owner,
                5_300,
            )
            .unwrap();
        let ny_page = store
            .list_items_page_filtered(
                Some(&bot_a),
                Some(BotItemsStatusFilter::Status(BotItemStatus::NeedsYou)),
                false,
                false,
                Some(10),
                None,
                5_400,
            )
            .unwrap();
        assert_eq!(ny_page.total_count, 1);
        assert_eq!(ny_page.items.len(), 1);
        assert_eq!(ny_page.items[0].uid, needs_you_item.uid);

        // delete_bot_items also cleans items_seen.
        assert!(store.items_seen_event_id(&bot_a).unwrap() > 0);
        store.delete_bot_items(&bot_a).unwrap();
        assert_eq!(store.items_seen_event_id(&bot_a).unwrap(), 0);

        // D-32a: items_seen_event_id on a fresh path must NOT create the SQLite file.
        let untouched_db = dir.join("never-opened.db");
        let lazy_store = BotItemsStore::new(untouched_db.clone());
        assert_eq!(lazy_store.items_seen_event_id(&bot_a).unwrap(), 0);
        assert!(!untouched_db.exists());
        assert!(!lazy_store.is_open());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn answer_item_matches_decision_id_and_persists_multi_compat() {
        let dir = std::env::temp_dir().join(format!(
            "krill-bot-items-dec-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let store = BotItemsStore::new(dir.join("items.db"));
        let bot_agy = BotId::new("bot-agy").unwrap();
        let bot_actor = BotItemActor::Bot {
            bot_id: bot_agy.clone(),
            session_id: None,
        };
        let owner = BotItemActor::Owner {
            device: Some("phone".to_owned()),
        };

        let item = store
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot_agy.clone(),
                    title: "Release rollout".to_owned(),
                    description: None,
                    done_means: Some("Rollout plan confirmed".to_owned()),
                    status: Some(BotItemStatus::NeedsYou),
                    links: Vec::new(),
                },
                &bot_actor,
                1_000,
            )
            .unwrap();

        let asked = store
            .update_item(
                &UpdateBotItemParams {
                    item_id: item.uid.clone(),
                    title: None,
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    comment: Some("Confirm rollout target and channels".to_owned()),
                    decisions: vec![
                        BotItemDecision {
                            question: "Target env?".to_owned(),
                            options: vec!["Staging".to_owned(), "Prod".to_owned()],
                            recommended: Some(0),
                            multi: false,
                        },
                        BotItemDecision {
                            question: "Notify channels?".to_owned(),
                            options: vec![
                                "Slack".to_owned(),
                                "Email".to_owned(),
                                "Changelog".to_owned(),
                            ],
                            recommended: Some(0),
                            multi: true,
                        },
                    ],
                    close_note: None,
                    snoozed_until_ms: FieldPatch::Unchanged,
                },
                &bot_actor,
                1_500,
            )
            .unwrap();
        let ask_event = asked
            .events
            .iter()
            .rev()
            .find(|e| !e.decisions.is_empty())
            .unwrap();
        assert!(!ask_event.decisions[0].multi);
        assert!(ask_event.decisions[1].multi);

        // MB-02: Matching by "<event_id>:1" targets the second decision, records it with its
        // question prefix, and keeps the item in NeedsYou without waking the bot until all
        // decisions on the active ask event are answered.
        let by_ev_idx = store
            .answer_item(
                &item.uid,
                None,
                Some(2),
                Some(&format!("{}:1", ask_event.id)),
                Some("req-partial-1"),
                &owner,
                2_000,
            )
            .unwrap();
        assert_eq!(by_ev_idx.answer_summary, "Notify channels? → Changelog");
        assert!(!by_ev_idx.should_wake_bot);
        assert_eq!(by_ev_idx.item.status, BotItemStatus::NeedsYou);

        // Re-submitting the same request_id is idempotent.
        let dup_partial = store
            .answer_item(
                &item.uid,
                None,
                Some(2),
                Some(&format!("{}:1", ask_event.id)),
                Some("req-partial-1"),
                &owner,
                2_050,
            )
            .unwrap();
        assert!(!dup_partial.should_wake_bot);
        assert_eq!(dup_partial.answer_summary, "already answered");
        assert_eq!(dup_partial.item.status, BotItemStatus::NeedsYou);

        // Updating the same decision ("1") still leaves "Target env?" unanswered -> stays NeedsYou.
        let by_num = store
            .answer_item(
                &item.uid,
                Some("Email"),
                Some(1),
                Some("1"),
                None,
                &owner,
                2_100,
            )
            .unwrap();
        assert_eq!(by_num.answer_summary, "Notify channels? → Email");
        assert!(!by_num.should_wake_bot);
        assert_eq!(by_num.item.status, BotItemStatus::NeedsYou);

        // Answering the remaining decision ("Target env?") completes all decisions, flips to
        // Working, wakes the bot ONCE, and returns the combined summary of all decisions.
        let by_question = store
            .answer_item(
                &item.uid,
                None,
                Some(1),
                Some("Target env?"),
                None,
                &owner,
                2_200,
            )
            .unwrap();
        assert!(by_question.should_wake_bot);
        assert_eq!(by_question.item.status, BotItemStatus::Working);
        assert_eq!(
            by_question.answer_summary,
            "Target env? → Prod\nNotify channels? → Email"
        );

        // Subsequent named follow-up comment once Working does not re-wake the bot and passes through cleanly.
        let named = store
            .answer_item(
                &item.uid,
                Some("Notify channels? → Slack, Email"),
                Some(0),
                Some("1"),
                None,
                &owner,
                2_300,
            )
            .unwrap();
        assert!(!named.should_wake_bot);
        assert_eq!(named.answer_summary, "Notify channels? → Slack, Email");

        // Legacy SQLite row without "multi" deserializes as multi: false.
        store
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE item_events SET decisions_json = ?1 WHERE id = ?2",
                    params![
                        r#"[{"question":"Legacy?","options":["A","B"],"recommended":0}]"#,
                        i64::try_from(ask_event.id).unwrap()
                    ],
                )
                .map_err(sql_err)?;
                Ok(())
            })
            .unwrap();
        let reloaded = store.get_item(&item.uid).unwrap().unwrap();
        let legacy_ev = reloaded
            .events
            .iter()
            .find(|e| e.id == ask_event.id)
            .unwrap();
        assert_eq!(legacy_ev.decisions[0].question, "Legacy?");
        assert!(!legacy_ev.decisions[0].multi);

        // Text-only needs_you item (no decisions) answered via answer_item wakes bot and moves to Working.
        let text_only = store
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot_agy.clone(),
                    title: "Refund policy wording".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    links: Vec::new(),
                },
                &bot_actor,
                3_000,
            )
            .unwrap();
        store
            .add_comment(
                &text_only.uid,
                &bot_actor,
                "Please confirm whether partial refund note is needed.",
                &[],
                3_100,
            )
            .unwrap();
        let text_ans = store
            .answer_item(
                &text_only.uid,
                Some("Keep 7-day clause and add 1 line for prorated refunds."),
                None,
                None,
                None,
                &owner,
                3_200,
            )
            .unwrap();
        assert!(text_ans.should_wake_bot);
        assert_eq!(text_ans.item.status, BotItemStatus::Working);
        assert_eq!(
            text_ans.answer_summary,
            "Keep 7-day clause and add 1 line for prorated refunds."
        );

        // MB-02: Option-index-only answers advance across unanswered decisions sequentially.
        let seq_item = store
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot_agy,
                    title: "Sequential multi-decision".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    links: Vec::new(),
                },
                &bot_actor,
                4_000,
            )
            .unwrap();
        store
            .update_item(
                &UpdateBotItemParams {
                    item_id: seq_item.uid.clone(),
                    title: None,
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    comment: Some("Pick both".to_owned()),
                    decisions: vec![
                        BotItemDecision {
                            question: "Region?".to_owned(),
                            options: vec!["US".to_owned(), "EU".to_owned()],
                            recommended: Some(0),
                            multi: false,
                        },
                        BotItemDecision {
                            question: "Tier?".to_owned(),
                            options: vec!["Free".to_owned(), "Pro".to_owned()],
                            recommended: Some(1),
                            multi: false,
                        },
                    ],
                    close_note: None,
                    snoozed_until_ms: FieldPatch::Unchanged,
                },
                &bot_actor,
                4_100,
            )
            .unwrap();
        let first_opt = store
            .answer_item(&seq_item.uid, None, Some(1), None, None, &owner, 4_200)
            .unwrap();
        assert!(!first_opt.should_wake_bot);
        assert_eq!(first_opt.item.status, BotItemStatus::NeedsYou);
        assert_eq!(first_opt.answer_summary, "Region? → EU");
        let second_opt = store
            .answer_item(&seq_item.uid, None, Some(1), None, None, &owner, 4_300)
            .unwrap();
        assert!(second_opt.should_wake_bot);
        assert_eq!(second_opt.item.status, BotItemStatus::Working);
        assert_eq!(second_opt.answer_summary, "Region? → EU\nTier? → Pro");

        // MB-09: Hyphenated "needs-you" parses consistently.
        assert_eq!(
            BotItemsStatusFilter::parse("needs-you"),
            Some(BotItemsStatusFilter::Status(BotItemStatus::NeedsYou))
        );
        assert_eq!(parse_status("needs-you"), BotItemStatus::NeedsYou);

        // MB-07/MB-39: CJK/emoji UTF-8 clamping never exceeds byte limits or panics.
        let cjk = "가".repeat(120); // 360 bytes
        let clamped_title = clamp_item_title(&cjk);
        assert!(clamped_title.len() <= BOT_ITEM_TITLE_MAX_BYTES);
        assert!(!clamped_title.is_empty());
        let clamped_ask = clamp_utf8_bytes_and_chars(&cjk, 120, BOT_ITEM_TITLE_MAX_BYTES);
        assert!(clamped_ask.len() <= BOT_ITEM_TITLE_MAX_BYTES);
        assert!(clamped_ask.chars().count() <= 120);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn r2m01_r2m02_markdown_decisions_backcompat_and_stale_decision_id_rejected() {
        let (dir, db_path) = temp_db("r2m01-02");
        let store = BotItemsStore::new(db_path);
        let bot_id = BotId::new("bot-md").unwrap();
        let bot_actor = BotItemActor::Bot {
            bot_id: bot_id.clone(),
            session_id: None,
        };
        let owner = BotItemActor::Owner {
            device: Some("mobile".to_owned()),
        };

        let item = store
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot_id.clone(),
                    title: "Markdown multi-decision".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    links: Vec::new(),
                },
                &bot_actor,
                1_000,
            )
            .unwrap();
        let updated = store
            .update_item(
                &UpdateBotItemParams {
                    item_id: item.uid.clone(),
                    title: None,
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    comment: Some("Confirm deploy settings".to_owned()),
                    decisions: vec![
                        BotItemDecision {
                            question: "**Deploy target?**".to_owned(),
                            options: vec!["Staging".to_owned(), "Prod".to_owned()],
                            recommended: Some(0),
                            multi: false,
                        },
                        BotItemDecision {
                            question: "# `Region?`".to_owned(),
                            options: vec!["US".to_owned(), "EU".to_owned()],
                            recommended: Some(0),
                            multi: false,
                        },
                    ],
                    close_note: None,
                    snoozed_until_ms: FieldPatch::Unchanged,
                },
                &bot_actor,
                1_100,
            )
            .unwrap();
        let ask_ev_id = updated
            .events
            .iter()
            .rev()
            .find(|e| !e.decisions.is_empty())
            .unwrap()
            .id;

        // R2M-02: stale or invalid decision_id on multi-decision NeedsYou MUST return InvalidInput
        // instead of falling through to freeform and waking the bot.
        let stale_err = store
            .answer_item(
                &item.uid,
                Some("Staging"),
                Some(0),
                Some("99999:0"),
                None,
                &owner,
                1_150,
            )
            .unwrap_err();
        assert_eq!(stale_err.kind(), io::ErrorKind::InvalidInput);
        let invalid_err = store
            .answer_item(
                &item.uid,
                Some("Staging"),
                None,
                Some("nonexistent-decision"),
                None,
                &owner,
                1_160,
            )
            .unwrap_err();
        assert_eq!(invalid_err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            store.get_item(&item.uid).unwrap().unwrap().status,
            BotItemStatus::NeedsYou
        );

        // R2M-01: Mobile/Desktop normalized prefix ("Deploy target? → Staging") matches Decision 0
        // without double-prefixing and keeps status NeedsYou until Decision 1 is answered.
        let ans1 = store
            .answer_item(
                &item.uid,
                Some("Deploy target? → Staging"),
                Some(0),
                Some(&format!("{ask_ev_id}:0")),
                None,
                &owner,
                1_200,
            )
            .unwrap();
        assert!(!ans1.should_wake_bot);
        assert_eq!(ans1.item.status, BotItemStatus::NeedsYou);
        assert_eq!(ans1.answer_summary, "Deploy target? → Staging");
        assert!(
            !ans1
                .item
                .events
                .last()
                .unwrap()
                .text
                .contains("**Deploy target?**"),
            "must not double-prefix with raw Markdown"
        );

        // Answering Decision 1 via option_index uses stripped question line and completes all decisions.
        let ans2 = store
            .answer_item(
                &item.uid,
                None,
                Some(1),
                Some(&format!("{ask_ev_id}:1")),
                None,
                &owner,
                1_300,
            )
            .unwrap();
        assert!(ans2.should_wake_bot);
        assert_eq!(ans2.item.status, BotItemStatus::Working);
        assert_eq!(
            ans2.answer_summary,
            "Deploy target? → Staging\nRegion? → EU"
        );

        // R2M-01 back-compat: historical raw-Markdown prefix is still recognized on read.
        let hist = store
            .create_item(
                &CreateBotItemParams {
                    bot_id,
                    title: "Historical raw markdown".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    links: Vec::new(),
                },
                &bot_actor,
                2_000,
            )
            .unwrap();
        store
            .update_item(
                &UpdateBotItemParams {
                    item_id: hist.uid.clone(),
                    title: None,
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::NeedsYou),
                    comment: Some("Pick two".to_owned()),
                    decisions: vec![
                        BotItemDecision {
                            question: "**Env?**".to_owned(),
                            options: vec!["Dev".to_owned(), "Prod".to_owned()],
                            recommended: None,
                            multi: false,
                        },
                        BotItemDecision {
                            question: "**Mode?**".to_owned(),
                            options: vec!["Fast".to_owned(), "Safe".to_owned()],
                            recommended: None,
                            multi: false,
                        },
                    ],
                    close_note: None,
                    snoozed_until_ms: FieldPatch::Unchanged,
                },
                &bot_actor,
                2_100,
            )
            .unwrap();
        let raw_ans = store
            .answer_item(
                &hist.uid,
                Some("**Env?** → Prod"),
                None,
                None,
                None,
                &owner,
                2_200,
            )
            .unwrap();
        assert!(!raw_ans.should_wake_bot);
        assert_eq!(raw_ans.item.status, BotItemStatus::NeedsYou);

        let norm_ans = store
            .answer_item(
                &hist.uid,
                Some("Mode? → Safe"),
                None,
                None,
                None,
                &owner,
                2_300,
            )
            .unwrap();
        assert!(norm_ans.should_wake_bot);
        assert_eq!(norm_ans.item.status, BotItemStatus::Working);
        assert_eq!(norm_ans.answer_summary, "Env? → Prod\nMode? → Safe");

        fs::remove_dir_all(&dir).ok();
    }
}
