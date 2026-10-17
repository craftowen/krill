//! MK-059: 실제 PTY 12,000행 → 세그먼트 → UDS 검색 → History 좌표/셀 하이라이트 검증.
#![expect(clippy::unwrap_used, reason = "통합 테스트 헬퍼")]

use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use krill_proto::SessionId;
use krill_proto::daemon::client_hello;
use krill_proto::holder::WinSize;
use krill_proto::ipc::{self, ClientRole, FEATURES, PROTO_MAX, PROTO_MIN, ServerHello};
use krill_proto::screen::{self, Attach, Flags, HistoryRequest, HistoryRows, Msg, Refused, Stream};
use krill_proto::search::{self, Cursor, Direction, SearchRequest, SearchResult, Source};
use krilld::attach::{self, Tap};
use krilld::store::{Limits, Store};
use krilld::{Appearance, Session};
use rustix::termios::Winsize;

fn request(query: &str, direction: Direction, previous: Option<&SearchResult>) -> SearchRequest {
    SearchRequest {
        req: 7,
        query: query.into(),
        direction,
        after: previous.and_then(|reply| {
            reply.hit.as_ref().map(|hit| Cursor {
                snapshot: reply.snapshot,
                index: hit.index,
            })
        }),
    }
}

fn ask(stream: &mut UnixStream, req: &SearchRequest) -> SearchResult {
    stream.write_all(&req.encode().unwrap()).unwrap();
    loop {
        let (kind, body) = ipc::read_frame(stream).unwrap().unwrap();
        if kind == search::kind::SEARCH_RESULT {
            let reply = SearchResult::decode(kind, &body).unwrap();
            assert_eq!(reply.req, req.req);
            return reply;
        }
        assert!(matches!(kind, screen::kind::UPDATE | screen::kind::CLOSED));
    }
}

