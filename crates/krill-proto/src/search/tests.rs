use super::*;
use crate::ipc::read_frame;
use proptest::prelude::*;

#[test]
fn frozen_v1_both_directions() {
    let request = SearchRequest {
        req: 1,
        query: "x".into(),
        direction: Direction::Next,
        after: None,
    };
    let result = SearchResult {
        req: 1,
        snapshot: Snapshot::default(),
        total: 0,
        hit: None,
        wrapped: false,
        stale: false,
        incomplete: true,
        approximate: false,
    };
    let request_bytes = [6, 0, 0, 0, 0x15, 1, 1, b'x', 0, 0];
    let result_bytes = [12, 0, 0, 0, 0x2a, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0];
    assert_eq!(request.encode().unwrap(), request_bytes);
    assert_eq!(result.encode().unwrap(), result_bytes);
    let (kind, body) = read_frame(&mut request_bytes.as_slice()).unwrap().unwrap();
    assert_eq!(SearchRequest::decode(kind, &body).unwrap(), request);
    let (kind, body) = read_frame(&mut result_bytes.as_slice()).unwrap().unwrap();
    assert_eq!(SearchResult::decode(kind, &body).unwrap(), result);
}

#[test]
fn cursor_and_highlight_roundtrip() {
    let snapshot = Snapshot {
        revision: Revision(7),
        seq: Seq(900),
        oldest: Seq(100),
        pages: 3,
    };
    let request = SearchRequest {
        req: 42,
        query: "한글".into(),
        direction: Direction::Previous,
        after: Some(Cursor { snapshot, index: 2 }),
    };
    let result = SearchResult {
        req: 42,
        snapshot,
        total: 3,
        hit: Some(Hit {
            index: 1,
            source: Source::Cold { page: 2 },
            row: 10,
            total_rows: 100,
            column: 4,
            cols: 2,
            cells: vec![Cell {
                ch: '한',
                ..Cell::default()
            }],
        }),
        wrapped: true,
        stale: false,
        incomplete: false,
        approximate: true,
    };
    let (kind, body) = read_frame(&mut request.encode().unwrap().as_slice())
        .unwrap()
        .unwrap();
    assert_eq!(SearchRequest::decode(kind, &body).unwrap(), request);
    let (kind, body) = read_frame(&mut result.encode().unwrap().as_slice())
        .unwrap()
        .unwrap();
    assert_eq!(SearchResult::decode(kind, &body).unwrap(), result);
}

#[test]
fn invalid_queries_and_wrong_kind_are_rejected() {
    for query in [String::new(), "한".repeat(MAX_QUERY)] {
        let request = SearchRequest {
            req: 0,
            query,
            direction: Direction::Next,
            after: None,
        };
        let body = postcard::to_stdvec(&request).unwrap();
        assert!(SearchRequest::decode(kind::SEARCH, &body).is_err());
    }
    assert!(SearchRequest::decode(kind::SEARCH_RESULT, &[]).is_err());
}

proptest! {
    #[test]
    fn arbitrary_bodies_do_not_panic(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        SearchRequest::decode(kind::SEARCH, &bytes).ok();
        SearchResult::decode(kind::SEARCH_RESULT, &bytes).ok();
    }
}

#[test]
fn options_request_has_frozen_bytes_and_preserves_v1() {
    let request = SearchOptionsRequest {
        request: SearchRequest {
            req: 1,
            query: "x".into(),
            direction: Direction::Next,
            after: None,
        },
        options: Options::default(),
    };
    let bytes = [8, 0, 0, 0, 0x46, 1, 1, b'x', 0, 0, 0, 0];
    assert_eq!(request.encode().unwrap(), bytes);
    let (kind, body) = read_frame(&mut bytes.as_slice()).unwrap().unwrap();
    assert_eq!(SearchOptionsRequest::decode(kind, &body).unwrap(), request);
    let legacy = [6, 0, 0, 0, 0x15, 1, 1, b'x', 0, 0];
    assert_eq!(request.request.encode().unwrap(), legacy);
}

#[test]
fn options_match_unicode_and_validate_regular_expressions() {
    assert!(Options::default().compile("Ä가").unwrap().is_match("ä가"));
    assert!(
        !Options {
            case_sensitive: true,
            regex: false
        }
        .compile("Ä")
        .unwrap()
        .is_match("ä")
    );
    assert!(Options::default().compile("a.b").unwrap().is_match("a.b"));
    assert!(!Options::default().compile("a.b").unwrap().is_match("axb"));
    assert!(
        Options {
            regex: true,
            ..Options::default()
        }
        .compile("N[0-9]+")
        .unwrap()
        .is_match("n123")
    );
    assert!(
        Options {
            regex: true,
            ..Options::default()
        }
        .compile("[")
        .is_err()
    );
}

proptest! {
    #[test]
    fn options_decoder_never_panics(body in proptest::collection::vec(any::<u8>(), 0..2048)) {
        drop(SearchOptionsRequest::decode(kind::SEARCH_OPTIONS, &body));
    }
}
