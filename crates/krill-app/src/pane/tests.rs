use krill_proto::input::{Fate, Input, InputAck, InputId, InputResult};

use krill_proto::screen::{Op, Revision, Seq, encode_update};

use super::*;

/// AS-27: 데몬이 없으면 디스크 콜드 읽기가 조용히 `None`이다(빈 화면 폴백 유지).
#[test]
fn load_cold_is_none_without_daemon() {
    let dir = std::env::temp_dir().join(format!("mk-pane-cold-{}", std::process::id()));
    let paths = krill_proto::daemon::Paths {
        socket: dir.join("krilld.sock"),
        holder: dir.join("krill-holder"),
        krilld: dir.join("krilld"),
    };
    assert!(load_cold(&paths, SessionId(1)).is_none());
}

fn row(text: &str) -> Vec<Cell> {
    text.chars()
        .map(|ch| Cell {
            ch,
            ..Cell::default()
        })
        .collect()
}

/// `out`에 쓴 프레임들을 꺼낸다.
fn sent(out: &mut Vec<u8>) -> Vec<(u8, Vec<u8>)> {
    let bytes = std::mem::take(out);
    let mut r = &bytes[..];
    std::iter::from_fn(|| ipc::read_frame(&mut r).unwrap()).collect()
}

fn feed(p: &mut Pane<Vec<u8>>, frame: &[u8]) -> Events {
    let (k, b) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
    p.frame(k, &b).unwrap()
}

fn update(p: &mut Pane<Vec<u8>>, rev: u64, ops: Vec<Op>) -> Events {
    let frames = encode_update(Revision(rev), Seq(rev), ops).unwrap();
    frames.iter().map(|f| feed(p, f)).last().unwrap()
}

/// 3행 화면 스냅샷과 그 아래 줄 내용.
fn snapshot(lines: [&str; 3]) -> Vec<Op> {
    let mut ops = vec![Op::Reset { cols: 10, lines: 3 }];
    ops.extend((0u16..).zip(lines).map(|(line, t)| Op::Line {
        line,
        cells: row(t),
    }));
    ops
}

fn linked() -> Pane<Vec<u8>> {
    let mut p = Pane::default();
    assert!(
        p.link(SessionId(1), 9, Vec::new(), input::FEAT_INPUT, Now::get())
            .is_empty()
    );
    p
}

fn out(p: &mut Pane<Vec<u8>>) -> &mut Vec<u8> {
    p.out.as_mut().unwrap()
}

#[test]
fn updates_apply_bell_and_resync_once() {
    let mut p = linked();
    let ev = update(&mut p, 1, snapshot(["$", "", ""]));
    assert!(ev.redraw && !ev.bell);
    assert_eq!(p.shown().rows[0], row("$"));
    assert_eq!(p.known(SessionId(1)).unwrap().revision, Revision(1));
    assert_eq!(p.known(SessionId(2)), None);

    let ev = update(&mut p, 2, vec![Op::Bell, Op::Title("t".into())]);
    assert!(ev.bell && ev.redraw);
    assert_eq!(p.screen().title, "t");

    // 적용할 수 없는 델타 둘 → RESYNC는 한 번만.
    for rev in [3, 4] {
        let ev = update(&mut p, rev, vec![Op::Scroll(5)]);
        assert!(!ev.redraw);
    }
    let frames = sent(out(&mut p));
    assert_eq!(frames, vec![(kind::RESYNC, Vec::new())]);
    assert_eq!(p.known(SessionId(1)), None);
    // 스냅샷이 오면 다시 정상.
    assert!(update(&mut p, 5, snapshot(["a", "b", "c"])).redraw);
    assert!(update(&mut p, 6, vec![Op::Scroll(9)]).eq(&Events::default()));
    assert_eq!(sent(out(&mut p)).len(), 1);
}

#[test]
fn input_goes_through_journal_and_ack() {
    let mut p = Pane::default();
    // 붙기 전 입력은 보류했다가 붙으면 보낸다.
    assert_eq!(p.input(b"ls\r", Now::get()), None);
    let notices = p.link(SessionId(4), 1, Vec::new(), input::FEAT_INPUT, Now::get());
    assert!(notices.is_empty());
    let frames = sent(out(&mut p));
    let first = Input::decode(frames[0].0, &frames[0].1).unwrap();
    assert_eq!(first.bytes, b"ls\r");
    let ack = InputAck {
        id: first.id,
        result: InputResult::Applied,
    };
    let ev = feed(&mut p, &ack.encode().unwrap());
    assert_eq!(ev.notice, None);

    // ack 전에 끊기면 결과를 모른다고 알린다. 다른 세션에 붙으면 보류 입력은 버린다.
    p.input(b"rm x", Now::get());
    let n = p.unlink().unwrap();
    assert_eq!(n.fate, Fate::Uncertain);
    p.input(b"q", Now::get());
    let notices = p.link(SessionId(5), 1, Vec::new(), input::FEAT_INPUT, Now::get());
    assert_eq!(notices[0].fate, Fate::NewSession);
    assert!(sent(out(&mut p)).is_empty());

    // 세션이 끝나 쓰지 않은 입력.
    p.input(b"x", Now::get());
    let frames = sent(out(&mut p));
    let id = Input::decode(frames[0].0, &frames[0].1).unwrap().id;
    assert_eq!(id, InputId(3));
    let ack = InputAck {
        id,
        result: InputResult::Closed,
    };
    let ev = feed(&mut p, &ack.encode().unwrap());
    assert_eq!(ev.notice.unwrap().fate, Fate::SessionClosed);

    p.input(b"observer", Now::get());
    let denied = sent(out(&mut p));
    let id = Input::decode(denied[0].0, &denied[0].1).unwrap().id;
    let ev = feed(
        &mut p,
        &InputAck {
            id,
            result: InputResult::Denied,
        }
        .encode()
        .unwrap(),
    );
    let notice = ev.notice.unwrap();
    assert_eq!(notice.fate, Fate::Unsupported);
    assert_eq!(notice.bytes, b"observer");
}

