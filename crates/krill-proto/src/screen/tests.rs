use super::*;
use crate::ipc::read_frame;

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::new(), |mut s, x| {
        write!(s, "{x:02x}").unwrap();
        s
    })
}

fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

fn frame(bytes: &[u8]) -> (u8, Vec<u8>) {
    read_frame(&mut &bytes[..]).unwrap().unwrap()
}

fn cell(ch: char) -> Cell {
    Cell {
        ch,
        ..Cell::default()
    }
}

/// 모든 필드를 쓰는 칸: 폭 0 문자, 세 가지 색 종류, OWE-481 속성 비트와 밑줄 색(SGR 58).
fn styled() -> Cell {
    Cell {
        ch: '한',
        zerowidth: vec!['\u{200d}'],
        fg: Color::Indexed(9),
        bg: Color::Rgb(Rgb { r: 1, g: 2, b: 3 }),
        underline: Color::Rgb(Rgb {
            r: 255,
            g: 0,
            b: 128,
        }),
        flags: Flags::WIDE | Flags::UNDERLINE | Flags::STRIKEOUT | Flags::DIM | Flags::LINK,
    }
}

fn update() -> Update {
    Update {
        cursor_explicit: None,
        revision: Revision(300),
        seq: Seq(70_000),
        done: true,
        ops: vec![
            Op::Reset {
                cols: 80,
                lines: 24,
            },
            Op::Scroll(-2),
            Op::Line {
                line: 3,
                cells: vec![cell('a'), styled()],
            },
            Op::Cursor(Cursor {
                line: 3,
                column: 2,
                visible: true,
            }),
        ],
    }
}

fn attach_screen() -> Attach {
    Attach {
        session: SessionId(7),
        stream: Stream::Screen {
            known: Some(Known {
                instance: 0x0102,
                revision: Revision(5),
            }),
        },
    }
}

fn attach_raw() -> Attach {
    Attach {
        session: SessionId(7),
        stream: Stream::Raw { from: Seq(1 << 20) },
    }
}

fn history_request() -> HistoryRequest {
    HistoryRequest {
        req: 9,
        source: Source::Cold { page: 2 },
        skip: 10,
        count: 500,
    }
}

fn history_rows() -> HistoryRows {
    HistoryRows {
        req: 9,
        total: 1_000,
        pages: 3,
        skip: 10,
        rows: vec![vec![cell('x')], Vec::new()],
    }
}

/// v1 고정 바이트. 바뀌면 호환 위반이다(새 메시지는 줄을 추가만 한다).
const ATTACH_SCREEN_V1: &str = "0700000010070001820205";
const ATTACH_RAW_V1: &str = "06000000100701808040";
const ATTACHED_V1: &str = "0400000020ff012c";
const UPDATE_V1: &str = "3000000021ac02f0a204010400501801030203020161000000000003ed959c0103e2808d01090201020302ff0080e80503030201";
const OUTPUT_V1: &str = "0d00000022050000000000000068690d0a";
const GAP_V1: &str = "03000000230500";
const CLOSED_V1: &str = "03000000248001";
const REFUSED_V1: &str = "020000002500";
const HISTORY_V1: &str = "07000000120901020af403";
const HISTORY_ROWS_V1: &str = "100000002609e807030a02010178000000000000";
/// [`FEAT_SCREEN_META`] 연산(OWE-490). 바뀌면 호환 위반이다.
const UPDATE_META_V1: &str = "2b0000002102030104040eebb98ceb939c20e28093207a73680506020107010102050c68747470733a2f2f782e792f";

fn update_meta() -> Update {
    Update {
        cursor_explicit: None,
        revision: Revision(2),
        seq: Seq(3),
        done: true,
        ops: vec![
            Op::Title("빌드 – zsh".into()),
            Op::Bell,
            Op::CursorStyle(CursorStyle {
                shape: CursorShape::Bar,
                blink: true,
            }),
            Op::Links {
                line: 1,
                links: vec![Link {
                    column: 2,
                    cols: 5,
                    uri: "https://x.y/".into(),
                }],
            },
        ],
    }
}

#[test]
fn fixture_meta_ops_both_directions() {
    assert_eq!(hex(&update_meta().encode().unwrap()), UPDATE_META_V1);
    let (k, b) = frame(&unhex(UPDATE_META_V1));
    assert_eq!(Update::decode(k, &b).unwrap(), update_meta());
    // v1 연산은 그대로다(새 variant는 뒤에 붙었다).
    assert!(update().ops.iter().all(|op| !op.is_meta()));
    assert!(update_meta().ops.iter().all(Op::is_meta));
}

