//! 화면·원본 전송 서버(MK-023). 와이어 포맷·흐름·두 번호(원본 seq, 화면 revision)의 정의는 [`krill_proto::screen`].
//!
//! - 연결마다 요청·쓰기 스레드. 첫 검색 때 별도 검색 스레드를 쓴다(대기 요청은 최신 하나).
//!   [`serve`]를 부른 스레드가 요청(RESYNC·HISTORY)을 읽고, 쓰기 스레드가 화면 델타나 원본을 보낸다.
//! - 쓰기 스레드는 세션 [`Tap`]을 기다렸다가 지금 화면을 떠서 "클라이언트가 가진 화면"과의 차이만 보낸다(당김 방식,
//!   연결별 큐 = 프레임 하나). 원본 스트림은 세션 링 → 디스크 → Gap 순서로 이어 준다. PTY 리더는 아무도 기다리지 않는다.
//! - 델타는 행 교체 + 화면 전체 행 이동(MK-010 결정 2). 이동량은 행 해시로 고르고, 이동한 뒤 실제 행 값을 비교해
//!   다른 행만 보내므로 정확성은 해시에 기대지 않는다.
//! - 화면 부가 정보(제목·벨·커서 모양·링크 URI, OWE-490)는 같은 diff에서 나오고, 클라이언트가
//!   [`FEAT_SCREEN_META`]를 협상하지 않았으면 걸러 낸다.
//! - OSC 52 쓰기(MK-033, [`krill_proto::clipboard`]): 화면 프레임을 뜰 때 엔진의 쓰기 수가 늘었으면 마지막 값을 보낸다.
//! - 입력(MK-026, [`krill_proto::input`]): 요청 스레드가 [`Input`]을 받은 순서대로 PTY에 쓰고 하나마다 [`InputAck`]로
//!   답한다. 다시 보내기·중복 제거는 클라이언트 장부가 맡는다(한 번 보낸 입력은 다시 오지 않는다).
//! - 역할(MK-050, [`krill_proto::roles`]): observer는 읽기만 한다. 세션별 입력 잠금으로 입력 프레임 전체를 직렬화하고,
//!   마지막 입력·포커스한 interactive의 뷰포트를 PTY와 에뮬레이터에 적용한다.
//! - 감시 연결(MK-026): 화면 feature 없이 협상만 한 연결은 클라이언트가 닫을 때까지 열어 둔다. krilld가 죽으면
//!   커널이 닫아 앱이 바로 안다.
//!
//! 잠정 결정 의존: 링 밖 원본 이어받기와 콜드 페이지는 MK-024 세그먼트 저장소(`store.rs`의 `Store::raw`·`History`)를
//! 쓴다. 거부되면 이 파일의 `Read::Disk`·`Source::Cold` 처리와 `Store::raw`만 걷어 낸다. 키프레임은 클라이언트
//! 전송용(보이는 화면)이라 MK-006 포크(데몬 권위 키프레임: 파서 꼬리·모드)에 기대지 않는다.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::net::Shutdown;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::clipboard::{Clipboard, FEAT_CLIPBOARD};
use krill_proto::input::{self, FEAT_INPUT, Input, InputAck, InputResult};
use krill_proto::ipc::{self, ServerHello};
use krill_proto::roles::{self as role_proto, FEAT_ROLES, Role};
use krill_proto::screen::{
    Attach, Attached, Cell, Closed, CursorStyle, FEAT_SCREEN, FEAT_SCREEN_META, GapMsg,
    HistoryRequest, HistoryRows, Known, Link, MAX_HISTORY_ROWS, MAX_OUTPUT, Msg, Op, Refused,
    Revision, Screen, Seq, Source, Stream, kind,
};

use krill_proto::search::{self, FEAT_SEARCH, SearchRequest};

use crate::session::lock;
pub use crate::session::{Pulse, Read, Tap};
use crate::store::Piece;

pub(crate) mod roles;
mod search_worker;

/// 계속 출력 중일 때 화면 프레임 사이 최소 간격(약 120Hz). 쉬다가 처음 바뀐 화면은 바로 보낸다(입력 에코).
// ponytail: 고정값. 앱 vsync 합치기(MK-027)와 맞춰 볼 필요가 생기면 클라이언트가 Attach에 싣는다.
const FRAME_MIN: Duration = Duration::from_millis(8);
/// 클라이언트가 이만큼 읽지 않으면 끊는다(재접속하면 스냅샷부터).
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Hello 뒤 첫 요청([`Attach`] 등)을 기다리는 최대 시간.
pub(crate) const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

