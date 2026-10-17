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
        let mut scan = Scan {
            query,
            target: Some(0),
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
