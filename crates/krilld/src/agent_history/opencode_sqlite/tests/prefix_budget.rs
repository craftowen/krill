use super::*;

#[test]
fn long_part_backed_transcript_keeps_per_page_and_prefix_budgets_separate() {
    let (root, path, mut db) = fixture("long-parts");
    let tx = db.transaction().unwrap();
    tx.execute_batch(
        "DELETE FROM part; DELETE FROM message; CREATE INDEX parts_by_message ON part(message_id);",
    )
    .unwrap();
    for n in 0..2100 {
        let id = format!("m{n:06}");
        tx.execute(
            "INSERT INTO message VALUES(?1,'ses_sqlite_001','{\"role\":\"user\"}',?2)",
            rusqlite::params![id, n],
        )
        .unwrap();
        for part in ["a", "b"] {
            tx.execute(
                "INSERT INTO part VALUES(?1,?2,?3,0)",
                rusqlite::params![
                    format!("{id}-{part}"),
                    id,
                    format!(r#"{{"type":"text","text":"{n}-{part}"}}"#)
                ],
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();
    let mut cursor = 0;
    let mut observed = Vec::new();
    loop {
        let page =
            chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, cursor);
        assert!(page.bound && !page.reset);
        assert!(page.items.len() <= 128);
        observed.extend(page.items.into_iter().map(|item| item.text));
        cursor = page.next_offset;
        if cursor >= (1 << 52) {
            break;
        }
    }
    assert_eq!(
        observed,
        (0..2100)
            .flat_map(|n| [format!("{n}-a"), format!("{n}-b")])
            .collect::<Vec<_>>()
    );
    db.execute("INSERT INTO message VALUES('last','ses_sqlite_001','{\"role\":\"user\",\"content\":\"after parts\"}',3000)",[]).unwrap();
    let appended =
        chat::poll_transcript_file(VaultAgent::OpenCode, "ses_sqlite_001", &path, cursor);
    assert!(appended.bound && !appended.reset);
    assert_eq!(appended.items.first().unwrap().text, "after parts");
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}
