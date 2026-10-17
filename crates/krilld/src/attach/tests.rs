use krill_proto::screen::{Color, Cursor, Flags, Link};

use super::*;

fn row(text: &str) -> Vec<Cell> {
    text.chars()
        .map(|ch| Cell {
            ch,
            ..Cell::default()
        })
        .collect()
}

fn screen(lines: &[&str]) -> Screen {
    let mut s = Screen::blank(20, u16::try_from(lines.len()).unwrap());
    s.rows = lines.iter().map(|l| row(l)).collect();
    s
}

/// `diff`를 클라이언트 쪽 `apply`로 적용하면 정확히 `next`가 된다. 연산 목록도 돌려준다.
fn roundtrip(prev: &Screen, next: &Screen) -> Vec<Op> {
    let ops = diff(prev, next);
    let mut got = prev.clone();
    got.apply(ops.clone()).unwrap();
    assert_eq!(&got, next);
    ops
}

#[test]
fn scroll_up_sends_shift_and_new_line_only() {
    let prev = screen(&["a", "b", "c", "d"]);
    let next = screen(&["b", "c", "d", "e"]);
    let ops = roundtrip(&prev, &next);
    assert_eq!(
        ops,
        vec![
            Op::Scroll(1),
            Op::Line {
                line: 3,
                cells: row("e")
            }
        ]
    );
}

#[test]
fn scroll_down_and_partial_changes() {
    let prev = screen(&["a", "b", "c", "d"]);
    let next = screen(&["x", "", "a", "B"]);
    let ops = roundtrip(&prev, &next);
    assert_eq!(ops[0], Op::Scroll(-2));
}

#[test]
fn no_shift_when_it_does_not_help() {
    let prev = screen(&["a", "", "", ""]);
    let next = screen(&["a", "b", "", ""]);
    let ops = roundtrip(&prev, &next);
    assert_eq!(
        ops,
        vec![Op::Line {
            line: 1,
            cells: row("b")
        }]
    );
    assert!(roundtrip(&next, &next).is_empty());
}

#[test]
fn size_change_and_cursor_and_attributes() {
    let prev = screen(&["a", "b"]);
    let mut next = Screen::blank(30, 3);
    next.rows[1] = vec![Cell {
        ch: '한',
        zerowidth: vec![],
        fg: Color::Indexed(1),
        bg: Color::Default,
        underline: Color::Indexed(2),
        flags: Flags::WIDE | Flags::UNDERLINE,
    }];
    next.cursor = Cursor {
        line: 2,
        column: 5,
        visible: false,
    };
    let ops = roundtrip(&prev, &next);
    assert_eq!(ops[0], Op::Reset { cols: 30, lines: 3 });
    let mut moved = next.clone();
    moved.cursor.column = 6;
    assert_eq!(roundtrip(&next, &moved), vec![Op::Cursor(moved.cursor)]);
}

/// 결정적 의사 난수 화면 쌍에서 diff → apply가 항상 같은 화면을 만든다.
#[test]
fn random_screens_roundtrip() {
    let mut x: u64 = 0x6d6b_0023;
    let mut next_u = |m: usize| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        usize::try_from(x % 1_000_003).unwrap() % m
    };
    let words = ["", "a", "bb", "한글", "ccc", "d"];
    for _ in 0..2_000 {
        let lines = 1 + next_u(8);
        let pick = |n: usize| words[n % words.len()];
        let a: Vec<&str> = (0..lines).map(|_| pick(next_u(6))).collect();
        let mut b: Vec<&str> = if next_u(2) == 0 {
            let k = next_u(lines);
            a[k..]
                .iter()
                .copied()
                .chain(std::iter::repeat_n("", k))
                .collect()
        } else {
            a.clone()
        };
        for _ in 0..next_u(3) {
            let i = next_u(lines);
            b[i] = pick(next_u(6));
        }
        roundtrip(&screen(&a), &screen(&b));
    }
}

