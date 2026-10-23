use super::*;
use krill_proto::search::{Cursor, Snapshot};

#[test]
fn highlights_use_terminal_cells_for_wide_and_combining_characters() {
    let cells = vec![
        Cell {
            ch: 'a',
            ..Cell::default()
        },
        Cell {
            ch: '한',
            flags: Flags::WIDE,
            ..Cell::default()
        },
        Cell {
            flags: Flags::SPACER,
            ..Cell::default()
        },
        Cell {
            ch: 'e',
            zerowidth: vec!['\u{301}'],
            ..Cell::default()
        },
        Cell {
            ch: '🙂',
            flags: Flags::WIDE,
            ..Cell::default()
        },
        Cell {
            flags: Flags::SPACER,
            ..Cell::default()
        },
    ];
    for (query, column, cols) in [("한e\u{301}", 1, 3), ("\u{301}", 3, 1), ("🙂", 4, 2)] {
        let pattern = krill_proto::search::Options {
            case_sensitive: true,
            regex: false,
        }
        .compile(query)
        .unwrap();
        let mut scan = Scan {
            pattern: &pattern,
            target: Some(0),
            forward: true,
            total: 0,
            first: None,
            last: None,
            selected: None,
        };
        scan.rows(Source::Screen, std::slice::from_ref(&cells));
        let hit = scan.selected.unwrap();
        assert_eq!((hit.column, hit.cols), (column, cols));
        assert_eq!(scan.total, 1);
    }
}

#[test]
fn cursor_survives_segment_seal_but_not_loss() {
    let cursor = Cursor {
        snapshot: Snapshot {
            oldest: Seq(0),
            pages: 2,
            ..Snapshot::default()
        },
        index: 1,
    };
    // 세그먼트 봉인으로 pages가 늘어도 oldest가 같으면 유지한다.
    let sealed = Snapshot {
        oldest: Seq(0),
        pages: 3,
        ..Snapshot::default()
    };
    assert!(cursor_valid(&cursor, &sealed, 3));
    // pages가 같아도 유지한다.
    let same = Snapshot {
        oldest: Seq(0),
        pages: 2,
        ..Snapshot::default()
    };
    assert!(cursor_valid(&cursor, &same, 3));
    // pages가 줄면(중간 손실) 무효로 본다.
    let lost = Snapshot {
        oldest: Seq(0),
        pages: 1,
        ..Snapshot::default()
    };
    assert!(!cursor_valid(&cursor, &lost, 3));
    // GC로 oldest가 바뀌면 무효로 본다.
    let gc = Snapshot {
        oldest: Seq(100),
        pages: 3,
        ..Snapshot::default()
    };
    assert!(!cursor_valid(&cursor, &gc, 3));
    // 범위를 벗어나면 무효로 본다.
    assert!(!cursor_valid(&cursor, &sealed, 1));
}

#[test]
fn batched_cold_scan_keeps_exact_total_cursor_and_page_coordinates() {
    let pattern = krill_proto::search::Options::default()
        .compile("a")
        .unwrap();
    let mut scan = Scan {
        pattern: &pattern,
        target: Some(3),
        forward: true,
        total: 0,
        first: None,
        last: None,
        selected: None,
    };
    let source = Source::Cold { page: 7 };
    let row = |text: &str| {
        text.chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect()
    };
    scan.rows_at(source, &[row("aa"), row("skip")], 0);
    scan.rows_at(source, &[row("aaa")], 2);
    scan.finish_page(source, 3);
    assert_eq!(scan.total, 5);
    let hit = scan.selected.unwrap();
    assert_eq!(
        (hit.index, hit.row, hit.column, hit.total_rows),
        (3, 2, 1, 3)
    );
    assert_eq!(scan.first.unwrap().index, 0);
    assert!(
        scan.last.is_none(),
        "forward queries do not need cells from intermediate last hits"
    );
    let mut backwards = Scan {
        pattern: &pattern,
        target: None,
        forward: false,
        total: 0,
        first: None,
        last: None,
        selected: None,
    };
    backwards.rows_at(source, &[row("aa"), row("skip")], 0);
    backwards.rows_at(source, &[row("aaa")], 2);
    backwards.finish_page(source, 3);
    assert_eq!(backwards.total, 5);
    assert_eq!(backwards.last.unwrap().index, 4);
    assert!(backwards.first.is_none());
}

#[cfg(unix)]
#[test]
fn queued_cold_search_refreshes_its_snapshot_after_viewport_clear() {
    use std::fs::{self, File};
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use krill_proto::SessionId;
    use krill_proto::holder::WinSize;

    let root = std::env::temp_dir().join(format!("queued-search-clear-{}", std::process::id()));
    let store = crate::store::Store::open(&root, crate::store::Limits::default()).unwrap();
    let id = SessionId(99);
    let size = WinSize { cols: 40, rows: 3 };
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let mut output = File::from(pty.user);
    let session = crate::Session::start(
        id,
        pty.controller,
        size,
        Arc::new(crate::Appearance::load(PathBuf::new())),
        Some(store.recorder(id, size).unwrap()),
    )
    .unwrap();
    let tap = session.tap();
    let old = b"OLD\r\na\r\nb\r\nc\r\nkeep\r\n";
    output.write_all(old).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while tap.pulse().end < u64::try_from(old.len()).unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!store.history(id).unwrap().is_empty());
    let active = replay::acquire(&|| false).unwrap();
    let waiting = tap.clone();
    let checks = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&checks);
    let worker = std::thread::spawn(move || {
        search_cancellable(
            &waiting,
            &SearchRequest {
                req: 1,
                query: "OLD".into(),
                direction: Direction::Next,
                after: None,
            },
            krill_proto::search::Options::default(),
            &|| {
                observed.fetch_add(1, Ordering::AcqRel);
                false
            },
        )
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while checks.load(Ordering::Acquire) < 2 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(tap.clear_viewport());
    let new = b"\x1b[2J\x1b[HNEW\r\nlast\r\n";
    output.write_all(new).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while tap.pulse().end < u64::try_from(old.len() + new.len()).unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(active);
    let result = worker.join().unwrap();
    assert_eq!(
        result.total, 0,
        "a waiting search must not reuse the pre-clear manifest/grid"
    );
    assert!(result.hit.is_none());
    drop(output);
    session.wait().unwrap();
    fs::remove_dir_all(root).unwrap();
}