#[test]
fn apply_meta_ops() {
    let mut s = Screen::blank(10, 3);
    s.apply(update_meta().ops).unwrap();
    assert_eq!(s.title, "빌드 – zsh");
    assert_eq!(s.bells, 1);
    assert_eq!(s.cursor_style.shape, CursorShape::Bar);
    assert_eq!(s.link_at(1, 2), Some("https://x.y/"));
    assert_eq!(s.link_at(1, 6), Some("https://x.y/"));
    assert_eq!(s.link_at(1, 7), None);
    assert_eq!(s.link_at(0, 2), None);
    // 행 이동은 링크도 옮기고, 행 교체는 그 행의 링크를 지운다.
    s.apply(vec![Op::Scroll(1)]).unwrap();
    assert_eq!(s.link_at(0, 3), Some("https://x.y/"));
    s.apply(vec![Op::Scroll(-2)]).unwrap();
    assert_eq!(s.link_at(2, 3), Some("https://x.y/"));
    s.apply(vec![Op::Line {
        line: 2,
        cells: vec![cell('a')],
    }])
    .unwrap();
    assert_eq!(s.link_at(2, 3), None);
    // 키프레임은 부가 정보도 지운다.
    s.apply(vec![Op::Reset { cols: 4, lines: 2 }]).unwrap();
    assert!(s.title.is_empty() && s.cursor_style == CursorStyle::default());
    assert_eq!(s.links.len(), 2);

    let link = |column, cols, n| Link {
        column,
        cols,
        uri: "u".repeat(n),
    };
    let bad = [
        Op::Title("t".repeat(MAX_TITLE + 1)),
        Op::Links {
            line: 2,
            links: vec![],
        },
        Op::Links {
            line: 0,
            links: vec![link(3, 2, 1)],
        },
        Op::Links {
            line: 0,
            links: vec![link(0, 0, 1)],
        },
        Op::Links {
            line: 0,
            links: vec![link(0, 1, MAX_URI + 1)],
        },
        Op::Links {
            line: 0,
            links: vec![link(0, 1, 1); 5],
        },
    ];
    for op in bad {
        let before = s.clone();
        assert!(s.apply(vec![op.clone()]).is_err(), "{op:?}");
        assert_eq!(s, before, "{op:?}");
    }

    // MK-076: 제목에 섞인 제어문자(C0/DEL/C1)와 양방향(bidi) 재정의·격리 문자는 걸러낸다.
    s.apply(vec![Op::Title(
        "안전\x00\x07\x1b\r\n\x7f\u{009b}\u{202e}txt.exe\u{202c}\u{2066}제목\u{2069}".into(),
    )])
    .unwrap();
    assert_eq!(s.title, "안전txt.exe제목");
}

#[test]
fn fixtures_v1_both_directions() {
    let attached = Attached {
        seq: Seq(255),
        revision: Revision(44),
    };
    let gap = GapMsg {
        start: Seq(5),
        len: 0,
    };
    let closed = Closed { seq: Seq(128) };
    let cases: [(&str, Vec<u8>); 10] = [
        (ATTACH_SCREEN_V1, attach_screen().encode().unwrap()),
        (ATTACH_RAW_V1, attach_raw().encode().unwrap()),
        (ATTACHED_V1, attached.encode().unwrap()),
        (UPDATE_V1, update().encode().unwrap()),
        (OUTPUT_V1, encode_output(Seq(5), b"hi\r\n").unwrap()),
        (GAP_V1, gap.encode().unwrap()),
        (CLOSED_V1, closed.encode().unwrap()),
        (REFUSED_V1, Refused::NoSession.encode().unwrap()),
        (HISTORY_V1, history_request().encode().unwrap()),
        (HISTORY_ROWS_V1, history_rows().encode().unwrap()),
    ];
    for (want, got) in &cases {
        assert_eq!(&hex(got), want);
    }

    let (k, b) = frame(&unhex(ATTACH_SCREEN_V1));
    assert_eq!(Attach::decode(k, &b).unwrap(), attach_screen());
    let (k, b) = frame(&unhex(ATTACH_RAW_V1));
    assert_eq!(Attach::decode(k, &b).unwrap(), attach_raw());
    let (k, b) = frame(&unhex(ATTACHED_V1));
    assert_eq!(Attached::decode(k, &b).unwrap(), attached);
    let (k, b) = frame(&unhex(UPDATE_V1));
    assert_eq!(Update::decode(k, &b).unwrap(), update());
    let (k, b) = frame(&unhex(OUTPUT_V1));
    assert_eq!(decode_output(k, &b).unwrap(), (Seq(5), &b"hi\r\n"[..]));
    let (k, b) = frame(&unhex(GAP_V1));
    assert_eq!(GapMsg::decode(k, &b).unwrap(), gap);
    let (k, b) = frame(&unhex(CLOSED_V1));
    assert_eq!(Closed::decode(k, &b).unwrap(), closed);
    let (k, b) = frame(&unhex(REFUSED_V1));
    assert_eq!(Refused::decode(k, &b).unwrap(), Refused::NoSession);
    let (k, b) = frame(&unhex(HISTORY_V1));
    assert_eq!(HistoryRequest::decode(k, &b).unwrap(), history_request());
    let (k, b) = frame(&unhex(HISTORY_ROWS_V1));
    assert_eq!(HistoryRows::decode(k, &b).unwrap(), history_rows());
}

