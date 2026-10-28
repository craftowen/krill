use super::*;

#[cfg(test)]
mod recheck_opaque_boundary {
    use super::*;
    #[test]
    fn recheck_independent_replay_after_end_to_partial_append_preserves_observed_page() {
        let root =
            std::env::temp_dir().join(format!("krill-opaque-partial-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
        let path = root.join("opencode.db");
        let mut db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE session(id TEXT PRIMARY KEY,directory TEXT); CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,data TEXT,time_created INTEGER);INSERT INTO session VALUES('s','/workspace');INSERT INTO message VALUES('first','s','{\"role\":\"user\",\"content\":\"first\"}',0);").unwrap();
        let initial = poll_transcript_file(VaultAgent::OpenCode, "s", &path, 0);
        assert!(initial.bound && !initial.reset);
        let start = initial.next_offset;
        assert!(start >= OPENCODE_END);
        let tx = db.transaction().unwrap();
        for n in 1..=150 {
            tx.execute(
                "INSERT INTO message VALUES(?1,'s',?2,?3)",
                rusqlite::params![
                    format!("m{n:05}"),
                    format!(r#"{{"role":"user","content":"new-{n}"}}"#),
                    n
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let observed = poll_transcript_file(VaultAgent::OpenCode, "s", &path, start);
        assert!(observed.bound && !observed.reset);
        assert_eq!(observed.items.len(), 128);
        assert_eq!(observed.items.first().unwrap().text, "new-1");
        assert_eq!(observed.items.last().unwrap().text, "new-128");
        eprintln!(
            "observed start={start} end={} start>end={}",
            observed.next_offset,
            start > observed.next_offset
        );
        let replay = replay_page(observed.clone(), start, observed.next_offset)
            .expect("valid observed opaque page must replay even when token number decreases");
        assert!(replay.bound && !replay.reset);
        assert_eq!(replay.items, observed.items);
        assert_eq!(replay.next_offset, observed.next_offset);
        // Backfill from an opaque end must decode its row position, rather than clamp
        // the large token number to the current row count.
        let (prior, position) = latest_page(observed.clone(), Some(start)).unwrap();
        assert_eq!(position, 0);
        assert_eq!(prior.next_offset, start);
        assert_eq!(prior.items, initial.items);
        // Decoded row order still rejects a reversed range even if token numbers rise.
        let backwards = replay_page(observed.clone(), observed.next_offset, start).unwrap();
        assert!(!backwards.bound && backwards.reset && backwards.items.is_empty());
        // Stale and foreign proof boundaries cannot select a successful replay.
        assert!(latest_page(observed.clone(), Some((1 << 53) - 1)).is_err());
        let stale = replay_page(observed.clone(), (1 << 53) - 1, observed.next_offset).unwrap();
        assert!(!stale.bound && stale.reset && stale.items.is_empty());
        db.execute_batch(
            "UPDATE message SET data='{\"role\":\"user\",\"content\":\"edited\"}' WHERE id='first'",
        )
        .unwrap();
        let changed = replay_page(observed.clone(), start, observed.next_offset).unwrap();
        assert!(!changed.bound && changed.reset && changed.items.is_empty());
        assert!(latest_page(observed, Some(start)).is_err());
        drop(db);
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod recheck_stable_boundary {
    use super::*;
    #[test]
    fn recheck_independent_replay_keeps_observed_end_token_after_later_append() {
        let root = std::env::temp_dir().join(format!("krill-opaque-stable-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
        let path = root.join("opencode.db");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE session(id TEXT PRIMARY KEY,directory TEXT); CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,data TEXT,time_created INTEGER);INSERT INTO session VALUES('s','/workspace');INSERT INTO message VALUES('first','s','{\"role\":\"user\",\"content\":\"first\"}',0);").unwrap();
        let observed = poll_transcript_file(VaultAgent::OpenCode, "s", &path, 0);
        assert!(observed.bound && !observed.reset);
        let end = observed.next_offset;
        db.execute_batch("INSERT INTO message VALUES('second','s','{\"role\":\"assistant\",\"content\":\"second\"}',1);").unwrap();
        let current = poll_transcript_file(VaultAgent::OpenCode, "s", &path, end);
        assert!(current.bound && !current.reset);
        let replay = replay_page(current, 0, end).unwrap();
        assert!(replay.bound && !replay.reset);
        assert_eq!(replay.items, observed.items);
        eprintln!(
            "requested_end={end} actual_end={} same_content=true",
            replay.next_offset
        );
        assert_eq!(
            replay.next_offset, end,
            "replay changed the immutable observed boundary just because later rows appended"
        );
        drop(db);
        fs::remove_dir_all(root).unwrap();
    }
}