/// krilld 대신 히스토리 요청에 답한다: 핫 `hot`행(아래부터 "h0", "h1"…), 콜드 페이지는 `pages[i]`(위부터).
fn answer(p: &mut Pane<Vec<u8>>, hot: usize, pages: &[Vec<String>]) {
    for _ in 0..20 {
        let reqs = sent(out(p));
        if reqs.is_empty() {
            return;
        }
        for (k, b) in reqs {
            let r = HistoryRequest::decode(k, &b).unwrap();
            let all: Vec<Vec<Cell>> = match r.source {
                Source::Hot => (0..hot).rev().map(|i| row(&format!("h{i}"))).collect(),
                Source::Cold { page } => pages[page as usize].iter().map(|t| row(t)).collect(),
            };
            let end = all.len().saturating_sub(r.skip as usize);
            // 한 번에 최대 3행만 준다(프레임 맞춤으로 덜 오는 경우).
            let start = end.saturating_sub(usize::from(r.count).min(3));
            let reply = HistoryRows {
                req: r.req,
                total: u32::try_from(all.len()).unwrap(),
                pages: u32::try_from(pages.len()).unwrap(),
                skip: r.skip,
                rows: all[start..end].to_vec(),
            };
            feed(p, &reply.encode().unwrap());
        }
    }
    panic!("history requests did not settle");
}

fn texts(s: &Screen) -> Vec<String> {
    s.rows
        .iter()
        .map(|r| r.iter().map(|c| c.ch).collect())
        .collect()
}

#[test]
fn scroll_fetches_hot_then_cold_without_overlap() {
    let mut p = linked();
    update(&mut p, 1, snapshot(["s0", "s1", "s2"]));
    // 핫 4행(h3이 가장 오래됨). 최신 콜드 페이지는 핫 맨 위 두 줄과 겹친다.
    let pages = vec![
        vec!["c0".to_owned(), "c1".to_owned()],
        vec!["d0".into(), "d1".into(), "h3".into(), "h2".into()],
    ];
    assert!(!p.scroll(2)); // 아직 받은 행이 없다
    answer(&mut p, 4, &pages);
    assert_eq!(texts(&p.shown()), ["h1", "h0", "s0"]);
    assert!(!p.shown().cursor.visible);

    assert!(p.scroll(100));
    answer(&mut p, 4, &pages);
    // 다 받을 때까지 스크롤을 반복한다.
    for _ in 0..10 {
        p.scroll(100);
        answer(&mut p, 4, &pages);
    }
    // 맨 위: 콜드 0쪽 위, 그 아래 콜드 1쪽에서 겹침을 뺀 d0·d1, 그 아래 핫.
    assert_eq!(p.history.phase, Phase::Done);
    let all: Vec<String> = p
        .history
        .rows
        .iter()
        .rev()
        .map(|r| r.iter().map(|c| c.ch).collect())
        .collect();
    assert_eq!(all, ["c0", "c1", "d0", "d1", "h3", "h2", "h1", "h0"]);
    assert_eq!(texts(&p.shown()), ["c0", "c1", "d0"]);

    // 입력하면 바닥으로 돌아가고 받은 행을 버린다.
    p.input(b"x", Now::get());
    assert_eq!(texts(&p.shown()), ["s0", "s1", "s2"]);
    assert!(p.history.rows.is_empty());
    // 늦게 온 답은 버린다.
    let stale = HistoryRows {
        req: 1,
        total: 9,
        pages: 0,
        skip: 0,
        rows: vec![row("zz")],
    };
    feed(&mut p, &stale.encode().unwrap());
    assert!(p.history.rows.is_empty());
}

#[test]
fn overlap_rule() {
    let r = |t: &str| row(t);
    let hot = vec![r("h0"), r("h1"), r("h2")]; // 아래부터, h2가 맨 위
    // 아래부터: 겹친 h1·h2 뒤에 더 오래된 o0.
    assert_eq!(after_overlap(&[r("h1"), r("h2"), r("o0")], &hot), 2);
    // 겹침이 없으면 다 남긴다.
    assert_eq!(after_overlap(&[r("x"), r("y")], &hot), 0);
    assert_eq!(after_overlap(&[r("h2")], &[]), 0);
}

#[test]
fn bad_frames_disconnect() {
    let mut p = linked();
    assert!(p.frame(0x7f, &[]).is_err());
    assert!(p.frame(kind::UPDATE, &[0xff]).is_err());
    assert!(!p.is_closed());
    assert!(p.frame(kind::CLOSED, &[0]).unwrap().closed);
    assert!(p.is_closed());
    p.link(SessionId(2), 9, Vec::new(), input::FEAT_INPUT, Now::get());
    assert!(!p.is_closed());
}