fn connection(tap: Tap, features: u64) -> (UnixStream, thread::JoinHandle<std::io::Result<()>>) {
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let worker = thread::spawn(move || {
        let hello = ServerHello {
            proto_min: PROTO_MIN,
            proto_max: PROTO_MAX,
            features: FEATURES,
            build: "search-test".into(),
            instance_id: 1,
        };
        attach::serve(server, &hello, |id| (id == tap.id()).then_some(tap))
    });
    let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
    ServerHello::decode(kind, &body).unwrap();
    let mut hello = client_hello(ClientRole::App, "search-test");
    // 검색 전용 클라이언트는 입력 모드 메시지를 소비하지 않는다.
    hello.features = features & !krill_proto::term_modes::FEAT_TERM_MODES;
    client.write_all(&hello.encode().unwrap()).unwrap();
    client
        .write_all(
            &Attach {
                session: SessionId(1),
                stream: Stream::Screen { known: None },
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    let (kind, _) = ipc::read_frame(&mut client).unwrap().unwrap();
    assert_eq!(kind, screen::kind::ATTACHED);
    // 종료된 세션의 초기 화면과 Closed를 소비한 뒤 요청을 보낸다.
    loop {
        let (kind, _) = ipc::read_frame(&mut client).unwrap().unwrap();
        if kind == screen::kind::CLOSED {
            break;
        }
        assert_eq!(kind, screen::kind::UPDATE);
    }
    (client, worker)
}

#[test]
fn next_previous_wrap_and_highlight_over_twelve_thousand_lines() {
    let dir = std::env::temp_dir().join(format!("mk059-history-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let store = Store::open(
        &dir,
        Limits {
            seal_bytes: 32 << 10,
            ..Limits::default()
        },
    )
    .unwrap();
    let size = WinSize { cols: 80, rows: 24 };
    let pty = rustix_openpty::openpty(
        None,
        Some(&Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
    )
    .unwrap();
    let mut child = Command::new("/usr/bin/perl")
        .args(["-e", "for $i (1..12000) { printf \"%05d \\e[31m한글🙂\\e[0m %s\\n\", $i, ($i == 1 || $i == 6000 || $i == 12000) ? 'needle' : 'plain' }"])
        .stdin(Stdio::from(pty.user.try_clone().unwrap()))
        .stdout(Stdio::from(pty.user.try_clone().unwrap()))
        .stderr(Stdio::from(pty.user))
        .spawn().unwrap();
    let session = Session::start(
        SessionId(1),
        pty.controller,
        size,
        Arc::new(Appearance::load(PathBuf::new())),
        Some(store.recorder(SessionId(1), size).unwrap()),
    )
    .unwrap();
    assert!(child.wait().unwrap().success());
    let tap = session.tap();
    tap.wait(|pulse| pulse.closed);
    assert_eq!(tap.history(0, 0).0, 1_000);
    assert!(store.history(SessionId(1)).unwrap().len() > 3);
    let (mut client, worker) = connection(tap.clone(), FEATURES);

    let first = ask(&mut client, &request("needle", Direction::Next, None));
    assert_eq!(first.total, 3, "중복 핫/콜드 결과 없음");
    assert!(
        !first.incomplete && !first.stale && first.approximate,
        "{first:?}"
    );
    let first_hit = first.hit.as_ref().unwrap();
    assert_eq!(first_hit.index, 0);
    assert_eq!((first_hit.column, first_hit.cols), (13, 6));
    assert!(matches!(first_hit.source, Source::Cold { page: 0 }));
    let middle = ask(
        &mut client,
        &request("needle", Direction::Next, Some(&first)),
    );
    assert_eq!(middle.hit.as_ref().unwrap().index, 1);
    assert!(matches!(middle.hit.as_ref().unwrap().source, Source::Cold { page } if page > 0));
    let last = ask(
        &mut client,
        &request("needle", Direction::Next, Some(&middle)),
    );
    assert_eq!(last.hit.as_ref().unwrap().source, Source::Screen);
    let wrapped = ask(
        &mut client,
        &request("needle", Direction::Next, Some(&last)),
    );
    assert!(wrapped.wrapped);
    assert_eq!(wrapped.hit, first.hit);
    let previous = ask(
        &mut client,
        &request("needle", Direction::Previous, Some(&first)),
    );
    assert!(previous.wrapped);
    assert_eq!(previous.hit, last.hit);
    let back = ask(
        &mut client,
        &request("needle", Direction::Previous, Some(&last)),
    );
    assert_eq!(back.hit, middle.hit);
    assert_eq!(
        ask(&mut client, &request("needle", Direction::Previous, None)).hit,
        last.hit
    );
    assert!(
        ask(&mut client, &request("absent", Direction::Next, None))
            .hit
            .is_none()
    );

    let mut stale = request("needle", Direction::Next, Some(&middle));
    stale.after.as_mut().unwrap().snapshot.oldest.0 += 1;
    let reset = ask(&mut client, &stale);
    assert!(reset.stale);
    assert_eq!(reset.hit, first.hit);

    // 검색 결과의 위치로 기존 History API에서 같은 행을 찾는다.
    let Source::Cold { page } = first_hit.source else {
        unreachable!()
    };
    client
        .write_all(
            &HistoryRequest {
                req: 9,
                source: screen::Source::Cold { page },
                skip: first_hit.total_rows - first_hit.row - 1,
                count: 1,
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    loop {
        let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        if kind == screen::kind::HISTORY_ROWS {
            let history = HistoryRows::decode(kind, &body).unwrap();
            assert_eq!(history.rows, vec![first_hit.cells.clone()]);
            break;
        }
    }
    let korean = ask(&mut client, &request("한글🙂", Direction::Next, None));
    assert_eq!(korean.total, 12_000);
    let hit = korean.hit.unwrap();
    assert_eq!((hit.column, hit.cols), (6, 6));
    assert!(hit.cells.get(6).unwrap().flags.contains(Flags::WIDE));
    drop(client);
    worker.join().unwrap().unwrap();

    // GC로 첫 페이지가 사라지면 같은 출력 seq에서도 cursor를 초기화하고 손실을 알린다.
    let mut segments: Vec<_> = fs::read_dir(store.session_dir(SessionId(1)))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    segments.sort();
    fs::remove_file(segments.get(1).unwrap()).unwrap();
    let hole = krilld::search::search(&tap, &request("needle", Direction::Next, Some(&middle)));
    assert!(hole.incomplete && hole.stale, "중간 세그먼트 손실도 표시");
    assert_eq!(hole.snapshot.oldest.0, 0);
    fs::remove_file(segments.first().unwrap()).unwrap();
    let gc = krilld::search::search(&tap, &request("needle", Direction::Next, Some(&middle)));
    assert!(gc.incomplete && gc.stale);
    assert!(gc.snapshot.oldest.0 > 0);

    // FEAT_SEARCH를 협상하지 않은 구 클라이언트는 전용 kind를 쓸 수 없다.
    let (mut old, worker) = connection(tap, FEATURES & !search::FEAT_SEARCH);
    old.write_all(&request("needle", Direction::Next, None).encode().unwrap())
        .unwrap();
    loop {
        let (kind, body) = ipc::read_frame(&mut old).unwrap().unwrap();
        if kind == screen::kind::REFUSED {
            assert_eq!(Refused::decode(kind, &body).unwrap(), Refused::BadRequest);
            break;
        }
    }
    drop(old);
    worker.join().unwrap().unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hot_fallback_and_single_match_wrap_without_recording() {
    let size = WinSize { cols: 80, rows: 24 };
    let pty = rustix_openpty::openpty(
        None,
        Some(&Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
    )
    .unwrap();
    let mut child = Command::new("/usr/bin/perl")
        .args([
            "-e",
            "print qq(hot needle\\n); print qq(filler\\n) for 1..40",
        ])
        .stdin(Stdio::from(pty.user.try_clone().unwrap()))
        .stdout(Stdio::from(pty.user.try_clone().unwrap()))
        .stderr(Stdio::from(pty.user))
        .spawn()
        .unwrap();
    let session = Session::start(
        SessionId(1),
        pty.controller,
        size,
        Arc::new(Appearance::load(PathBuf::new())),
        None,
    )
    .unwrap();
    assert!(child.wait().unwrap().success());
    let tap = session.tap();
    tap.wait(|pulse| pulse.closed);
    let (mut client, worker) = connection(tap, FEATURES);
    let first = ask(&mut client, &request("needle", Direction::Next, None));
    assert!(first.incomplete && !first.approximate && !first.stale);
    assert_eq!(first.total, 1);
    assert_eq!(first.hit.as_ref().unwrap().source, Source::Hot);
    for direction in [Direction::Next, Direction::Previous] {
        let wrapped = ask(&mut client, &request("needle", direction, Some(&first)));
        assert!(wrapped.wrapped);
        assert_eq!(wrapped.hit, first.hit);
    }
    drop(client);
    worker.join().unwrap().unwrap();
}

#[test]
fn next_previous_keep_cursor_when_output_is_appended() {
    let dir = std::env::temp_dir().join(format!("owe597-cursor-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let store = Store::open(&dir, Limits::default()).unwrap();
    let size = WinSize { cols: 80, rows: 24 };
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let mut output = fs::File::from(pty.user);
    let session = Session::start(
        SessionId(1),
        pty.controller,
        size,
        Arc::new(Appearance::load(PathBuf::new())),
        Some(store.recorder(SessionId(1), size).unwrap()),
    )
    .unwrap();
    let tap = session.tap();
    output
        .write_all(b"needle one\nneedle two\nneedle three\n")
        .unwrap();
    output.write_all(&b"filler\n".repeat(40)).unwrap();
    // 마지막 화면의 filler로 초기 출력이 전부 처리됐음을 확인한다.
    tap.wait(|p| p.end >= 350);
    let mut previous = krilld::search::search(&tap, &request("needle", Direction::Next, None));
    assert_eq!(previous.total, 3);
    assert_eq!(previous.hit.as_ref().unwrap().index, 0);

    for (direction, index, wrapped) in [
        (Direction::Next, 1, false),
        (Direction::Next, 2, false),
        (Direction::Previous, 1, false),
        (Direction::Previous, 0, false),
        (Direction::Previous, 2, true),
        (Direction::Next, 0, true),
    ] {
        let end = tap.pulse().end;
        output.write_all(b"appended filler\n").unwrap();
        tap.wait(|p| p.end > end);
        let next = krilld::search::search(&tap, &request("needle", direction, Some(&previous)));
        assert_ne!(previous.snapshot.seq, next.snapshot.seq);
        assert_ne!(previous.snapshot.revision, next.snapshot.revision);
        assert_eq!(previous.snapshot.oldest, next.snapshot.oldest);
        assert_eq!(previous.snapshot.pages, next.snapshot.pages);
        assert!(!next.stale, "append must preserve the cursor: {next:?}");
        assert_eq!(next.hit.as_ref().unwrap().index, index);
        assert_eq!(next.wrapped, wrapped);
        previous = next;
    }
    drop(output);
    session.wait().unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn search_returns_non_stale_hit_while_output_continues() {
    let dir = std::env::temp_dir().join(format!("mk059-streaming-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let store = Store::open(
        &dir,
        Limits {
            seal_bytes: 16 << 10,
            ..Limits::default()
        },
    )
    .unwrap();
    let size = WinSize { cols: 80, rows: 24 };
    let pty = rustix_openpty::openpty(
        None,
        Some(&Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
    )
    .unwrap();
    let mut child = Command::new("/usr/bin/perl")
        .args([
            "-e",
            "$|=1; print qq(00001 needle-start\\n); for $i (1..4000) { printf qq(%05d filler-line-for-cold-segments\\n), $i } print qq(READY\\n); while (1) { print qq(streaming-output-line\\n); select(undef, undef, undef, 0.001); }",
        ])
        .stdin(Stdio::from(pty.user.try_clone().unwrap()))
        .stdout(Stdio::from(pty.user.try_clone().unwrap()))
        .stderr(Stdio::from(pty.user))
        .spawn()
        .unwrap();
    let session = Session::start(
        SessionId(1),
        pty.controller,
        size,
        Arc::new(Appearance::load(PathBuf::new())),
        Some(store.recorder(SessionId(1), size).unwrap()),
    )
    .unwrap();
    let tap = session.tap();
    tap.wait(|pulse| pulse.end >= 60_000 || pulse.closed);
    assert!(store.history(SessionId(1)).unwrap().len() >= 2);

    for _ in 0..5 {
        let res = krilld::search::search(&tap, &request("needle-start", Direction::Next, None));
        assert!(
            !res.stale,
            "search should not be stale during live output: {res:?}"
        );
        assert!(
            res.hit.is_some(),
            "search should return hit during live output: {res:?}"
        );
    }

    child.kill().ok();
    child.wait().ok();
    fs::remove_dir_all(dir).ok();
}
