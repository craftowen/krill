//! Read-only `OpenCode` SQLite adapter. Schema reference: Orca ai-vault scanner,
//! Copyright (c) 2026 Lovecast Inc., MIT. No provider CLI, migrations or exports run.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, limits::Limit};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const MAX_SESSIONS: usize = 256;
const MAX_MESSAGES: usize = 2048;
const MAX_PARTS: usize = 4096;
const MAX_PREFIX_ROWS: usize = 100_000;
const MAX_BYTES: usize = 8 * 1024 * 1024;

pub(super) struct Session {
    pub id: String,
    pub cwd: String,
    pub title: String,
    pub modified: u64,
    pub created: u64,
    pub messages: Vec<Value>,
    pub message_count: u64,
    pub prefix_digest: [u8; 32],
    pub message_digests: Vec<[u8; 32]>,
    pub verified_end: Option<[u8; 32]>,
}

pub(super) struct Batch {
    pub sessions: Vec<Session>,
    pub truncated: bool,
    pub skipped: bool,
    pub bytes: usize,
}

pub(super) fn candidates(root: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        root.join("opencode.db"),
        root.join("data/opencode/opencode.db"),
        root.join("data/opencode.db"),
    ];
    if root.extension().is_some_and(|ext| ext == "db") {
        paths.insert(0, root.into());
    }
    if root.ends_with(".config/opencode")
        && let Some(home) = root.parent().and_then(Path::parent)
    {
        paths.push(home.join(".local/share/opencode/opencode.db"));
    }
    paths.into_iter().filter(|p| p.is_file()).collect()
}

fn columns(db: &Connection, table: &str) -> rusqlite::Result<BTreeSet<String>> {
    // Names are internal constants, and views are deliberately not accepted.
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(BTreeSet::new());
    }
    db.prepare(&format!("PRAGMA table_info(\"{table}\")"))?
        .query_map([], |row| row.get(1))?
        .collect()
}

fn needs(columns: &BTreeSet<String>, names: &[&str]) -> rusqlite::Result<()> {
    if names.iter().all(|name| columns.contains(*name)) {
        Ok(())
    } else {
        Err(rusqlite::Error::InvalidQuery)
    }
}

/// The read transaction sees committed WAL content without checkpointing or creating a DB.
/// Busy waits, VM work, cell size, row count and decoded bytes all have explicit bounds.
pub(super) fn read(path: &Path, id: Option<&str>, messages: bool) -> rusqlite::Result<Batch> {
    read_window(path, id, messages, 0, None, None)
}

pub(super) fn read_page(path: &Path, id: &str, offset: u64) -> rusqlite::Result<Batch> {
    read_page_verified(path, id, offset, None)
}

pub(super) fn read_page_verified(
    path: &Path,
    id: &str,
    offset: u64,
    end: Option<u64>,
) -> rusqlite::Result<Batch> {
    read_window(path, Some(id), true, offset, Some(128), end)
}