fn link(column: u16, cols: u16, uri: &str) -> Link {
    Link {
        column,
        cols,
        uri: uri.into(),
    }
}

/// OWE-490: 제목·커서 모양·링크는 diff → apply로 똑같이 옮겨지고, 행 이동은 링크도 옮긴다.
#[test]
fn meta_ops_roundtrip() {
    let mut prev = screen(&["a", "b", "c", "d"]);
    prev.links[1] = vec![link(0, 1, "https://b/")];
    let mut next = screen(&["b", "c", "d", "e"]);
    next.links[0] = vec![link(0, 1, "https://b/")];
    next.title = "제목".into();
    next.cursor_style = CursorStyle {
        shape: krill_proto::screen::CursorShape::Underline,
        blink: false,
    };
    let ops = roundtrip(&prev, &next);
    assert_eq!(ops[0], Op::Title("제목".into()));
    assert!(ops.contains(&Op::Scroll(1)));
    assert!(
        !ops.iter().any(|o| matches!(o, Op::Links { .. })),
        "{ops:?}"
    );

    // 글자는 같고 링크만 바뀐 행은 Links만, 바뀐 행은 Line 뒤에 Links.
    let mut relinked = next.clone();
    relinked.links[0] = vec![link(0, 1, "https://c/")];
    relinked.rows[3] = row("E");
    relinked.links[3] = vec![link(0, 1, "https://e/")];
    let ops = roundtrip(&next, &relinked);
    assert_eq!(
        ops,
        vec![
            Op::Links {
                line: 0,
                links: vec![link(0, 1, "https://c/")]
            },
            Op::Line {
                line: 3,
                cells: row("E")
            },
            Op::Links {
                line: 3,
                links: vec![link(0, 1, "https://e/")]
            },
        ]
    );

    // 벨은 여러 번 울려도 한 번.
    let mut rang = relinked.clone();
    rang.bells += 3;
    assert_eq!(diff(&relinked, &rang), vec![Op::Bell]);
}

/// 스냅샷은 부가 정보를 기본값이 아닌 것만 싣고 벨은 싣지 않는다.
#[test]
fn snapshot_meta_and_filter() {
    let mut s = screen(&["a", ""]);
    s.title = "t".into();
    s.bells = 5;
    s.links[0] = vec![link(0, 1, "u")];
    let ops = snapshot(&s);
    assert!(ops.contains(&Op::Title("t".into())));
    assert!(!ops.contains(&Op::Bell));
    assert!(!ops.iter().any(|o| matches!(o, Op::CursorStyle(_))));
    let mut got = Screen::default();
    got.apply(ops.clone()).unwrap();
    assert_eq!(got.title, "t");
    assert_eq!(got.link_at(0, 0), Some("u"));
    // 구 클라이언트용으로 거르면 v1 연산만 남는다.
    assert!(ops.iter().filter(|o| !o.is_meta()).count() < ops.len());
}

/// OWE-586: 감시 연결 ping에 nonce를 되돌린 pong으로 답한다. 모르는 kind는 기존대로 거부한다.
#[test]
fn watch_answers_ping_with_pong() {
    use std::os::unix::net::UnixStream;

    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let worker = thread::spawn(move || watch(server));
    client
        .write_all(&krill_proto::daemon::ping(41).unwrap())
        .unwrap();
    let (kind, body) = krill_proto::ipc::read_frame(&mut client).unwrap().unwrap();
    assert_eq!(kind, krill_proto::daemon::PONG);
    assert_eq!(krill_proto::daemon::pong_nonce(kind, &body).unwrap(), 41);
    client
        .write_all(&krill_proto::ipc::encode_frame(0x7f, &[]).unwrap())
        .unwrap();
    let (kind, _) = krill_proto::ipc::read_frame(&mut client).unwrap().unwrap();
    assert_eq!(kind, krill_proto::screen::kind::REFUSED);
    drop(client);
    worker.join().unwrap().unwrap();
}