/// [`Flags`] 비트 값은 와이어 포맷이다. 바꾸면 구 앱이 다른 속성으로 그린다.
#[test]
fn flag_bits_are_frozen() {
    let bits = [
        Flags::BOLD,
        Flags::ITALIC,
        Flags::INVERSE,
        Flags::WIDE,
        Flags::SPACER,
        Flags::UNDERLINE,
        Flags::STRIKEOUT,
        Flags::DIM,
        Flags::HIDDEN,
        Flags::LINK,
        Flags::WRAPLINE,
    ];
    for (i, f) in bits.iter().enumerate() {
        assert_eq!(f.0, 1 << i);
    }
}

#[test]
fn wrong_kind_is_rejected() {
    let (_, body) = frame(&attach_raw().encode().unwrap());
    assert_eq!(
        Update::decode(kind::ATTACH, &body),
        Err(Error::UnexpectedKind {
            want: kind::UPDATE,
            got: kind::ATTACH
        })
    );
    assert!(decode_output(kind::UPDATE, &[0; 8]).is_err());
    assert!(decode_output(kind::OUTPUT, &[0; 7]).is_err());
    assert!(encode_output(Seq(0), &vec![0; MAX_OUTPUT + 1]).is_err());
}

#[test]
fn apply_snapshot_scroll_and_lines() {
    let mut s = Screen::default();
    // 스냅샷 전 델타는 거부된다(행이 없다).
    assert!(
        s.apply(vec![Op::Line {
            line: 0,
            cells: vec![]
        }])
        .is_err()
    );
    s.apply(vec![
        Op::Reset { cols: 4, lines: 3 },
        Op::Line {
            line: 0,
            cells: vec![cell('a')],
        },
        Op::Line {
            line: 1,
            cells: vec![cell('b')],
        },
        Op::Line {
            line: 2,
            cells: vec![cell('c')],
        },
    ])
    .unwrap();
    s.apply(vec![Op::Scroll(1)]).unwrap();
    assert_eq!(s.rows, vec![vec![cell('b')], vec![cell('c')], vec![]]);
    s.apply(vec![Op::Scroll(-2)]).unwrap();
    assert_eq!(s.rows, vec![vec![], vec![], vec![cell('b')]]);
    assert_eq!(s.rows.len(), 3);
}

#[test]
fn apply_rejects_out_of_range() {
    let mut s = Screen::blank(4, 3);
    let bad = [
        Op::Reset { cols: 0, lines: 3 },
        Op::Reset {
            cols: MAX_COLS + 1,
            lines: 3,
        },
        Op::Reset {
            cols: 4,
            lines: MAX_LINES + 1,
        },
        Op::Scroll(3),
        Op::Scroll(-3),
        Op::Scroll(i16::MIN),
        Op::Line {
            line: 3,
            cells: vec![],
        },
        Op::Line {
            line: 0,
            cells: vec![cell('x'); 5],
        },
        Op::Line {
            line: 0,
            cells: vec![Cell {
                zerowidth: vec!['\u{301}'; MAX_ZEROWIDTH + 1],
                ..cell('e')
            }],
        },
        Op::Cursor(Cursor {
            line: 3,
            column: 0,
            visible: true,
        }),
        Op::Cursor(Cursor {
            line: 0,
            column: 4,
            visible: true,
        }),
    ];
    for op in bad {
        let before = s.clone();
        assert!(s.apply(vec![op.clone()]).is_err(), "{op:?}");
        assert_eq!(s, before, "{op:?}");
    }
}