fn read_window(
    path: &Path,
    id: Option<&str>,
    messages: bool,
    offset: u64,
    page_size: Option<usize>,
    verify_end: Option<u64>,
) -> rusqlite::Result<Batch> {
    let mut db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_millis(50))?;
    db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 1024 * 1024)?;
    let deadline = Instant::now() + Duration::from_millis(200);
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
    db.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; BEGIN;")?;
    let schema = columns(&db, "session")?;
    needs(&schema, &["id", "directory"])?;
    let title = if schema.contains("title") {
        "title"
    } else {
        "''"
    };
    let modified = if schema.contains("time_updated") {
        "time_updated"
    } else {
        "0"
    };
    let created = if schema.contains("time_created") {
        "time_created"
    } else {
        "0"
    };
    let mut batch = Batch {
        sessions: Vec::new(),
        truncated: false,
        skipped: false,
        bytes: 0,
    };
    let mut stmt = db.prepare(&format!(
        "SELECT id,directory,{title},{modified},{created} FROM session WHERE (?1 IS NULL OR id=?1) ORDER BY 4 DESC,id LIMIT ?2"
    ))?;
    let limit = i64::try_from(MAX_SESSIONS.saturating_add(1)).unwrap_or(257);
    let mut rows = stmt.query(rusqlite::params![id, limit])?;
    while let Some(row) = rows.next()? {
        if batch.sessions.len() >= MAX_SESSIONS
            || batch.bytes >= MAX_BYTES
            || Instant::now() >= deadline
        {
            batch.truncated = true;
            break;
        }
        let id: String = row.get(0)?;
        let cwd: String = row.get(1)?;
        let title: Option<String> = row.get(2)?;
        let millis: Option<i64> = row.get(3)?;
        let created: Option<i64> = row.get(4)?;
        let title = title.unwrap_or_default();
        batch.bytes = batch
            .bytes
            .saturating_add(id.len())
            .saturating_add(cwd.len())
            .saturating_add(title.len());
        batch.sessions.push(Session {
            id,
            cwd,
            title,
            modified: u64::try_from(millis.unwrap_or(0)).unwrap_or(0) / 1000,
            created: u64::try_from(created.unwrap_or(0)).unwrap_or(0) / 1000,
            messages: Vec::new(),
            message_count: 0,
            prefix_digest: Sha256::digest([]).into(),
            message_digests: Vec::new(),
            verified_end: verify_end
                .filter(|n| *n == 0)
                .map(|_| Sha256::digest([]).into()),
        });
    }
    drop(rows);
    drop(stmt);
    if messages && !batch.sessions.is_empty() {
        let mut schema = columns(&db, "message")?;
        let table = if schema.is_empty() {
            schema = columns(&db, "session_message")?;
            "session_message"
        } else {
            "message"
        };
        needs(&schema, &["id", "session_id", "data"])?;
        let parts = columns(&db, "part")?;
        let with_parts = !parts.is_empty();
        if with_parts {
            needs(&parts, &["message_id", "data"])?;
        }
        let order = if schema.contains("time_created") {
            "time_created,id"
        } else {
            "id"
        };
        let part_order = if parts.contains("time_created") {
            "time_created,rowid"
        } else {
            "rowid"
        };
        let mut message_count = 0usize;
        let mut part_count = 0usize;
        let mut page_part_count = 0usize;
        for session in &mut batch.sessions {
            if id.is_some() {
                session.message_count = u64::try_from(db.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE session_id=?1"),
                    [&session.id],
                    |row| row.get::<_, i64>(0),
                )?)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            }
            let mut stmt = db.prepare(&format!(
                "SELECT id,data FROM {table} WHERE session_id=?1 ORDER BY {order} LIMIT ?2"
            ))?;
            let mut rows = stmt.query(rusqlite::params![
                session.id,
                i64::try_from(page_size.map_or(MAX_MESSAGES.saturating_add(1), |size| {
                    usize::try_from(
                        offset
                            .saturating_add(u64::try_from(size).unwrap_or(128))
                            .max(verify_end.unwrap_or(0)),
                    )
                    .unwrap_or(usize::MAX)
                }))
                .map_err(|_| rusqlite::Error::InvalidQuery)?
            ])?;
            let mut digest = Sha256::new();
            let mut position = 0u64;
            while let Some(row) = rows.next()? {
                if message_count
                    >= if page_size.is_some() {
                        MAX_PREFIX_ROWS
                    } else {
                        MAX_MESSAGES
                    }
                    || batch.bytes >= MAX_BYTES
                    || Instant::now() >= deadline
                {
                    batch.truncated = true;
                    break;
                }
                message_count = message_count.saturating_add(1);
                let message_id: String = row.get(0)?;
                let data: String = row.get(1)?;
                batch.bytes = batch
                    .bytes
                    .saturating_add(data.len())
                    .saturating_add(message_id.len());
                digest.update(b"message");
                hash_field(&mut digest, message_id.as_bytes());
                hash_field(&mut digest, data.as_bytes());
                let Ok(mut value) = serde_json::from_str::<Value>(&data) else {
                    batch.skipped = true;
                    continue;
                };
                let Some(object) = value.as_object_mut() else {
                    batch.skipped = true;
                    continue;
                };
                object.insert("id".into(), Value::String(message_id.clone()));
                if with_parts {
                    let mut stmt = db.prepare(&format!(
                        "SELECT data FROM part WHERE message_id=?1 ORDER BY {part_order} LIMIT ?2"
                    ))?;
                    let mut rows = stmt.query(rusqlite::params![
                        message_id,
                        i64::try_from(MAX_PARTS.saturating_add(1)).unwrap_or(4097)
                    ])?;
                    let mut parts = Vec::new();
                    while let Some(row) = rows.next()? {
                        let visible = position >= offset
                            && page_size.is_none_or(|size| {
                                position < offset.saturating_add(u64::try_from(size).unwrap_or(128))
                            });
                        if parts.len() >= MAX_PARTS
                            || part_count
                                >= if page_size.is_some() {
                                    MAX_PREFIX_ROWS
                                } else {
                                    MAX_PARTS
                                }
                            || (visible && page_part_count >= MAX_PARTS)
                            || batch.bytes >= MAX_BYTES
                            || Instant::now() >= deadline
                        {
                            batch.truncated = true;
                            break;
                        }
                        part_count = part_count.saturating_add(1);
                        if visible {
                            page_part_count = page_part_count.saturating_add(1);
                        }
                        let data: String = row.get(0)?;
                        batch.bytes = batch.bytes.saturating_add(data.len());
                        digest.update(b"part");
                        hash_field(&mut digest, data.as_bytes());
                        match serde_json::from_str::<Value>(&data) {
                            Ok(part) => parts.push(part),
                            Err(_) => batch.skipped = true,
                        }
                    }
                    object.insert("parts".into(), Value::Array(parts));
                }
                position = position.saturating_add(1);
                let proof: [u8; 32] = digest.clone().finalize().into();
                if position <= offset {
                    session.prefix_digest = proof;
                } else if page_size.is_none_or(|size| {
                    position <= offset.saturating_add(u64::try_from(size).unwrap_or(128))
                }) {
                    session.messages.push(value);
                    session.message_digests.push(proof);
                }
                if verify_end == Some(position) {
                    session.verified_end = Some(proof);
                }
            }
        }
    }
    batch.truncated |= batch.bytes > MAX_BYTES || Instant::now() >= deadline;
    Ok(batch)
}