/// MK-032: 콜드 화면은 읽기 전용이고, 받은 스크롤백 안에서 오르내려도(바닥에 와도) 행을 버리지 않는다.
#[test]
fn cold_view_is_read_only_and_keeps_scrollback() {
    let mut p: Pane<Vec<u8>> = Pane::default();
    let cold = Cold {
        screen: snapshot(["s0", "s1", "s2"]),
        history: vec![row("h0"), row("h1")],
    };
    p.show_cold(SessionId(4), cold).unwrap();
    assert!(p.is_cold() && !p.is_linked());
    assert_eq!(p.session(), Some(SessionId(4)));
    assert_eq!(texts(&p.shown()), ["s0", "s1", "s2"]);
    assert!(!p.shown().cursor.visible);
    assert_eq!(p.input(b"x", Now::get()), None);

    assert!(p.scroll(5));
    assert_eq!(texts(&p.shown()), ["h0", "h1", "s0"]);
    assert!(p.scroll(-5));
    assert_eq!(texts(&p.shown()), ["s0", "s1", "s2"]);
    assert!(p.scroll(1));
    assert_eq!(texts(&p.shown()), ["h1", "s0", "s1"]);

    let bad = Cold {
        screen: vec![Op::Scroll(1)],
        history: Vec::new(),
    };
    assert!(
        Pane::<Vec<u8>>::default()
            .show_cold(SessionId(4), bad)
            .is_err()
    );
}

#[test]
fn osc52_write_becomes_clipboard_event() {
    use krill_proto::clipboard::Clipboard;
    let mut p = linked();
    let f = Clipboard {
        text: "복사".to_owned(),
    }
    .encode()
    .unwrap();
    assert_eq!(feed(&mut p, &f).clipboard.as_deref(), Some("복사"));
}

/// OWE-873: 스크롤 위치·받아 둔 행 수 보고와 로컬 스크롤백 비우기.
#[test]
fn scroll_report_and_clear_scrollback() {
    let mut p = linked();
    assert_eq!((p.scroll_offset(), p.scrollback_len()), (0, 0));
    update(&mut p, 1, snapshot(["s0", "s1", "s2"]));
    let pages = vec![vec!["c0".to_owned(), "c1".to_owned()]];
    assert!(!p.scroll(2));
    answer(&mut p, 4, &pages);
    assert_eq!(p.scroll_offset(), 2);
    assert_eq!(p.scrollback_len(), 6);
    // 바닥으로 돌아가면 보고가 0이 되고 받아 둔 행을 버린다.
    assert!(p.scroll(-2));
    assert_eq!((p.scroll_offset(), p.scrollback_len()), (0, 0));

    // 다시 올린 뒤 지우기: 바닥으로 고정하고 받아 둔 행을 버린다.
    assert!(!p.scroll(1));
    answer(&mut p, 4, &pages);
    assert!(p.scroll_offset() > 0);
    p.clear_scrollback();
    assert_eq!((p.scroll_offset(), p.scrollback_len()), (0, 0));
    assert_eq!(texts(&p.shown()), ["s0", "s1", "s2"]);
    // 이미 바닥이면 화면을 건드리지 않는다.
    p.clear_scrollback();
    assert_eq!(texts(&p.shown()), ["s0", "s1", "s2"]);
}

/// OWE-873: 콜드 화면에서도 스크롤 위치를 보고한다(읽기 전용 행은 둔다).
#[test]
fn cold_scroll_report_keeps_rows() {
    let mut p: Pane<Vec<u8>> = Pane::default();
    let cold = Cold {
        screen: snapshot(["s0", "s1", "s2"]),
        history: vec![row("h0"), row("h1")],
    };
    p.show_cold(SessionId(4), cold).unwrap();
    assert_eq!((p.scroll_offset(), p.scrollback_len()), (0, 2));
    assert!(p.scroll(1));
    assert_eq!((p.scroll_offset(), p.scrollback_len()), (1, 2));
    // 콜드는 바닥으로만 돌아가고 행은 버리지 않는다.
    p.clear_scrollback();
    assert_eq!((p.scroll_offset(), p.scrollback_len()), (0, 2));
}

/// 보낸 역할 요청들(다른 프레임은 건너뛴다).
fn role_requests(bytes: &[u8]) -> Vec<roles::Request> {
    let mut r = bytes;
    let mut got = Vec::new();
    while let Some((k, b)) = ipc::read_frame(&mut r).unwrap() {
        if k == roles::kind::ROLE {
            got.push(roles::Request::decode(k, &b).unwrap());
        }
    }
    got
}