/// 연결 하나의 쓰기 쪽과 요청 스레드가 쓰기 스레드에 거는 신호.
struct Conn {
    /// 프레임 단위로 잠가 요청 응답과 델타가 섞이지 않게 한다.
    out: Mutex<UnixStream>,
    resync: AtomicBool,
    gone: AtomicBool,
}

impl Conn {
    fn send(&self, frame: &[u8]) -> io::Result<()> {
        lock(&self.out).write_all(frame)
    }

    fn update(&self, revision: Revision, seq: Seq, ops: Vec<Op>) -> io::Result<()> {
        let frames = krill_proto::screen::encode_update(revision, seq, ops)?;
        let mut out = lock(&self.out);
        frames.iter().try_for_each(|f| out.write_all(f))
    }

    fn gone(&self) -> bool {
        self.gone.load(Ordering::SeqCst)
    }

    fn flagged(&self) -> bool {
        self.gone() || self.resync.load(Ordering::SeqCst)
    }
}

/// 연결 하나를 끝까지 처리한다: Hello 협상 → [`Attach`] → 스트림. `lookup`은 세션 id로 [`Tap`]을 찾는다.
/// 클라이언트가 [`FEAT_SCREEN`]을 협상하지 않았으면 감시 연결이다: 클라이언트가 닫을 때까지 열어 두고, 오는
/// 프레임에는 [`Refused::BadRequest`]로 답한다.
pub fn serve(
    mut stream: UnixStream,
    hello: &ServerHello,
    lookup: impl FnOnce(SessionId) -> Option<Tap>,
) -> io::Result<()> {
    let (_, negotiated) = crate::ipc::handshake(&mut stream, hello)?;
    if negotiated.features & FEAT_SCREEN == 0 {
        return watch(stream);
    }
    stream.set_read_timeout(Some(ATTACH_TIMEOUT))?;
    let (role, first) = roles::first(&mut stream, negotiated.features)?;
    let Some((frame_kind, body)) = first else {
        return Ok(()); // 협상만 하고 닫은 클라이언트
    };
    let attach = Attach::decode(frame_kind, &body)?;
    let tap = lookup(attach.session);
    run_as(
        stream,
        attach,
        hello.instance_id,
        negotiated.features,
        tap,
        role,
    )
}

/// 감시 연결(MK-026, OWE-586): 클라이언트가 닫을 때까지 열어 두고, 오는 프레임에는 [`Refused::BadRequest`]로
/// 답한다. [`krill_proto::daemon::PING`]에는 nonce를 되돌린 [`krill_proto::daemon::PONG`]으로 답해 EOF 없이 멈춘
/// 데몬 감시(앱 감시 연결 하트비트)에 응한다.
/// krilld가 죽으면 커널이 닫아 클라이언트가 바로 안다.
pub fn watch(mut stream: UnixStream) -> io::Result<()> {
    stream.set_read_timeout(None)?;
    while let Some((kind, body)) = ipc::read_frame(&mut stream)? {
        if kind == krill_proto::daemon::PING
            && let Ok(nonce) = krill_proto::daemon::ping_nonce(kind, &body)
        {
            stream.write_all(&krill_proto::daemon::pong(nonce)?)?;
        } else {
            stream.write_all(&Refused::BadRequest.encode()?)?;
        }
    }
    Ok(())
}

/// [`Attach`]를 받은 뒤의 스트림. `tap`이 `None`이면(없는 세션·권한 밖) [`Refused::NoSession`]을 보내고 닫는다.
/// `instance`는 이 krilld의 `ServerHello::instance_id`, `features`는 협상한 feature 비트(입력은 [`FEAT_INPUT`]이
/// 있을 때만 받는다).
pub fn run(
    stream: UnixStream,
    attach: Attach,
    instance: u64,
    features: u64,
    tap: Option<Tap>,
) -> io::Result<()> {
    run_as(stream, attach, instance, features, tap, Role::Interactive)
}