#[test]
fn big_snapshot_splits_into_frames_and_reassembles() {
    let (cols, lines) = (MAX_COLS, MAX_LINES);
    let row: Vec<Cell> = (0..cols).map(|_| styled()).collect();
    let mut ops = vec![Op::Reset { cols, lines }];
    ops.extend((0..lines).map(|line| Op::Line {
        line,
        cells: row.clone(),
    }));
    let frames = encode_update(Revision(1), Seq(2), ops).unwrap();
    assert!(frames.len() > 1);
    let mut view = View::default();
    for (i, f) in frames.iter().enumerate() {
        assert!(f.len() <= MAX_FRAME + 4);
        let (k, b) = frame(f);
        let u = Update::decode(k, &b).unwrap();
        assert_eq!(u.done, i + 1 == frames.len());
        view.apply(u).unwrap();
        assert_eq!(view.revision.is_some(), i + 1 == frames.len());
    }
    assert_eq!(view.revision, Some(Revision(1)));
    assert_eq!(view.seq, Seq(2));
    assert!(view.screen.rows.iter().all(|r| *r == row));
    assert_eq!(view.known(9).unwrap().instance, 9);
}

#[test]
fn empty_update_is_one_done_frame() {
    let frames = encode_update(Revision(3), Seq(4), Vec::new()).unwrap();
    assert_eq!(frames.len(), 1);
    let (k, b) = frame(&frames[0]);
    let u = Update::decode(k, &b).unwrap();
    assert!(u.done && u.ops.is_empty());
}

/// 변형한 fixture를 디코드·적용해도 panic하지 않는다.
#[test]
fn mutated_bytes_never_panic() {
    // ponytail: xorshift 무작위 입력. 커버리지 기반 퍼징은 MK-056.
    let mut x: u64 = 0x6d6b_0023_0000_0001;
    let seeds: Vec<Vec<u8>> = [
        ATTACH_SCREEN_V1,
        ATTACH_RAW_V1,
        UPDATE_V1,
        OUTPUT_V1,
        HISTORY_V1,
        HISTORY_ROWS_V1,
    ]
    .iter()
    .chain(&[UPDATE_META_V1])
    .map(|h| unhex(h))
    .collect();
    for i in 0..200_000 {
        let mut b = seeds[i % seeds.len()].clone();
        for _ in 0..=i % 4 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let at = 5 + usize::try_from(x % 1024).unwrap() % (b.len() - 5);
            b[at] = x.to_le_bytes()[4];
        }
        let (k, body) = frame(&b);
        Attach::decode(kind::ATTACH, &body).ok();
        HistoryRequest::decode(kind::HISTORY, &body).ok();
        HistoryRows::decode(kind::HISTORY_ROWS, &body).ok();
        decode_output(k, &body).ok();
        if let Ok(u) = Update::decode(kind::UPDATE, &body) {
            let mut v = View::default();
            v.apply(u).ok();
        }
    }
}

#[test]
fn oversize_links_row_is_truncated_to_frame_budget() {
    // PH-03: 2KiB URI 링크가 한 행에 511개 이상이면 Update 인코딩이 BadLength였다.
    // 앞부분 링크만 남기고 자르면 프레임에 들어가고 화면에 적용된다.
    let links: Vec<Link> = (0..600u16)
        .map(|i| Link {
            column: i,
            cols: 1,
            uri: "u".repeat(MAX_URI),
        })
        .collect();
    let frames = encode_update(
        Revision(1),
        Seq(2),
        vec![
            Op::Reset {
                cols: 2048,
                lines: 1,
            },
            Op::Links { line: 0, links },
        ],
    )
    .unwrap();
    for f in &frames {
        assert!(f.len() <= MAX_FRAME + 4, "frame {}B", f.len());
    }
    let mut view = View::default();
    for f in &frames {
        let (k, b) = frame(f);
        view.apply(Update::decode(k, &b).unwrap()).unwrap();
    }
    let kept = view.screen.links.first().map_or(0, Vec::len);
    assert!(kept > 0 && kept < 600, "kept {kept}");
    assert_eq!(
        view.screen.link_at(0, 0),
        Some("u".repeat(MAX_URI).as_str())
    );
    // 예산 안의 행은 그대로다.
    let small: Vec<Link> = (0..3u16)
        .map(|i| Link {
            column: i,
            cols: 1,
            uri: "https://x.y/".into(),
        })
        .collect();
    let frames = encode_update(
        Revision(2),
        Seq(3),
        vec![
            Op::Reset { cols: 80, lines: 1 },
            Op::Links {
                line: 0,
                links: small,
            },
        ],
    )
    .unwrap();
    let mut view = View::default();
    for f in &frames {
        let (k, b) = frame(f);
        view.apply(Update::decode(k, &b).unwrap()).unwrap();
    }
    assert_eq!(view.screen.links.first().map(Vec::len), Some(3));
}