#[test]
fn fit_sends_resize_and_focus_once_per_size_and_on_relink() {
    use roles::Request::{Focus, Resize};
    let mut p = Pane::default();
    // 붙기 전에 잰 크기는 붙을 때 보낸다. 범위 밖은 데몬 한도로 자른다.
    assert!(p.fit(0, 5_000).is_none());
    assert_eq!(p.grid(), Some((1, MAX_LINES)));
    assert!(p.fit(120, 40).is_none());
    assert!(
        p.link(
            SessionId(1),
            1,
            Vec::new(),
            input::FEAT_INPUT | FEAT_ROLES,
            Now::get()
        )
        .is_empty()
    );
    let resize = Resize {
        cols: 120,
        rows: 40,
    };
    assert_eq!(role_requests(out(&mut p)), [resize, Focus]);
    // 같은 크기는 다시 보내지 않는다.
    out(&mut p).clear();
    assert!(p.fit(120, 40).is_none());
    assert!(out(&mut p).is_empty());
    assert!(p.fit(90, 20).is_none());
    assert_eq!(
        role_requests(out(&mut p)),
        [Resize { cols: 90, rows: 20 }, Focus]
    );
    // 응답은 연결을 끊지 않는다.
    let ack = roles::Ack::Applied.encode().unwrap();
    let (k, b) = ipc::read_frame(&mut &ack[..]).unwrap().unwrap();
    assert_eq!(p.frame(k, &b).unwrap(), Events::default());
    // 다시 붙으면(새 연결) 같은 크기라도 다시 보낸다. FEAT_ROLES가 없으면 보내지 않는다.
    p.unlink();
    p.link(
        SessionId(1),
        1,
        Vec::new(),
        input::FEAT_INPUT | FEAT_ROLES,
        Now::get(),
    );
    assert_eq!(
        role_requests(out(&mut p)),
        [Resize { cols: 90, rows: 20 }, Focus]
    );
    p.link(SessionId(1), 1, Vec::new(), input::FEAT_INPUT, Now::get());
    assert!(p.fit(70, 10).is_none());
    assert!(role_requests(out(&mut p)).is_empty());
}

/// OWE-579: 쓰기가 실패하면 연결을 닫는다(읽는 쪽이 EOF를 받아 셸이 다시 붙는다). 열어 두면 krilld는 붙은 줄 알고 입력은
/// 영영 보류된다.
#[test]
fn write_failure_closes_the_connection() {
    let (a, b) = UnixStream::pair().unwrap();
    a.shutdown(std::net::Shutdown::Write).unwrap();
    let mut reader = a.try_clone().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut p = Pane::default();
    p.link(
        SessionId(1),
        9,
        Writer::spawn(a).unwrap(),
        input::FEAT_INPUT | FEAT_ROLES,
        Now::get(),
    );
    p.input(b"x", Now::get());
    assert_eq!(std::io::Read::read(&mut reader, &mut [0; 1]).unwrap(), 0);
    drop(b);
}

/// OWE-579: 큰 붙여넣기는 부른 쪽(UI 스레드)을 막지 않는다. krilld가 늦게 읽어도 순서대로 다 간다.
#[test]
fn large_paste_does_not_block_the_caller() {
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    let mut p = Pane::default();
    p.link(
        SessionId(1),
        9,
        Writer::spawn(ours).unwrap(),
        input::FEAT_INPUT | FEAT_ROLES,
        Now::get(),
    );
    let paste: Vec<u8> = (0..1 << 20).map(|i: u32| i.to_le_bytes()[1]).collect();
    let start = std::time::Instant::now();
    assert!(p.input(&paste, Now::get()).is_none());
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "{:?}",
        start.elapsed()
    );
    assert!(p.is_linked());
    std::thread::sleep(Duration::from_millis(300)); // 느린 krilld
    let mut got = Vec::new();
    while got.len() < paste.len() {
        let (k, body) = ipc::read_frame(&mut theirs).unwrap().unwrap();
        got.extend(Input::decode(k, &body).unwrap().bytes);
    }
    assert_eq!(got, paste);
}

/// OWE-583: CLOSED 뒤 마지막 화면을 남기고 입력 소켓을 닫는다.
#[test]
fn closed_session_keeps_screen_and_drops_input() {
    let mut p = linked();
    update(
        &mut p,
        1,
        vec![
            Op::Reset { cols: 4, lines: 1 },
            Op::Title("finished".into()),
        ],
    );
    let before = p.shown();
    assert!(p.frame(kind::CLOSED, &[0]).unwrap().closed);
    assert!(!p.is_linked(), "finished session must detach");
    assert_eq!(p.shown().title, before.title);
    assert!(p.input(b"ignored", Now::get()).is_none());
    assert_eq!(p.journal.held_bytes(), 0);
    assert!(!p.shown().cursor.visible);
}