pub(crate) fn run_as(
    mut stream: UnixStream,
    attach: Attach,
    instance: u64,
    features: u64,
    tap: Option<Tap>,
    role: Role,
) -> io::Result<()> {
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let Some(tap) = tap else {
        stream.write_all(&Refused::NoSession.encode()?)?;
        return Ok(());
    };
    let client = roles::Client::new(tap.clone(), role)?;
    let conn = Arc::new(Conn {
        out: Mutex::new(stream.try_clone()?),
        resync: AtomicBool::new(false),
        gone: AtomicBool::new(false),
    });
    let p = tap.pulse();
    conn.send(
        &Attached {
            seq: Seq(p.end),
            revision: Revision(p.revision),
        }
        .encode()?,
    )?;
    let writer = thread::Builder::new()
        .name(format!("attach-{}", attach.session.0))
        .spawn({
            let (tap, conn) = (tap.clone(), Arc::clone(&conn));
            move || {
                if matches!(role, Role::InteractiveWithSize { .. }) {
                    crate::alloc::mobile_thread().ok();
                }
                let r = match attach.stream {
                    Stream::Screen { known } => {
                        // 화면 구독 동안 핫 스크롤백을 쌓는다(MK-035). 놓으면 유예 뒤 강등.
                        let _viewer = tap.watch();
                        screen_loop(&tap, &conn, known, instance, features)
                    }
                    Stream::Raw { from } => raw_loop(&tap, &conn, from),
                };
                if r.is_err() {
                    // 요청 스레드의 read를 풀어 연결을 끝낸다.
                    lock(&conn.out).shutdown(Shutdown::Both).ok();
                }
                r
            }
        })?;
    let r = requests(&mut stream, &tap, &conn, attach.stream, features, &client);
    drop(client);
    conn.gone.store(true, Ordering::SeqCst);
    tap.wake();
    let w = writer
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("attach writer panicked")));
    tap.finish_input();
    r.and(w)
}

/// 클라이언트 요청을 EOF까지 읽는다. `features`는 협상한 feature 비트.
fn requests(
    s: &mut UnixStream,
    tap: &Tap,
    conn: &Arc<Conn>,
    stream: Stream,
    features: u64,
    client: &roles::Client,
) -> io::Result<()> {
    let mut search: Option<search_worker::Worker> = None;
    while let Some((k, body)) = ipc::read_frame(s)? {
        match (k, stream) {
            (role_proto::kind::ROLE, _) if features & FEAT_ROLES != 0 => {
                let req = role_proto::Request::decode(k, &body)?;
                conn.send(&client.request(req).encode()?)?;
            }
            (input::kind::INPUT, _) if features & FEAT_INPUT != 0 => {
                let i = Input::decode(k, &body)?;
                let result = match client.input(&i.bytes, s.as_fd()) {
                    Ok(()) => InputResult::Applied,
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => InputResult::Denied,
                    Err(_) => InputResult::Closed,
                };
                conn.send(&InputAck { id: i.id, result }.encode()?)?;
            }
            (search::kind::SEARCH, Stream::Screen { .. }) if features & FEAT_SEARCH != 0 => {
                let req = SearchRequest::decode(k, &body)?;
                if search.is_none() {
                    search = Some(search_worker::Worker::start(tap.clone(), Arc::clone(conn))?);
                }
                if let Some(worker) = &search {
                    worker.submit(req);
                }
            }
            (kind::RESYNC, Stream::Screen { .. }) => {
                conn.resync.store(true, Ordering::SeqCst);
                tap.wake();
            }
            (kind::HISTORY, Stream::Screen { .. }) => {
                let req = HistoryRequest::decode(k, &body)?;
                conn.send(&history(tap, &req).encode()?)?;
            }
            _ => conn.send(&Refused::BadRequest.encode()?)?,
        }
    }
    Ok(())
}

/// 지연 히스토리: 요청한 범위의 스크롤백 행. 콜드 페이지를 못 읽으면 빈 응답.
fn history(tap: &Tap, req: &HistoryRequest) -> HistoryRows {
    let count = usize::from(req.count.min(MAX_HISTORY_ROWS));
    let skip = usize::try_from(req.skip).unwrap_or(usize::MAX);
    let cold = tap.store().and_then(|s| s.history(tap.id()).ok());
    let (total, rows) = match req.source {
        Source::Hot => tap.history(skip, count),
        Source::Cold { page } => cold
            .as_ref()
            .and_then(|history| history.rows(usize::try_from(page).ok()?, skip, count).ok())
            .unwrap_or_default(),
    };
    let mut reply = HistoryRows {
        req: req.req,
        total: u32::try_from(total).unwrap_or(u32::MAX),
        pages: cold.map_or(0, |h| u32::try_from(h.len()).unwrap_or(u32::MAX)),
        skip: req.skip,
        rows,
    };
    reply.fit();
    reply
}