// PH-05: 화면 Update·히스토리·원본 디코더와 적용기는 proptest 대상이다(CONVENTIONS §6).
proptest::proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(
        bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..256),
    ) {
        if let Ok(u) = Update::decode(kind::UPDATE, &bytes) {
            View::default().apply(u).ok();
        }
        HistoryRows::decode(kind::HISTORY_ROWS, &bytes).ok();
        decode_output(kind::OUTPUT, &bytes).ok();
    }
}

#[test]
fn history_fit_keeps_newest_rows_within_one_frame() {
    let row: Vec<Cell> = (0..MAX_COLS).map(|_| styled()).collect();
    let mut h = HistoryRows {
        req: 1,
        total: 100,
        pages: 0,
        skip: 0,
        rows: (0..100u16)
            .map(|i| {
                let mut r = row.clone();
                r[0].ch = char::from_u32(0x41 + u32::from(i)).unwrap();
                r
            })
            .collect(),
    };
    h.fit();
    assert!(!h.rows.is_empty() && h.rows.len() < 100);
    assert_eq!(
        h.rows.last().unwrap()[0].ch,
        char::from_u32(0x41 + 99).unwrap()
    );
    let f = h.encode().unwrap();
    assert!(f.len() <= MAX_FRAME + 4);
}

#[test]
fn frozen_history_has_new_kinds_and_old_history_stays_frozen() {
    let req = HistoryViewRequest {
        req: 1,
        view: 2,
        skip: 3,
        count: 4,
    };
    assert_eq!(hex(&req.encode().unwrap()), "050000001801020304");
    let (kind, body) = frame(&req.encode().unwrap());
    assert_eq!(HistoryViewRequest::decode(kind, &body).unwrap(), req);
    assert!(HistoryRequest::decode(kind, &body).is_err());
    let mut reply = HistoryViewRows {
        req: 1,
        view: 2,
        session: crate::SessionId(7),
        epoch: [1, 2, 3],
        status: HistoryViewStatus::Ready,
        identity: Some(HistoryIdentity {
            session: crate::SessionId(7),
            source: Source::Hot,
            start_seq: Seq(0),
            end_seq: Seq(9),
            digest: [3; 32],
        }),
        skip: 3,
        total: 8,
        complete: true,
        rows: vec![vec![cell('a')], vec![cell('b')]],
    };
    let (kind, body) = frame(&reply.encode().unwrap());
    assert_eq!(HistoryViewRows::decode(kind, &body).unwrap(), reply);
    reply.rows = vec![Vec::new(); usize::from(MAX_HISTORY_ROWS) + 1];
    let (kind, body) = frame(&reply.encode().unwrap());
    assert!(HistoryViewRows::decode(kind, &body).is_err());
}

#[test]
fn cursor_explicit_postfix_is_compatible_bounded_and_applied_after_reset() {
    let mut current = update();
    let old = current.encode().unwrap();
    let (kind, body) = frame(&old);
    assert_eq!(Update::decode(kind, &body).unwrap().cursor_explicit, None);
    current.cursor_explicit = Some(true);
    let (kind, body) = frame(&current.encode().unwrap());
    assert_eq!(Update::decode(kind, &body).unwrap(), current);
    let old_decoder = postcard::from_bytes::<Update>(&body).unwrap();
    assert_eq!(old_decoder.cursor_explicit, None);
    assert_eq!(old_decoder.ops, current.ops);
    let mut malformed = postcard::to_stdvec(&current).unwrap();
    malformed.push(2); // postcard bool is only zero or one.
    assert!(Update::decode(kind, &malformed).is_err());
    let mut view = View::default();
    view.apply(current).unwrap();
    assert!(view.screen.cursor_explicit);
    let mut reset = update();
    reset.cursor_explicit = Some(false);
    view.apply(reset).unwrap();
    assert!(!view.screen.cursor_explicit);
}

#[test]
fn cursor_explicit_is_carried_only_by_final_split_update() {
    let ops = vec![Op::Title("a".repeat(MAX_TITLE)); 1100];
    let frames = encode_update_with_cursor(Revision(1), Seq(1), ops, Some(true)).unwrap();
    assert!(frames.len() > 1);
    let mut view = View::default();
    for bytes in frames {
        let (kind, body) = frame(&bytes);
        let update = Update::decode(kind, &body).unwrap();
        assert_eq!(update.cursor_explicit, update.done.then_some(true));
        view.apply(update).unwrap();
    }
    assert_eq!(view.revision, Some(Revision(1)));
    assert!(view.screen.cursor_explicit);
}