#[test]
fn search_requests_and_highlights_with_feat_search_check() {
    use crate::terminal::Highlight;
    use krill_proto::search::{
        Cursor, Direction, Hit, SearchRequest, SearchResult, Snapshot, Source as SearchSource,
    };

    let mut p = linked();
    update(&mut p, 1, snapshot(["alpha", "beta hit", "gamma"]));

    // FEAT_SEARCH 미협상 데몬에는 SEARCH 요청을 보내지 않는다.
    p.set_searchable(false);
    assert!(!p.is_searchable());
    assert!(!p.search("hit", Direction::Next));
    assert!(sent(out(&mut p)).is_empty());

    // FEAT_SEARCH가 협상되면 첫 검색은 after: None으로 보낸다.
    p.set_searchable(true);
    assert!(p.is_searchable());
    assert!(p.search("hit", Direction::Next));
    let frames = sent(out(&mut p));
    assert_eq!(frames.len(), 1);
    let req1 = SearchRequest::decode(frames[0].0, &frames[0].1).unwrap();
    assert_eq!(req1.query, "hit");
    assert_eq!(req1.direction, Direction::Next);
    assert_eq!(req1.after, None);

    // 화면(Screen) 일치 응답: 해당 행·열을 하이라이트한다.
    let snap = Snapshot {
        revision: Revision(1),
        seq: Seq(1),
        oldest: Seq(0),
        pages: 0,
    };
    let res1 = SearchResult {
        req: req1.req,
        snapshot: snap,
        total: 2,
        hit: Some(Hit {
            index: 1,
            source: SearchSource::Screen,
            row: 1,
            total_rows: 3,
            column: 5,
            cols: 3,
            cells: row("beta hit"),
        }),
        wrapped: false,
        stale: false,
        incomplete: false,
        approximate: false,
    };
    assert!(feed(&mut p, &res1.encode().unwrap()).redraw);
    assert_eq!(
        p.highlight(),
        Some(Highlight {
            line: 1,
            column: 5,
            cols: 3,
        })
    );
    assert_eq!(p.search_result().unwrap().total, 2);

    // 같은 검색어로 이전(Previous) 탐색: 직전 결과의 Cursor(snapshot, index)를 after에 실어 보낸다.
    assert!(p.search("hit", Direction::Previous));
    let frames = sent(out(&mut p));
    let req2 = SearchRequest::decode(frames[0].0, &frames[0].1).unwrap();
    assert_eq!(req2.direction, Direction::Previous);
    assert_eq!(
        req2.after,
        Some(Cursor {
            snapshot: snap,
            index: 1,
        })
    );

    // 스크롤백(Hot) 일치 응답: 이웃 행을 가져오는 HISTORY 요청을 보내고, 답이 오면
    // 일치 행이 뷰포트 안에 오도록 스크롤 오프셋을 옮겨 앞뒤 문맥과 함께 보여준다(OWE-868).
    // 화면 3행: 위 1행·아래 1행 문맥, 깊이 3 = 4 - 0 - 1, skip = 3 - 1 = 2.
    let res2 = SearchResult {
        req: req2.req,
        snapshot: snap,
        total: 2,
        hit: Some(Hit {
            index: 0,
            source: SearchSource::Hot,
            row: 0,
            total_rows: 4,
            column: 0,
            cols: 3,
            cells: row("hit in hot"),
        }),
        wrapped: false,
        stale: false,
        incomplete: false,
        approximate: false,
    };
    assert!(feed(&mut p, &res2.encode().unwrap()).redraw);
    let frames = sent(out(&mut p));
    assert_eq!(frames.len(), 1);
    let hist_req = HistoryRequest::decode(frames[0].0, &frames[0].1).unwrap();
    assert_eq!(hist_req.source, Source::Hot);
    assert_eq!(hist_req.skip, 2);
    assert_eq!(hist_req.count, 3);
    // 아직 이웃 행이 오기 전에는 활성 하이라이트가 없다.
    assert_eq!(p.highlight(), None);
    let reply = HistoryRows {
        req: hist_req.req,
        total: 4,
        pages: 0,
        skip: 2,
        rows: vec![row("hit in hot"), row("newer")],
    }
    .encode()
    .unwrap();
    assert!(feed(&mut p, &reply).redraw);
    assert_eq!(
        p.shown().rows,
        vec![row("hit in hot"), row("newer"), row("alpha")]
    );
    assert_eq!(
        p.highlight(),
        Some(Highlight {
            line: 0,
            column: 0,
            cols: 3,
        })
    );
    // 화면 안의 모든 일치 범위를 모은다(현재 쿼리 "hit": 첫 행에만 있다).
    assert_eq!(
        p.highlights(),
        &[Highlight {
            line: 0,
            column: 0,
            cols: 3,
        }]
    );

    // 검색 닫기(clear_search): 하이라이트·일치 범위·고정 보기를 지우고 라이브 화면으로 돌아온다.
    p.clear_search();
    assert_eq!(p.highlight(), None);
    assert!(p.highlights().is_empty());
    assert_eq!(p.shown().rows[0], row("alpha"));
}

