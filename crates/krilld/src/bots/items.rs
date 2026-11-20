// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Lazily opened SQLite store (`<state_dir>/bots/bot-items.db`) for Krill Bot deliverables,
//! links, and immutable audit events.
//!
//! Zero SQLite connections are opened until the first bot item operation. When opened, uses WAL,
//! `busy_timeout = 5000`, `cache_size = -1000` (~1 MiB), and `foreign_keys = ON`.

use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use krill_proto::bots::{
    BOT_ITEM_COMMENT_MAX_BYTES, BOT_ITEM_DESCRIPTION_MAX_BYTES, BOT_ITEM_TITLE_MAX_BYTES, BotId,
    BotItem, BotItemActor, BotItemDecision, BotItemEvent, BotItemEventKind, BotItemLink,
    BotItemStatus, CreateBotItemParams, UpdateBotItemParams,
};
use krill_proto::os::fs::DirBuilderExt as _;
use rusqlite::{Connection, OptionalExtension as _, params};

use crate::session::lock;

static ITEM_UID_SEQ: AtomicU64 = AtomicU64::new(1);

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
                let mut item = row.map_err(sql_err)?;
                item.links = load_links(conn, &item.uid)?;
                item.events = load_events(conn, &item.uid)?;
                items.push(item);
            }
            Ok(items)
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
            tx.execute(
                "INSERT INTO items \
                 (uid, number, owner_bot_id, title, description, done_means, status, close_note, \
                  snoozed_until_ms, version, last_activity_at_ms, created_at_ms, updated_at_ms, closed_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, 1, ?8, ?8, ?8, ?9)",
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
            if let Some(t) = next_title
                && t != item.title
            {
                item.title = t;
                edited = true;
            }
            if let Some(d) = next_desc
                && d != item.description
            {
                item.description = d;
                edited = true;
            }
            if let Some(dm) = next_done
                && dm != item.done_means
            {
                item.done_means = dm;
                edited = true;
            }
            if let Some(cn) = next_close_note {
                item.close_note = cn;
                edited = true;
            }

            let status_changed = params_in.status.filter(|&s| s != item.status);
            if let Some(new_status) = status_changed {
                item.status = new_status;
                if new_status.is_open() {
                    item.closed_at_ms = None;
                } else {
                    item.closed_at_ms = Some(now_ms);
                }
            }

            let prev_snooze = item.snoozed_until_ms;
            params_in
                .snoozed_until_ms
                .clone()
                .apply(&mut item.snoozed_until_ms);
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

            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            tx.execute(
                "UPDATE items SET title = ?1, description = ?2, done_means = ?3, status = ?4, \
                 close_note = ?5, snoozed_until_ms = ?6, version = ?7, last_activity_at_ms = ?8, \
                 updated_at_ms = ?8, closed_at_ms = ?9 WHERE uid = ?10",
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
                    uid,
                ],
            )
            .map_err(sql_err)?;

            if let Some(new_status) = status_changed {
                let text = params_in
                    .comment
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map_or_else(
                        || format!("status -> {}", new_status.as_str()),
                        ToOwned::to_owned,
                    );
                insert_event(
                    &tx,
                    &uid,
                    BotItemEventKind::Status,
                    actor,
                    &text,
                    &params_in.decisions,
                    now_ms,
                )?;
            } else if let Some(comment) = params_in
                .comment
                .as_deref()
                .filter(|s| !s.trim().is_empty())
            {
                insert_event(
                    &tx,
                    &uid,
                    BotItemEventKind::Commented,
                    actor,
                    comment,
                    &params_in.decisions,
                    now_ms,
                )?;
            } else if edited {
                insert_event(
                    &tx,
                    &uid,
                    BotItemEventKind::Edited,
                    actor,
                    "updated item",
                    &params_in.decisions,
                    now_ms,
                )?;
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
            insert_event(
                &tx,
                &uid,
                BotItemEventKind::Commented,
                actor,
                &cleaned,
                decisions,
                now_ms,
            )?;
            tx.execute(
                "UPDATE items SET version = version + 1, last_activity_at_ms = ?1, updated_at_ms = ?1 WHERE uid = ?2",
                params![u64_to_i64(now_ms), uid],
            )
            .map_err(sql_err)?;
            tx.commit().map_err(sql_err)?;
            load_item_by_uid(conn, &uid)?
                .ok_or_else(|| io::Error::other("commented bot item vanished"))
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

    /// Deletes all items (and cascaded links/events) owned by `bot_id`.
    pub fn delete_bot_items(&self, bot_id: &BotId) -> io::Result<usize> {
        if !self.db_path.exists() && !self.is_open() {
            return Ok(0);
        }
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM items WHERE owner_bot_id = ?1",
                params![bot_id.as_str()],
            )
            .map_err(sql_err)
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
         CREATE INDEX IF NOT EXISTS idx_item_events_uid ON item_events(item_uid);",
    )
    .map_err(sql_err)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).ok();
    }

    Ok(conn)
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
        if matches.len() == 1 {
            return Ok(matches.into_iter().next());
        }
    }
    Ok(None)
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
}
