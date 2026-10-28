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

#[cfg(test)]
mod multipart_boundaries {
    use super::*;

    fn fixture(label: &str, counts: &[usize]) -> (PathBuf, PathBuf, rusqlite::Connection) {
        let root =
            std::env::temp_dir().join(format!("krill-multipart-{label}-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
        let path = root.join("opencode.db");
        let mut db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE session(id TEXT PRIMARY KEY,directory TEXT); CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,data TEXT,time_created INTEGER); CREATE TABLE part(id TEXT PRIMARY KEY,message_id TEXT,data TEXT,time_created INTEGER); CREATE INDEX part_message ON part(message_id); INSERT INTO session VALUES('s','/workspace');").unwrap();
        let tx = db.transaction().unwrap();
        for (m, count) in counts.iter().enumerate() {
            tx.execute(
                "INSERT INTO message VALUES(?1,'s','{\"role\":\"assistant\"}',?2)",
                rusqlite::params![format!("m{m:06}"), i64::try_from(m).unwrap()],
            )
            .unwrap();
            for i in 0..*count {
                tx.execute(
                    "INSERT INTO part VALUES(?1,?2,?3,?4)",
                    rusqlite::params![
                        format!("p{m}-{i:04}"),
                        format!("m{m:06}"),
                        format!(r#"{{"type":"text","text":"part-{m}-{i}"}}"#),
                        i64::try_from(i).unwrap()
                    ],
                )
                .unwrap();
            }
        }
        tx.commit().unwrap();
        (root, path, db)
    }

    #[test]
    fn independent_latest_mixed_part_records_replays_without_omission() {
        let (root, path, db) = fixture("independent", &[100, 50, 1]);
        let seed = poll_transcript_file(VaultAgent::OpenCode, "s", &path, 0);
        let (latest, start) = latest_page(seed, None).unwrap();
        assert!(latest.bound && !latest.reset);
        let replay = replay_page(latest.clone(), start, latest.next_offset).unwrap();
        assert!(replay.bound && !replay.reset);
        assert_eq!(
            replay.items, latest.items,
            "every item from observed latest must replay"
        );
        assert_eq!(replay.next_offset, latest.next_offset);
        drop(db);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn asymmetric_groups_keep_atomic_forward_tail_history_and_append_boundaries() {
        for (case, counts) in [
            vec![100, 50, 1],
            vec![1, 50, 100],
            vec![64, 64, 1],
            vec![1, 128, 1],
            vec![129, 1, 50],
            vec![1, 50, 129],
            vec![64, 64, 0],
            vec![0, 129, 0, 1, 0],
            vec![0, 100, 0, 50, 0, 1, 0],
        ]
        .iter()
        .enumerate()
        {
            let (root, path, db) = fixture(&case.to_string(), counts);
            let expected: Vec<_> = counts
                .iter()
                .enumerate()
                .flat_map(|(m, count)| (0..*count).map(move |i| format!("part-{m}-{i}")))
                .collect();
            let seed = poll_transcript_file(VaultAgent::OpenCode, "s", &path, 0);
            let mut forward = Vec::new();
            let mut observed = Vec::new();
            let mut cursor = 0;
            loop {
                let page = poll_transcript_file(VaultAgent::OpenCode, "s", &path, cursor);
                assert!(page.bound && !page.reset);
                assert_atomic_budget(&page);
                let replay = replay_page(page.clone(), cursor, page.next_offset).unwrap();
                assert_eq!(replay, page);
                forward.extend(page.items.iter().map(|item| item.text.clone()));
                observed.push((cursor, page.clone()));
                cursor = page.next_offset;
                if cursor >= OPENCODE_END {
                    break;
                }
            }
            assert_eq!(forward, expected);
            let (latest, mut start) = latest_page(seed.clone(), None).unwrap();
            assert_atomic_budget(&latest);
            assert_eq!(
                replay_page(latest.clone(), start, latest.next_offset).unwrap(),
                latest
            );
            let mut backwards = latest.items.clone();
            while start > 0 {
                let (page, next_start) = latest_page(seed.clone(), Some(start)).unwrap();
                assert!(next_start < start);
                assert_eq!(page.next_offset, start);
                assert_atomic_budget(&page);
                assert_eq!(replay_page(page.clone(), next_start, start).unwrap(), page);
                let mut items = page.items;
                items.extend(backwards);
                backwards = items;
                start = next_start;
            }
            assert_eq!(
                backwards
                    .iter()
                    .map(|item| item.text.clone())
                    .collect::<Vec<_>>(),
                expected
            );
            db.execute("INSERT INTO message VALUES('z','s','{\"role\":\"user\",\"content\":\"appended\"}',1000)", []).unwrap();
            for (start, page) in observed {
                assert_eq!(
                    replay_page(page.clone(), start, page.next_offset).unwrap(),
                    page
                );
            }
            // A former live-end token stays immutable in both replay and latest-before.
            let end = latest.next_offset;
            let (historical, historical_start) = latest_page(seed, Some(end)).unwrap();
            assert_eq!(historical.items, latest.items);
            assert_eq!(historical.next_offset, end);
            assert_eq!(
                replay_page(historical.clone(), historical_start, end).unwrap(),
                historical
            );
            let appended = poll_transcript_file(VaultAgent::OpenCode, "s", &path, cursor);
            assert!(appended.bound && !appended.reset);
            assert_eq!(appended.items.len(), 1);
            assert_eq!(appended.items[0].text, "appended");
            drop(db);
            fs::remove_dir_all(root).unwrap();
        }
    }

    fn assert_atomic_budget(page: &ChatPollSnapshot) {
        if page.items.len() > MAX_CHAT_ITEMS {
            let message = page.items.first().unwrap().text.split('-').nth(1).unwrap();
            assert!(
                page.items
                    .iter()
                    .all(|item| item.text.split('-').nth(1) == Some(message)),
                "only one atomic message may exceed the item budget"
            );
        }
    }
}