fn hash_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    digest.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_history::{chat, vault};
    use krill_proto::agent_history::{SearchQuery, VaultAgent};

    mod prefix_budget;

    fn fixture(name: &str) -> (PathBuf, PathBuf, Connection) {
        let root = std::env::temp_dir().join(format!(
            "krill-opencode-sqlite-{name}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("data/opencode")).unwrap();
        let path = root.join("data/opencode/opencode.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE session(id TEXT PRIMARY KEY,directory TEXT,title TEXT,time_created INTEGER,time_updated INTEGER);
            CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,data TEXT,time_created INTEGER);
            CREATE TABLE part(id TEXT PRIMARY KEY,message_id TEXT,data TEXT,time_created INTEGER);
            INSERT INTO session VALUES('ses_sqlite_001','/workspace/sqlite','SQLite fixture',1000,2000);
            INSERT INTO message VALUES('msg1','ses_sqlite_001','{\"role\":\"user\"}',1000);
            INSERT INTO message VALUES('msg2','ses_sqlite_001','{\"role\":\"assistant\"}',2000);
            INSERT INTO part VALUES('p1','msg1','{\"type\":\"text\",\"text\":\"alpha in first turn\"}',1000);
            INSERT INTO part VALUES('p2','msg2','{\"type\":\"text\",\"text\":\"beta in second turn\"}',2000);").unwrap();
        (root, path, db)
    }

    #[cfg(unix)]
    #[test]
    fn wal_only_transcript_search_and_chat_binding_are_readonly_and_incremental() {
        let (root, path, writer) = fixture("wal");
        let before_db = std::fs::read(&path).unwrap();
        let wal = path.with_extension("db-wal");
        let before_wal = std::fs::read(&wal).unwrap();
        let roots = vault::VaultRoots {
            opencode: vec![root.clone()],
            ..Default::default()
        };
        let result = vault::search_in(
            &roots,
            &SearchQuery {
                query: "alpha beta".into(),
                ..Default::default()
            },
        );
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits.first().unwrap().agent, VaultAgent::OpenCode);
        assert!(!result.skipped && !result.truncated);
        let mut accounts = crate::usage::accounts::AccountsFile::default();
        accounts
            .accounts
            .push(crate::usage::accounts::StoredAccount {
                id: "opencode-isolated".into(),
                provider: "opencode".into(),
                label: "SQLite fixture".into(),
                email: None,
                profile_dir: root.display().to_string(),
            });
        let mut target = result.hits.first().unwrap().target();
        assert_eq!(
            vault::profile_dir_for_vault_target_in(&accounts, &target, None, None),
            Some(root.clone())
        );
        target.cwd.clear();
        assert_eq!(
            vault::profile_dir_for_vault_target_in(&accounts, &target, None, None),
            Some(root.clone())
        );
        let stored = crate::agent::control::StoredLaunch {
            profile: "opencode".into(),
            permission: krill_proto::agent::Permission::Manual,
            session_id: Some("ses_sqlite_001".into()),
            transcript_path: None,
            cwd: b"/workspace/sqlite".to_vec(),
            hibernation_blocked: false,
            account: None,
        };
        let page = chat::poll_session_in(&roots, Some(&stored), b"", 0);
        assert!(page.bound);
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items.first().unwrap().text, "alpha in first turn");
        assert_eq!(page.items.get(1).unwrap().text, "beta in second turn");
        let idle = chat::poll_transcript_file(
            VaultAgent::OpenCode,
            "ses_sqlite_001",
            &path,
            page.next_offset,
        );
        assert!(idle.bound && !idle.reset && idle.items.is_empty());
        let (latest, start) = chat::latest_page(page.clone(), None).unwrap();
        assert_eq!(start, 0);
        assert_eq!(latest.items, page.items);
        assert_eq!(
            chat::replay_page(page.clone(), 0, page.next_offset)
                .unwrap()
                .items,
            page.items
        );
        assert_eq!(std::fs::read(&path).unwrap(), before_db);
        assert_eq!(std::fs::read(&wal).unwrap(), before_wal);
        writer
            .execute(
                "UPDATE part SET data=?1 WHERE id='p2'",
                [r#"{"type":"text","text":"gamma changed"}"#],
            )
            .unwrap();
        let changed = chat::poll_transcript_file(
            VaultAgent::OpenCode,
            "ses_sqlite_001",
            &path,
            page.next_offset,
        );
        assert!(changed.bound && changed.reset);
        assert_eq!(changed.items.get(1).unwrap().text, "gamma changed");
        drop(writer);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn absent_or_unsupported_database_is_not_created_or_migrated() {
        let (root, path, db) = fixture("schema");
        let missing = root.join("missing.db");
        assert!(read(&missing, None, true).is_err());
        assert!(!missing.exists());
        db.execute_batch("DROP TABLE message; CREATE TABLE message(unrecognized TEXT);")
            .unwrap();
        let before = std::fs::read(path.with_extension("db-wal")).unwrap();
        assert!(read(&path, None, true).is_err());
        assert_eq!(
            std::fs::read(path.with_extension("db-wal")).unwrap(),
            before
        );
        let result = vault::search_in(
            &vault::VaultRoots {
                opencode: vec![root.clone()],
                ..Default::default()
            },
            &SearchQuery::default(),
        );
        assert!(result.skipped);
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn minimal_schema_has_no_optional_columns_and_session_ids_are_bound_parameters() {
        let (root, path, db) = fixture("minimal");
        db.execute_batch("DROP TABLE session; CREATE TABLE session(id TEXT PRIMARY KEY,directory TEXT); INSERT INTO session VALUES('ses_sqlite_001','/workspace/sqlite');").unwrap();
        let batch = read(&path, Some("ses_sqlite_001"), true).unwrap();
        assert_eq!(batch.sessions.len(), 1);
        assert!(batch.sessions.first().unwrap().title.is_empty());
        assert!(
            read(&path, Some("' OR 1=1 --"), false)
                .unwrap()
                .sessions
                .is_empty()
        );
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn large_transcript_pages_reach_end_and_then_show_new_appends() {
        let (root, path, mut db) = fixture("bounds");
        let tx = db.transaction().unwrap();
        for index in 0..MAX_MESSAGES {
            tx.execute(
                "INSERT INTO message VALUES(?1,'ses_sqlite_001',?2,3000)",
                rusqlite::params![
                    format!("extra{index:05}"),
                    r#"{"role":"user","content":"bounded"}"#
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let batch = read(&path, Some("ses_sqlite_001"), true).unwrap();
        assert!(batch.truncated);
        let mut page = chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, 0);
        let mut seen = page.items.len();
        while page.next_offset < (1 << 52) {
            let offset = page.next_offset;
            page =
                chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, offset);
            assert!(page.bound && !page.reset && page.next_offset > offset);
            seen += page.items.len();
        }
        assert_eq!(seen, MAX_MESSAGES + 2);
        db.execute(
            "INSERT INTO message VALUES('last','ses_sqlite_001',?1,4000)",
            [r#"{"role":"user","content":"after-boundary"}"#],
        )
        .unwrap();
        let appended = chat::poll_transcript_file(
            VaultAgent::OpenCode,
            "ses_sqlite_001",
            &path,
            page.next_offset,
        );
        assert!(appended.bound && !appended.reset);
        assert_eq!(appended.items.len(), 1);
        assert_eq!(appended.items.first().unwrap().text, "after-boundary");
        let (latest, start) = chat::latest_page(appended.clone(), None).unwrap();
        assert!(start > 0);
        assert_eq!(latest.items.last().unwrap().text, "after-boundary");
        assert_eq!(
            chat::replay_page(appended.clone(), start, appended.next_offset)
                .unwrap()
                .items,
            latest.items
        );
        db.execute("UPDATE message SET data='invalid JSON' WHERE id='msg1'", [])
            .unwrap();
        let failed = chat::poll_transcript_file(
            VaultAgent::OpenCode,
            "ses_sqlite_001",
            &path,
            appended.next_offset,
        );
        assert!(!failed.bound && failed.reset && failed.next_offset == 0);
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn independent_2307_rows_order_history_reset_wal_rollback_and_rebind() {
        let (root, path, mut db) = fixture("independent-large");
        let tx = db.transaction().unwrap();
        tx.execute_batch("DELETE FROM part; DELETE FROM message;")
            .unwrap();
        for n in 0..2307 {
            tx.execute(
                "INSERT INTO message VALUES(?1,'ses_sqlite_001',?2,?3)",
                rusqlite::params![
                    format!("m{n:06}"),
                    format!(r#"{{"role":"user","content":"row-{n}"}}"#),
                    n
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let mut cursor = 0;
        let mut texts = Vec::new();
        let mut last;
        loop {
            last =
                chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, cursor);
            assert!(last.bound && !last.reset);
            texts.extend(last.items.iter().map(|i| i.text.clone()));
            assert!(last.next_offset > cursor);
            cursor = last.next_offset;
            if cursor >= (1 << 52) {
                break;
            }
        }
        assert_eq!(
            texts,
            (0..2307).map(|n| format!("row-{n}")).collect::<Vec<_>>()
        );
        let (latest, start) = chat::latest_page(last.clone(), None).unwrap();
        assert_eq!(start, 2179);
        assert_eq!(
            latest
                .items
                .iter()
                .map(|i| i.text.clone())
                .collect::<Vec<_>>(),
            texts[2179..]
        );
        let (older, older_start) = chat::latest_page(last.clone(), Some(start)).unwrap();
        assert_eq!(older_start, 2051);
        assert_eq!(
            older
                .items
                .iter()
                .map(|i| i.text.clone())
                .collect::<Vec<_>>(),
            texts[2051..2179]
        );
        assert_eq!(
            chat::replay_page(last, older_start, start).unwrap().items,
            older.items
        );
        let before_db = std::fs::read(&path).unwrap();
        let before_wal = std::fs::read(path.with_extension("db-wal")).unwrap();
        db.execute_batch("BEGIN IMMEDIATE; UPDATE message SET data='invalid' WHERE id='m000000';")
            .unwrap();
        let idle =
            chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, cursor);
        assert!(idle.bound && idle.items.is_empty());
        db.execute_batch("ROLLBACK;").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before_db);
        assert_eq!(
            std::fs::read(path.with_extension("db-wal")).unwrap(),
            before_wal
        );
        db.execute("DELETE FROM message WHERE time_created>130", [])
            .unwrap();
        let reset =
            chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, cursor);
        assert!(reset.bound && reset.reset && reset.items.len() == 128);
        assert_eq!(reset.items.first().unwrap().text, "row-0");
        let resumed = chat::poll_transcript_file(
            VaultAgent::OpenCode,
            "ses_sqlite_001",
            &path,
            reset.next_offset,
        );
        assert!(resumed.bound && !resumed.reset && resumed.items.len() == 3);
        assert_eq!(resumed.items.last().unwrap().text, "row-130");
        db.execute("DELETE FROM session", []).unwrap();
        let missing =
            chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, cursor);
        assert!(!missing.bound && missing.reset);
        db.execute(
            "INSERT INTO session VALUES('ses_sqlite_001','/workspace/sqlite','rebound',1,1)",
            [],
        )
        .unwrap();
        let rebound = chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, 0);
        assert!(rebound.bound && !rebound.items.is_empty());
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn independent_append_with_equal_timestamp_lower_id_must_not_lose_new_turn() {
        let (root, path, db) = fixture("independent-tied-append");
        let initial = chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, 0);
        db.execute(
            "INSERT INTO message VALUES('msg0','ses_sqlite_001',?1,2000)",
            [r#"{"role":"user","content":"new tied turn"}"#],
        )
        .unwrap();
        let next = chat::poll_transcript_file(
            VaultAgent::OpenCode,
            "ses_sqlite_001",
            &path,
            initial.next_offset,
        );
        let observed = next
            .items
            .iter()
            .map(|i| i.text.clone())
            .collect::<Vec<_>>();
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
        assert!(next.bound && next.reset);
        assert!(observed.contains(&"new tied turn".to_owned()));
        assert!(
            next.reset || observed.contains(&"new tied turn".to_owned()),
            "new row lost without reset: {observed:?}"
        );
    }

    #[test]
    fn independent_edit_and_append_must_refresh_previously_emitted_turn() {
        let (root, path, db) = fixture("independent-edit-append");
        let initial = chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, 0);
        db.execute_batch(r#"BEGIN; UPDATE part SET data='{"type":"text","text":"updated streaming response"}' WHERE id='p2'; INSERT INTO message VALUES('msg3','ses_sqlite_001','{"role":"user","content":"next turn"}',3000); COMMIT;"#).unwrap();
        let next = chat::poll_transcript_file(
            VaultAgent::OpenCode,
            "ses_sqlite_001",
            &path,
            initial.next_offset,
        );
        let observed = next
            .items
            .iter()
            .map(|i| i.text.clone())
            .collect::<Vec<_>>();
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
        assert!(next.bound && next.reset);
        assert!(observed.contains(&"updated streaming response".to_owned()));
        assert!(observed.contains(&"next turn".to_owned()));
        assert!(
            next.reset || observed.contains(&"updated streaming response".to_owned()),
            "edit lost without reset: {observed:?}"
        );
    }
    #[test]
    fn partial_live_cursor_verifies_old_content_and_order_before_continuing() {
        let (root, path, mut db) = fixture("partial-prefix");
        let tx = db.transaction().unwrap();
        for n in 0..300 {
            tx.execute(
                "INSERT INTO message VALUES(?1,'ses_sqlite_001',?2,3000)",
                rusqlite::params![format!("m{n:06}"), r#"{"role":"user","content":"tail"}"#],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let first = chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, 0);
        assert!(first.bound && !first.reset && first.next_offset < (1 << 52));
        assert_eq!(first.items.len(), 128);
        let partial_replay = chat::replay_page(first.clone(), 100, first.next_offset).unwrap();
        assert!(partial_replay.bound && !partial_replay.reset);
        assert_eq!(partial_replay.items, first.items[100..]);
        assert_eq!(partial_replay.next_offset, first.next_offset);
        db.execute_batch(r#"BEGIN; UPDATE part SET data='{"type":"text","text":"changed prefix"}' WHERE id='p1';
            INSERT INTO message VALUES('new-last','ses_sqlite_001','{"role":"user","content":"append"}',4000); COMMIT;"#).unwrap();
        let refreshed = chat::poll_transcript_file(
            VaultAgent::OpenCode,
            "ses_sqlite_001",
            &path,
            first.next_offset,
        );
        assert!(refreshed.bound && refreshed.reset);
        assert_eq!(refreshed.items.first().unwrap().text, "changed prefix");
        let continuation = chat::poll_transcript_file(
            VaultAgent::OpenCode,
            "ses_sqlite_001",
            &path,
            refreshed.next_offset,
        );
        assert!(continuation.bound && !continuation.reset && continuation.items.len() == 128);
        let stale_replay = chat::replay_page(first.clone(), 0, first.next_offset).unwrap();
        assert!(!stale_replay.bound && stale_replay.reset);
        let unknown = chat::poll_transcript_file(
            VaultAgent::OpenCode,
            "ses_sqlite_001",
            &path,
            (1 << 52) | 0x7b,
        );
        assert!(unknown.bound && unknown.reset && unknown.items.len() == 128);
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_page_and_cell_refuse_successful_binding() {
        let (root, path, mut db) = fixture("explicit-budget");
        let tx = db.transaction().unwrap();
        for n in 0..10 {
            let content = format!(r#"{{"role":"user","content":"{}"}}"#, "x".repeat(900_000));
            tx.execute(
                "INSERT INTO message VALUES(?1,'ses_sqlite_001',?2,3000)",
                rusqlite::params![format!("large{n}"), content],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let too_big = chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, 0);
        assert!(!too_big.bound && too_big.next_offset == 0);
        db.execute(
            "UPDATE message SET data=?1 WHERE id='msg1'",
            ["x".repeat(1024 * 1024 + 1)],
        )
        .unwrap();
        let cell = chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, 0);
        assert!(!cell.bound && cell.next_offset == 0);
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}