#[test]
fn search_cold_match_installs_fixed_view_with_context() {
    use crate::terminal::Highlight;
    use krill_proto::search::{
        Direction, Hit, SearchRequest, SearchResult, Snapshot, Source as SearchSource,
    };

    let mut p = linked();
    update(&mut p, 1, snapshot(["alpha", "beta hit", "gamma"]));
    p.set_searchable(true);
    assert!(p.search("hit", Direction::Next));
    let frames = sent(out(&mut p));
    let req = SearchRequest::decode(frames[0].0, &frames[0].1).unwrap();
    // 콜드 페이지 2의 5행 일치: 화면 3행이면 위 1·아래 1 문맥, skip = (10 - 5 - 1) - 1 = 3.
    let snap = Snapshot {
        revision: Revision(1),
        seq: Seq(1),
        oldest: Seq(0),
        pages: 3,
    };
    let res = SearchResult {
        req: req.req,
        snapshot: snap,
        total: 7,
        hit: Some(Hit {
            index: 0,
            source: SearchSource::Cold { page: 2 },
            row: 5,
            total_rows: 10,
            column: 1,
            cols: 2,
            cells: row("cold-hit"),
        }),
        wrapped: false,
        stale: false,
        incomplete: false,
        approximate: false,
    };
    assert!(feed(&mut p, &res.encode().unwrap()).redraw);
    let frames = sent(out(&mut p));
    assert_eq!(frames.len(), 1);
    let hist_req = HistoryRequest::decode(frames[0].0, &frames[0].1).unwrap();
    assert_eq!(hist_req.source, Source::Cold { page: 2 });
    assert_eq!(hist_req.skip, 3);
    assert_eq!(hist_req.count, 3);
    // 답 끝이 출처 끝에서 10 - 3 - 1 = 6행째라 일치(5행)는 끝에서 두 번째다.
    let reply = HistoryRows {
        req: hist_req.req,
        total: 10,
        pages: 3,
        skip: 3,
        rows: vec![row("cold-a"), row("cold-hit"), row("cold-b")],
    }
    .encode()
    .unwrap();
    assert!(feed(&mut p, &reply).redraw);
    assert_eq!(
        p.shown().rows,
        vec![row("cold-a"), row("cold-hit"), row("cold-b")]
    );
    assert_eq!(
        p.highlight(),
        Some(Highlight {
            line: 1,
            column: 1,
            cols: 2,
        })
    );
    // 손 스크롤은 고정 보기를 걷고 라이브로 돌아간다.
    assert!(p.scroll(1));
    assert_eq!(p.highlight(), None);
    assert_eq!(p.shown().rows[0], row("alpha"));
}

#[test]
fn search_highlights_all_visible_matches() {
    use crate::terminal::Highlight;
    use krill_proto::search::{
        Direction, Hit, SearchRequest, SearchResult, Snapshot, Source as SearchSource,
    };

    let mut p = linked();
    update(&mut p, 1, snapshot(["hit hit", "no", "hit"]));
    p.set_searchable(true);
    assert!(p.search("hit", Direction::Next));
    let frames = sent(out(&mut p));
    let req = SearchRequest::decode(frames[0].0, &frames[0].1).unwrap();
    let res = SearchResult {
        req: req.req,
        snapshot: Snapshot::default(),
        total: 3,
        hit: Some(Hit {
            index: 1,
            source: SearchSource::Screen,
            row: 2,
            total_rows: 3,
            column: 0,
            cols: 3,
            cells: row("hit"),
        }),
        wrapped: false,
        stale: false,
        incomplete: false,
        approximate: false,
    };
    assert!(feed(&mut p, &res.encode().unwrap()).redraw);
    // 활성 일치는 2행, 화면 안의 모든 일치는 0행 2곳 + 2행 1곳이다.
    assert_eq!(
        p.highlight(),
        Some(Highlight {
            line: 2,
            column: 0,
            cols: 3,
        })
    );
    assert_eq!(
        p.highlights(),
        &[
            Highlight {
                line: 0,
                column: 0,
                cols: 3,
            },
            Highlight {
                line: 0,
                column: 4,
                cols: 3,
            },
            Highlight {
                line: 2,
                column: 0,
                cols: 3,
            },
        ]
    );
}

#[test]
fn find_matches_maps_cells_like_daemon() {
    // 넓은 문자(다음 칸 SPACER)는 한 번에 2칸으로 잡고, 겹치지 않게 찾는다.
    let wide = Cell {
        ch: '한',
        flags: Flags::WIDE,
        ..Cell::default()
    };
    let spacer = Cell {
        ch: ' ',
        flags: Flags::SPACER,
        ..Cell::default()
    };
    assert_eq!(
        find_matches(&[wide.clone(), spacer.clone()], "한"),
        vec![(0, 2)]
    );
    assert!(find_matches(&[wide, spacer], " ").is_empty());
    assert_eq!(find_matches(&row("hit hit"), "hit"), vec![(0, 3), (4, 3)]);
    assert_eq!(find_matches(&row("aaa"), "aa"), vec![(0, 2)]);
    assert!(find_matches(&row("abc"), "").is_empty());
}

#[test]
fn context_latest_rows_ignore_viewport_and_preserve_scroll() {
    let mut p = linked();
    update(&mut p, 1, snapshot(["최신1", "최신2", ""]));
    assert_eq!(p.copy_context(1).unwrap().as_deref(), Some("최신2"));
    p.scroll(1);
    let scroll_req = HistoryRequest::decode(kind::HISTORY, &sent(out(&mut p))[0].1).unwrap();
    assert_eq!(p.copy_context(3).unwrap(), None);
    let req = HistoryRequest::decode(kind::HISTORY, &sent(out(&mut p))[0].1).unwrap();
    assert_ne!(req.req, scroll_req.req);
    let reply = |req, rows| {
        HistoryRows {
            req,
            rows,
            total: 2,
            pages: 0,
            skip: 0,
        }
        .encode()
        .unwrap()
    };
    // 스크롤 응답은 복사 작업을 완료시키지 않는다.
    let ev = feed(
        &mut p,
        &reply(scroll_req.req, vec![row("old1"), row("old2")]),
    );
    assert_eq!(ev.clipboard, None);
    let before = p.shown();
    let ev = feed(&mut p, &reply(req.req, vec![row("old2")]));
    assert_eq!(ev.clipboard.as_deref(), Some("old2\n최신1\n최신2"));
    assert_eq!(p.shown(), before);
    assert!(p.context.is_none());
}