/// `meta`: 클라이언트가 [`FEAT_SCREEN_META`]를 협상했다(아니면 그 연산을 걸러 낸다).
fn screen_loop(
    tap: &Tap,
    conn: &Conn,
    known: Option<Known>,
    instance: u64,
    features: u64,
) -> io::Result<()> {
    let meta = features & FEAT_SCREEN_META != 0;
    let modes_enabled = features & krill_proto::term_modes::FEAT_TERM_MODES != 0;
    let clip_enabled = features & FEAT_CLIPBOARD != 0;
    // 붙기 전에 쓴 클립보드 값은 보내지 않는다.
    let (mut clips, _) = tap.clipboard(0);
    let keep = |mut ops: Vec<Op>| {
        if !meta {
            ops.retain(|op| !op.is_meta());
        }
        ops
    };
    let (mut sent, rev, seq, mut modes) = tap.capture_with_modes();
    if modes_enabled {
        conn.send(&modes.encode()?)?;
    }
    let mut seen = rev.0;
    let mut last = None;
    if known
        != Some(Known {
            instance,
            revision: rev,
        })
    {
        conn.update(rev, seq, keep(snapshot(&sent)))?;
        last = Some(Instant::now());
    }
    let mut closed_sent = false;
    loop {
        let p = tap.wait(|p| p.revision > seen || (p.closed && !closed_sent) || conn.flagged());
        if conn.gone() {
            return Ok(());
        }
        let resync = conn.resync.swap(false, Ordering::SeqCst);
        if let Some(wait) = last
            .map(|t: Instant| (t + FRAME_MIN).saturating_duration_since(Instant::now()))
            .filter(|w| !resync && !w.is_zero())
        {
            thread::sleep(wait);
        }
        let (next, rev, seq, next_modes) = tap.capture_with_modes();
        if modes_enabled && (resync || modes != next_modes) {
            conn.send(&next_modes.encode()?)?;
        }
        modes = next_modes;
        if clip_enabled {
            let (n, text) = tap.clipboard(clips);
            if let Some(text) = text {
                conn.send(&Clipboard { text }.encode()?)?;
            }
            clips = n;
        }
        let ops = keep(if resync {
            snapshot(&next)
        } else {
            diff(&sent, &next)
        });
        if !ops.is_empty() {
            conn.update(rev, seq, ops)?;
            last = Some(Instant::now());
        }
        sent = next;
        seen = rev.0;
        if p.closed && !closed_sent {
            // 리더가 끝난 뒤 뜬 화면이 마지막 화면이다.
            conn.send(&Closed { seq }.encode()?)?;
            closed_sent = true;
        }
    }
}

fn raw_loop(tap: &Tap, conn: &Conn, from: Seq) -> io::Result<()> {
    let end = tap.pulse().end;
    let mut pos = from.0;
    if pos > end {
        // krilld가 크래시해 디스크에 못 쓴 원본이 있었다: 그 뒤 seq는 디스크 끝에서 다시 매겨졌다.
        conn.send(
            &GapMsg {
                start: Seq(end),
                len: 0,
            }
            .encode()?,
        )?;
        pos = end;
    }
    loop {
        if conn.gone() {
            return Ok(());
        }
        match tap.read(pos, MAX_OUTPUT) {
            Read::Bytes(b) => {
                conn.send(&krill_proto::screen::encode_output(Seq(pos), &b)?)?;
                pos += b.len() as u64;
            }
            Read::Disk { to } => {
                tap.disk(pos, to, |piece| match piece {
                    Piece::Data { seq, bytes } => {
                        let mut at = seq;
                        for chunk in bytes.chunks(MAX_OUTPUT) {
                            conn.send(&krill_proto::screen::encode_output(Seq(at), chunk)?)?;
                            at += chunk.len() as u64;
                        }
                        Ok(())
                    }
                    Piece::Gap { seq, len } => conn.send(
                        &GapMsg {
                            start: Seq(seq),
                            len,
                        }
                        .encode()?,
                    ),
                })?;
                pos = to;
            }
            Read::Idle(p) if p.closed => {
                conn.send(&Closed { seq: Seq(pos) }.encode()?)?;
                tap.wait(|_| conn.gone());
                return Ok(());
            }
            Read::Idle(_) => {
                tap.wait(|p| p.end > pos || p.closed || conn.flagged());
            }
        }
    }
}

/// 키프레임: 빈 화면에서 시작해 `s`를 만드는 연산. 부가 정보는 기본값이 아닌 것만, 벨은 넣지 않는다.
pub(crate) fn snapshot(s: &Screen) -> Vec<Op> {
    let mut ops = vec![Op::Reset {
        cols: s.cols,
        lines: s.lines,
    }];
    if !s.title.is_empty() {
        ops.push(Op::Title(s.title.clone()));
    }
    if s.cursor_style != CursorStyle::default() {
        ops.push(Op::CursorStyle(s.cursor_style));
    }
    for (line, r) in (0u16..).zip(&s.rows) {
        if !r.is_empty() {
            ops.push(Op::Line {
                line,
                cells: r.clone(),
            });
        }
        push_links(&mut ops, line, s.links.get(usize::from(line)));
    }
    ops.push(Op::Cursor(s.cursor));
    ops
}

