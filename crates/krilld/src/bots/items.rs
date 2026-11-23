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
    /// Parses a mobile/CLI status filter token (`needs_you`, `working`, `planned`, `done`,
    /// `canceled`/`cancelled`, `closed`, `snoozed`).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "planned" => Some(Self::Status(BotItemStatus::Planned)),
            "working" => Some(Self::Status(BotItemStatus::Working)),
            "needs_you" => Some(Self::Status(BotItemStatus::NeedsYou)),
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
        _decision_id: Option<&str>,
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

            let resolved_option = if let Some(idx) = option_index {
                let idx_usize = usize::try_from(idx).unwrap_or(usize::MAX);
                let latest_decisions = item
                    .events
                    .iter()
                    .rev()
                    .find(|e| !e.decisions.is_empty())
                    .map(|e| e.decisions.as_slice());
                let opt_label = latest_decisions
                    .and_then(|decs| decs.first())
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

            let answer_summary = match (resolved_option, text_trimmed) {
                (Some(opt), Some(txt)) => format!("{opt} — {txt}"),
                (Some(opt), None) => opt,
                (None, Some(txt)) => txt,
                (None, None) => {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty answer"));
                }
            };

            let answer_id = req_id_trimmed.unwrap_or_else(|| {
                let seq = ITEM_UID_SEQ.fetch_add(1, Ordering::Relaxed);
                format!("ans-{now_ms}-{seq}")
            });
            let should_wake_bot = mail_state.as_deref() == Some("owed");

            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            insert_event(
                &tx,
                &uid,
                BotItemEventKind::Commented,
                actor,
                &answer_summary,
                &[],
                now_ms,
            )?;
            let comment_row_id = tx.last_insert_rowid();
            let next_status = if item.status == BotItemStatus::NeedsYou {
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
                 mail_state = 'answered', answer_id = ?2, others_last_event_id = ?3, \
                 version = version + 1, last_activity_at_ms = ?4, updated_at_ms = ?4 \
                 WHERE uid = ?5",
                params![
                    next_status.as_str(),
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
                answer_summary,
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
        "needs_you" => BotItemStatus::NeedsYou,
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
}