#[test]
fn context_retries_short_history_and_cancels_on_disconnect() {
    let mut p = linked();
    update(&mut p, 1, snapshot(["now", "", ""]));
    assert_eq!(p.copy_context(4).unwrap(), None);
    let mut request = HistoryRequest::decode(kind::HISTORY, &sent(out(&mut p))[0].1).unwrap();
    let reply = |req, skip, total, rows| {
        HistoryRows {
            req,
            skip,
            total,
            rows,
            pages: 0,
        }
        .encode()
        .unwrap()
    };
    assert_eq!(
        feed(&mut p, &reply(request.req, 0, 3, vec![row("3")])).clipboard,
        None
    );
    request = HistoryRequest::decode(kind::HISTORY, &sent(out(&mut p))[0].1).unwrap();
    assert_eq!(request.skip, 1);
    let ev = feed(&mut p, &reply(request.req, 1, 3, vec![row("1"), row("2")]));
    assert_eq!(ev.clipboard.as_deref(), Some("1\n2\n3\nnow"));
    assert_eq!(p.copy_context(8).unwrap(), None);
    request = HistoryRequest::decode(kind::HISTORY, &sent(out(&mut p))[0].1).unwrap();
    p.unlink();
    assert_eq!(
        feed(&mut p, &reply(request.req, 0, 1, vec![row("late")])).clipboard,
        None
    );
}

#[test]
fn context_cold_fixed_wide_wrap_and_zero_count() {
    let mut p = Pane::<Vec<u8>>::default();
    p.view.screen = Screen {
        cols: 4,
        lines: 3,
        rows: vec![row("abcd"), row("한"), vec![]],
        ..Screen::default()
    };
    p.view.screen.rows[0][3].flags = krill_proto::screen::Flags::WRAPLINE;
    p.history = History::fixed(vec![row("old")]);
    p.state = State::Cold;
    assert_eq!(p.copy_context(4).unwrap().as_deref(), Some("old\nabcd한"));
    assert_eq!(p.copy_context(0).unwrap().as_deref(), Some("한"));
    assert!(p.out.is_none());
}

#[test]
fn context_fetches_cold_and_deduplicates_snapshot_without_hot_rows() {
    let mut p = linked();
    update(&mut p, 1, snapshot(["now", "", ""]));
    assert_eq!(p.copy_context(3).unwrap(), None);
    let request =
        |p: &mut Pane<Vec<u8>>| HistoryRequest::decode(kind::HISTORY, &sent(out(p))[0].1).unwrap();
    let req = request(&mut p);
    let hot = HistoryRows {
        req: req.req,
        skip: 0,
        total: 0,
        pages: 1,
        rows: vec![],
    };
    assert_eq!(feed(&mut p, &hot.encode().unwrap()).clipboard, None);
    let req = request(&mut p);
    assert_eq!(req.source, Source::Cold { page: 0 });
    let cold = HistoryRows {
        req: req.req,
        skip: 0,
        total: 3,
        pages: 1,
        rows: vec![row("old1"), row("old2"), row("now")],
    };
    assert_eq!(
        feed(&mut p, &cold.encode().unwrap()).clipboard.as_deref(),
        Some("old1\nold2\nnow")
    );
    assert!(p.context.is_none());
}

#[test]
fn context_repeated_copy_ignores_old_response_and_caps_lines() {
    let mut p = linked();
    update(&mut p, 1, snapshot(["now", "", ""]));
    assert_eq!(p.copy_context(2).unwrap(), None);
    let old = HistoryRequest::decode(kind::HISTORY, &sent(out(&mut p))[0].1).unwrap();
    assert_eq!(p.copy_context(3).unwrap(), None);
    let new = HistoryRequest::decode(kind::HISTORY, &sent(out(&mut p))[0].1).unwrap();
    assert_ne!(old.req, new.req);
    let reply = |req| {
        HistoryRows {
            req,
            skip: 0,
            total: 2,
            pages: 0,
            rows: vec![row("1"), row("2")],
        }
        .encode()
        .unwrap()
    };
    assert_eq!(feed(&mut p, &reply(old.req)).clipboard, None);
    assert_eq!(
        feed(&mut p, &reply(new.req)).clipboard.as_deref(),
        Some("1\n2\nnow")
    );
    assert_eq!(p.copy_context(u16::MAX).unwrap(), None);
    let req = HistoryRequest::decode(kind::HISTORY, &sent(out(&mut p))[0].1).unwrap();
    assert_eq!(req.count, crate::commands::MAX_CONTEXT_LINES - 1);
}

#[test]
fn stalled_input_clears_on_ack_and_never_replays() {
    let mut p = linked();
    let now = Now::get();
    p.input(b"command\r", now);
    let later = Now {
        awake: now.awake + input::PROBE,
        ..now
    };
    assert!(p.stalled(later));
    let frames = sent(out(&mut p));
    let (k, b) = &frames[0];
    let input = Input::decode(*k, b).unwrap();
    feed(
        &mut p,
        &InputAck {
            id: input.id,
            result: InputResult::Applied,
        }
        .encode()
        .unwrap(),
    );
    assert!(!p.stalled(later));
    assert!(sent(out(&mut p)).is_empty());
    p.input(b"uncertain\r", now);
    assert!(p.stalled(later));
    assert_eq!(p.unlink().unwrap().fate, Fate::Uncertain);
    assert!(!p.stalled(later));
    p.link(SessionId(1), 9, Vec::new(), input::FEAT_INPUT, later);
    assert!(sent(out(&mut p)).is_empty());
}