/// 링크가 있으면 [`Op::Links`].
fn push_links(ops: &mut Vec<Op>, line: u16, links: Option<&Vec<Link>>) {
    if let Some(links) = links.filter(|l| !l.is_empty()) {
        ops.push(Op::Links {
            line,
            links: links.clone(),
        });
    }
}

/// `prev`(클라이언트가 가진 화면)를 `next`로 만드는 연산. 크기가 다르면 키프레임.
fn diff(prev: &Screen, next: &Screen) -> Vec<Op> {
    if prev.cols != next.cols || prev.lines != next.lines || prev.rows.len() != next.rows.len() {
        return snapshot(next);
    }
    let mut ops = Vec::new();
    if prev.title != next.title {
        ops.push(Op::Title(next.title.clone()));
    }
    if prev.bells != next.bells {
        ops.push(Op::Bell); // 프레임 사이 여러 번 울려도 한 번
    }
    if prev.cursor_style != next.cursor_style {
        ops.push(Op::CursorStyle(next.cursor_style));
    }
    let n = best_shift(&prev.rows, &next.rows);
    let mut links = prev.links.clone();
    links.resize(prev.rows.len(), Vec::new());
    let shifted;
    let base: &[Vec<Cell>] = if n == 0 {
        &prev.rows
    } else {
        ops.push(Op::Scroll(n));
        shifted = shift(&prev.rows, n);
        links = shift(&links, n);
        &shifted
    };
    let empty = Vec::new();
    for (((line, b), r), bl) in (0u16..).zip(base).zip(&next.rows).zip(&links) {
        let nl = next.links.get(usize::from(line)).unwrap_or(&empty);
        if b != r {
            ops.push(Op::Line {
                line,
                cells: r.clone(),
            });
            push_links(&mut ops, line, Some(nl));
        } else if bl != nl {
            ops.push(Op::Links {
                line,
                links: nl.clone(),
            });
        }
    }
    if prev.cursor != next.cursor {
        ops.push(Op::Cursor(next.cursor));
    }
    ops
}

/// [`Op::Scroll`]과 같은 이동을 적용한 행 사본(행이나 행별 링크).
fn shift<T: Clone + Default>(rows: &[T], n: i16) -> Vec<T> {
    let k = usize::from(n.unsigned_abs()).min(rows.len());
    let blank = std::iter::repeat_n(T::default(), k);
    if n > 0 {
        rows.iter().skip(k).cloned().chain(blank).collect()
    } else {
        blank
            .chain(rows.iter().take(rows.len() - k).cloned())
            .collect()
    }
}

fn row_hash(r: &[Cell]) -> u64 {
    let mut h = DefaultHasher::new();
    r.hash(&mut h);
    h.finish()
}

/// 행 해시가 가장 많이 맞는 이동량. 이동 없음보다 엄격히 많이 맞을 때만 이동한다. 빈 행끼리는 세지 않는다
/// (빈 화면 아래쪽이 우연히 맞아 이동을 고르지 않게).
// ponytail: 이동량마다 전체 행을 비교해 O(행²)(50행 2,500회 비교). 화면 행이 수백이 되면 위·아래 앵커 행으로 후보를 줄인다.
fn best_shift(prev: &[Vec<Cell>], next: &[Vec<Cell>]) -> i16 {
    let hp: Vec<Option<u64>> = prev
        .iter()
        .map(|r| (!r.is_empty()).then(|| row_hash(r)))
        .collect();
    let hn: Vec<Option<u64>> = next
        .iter()
        .map(|r| (!r.is_empty()).then(|| row_hash(r)))
        .collect();
    let len = hp.len();
    let score = |up: bool, k: usize| {
        (0..len.saturating_sub(k))
            .filter(|&i| {
                let (a, b) = if up {
                    (hn.get(i), hp.get(i + k))
                } else {
                    (hn.get(i + k), hp.get(i))
                };
                matches!((a, b), (Some(Some(x)), Some(Some(y))) if x == y)
            })
            .count()
    };
    let mut best = (score(true, 0), 0i16);
    for k in 1..len {
        let Ok(n) = i16::try_from(k) else { break };
        for (up, n) in [(true, n), (false, -n)] {
            let s = score(up, k);
            if s > best.0 {
                best = (s, n);
            }
        }
    }
    best.1
}

#[cfg(test)]
mod tests;