#[test]
fn missing_input_feature_notifies_without_sending_or_waiting_for_ack() {
    let mut p = Pane::default();
    p.link(SessionId(1), 9, Vec::new(), FEAT_SCREEN, Now::get());
    assert!(p.input(b"ls\r", Now::get()).is_some());
    assert!(sent(out(&mut p)).is_empty());
    assert!(!p.journal.stalled(Now {
        awake: std::time::Instant::now() + Duration::from_secs(10),
        ..Now::get()
    }));
    assert!(p.unlink().is_none());
}

#[test]
fn missing_input_feature_discards_held_input_with_notice() {
    let mut p = Pane::default();
    assert!(p.input(b"held", Now::get()).is_none());
    let notices = p.link(SessionId(1), 9, Vec::new(), FEAT_SCREEN, Now::get());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].bytes, b"held");
    assert!(sent(out(&mut p)).is_empty());
    assert_eq!(p.journal.held_bytes(), 0);
    assert!(p.unlink().is_none());
    assert!(
        p.link(SessionId(1), 9, Vec::new(), input::FEAT_INPUT, Now::get())
            .is_empty()
    );
    assert!(sent(out(&mut p)).is_empty());
    p.input(b"fresh", Now::get());
    let frames = sent(out(&mut p));
    assert_eq!(
        Input::decode(frames[0].0, &frames[0].1).unwrap().bytes,
        b"fresh"
    );
}

#[test]
fn ensure_session_in_refuses_fork_without_feat_session_fork() {
    use std::os::unix::net::UnixListener;
    let dir = std::env::temp_dir().join(format!("mk-pane-fork-feat-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let sock_path = dir.join("krilld.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();
    let server = std::thread::spawn(move || {
        let hello = ipc::ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features: FEAT_CONTROL,
            build: env!("CARGO_PKG_VERSION").into(),
            instance_id: 1,
        };
        let (mut watch, _) = listener.accept().unwrap();
        watch.write_all(&hello.encode().unwrap()).unwrap();
        let (k, b) = ipc::read_frame(&mut watch).unwrap().unwrap();
        ipc::ClientHello::decode(k, &b).unwrap();
        drop(watch);

        let (mut s, _) = listener.accept().unwrap();
        s.write_all(&hello.encode().unwrap()).unwrap();
        let (k, b) = ipc::read_frame(&mut s).unwrap().unwrap();
        ipc::ClientHello::decode(k, &b).unwrap();
        // 구 데몬에는 문자열 id("fork:...")를 담은 Request::Create를 보내지 않고 닫아야 한다.
        ipc::read_frame(&mut s).unwrap()
    });
    let paths = krill_proto::daemon::Paths {
        socket: sock_path,
        holder: dir.join("krill-holder"),
        krilld: dir.join("krilld"),
    };
    let err = ensure_session_in(
        &paths,
        None,
        &Create {
            account: None,
            program: Program::Agent {
                id: "fork:claude:42".into(),
            },
            cwd: Vec::new(),
            cols: 80,
            rows: 24,
        },
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert!(err.to_string().contains("update the daemon"), "{err}");
    assert!(server.join().unwrap().is_none());
    std::fs::remove_dir_all(&dir).ok();
}

/// OWE-842 시나리오 B: 죽은 세션의 콜드 스크롤백 + 마지막 화면이 새 라이브 세션 연결 뒤에도
/// 스크롤백 위쪽에 보존되어 위로 스크롤하면 이전 세션 로그를 볼 수 있다.
#[test]
fn owe_842_cold_to_live_preserves_prior_scrollback_and_screen_above_new_session() {
    let mut p = Pane::<Vec<u8>>::default();
    p.show_cold(
        SessionId(10),
        Cold {
            screen: snapshot(["cold-scr1", "cold-scr2", ""]),
            history: vec![row("cold-hist1"), row("cold-hist2")],
        },
    )
    .unwrap();
    assert!(p.is_cold());

    // 새 라이브 세션(SessionId(11))으로 전환하면 직전 콜드 히스토리 2행 + 콜드 화면 비어있지 않은 2행 = 총 4행이 prior로 보존된다.
    p.link(SessionId(11), 1, Vec::new(), input::FEAT_INPUT, Now::get());
    assert!(!p.is_cold());
    update(&mut p, 1, snapshot(["live-1", "live-2", "live-3"]));
    assert_eq!(p.shown().rows[0], row("live-1"));

    // 위로 2행 스크롤하면 prior의 아랫부분(cold-scr1, cold-scr2)과 live-1이 보인다.
    assert!(p.scroll(2));
    let shown2 = p.shown();
    assert_eq!(
        shown2.rows,
        vec![row("cold-scr1"), row("cold-scr2"), row("live-1")]
    );

    // 위로 끝까지(총 4행) 스크롤하면 가장 오래된 콜드 스크롤백부터 보인다.
    assert!(p.scroll(10));
    let shown4 = p.shown();
    assert_eq!(
        shown4.rows,
        vec![row("cold-hist1"), row("cold-hist2"), row("cold-scr1")]
    );
}
