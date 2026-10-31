//! MK-023 화면·원본 전송을 실제 PTY·프로세스·UDS로 검사한다.
//! - 원본 스트림: 끊고 다시 붙어 seq로 이어받은 바이트를 이어 붙이면 결정적 생성기 출력과 같다(링 밖은 디스크).
//! - 화면 스트림: 세션 10개에 출력이 흐르는 동안 재접속(스냅샷 수신까지) p95, 끝난 뒤 클라이언트 화면 = krilld 화면.
//! - 같은 revision 재접속은 스냅샷 생략, RESYNC는 스냅샷, 지연 히스토리(핫·콜드), 없는 세션 거부.
//! - footprint: 화면 클라이언트 프로세스는 출력 1천 줄과 10만 줄 뒤 footprint가 같다(스크롤백을 들지 않는다).
//!
//! 측정값(release)은 비공개 MK-023 스파이크 기록. 재현: `cargo test --release -p krilld --test attach -- --nocapture`.

#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]
#![expect(
    clippy::many_single_char_names,
    reason = "프레임은 (k, b) = (kind, body), 연결은 c, 세션은 s로 읽는다"
)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, fs, io, thread};

use krill_proto::SessionId;
use krill_proto::daemon::client_hello;
use krill_proto::holder::WinSize;
use krill_proto::ipc::{self, ClientRole, FEATURES, PROTO_MAX, PROTO_MIN, ServerHello};
use krill_proto::screen::{
    Attach, Attached, Closed, GapMsg, HistoryRequest, HistoryRows, Known, Msg, Op, Refused, Seq,
    Source, Stream, Update, View, decode_output, kind,
};
use krilld::attach::{self, Tap};
use krilld::ipc::Listener;
use krilld::store::{Limits, Store};
use krilld::{Appearance, Session};
use rustix::termios::Winsize;

const SMALL: WinSize = WinSize { cols: 80, rows: 24 };
/// MK-010 시나리오와 같은 크기.
const WIDE: WinSize = WinSize {
    cols: 120,
    rows: 40,
};
const INSTANCE: u64 = 77;
const CHILD_SOCK: &str = "MK023_CHILD_SOCK";
/// krilld 세션 링 크기(`session::RING`). 이보다 많이 밀린 재접속은 디스크에서 읽는다.
const RING: u64 = 256 * 1_024;

fn tmp(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("mk023-{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn store(dir: &Path, limits: Limits) -> Arc<Store> {
    Store::open(dir, limits).unwrap()
}

/// `/bin/sh -c script`를 새 PTY에 붙여 띄우고 세션을 시작한다. `store`가 있으면 디스크에 기록한다.
fn spawn(store: Option<&Arc<Store>>, id: u64, size: WinSize, script: &str) -> (Child, Session) {
    let ws = Winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = rustix_openpty::openpty(None, Some(&ws)).unwrap();
    let child = Command::new("/bin/sh")
        .args(["-c", script])
        .stdin(Stdio::from(pty.user.try_clone().unwrap()))
        .stdout(Stdio::from(pty.user.try_clone().unwrap()))
        .stderr(Stdio::from(pty.user))
        .spawn()
        .unwrap();
    let look = Arc::new(Appearance::load(PathBuf::from("/nonexistent/mk023/theme")));
    let rec = store.map(|s| s.recorder(SessionId(id), size).unwrap());
    let session = Session::start(SessionId(id), pty.controller, size, look, rec).unwrap();
    (child, session)
}

/// 결정적 생성기 한 줄(PTY가 `\n`을 `\r\n`으로 바꾼 뒤의 바이트). 색 SGR·한글이 섞인다.
fn gen_line(i: usize) -> String {
    format!("{i:07} 한글 \x1b[3{}m색\x1b[0m line\r\n", i % 8)
}

fn gen_bytes(n: usize) -> Vec<u8> {
    (1..=n).flat_map(|i| gen_line(i).into_bytes()).collect()
}

/// `gen_line` 1..=n을 출력한다. `every`줄마다 `pause`초 쉰다(0이면 쉬지 않음).
fn gen_script(n: usize, every: usize, pause: f64) -> String {
    format!(
        "/usr/bin/perl -e '$|=1; for $i (1..{n}) {{ printf \"%07d 한글 \\e[3%dm색\\e[0m line\\n\", $i, $i % 8; \
         select(undef, undef, undef, {pause}) if {every} && $i % {every} == 0 }}'"
    )
}

/// 소켓을 게시하고 연결마다 [`attach::serve`]를 돌린다.
fn server(dir: &Path, taps: Vec<Tap>) -> PathBuf {
    let sock = dir.join("run/k.sock");
    let lis = Listener::bind(&sock).unwrap();
    let hello = ServerHello {
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        features: FEATURES,
        build: "it".to_owned(),
        instance_id: INSTANCE,
    };
    let taps: Arc<HashMap<SessionId, Tap>> =
        Arc::new(taps.into_iter().map(|t| (t.id(), t)).collect());
    thread::spawn(move || {
        loop {
            let Ok(s) = lis.accept() else { continue };
            let (hello, taps) = (hello.clone(), Arc::clone(&taps));
            thread::spawn(move || attach::serve(s, &hello, |id| taps.get(&id).cloned()).ok());
        }
    });
    sock
}

/// Hello 협상 + Attach. 첫 응답 프레임(Attached 또는 Refused)을 돌려준다.
fn connect(sock: &Path, attach: Attach) -> (UnixStream, (u8, Vec<u8>)) {
    connect_features(
        sock,
        attach,
        FEATURES & !krill_proto::term_modes::FEAT_TERM_MODES,
    )
}

fn connect_features(sock: &Path, attach: Attach, features: u64) -> (UnixStream, (u8, Vec<u8>)) {
    let mut s = UnixStream::connect(sock).unwrap();
    let (k, b) = ipc::read_frame(&mut s).unwrap().unwrap();
    assert_eq!(ServerHello::decode(k, &b).unwrap().instance_id, INSTANCE);
    let mut hello = client_hello(ClientRole::App, "it");
    hello.features = features;
    s.write_all(&hello.encode().unwrap()).unwrap();
    s.write_all(&attach.encode().unwrap()).unwrap();
    let first = ipc::read_frame(&mut s).unwrap().unwrap();
    (s, first)
}

fn screen(id: u64, known: Option<Known>) -> Attach {
    Attach {
        session: SessionId(id),
        stream: Stream::Screen { known },
    }
}

fn frame(s: &mut UnixStream) -> (u8, Vec<u8>) {
    ipc::read_frame(s).unwrap().unwrap()
}

/// 화면 스트림을 `Closed`까지 따라간다. 받은 Update 수와 프레임 바이트 합을 돌려준다.
fn follow_to_close(s: &mut UnixStream, view: &mut View) -> (usize, usize) {
    let (mut updates, mut bytes) = (0, 0);
    loop {
        match frame(s) {
            (kind::UPDATE, b) => {
                bytes += b.len() + 5;
                view.apply(Update::decode(kind::UPDATE, &b).unwrap())
                    .unwrap();
                updates += 1;
            }
            (kind::CLOSED, _) => return (updates, bytes),
            (k, _) => unreachable!("unexpected kind {k:#04x}"),
        }
    }
}

/// 첫 Update 조각들을 `done`까지 받는다.
fn read_snapshot(s: &mut UnixStream, view: &mut View) {
    let mut first = true;
    loop {
        let (k, b) = frame(s);
        assert_eq!(k, kind::UPDATE);
        let u = Update::decode(k, &b).unwrap();
        if first {
            assert!(matches!(u.ops.first(), Some(Op::Reset { .. })), "{u:?}");
            first = false;
        }
        let done = u.done;
        view.apply(u).unwrap();
        if done {
            return;
        }
    }
}

/// `pct` 백분위(최근접 순위).
fn percentile(v: &mut [f64], pct: usize) -> f64 {
    v.sort_by(f64::total_cmp);
    v[(v.len() - 1) * pct / 100]
}

#[test]
fn raw_stream_resumes_by_seq_and_matches_generator_hash() {
    let dir = tmp("raw");
    let st = store(&dir, Limits::default());
    let n = 120_000;
    let (mut child, s) = spawn(Some(&st), 1, SMALL, &gen_script(n, 4_000, 0.02));
    let tap = s.tap();
    let sock = server(&dir, vec![s.tap()]);

    let mut got = Vec::new();
    let mut from = 0u64;
    let mut gaps = Vec::new();
    let mut behind_max = 0;
    let mut rounds = 0;
    'outer: loop {
        let (mut c, (k, b)) = connect(
            &sock,
            Attach {
                session: SessionId(1),
                stream: Stream::Raw { from: Seq(from) },
            },
        );
        let at = Attached::decode(k, &b).unwrap();
        behind_max = behind_max.max(at.seq.0.saturating_sub(from));
        rounds += 1;
        // 처음 네 번은 600KB쯤 받고 끊는다. 출력은 그동안에도 흐른다.
        let cut = (rounds < 5).then_some(got.len() + 600_000);
        loop {
            match frame(&mut c) {
                (kind::OUTPUT, b) => {
                    let (seq, bytes) = decode_output(kind::OUTPUT, &b).unwrap();
                    assert_eq!(seq.0, from, "seq 이어받기 위치");
                    got.extend_from_slice(bytes);
                    from += bytes.len() as u64;
                }
                (kind::GAP, b) => gaps.push(GapMsg::decode(kind::GAP, &b).unwrap()),
                (kind::CLOSED, b) => {
                    assert_eq!(Closed::decode(kind::CLOSED, &b).unwrap().seq.0, from);
                    break 'outer;
                }
                (k, _) => unreachable!("unexpected kind {k:#04x}"),
            }
            if cut.is_some_and(|c| got.len() >= c) {
                break;
            }
        }
        drop(c);
        if rounds == 2 {
            // 두 번째 끊김은 길게: 데몬이 링(256KiB)보다 많이 앞설 때까지 기다려 디스크 경로를 지난다.
            // 고정 대기는 고부하에서 생성기가 느려 링을 못 넘긴다(OWE-531).
            let deadline = Instant::now() + Duration::from_secs(30);
            while tap.pulse().end <= from + RING {
                assert!(Instant::now() < deadline, "30초 안에 링만큼 앞서지 않았다");
                thread::sleep(Duration::from_millis(10));
            }
        } else {
            thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(child.wait().unwrap().success());
    let want = gen_bytes(n);
    println!(
        "raw: {} bytes, {rounds} connections, max behind at reconnect {behind_max} B, crc32 got {:08x} want {:08x}",
        got.len(),
        crc32fast::hash(&got),
        crc32fast::hash(&want)
    );
    assert!(gaps.is_empty(), "{gaps:?}");
    assert!(behind_max > RING, "디스크 경로를 지나지 않았다");
    assert_eq!(got.len(), want.len());
    assert!(got == want, "원본 불일치");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn raw_stream_without_disk_reports_gap_beyond_ring() {
    let dir = tmp("gap");
    let n = 30_000; // 약 1MB: 링보다 크다
    let (mut child, s) = spawn(None, 1, SMALL, &gen_script(n, 0, 0.0));
    assert!(child.wait().unwrap().success());
    s.tap().wait(|p| p.closed);
    let sock = server(&dir, vec![s.tap()]);
    let (mut c, _) = connect(
        &sock,
        Attach {
            session: SessionId(1),
            stream: Stream::Raw { from: Seq(0) },
        },
    );
    let want = gen_bytes(n);
    let (k, b) = frame(&mut c);
    let gap = GapMsg::decode(k, &b).unwrap();
    assert_eq!(gap.start, Seq(0));
    let mut got = Vec::new();
    while let (kind::OUTPUT, b) = frame(&mut c) {
        let (seq, bytes) = decode_output(kind::OUTPUT, &b).unwrap();
        assert_eq!(seq.0, gap.len + got.len() as u64);
        got.extend_from_slice(bytes);
    }
    assert_eq!(gap.len + got.len() as u64, want.len() as u64);
    assert_eq!(got[..], want[usize::try_from(gap.len).unwrap()..]);
    fs::remove_dir_all(&dir).ok();
}

#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "macOS 전용: 지연 기준(p95 300ms)은 macOS 실측값"
)]
fn screen_reconnect_p95_and_final_screens_match() {
    let dir = tmp("screen");
    let st = store(&dir, Limits::default());
    // 세션 10개, 120×40, 약 4초 동안 흐르는 출력.
    let sessions: Vec<_> = (1..=10)
        .map(|id| spawn(Some(&st), id, WIDE, &gen_script(20_000, 50, 0.01)))
        .collect();
    let sock = server(&dir, sessions.iter().map(|(_, s)| s.tap()).collect());

    // 계속 붙어 있는 클라이언트: 델타를 끝까지 적용한 화면이 krilld 화면과 같아야 한다.
    let followers: Vec<_> = (1..=10u64)
        .map(|id| {
            let sock = sock.clone();
            thread::spawn(move || {
                let (mut c, (k, _)) = connect(&sock, screen(id, None));
                assert_eq!(k, kind::ATTACHED);
                let mut view = View::default();
                let (updates, bytes) = follow_to_close(&mut c, &mut view);
                (view, updates, bytes)
            })
        })
        .collect();

    // 출력이 흐르는 동안 재접속: 접속 → Hello → Attach → 스냅샷 끝까지 받기.
    let mut per_session = Vec::new();
    let mut per_round = Vec::new();
    for _ in 0..20 {
        let round = Instant::now();
        for id in 1..=10u64 {
            let t = Instant::now();
            // 새 View에는 이전 화면이 없다. known=None으로 항상 스냅샷부터 받는다.
            let (mut c, (k, _)) = connect(&sock, screen(id, None));
            assert_eq!(k, kind::ATTACHED);
            let mut view = View::default();
            read_snapshot(&mut c, &mut view);
            per_session.push(t.elapsed().as_secs_f64() * 1e3);
        }
        per_round.push(round.elapsed().as_secs_f64() * 1e3);
        thread::sleep(Duration::from_millis(100));
    }
    let (p50s, p95s) = (
        percentile(&mut per_session, 50),
        percentile(&mut per_session, 95),
    );
    let (p50r, p95r, maxr) = (
        percentile(&mut per_round, 50),
        percentile(&mut per_round, 95),
        percentile(&mut per_round, 100),
    );
    println!(
        "reconnect while streaming: per session p50 {p50s:.2} / p95 {p95s:.2} ms ({} samples), \
         10 sessions p50 {p50r:.2} / p95 {p95r:.2} / max {maxr:.2} ms ({} rounds)",
        per_session.len(),
        per_round.len()
    );
    assert!(p95r <= 300.0, "10세션 재접속 p95 {p95r:.1}ms");

    let (mut total_updates, mut total_bytes) = (0, 0);
    for ((mut child, s), f) in sessions.into_iter().zip(followers) {
        assert!(child.wait().unwrap().success());
        let (view, updates, bytes) = f.join().unwrap();
        total_updates += updates;
        total_bytes += bytes;
        let (daemon, rev, seq) = s.tap().capture();
        assert_eq!(view.screen, daemon, "세션 {:?}", s.id());
        assert_eq!(view.revision, Some(rev));
        assert_eq!(view.seq, seq);
        assert!(view.screen.rows.iter().flatten().any(|c| c.ch == '한'));
    }
    println!(
        "followers applied {total_updates} updates ({total_bytes} B, {} B/update) over 10 sessions, \
         raw output {} B",
        total_bytes / total_updates.max(1),
        10 * gen_bytes(20_000).len()
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn retained_view_applies_scroll_after_same_revision_reconnect() {
    let dir = tmp("known-scroll");
    let size = WinSize { cols: 20, rows: 4 };
    let (mut child, s) = spawn(
        None,
        1,
        size,
        "stty -echo; printf 'a\\nb\\nc\\nd'; read line; printf '\\ne'; read line",
    );
    // PTY의 LF → CRLF 변환 뒤 초기 출력은 10바이트다.
    s.tap().wait(|p| p.end >= 10);
    let sock = server(&dir, vec![s.tap()]);
    let (mut c, _) = connect(&sock, screen(1, None));
    let mut view = View::default();
    read_snapshot(&mut c, &mut view);
    drop(c);

    let (mut c, (k, b)) = connect(&sock, screen(1, view.known(INSTANCE)));
    assert_eq!(
        Some(Attached::decode(k, &b).unwrap().revision),
        view.revision
    );
    // 출력이 멈춘 같은 revision에서는 UPDATE가 오지 않는다(스냅샷 생략).
    c.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let err = ipc::read_frame(&mut c).unwrap_err();
    assert!(matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ));
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.tap().write_input(b"\n", None).unwrap();

    let (k, b) = frame(&mut c);
    let update = Update::decode(k, &b).unwrap();
    assert!(update.done);
    assert_eq!(update.ops.first(), Some(&Op::Scroll(1)));
    // 이전 화면을 버렸을 때의 원래 실패를 결정적으로 재현한다.
    assert_eq!(
        View::default().apply(update.clone()),
        Err(krill_proto::screen::ApplyError("scroll"))
    );
    view.apply(update).unwrap();
    // '\n' 뒤 'e'는 별도 PTY 읽기로 나뉠 수 있어 첫 UPDATE 뒤에 두 번째 UPDATE가 올 수 있다.
    // 첫 UPDATE만 적용하고 바로 capture와 비교하면 데몬이 앞서 flake한다(OWE-607).
    // 출력이 끝나면 결국 같아져야 하니 상한 안에서 UPDATE를 마저 읽어 같아질 때까지 따라간다.
    c.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (daemon, rev, seq) = s.tap().capture();
        if view.screen == daemon && view.revision == Some(rev) && view.seq == seq {
            // 보낸 뒤 아직 소켓에 남아 있는 UPDATE가 있으면 마저 적용한다.
            match ipc::read_frame(&mut c) {
                Ok(Some((k, b))) => {
                    assert_eq!(k, kind::UPDATE);
                    view.apply(Update::decode(k, &b).unwrap()).unwrap();
                    assert!(Instant::now() < deadline, "남은 UPDATE 적용 중 상한 초과");
                    continue;
                }
                Ok(None) => unreachable!("세션이 끝나지 않았는데 연결이 닫혔다"),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(e) => unreachable!("UPDATE 대기 중 읽기 실패: {e:?}"),
            }
        }
        assert!(Instant::now() < deadline, "capture와 같아지지 않았다");
        match ipc::read_frame(&mut c) {
            Ok(Some((k, b))) => {
                assert_eq!(k, kind::UPDATE);
                view.apply(Update::decode(k, &b).unwrap()).unwrap();
            }
            Ok(None) => unreachable!("세션이 끝나지 않았는데 연결이 닫혔다"),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                // 데몬이 아직 보내지 않았다. 다음 바퀴에서 capture를 다시 뜬다.
            }
            Err(e) => unreachable!("UPDATE 대기 중 읽기 실패: {e:?}"),
        }
    }
    let (daemon, rev, seq) = s.tap().capture();
    assert_eq!(view.screen, daemon);
    assert_eq!(view.revision, Some(rev));
    assert_eq!(view.seq, seq);

    // 같은 서버 revision이라도 새 View는 known=None으로 스냅샷을 받는다.
    let (mut fresh, _) = connect(&sock, screen(1, None));
    let mut fresh_view = View::default();
    read_snapshot(&mut fresh, &mut fresh_view);
    assert_eq!(fresh_view, view);

    child.kill().unwrap();
    child.wait().unwrap();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn parked_screen_reconnect_preserves_rep_spacing() {
    let dir = tmp("parked-rep");
    let gate = dir.join("continue");
    let st = store(&dir, Limits::default());
    let script = format!(
        "/usr/bin/perl -e '$|=1; print \"A \"; $end=time+25; \
         until (-e $ARGV[0]) {{ exit 2 if time>$end; select undef,undef,undef,0.01; }} \
         print \"\\e[5bZ\";' '{}'",
        gate.display()
    );
    let (mut child, session) = spawn(Some(&st), 1, SMALL, &script);
    let tap = session.tap();
    let progress = tap.wait(|p| p.end >= 2 || p.closed);
    assert!(progress.end >= 2, "child must print its initial character");
    let sock = server(&dir, vec![tap.clone()]);
    let (mut connection, _) = connect(&sock, screen(1, None));
    connection
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    read_snapshot(&mut connection, &mut View::default());
    drop(connection);
    let deadline = Instant::now() + Duration::from_secs(15);
    while !tap.is_parked() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !tap.is_parked() {
        child.kill().unwrap();
        child.wait().unwrap();
        panic!("disconnected screen must park before its next output");
    }
    let (mut connection, _) = connect(&sock, screen(1, None));
    connection
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut view = View::default();
    read_snapshot(&mut connection, &mut view);
    fs::write(&gate, []).unwrap();
    follow_to_close(&mut connection, &mut view);
    assert!(child.wait().unwrap().success());
    fs::remove_dir_all(&dir).unwrap();
    let row: String = view.screen.rows[0].iter().map(|cell| cell.ch).collect();
    assert_eq!(
        row.trim_end(),
        "A      Z",
        "REP spacing after a tab reconnect"
    );
    assert_eq!(view.screen.cursor.column, 8);
}

#[test]
fn same_revision_skips_snapshot_and_resync_resends() {
    let dir = tmp("known");
    let (mut child, s) = spawn(None, 1, SMALL, "printf 'hello 한글'; exec cat");
    s.tap().wait(|p| p.end >= "hello 한글".len() as u64);
    let sock = server(&dir, vec![s.tap()]);

    let (mut c, (k, b)) = connect(&sock, screen(1, None));
    let at = Attached::decode(k, &b).unwrap();
    let mut view = View::default();
    read_snapshot(&mut c, &mut view);
    assert_eq!(view.revision, Some(at.revision));
    drop(c);

    // 같은 세대·같은 revision: 스냅샷 없이 RESYNC를 보내야 스냅샷이 온다.
    let (mut c, (k, b)) = connect(&sock, screen(1, view.known(INSTANCE)));
    assert_eq!(Attached::decode(k, &b).unwrap().revision, at.revision);
    c.write_all(&ipc::encode_frame(kind::RESYNC, &[]).unwrap())
        .unwrap();
    let (k, b) = frame(&mut c);
    let u = Update::decode(k, &b).unwrap();
    assert!(matches!(u.ops.first(), Some(Op::Reset { .. })), "{u:?}");
    assert_eq!(u.revision, at.revision);

    // 다른 세대의 revision이면 바로 스냅샷.
    let other = Known {
        instance: INSTANCE + 1,
        ..view.known(INSTANCE).unwrap()
    };
    let (mut c2, _) = connect(&sock, screen(1, Some(other)));
    let mut v2 = View::default();
    read_snapshot(&mut c2, &mut v2);
    assert_eq!(v2.screen, view.screen);

    child.kill().unwrap();
    child.wait().unwrap();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn lazy_history_hot_and_cold_pages() {
    let dir = tmp("hist");
    let st = store(
        &dir,
        Limits {
            seal_bytes: 64 << 10,
            ..Limits::default()
        },
    );
    let n = 5_000;
    let (mut child, s) = spawn(Some(&st), 1, SMALL, &gen_script(n, 0, 0.0));
    assert!(child.wait().unwrap().success());
    s.tap().wait(|p| p.closed);
    let sock = server(&dir, vec![s.tap()]);
    let (mut c, _) = connect(&sock, screen(1, None));
    let mut view = View::default();
    read_snapshot(&mut c, &mut view);
    let text = |row: &[krill_proto::screen::Cell]| -> String {
        let t: String = row
            .iter()
            .filter(|c| !c.flags.contains(krill_proto::screen::Flags::SPACER))
            .map(|c| c.ch)
            .collect();
        t.trim_end().to_owned()
    };
    let mut ask = |req: HistoryRequest| -> HistoryRows {
        c.write_all(&req.encode().unwrap()).unwrap();
        loop {
            match frame(&mut c) {
                (kind::HISTORY_ROWS, b) => {
                    return HistoryRows::decode(kind::HISTORY_ROWS, &b).unwrap();
                }
                (kind::UPDATE | kind::CLOSED, _) => {}
                (k, _) => unreachable!("unexpected kind {k:#04x}"),
            }
        }
    };
    // 화면 24행 중 마지막 행은 빈 프롬프트 자리라 화면에 23줄, 핫 스크롤백 1,000행.
    let hot = ask(HistoryRequest {
        req: 1,
        source: Source::Hot,
        skip: 0,
        count: 3,
    });
    assert_eq!(hot.req, 1);
    assert_eq!(hot.total, 1_000);
    let rows: Vec<String> = hot.rows.iter().map(|r| text(r)).collect();
    let line = |i: usize| -> String {
        let l = gen_line(i);
        // 화면 문자열: SGR 없이, `\r\n` 없이.
        l.replace(&format!("\x1b[3{}m", i % 8), "")
            .replace("\x1b[0m", "")
            .trim_end()
            .to_owned()
    };
    let first_on_screen = n - 22;
    assert_eq!(
        rows,
        (first_on_screen - 3..first_on_screen)
            .map(line)
            .collect::<Vec<_>>()
    );
    // 색 속성이 실려 온다.
    assert!(
        hot.rows[0]
            .iter()
            .any(|c| c.ch == '색' && c.fg != krill_proto::screen::Color::Default)
    );

    let first = ask(HistoryRequest {
        req: 2,
        source: Source::Cold { page: 0 },
        skip: 0,
        count: 1_000,
    });
    assert!(first.pages >= 3, "페이지 {}", first.pages);
    assert!(first.total > 0);
    // 페이지 0의 맨 위는 생성기 첫 줄.
    let top = ask(HistoryRequest {
        req: 3,
        source: Source::Cold { page: 0 },
        skip: first.total - 1,
        count: 1,
    });
    assert_eq!(top.rows.len(), 1);
    assert_eq!(text(&top.rows[0]), line(1));

    // 없는 세션은 거부.
    let (_, (k, b)) = connect(&sock, screen(99, None));
    assert_eq!(Refused::decode(k, &b).unwrap(), Refused::NoSession);
    fs::remove_dir_all(&dir).ok();
}

/// Frozen ranges travel through the negotiated socket and cannot survive a clear or connection change.
#[test]
fn negotiated_frozen_history_wire_preserves_identity_and_invalidates_on_clear() {
    use krill_proto::screen::{HistoryViewRequest, HistoryViewRows, HistoryViewStatus};
    let dir = tmp("frozen-wire");
    let st = store(
        &dir,
        Limits {
            seal_bytes: 64 << 10,
            ..Limits::default()
        },
    );
    // Full-width ordinary rows must fit the frozen budget even though capture
    // vectors reserve more cells than they retain after trimming.
    let script =
        "/usr/bin/perl -e '$|=1; for $i (1..2500) { printf \"%07d 한글 %s\\n\", $i, \"x\" x 100 }'";
    let (mut child, session) = spawn(Some(&st), 71, WIDE, script);
    assert!(child.wait().unwrap().success());
    let tap = session.tap();
    tap.wait(|progress| progress.closed);
    let sock = server(&dir, vec![tap.clone()]);
    let (mut connection, _) = connect(&sock, screen(71, None));
    connection
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    read_snapshot(&mut connection, &mut View::default());
    let ask = |connection: &mut UnixStream, request: HistoryViewRequest| {
        connection.write_all(&request.encode().unwrap()).unwrap();
        loop {
            match frame(connection) {
                (kind::HISTORY_VIEW_ROWS, body) => {
                    return HistoryViewRows::decode(kind::HISTORY_VIEW_ROWS, &body).unwrap();
                }
                (kind::UPDATE | kind::CLOSED, _) => {}
                (kind, _) => unreachable!("unexpected frame {kind:#04x}"),
            }
        }
    };
    let opened = ask(
        &mut connection,
        HistoryViewRequest {
            req: 1,
            view: 0,
            skip: 0,
            count: 4,
        },
    );
    assert_eq!(opened.status, HistoryViewStatus::Ready);
    assert_ne!(opened.view, 0);
    assert_eq!(opened.session, SessionId(71));
    assert_eq!(opened.rows.len(), 4);
    let repeated = ask(
        &mut connection,
        HistoryViewRequest {
            req: 2,
            view: opened.view,
            skip: 0,
            count: 4,
        },
    );
    assert_eq!(repeated.identity, opened.identity);
    assert_eq!(repeated.rows, opened.rows);
    let oldest = ask(
        &mut connection,
        HistoryViewRequest {
            req: 3,
            view: opened.view,
            skip: u64::MAX,
            count: 4,
        },
    );
    assert_eq!(oldest.status, HistoryViewStatus::Ready);
    assert!(oldest.complete);
    assert!(oldest.total > 1_000);
    let row: String = oldest
        .rows
        .last()
        .unwrap()
        .iter()
        .filter(|cell| !cell.flags.contains(krill_proto::screen::Flags::SPACER))
        .map(|cell| cell.ch)
        .collect();
    assert!(row.starts_with("0000001 한글"), "{row}");

    let (mut another, _) = connect(&sock, screen(71, None));
    another
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    read_snapshot(&mut another, &mut View::default());
    let foreign = ask(
        &mut another,
        HistoryViewRequest {
            req: 4,
            view: opened.view,
            skip: 0,
            count: 4,
        },
    );
    assert_eq!(foreign.status, HistoryViewStatus::Invalidated);
    assert!(foreign.rows.is_empty());
    tap.clear_scrollback();
    let cleared = ask(
        &mut connection,
        HistoryViewRequest {
            req: 5,
            view: opened.view,
            skip: 0,
            count: 4,
        },
    );
    assert_eq!(cleared.status, HistoryViewStatus::Invalidated);
    assert!(cleared.rows.is_empty());
    assert!(cleared.identity.is_none());
    let fresh = ask(
        &mut connection,
        HistoryViewRequest {
            req: 6,
            view: 0,
            skip: 0,
            count: 4,
        },
    );
    assert_eq!(fresh.status, HistoryViewStatus::Ready);
    assert_ne!(fresh.view, opened.view);
    assert!(fresh.rows.is_empty());
    fs::remove_dir_all(&dir).ok();
}

/// 측정용 자식: 화면 스트림을 `Closed`까지 따라간 뒤 `READY`를 찍고 stdin이 닫힐 때까지 기다린다.
#[test]
#[ignore = "client_footprint_is_flat_in_output_length가 자식으로 실행한다"]
fn child_screen_client() {
    let Some(sock) = env::var_os(CHILD_SOCK) else {
        return;
    };
    let (mut c, (k, _)) = connect(Path::new(&sock), screen(1, None));
    assert_eq!(k, kind::ATTACHED);
    let mut view = View::default();
    let (updates, _) = follow_to_close(&mut c, &mut view);
    println!("READY rows={} updates={updates}", view.screen.rows.len());
    io::stdout().flush().unwrap();
    io::stdin().read_line(&mut String::new()).ok();
}

/// 화면 클라이언트 프로세스의 `phys_footprint`(`/usr/bin/footprint`)를 출력 1천 줄과 10만 줄에서 잰다.
fn client_footprint(n: usize) -> u64 {
    let dir = tmp(&format!("fp{n}"));
    let st = store(&dir, Limits::default());
    // 클라이언트가 먼저 붙도록 잠깐 기다렸다가 출력한다.
    let script = format!("sleep 0.5; {}", gen_script(n, 0, 0.0));
    let (mut producer, s) = spawn(Some(&st), 1, WIDE, &script);
    let sock = server(&dir, vec![s.tap()]);
    let mut c = Command::new(env::current_exe().unwrap())
        .args(["--exact", "child_screen_client", "--ignored", "--nocapture"])
        .env(CHILD_SOCK, &sock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(c.stdout.take().unwrap());
    let mut line = String::new();
    while !line.starts_with("READY") {
        line.clear();
        assert!(
            out.read_line(&mut line).unwrap() > 0,
            "자식이 READY 전에 끝남"
        );
    }
    let report = Command::new("/usr/bin/footprint")
        .args(["-p", &c.id().to_string(), "-f", "bytes"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&report.stdout);
    assert!(text.contains("Footprint: "), "footprint 출력:\n{text}");
    let bytes: u64 = text
        .split("Footprint: ")
        .nth(1)
        .and_then(|t| t.split(' ').next())
        .and_then(|v| v.parse().ok())
        .unwrap();
    println!("client n={n}: {} footprint {bytes} B", line.trim());
    drop(c.stdin.take());
    c.wait().unwrap();
    assert!(producer.wait().unwrap().success());
    fs::remove_dir_all(&dir).ok();
    bytes
}

#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "macOS 전용: /usr/bin/footprint")]
fn client_footprint_is_flat_in_output_length() {
    let small = client_footprint(1_000);
    let big = client_footprint(100_000);
    let diff = big.abs_diff(small);
    println!("client footprint: 1k lines {small} B, 100k lines {big} B, diff {diff} B");
    // 10만 줄 × 120열을 들고 있다면 수십 MB가 늘어난다. 할당기 잔여 변동만 허용한다.
    assert!(diff < 2 << 20, "footprint 차이 {diff} B");
}

/// 읽지 않는 클라이언트(소켓 버퍼가 가득 참)가 붙어 있어도 PTY 리더는 멈추지 않는다.
#[test]
fn stalled_clients_do_not_block_pty_reader() {
    let dir = tmp("stall");
    let st = store(&dir, Limits::default());
    let n = 60_000; // 약 2MB: 소켓 버퍼보다 훨씬 크다
    let (mut child, s) = spawn(Some(&st), 1, SMALL, &gen_script(n, 0, 0.0));
    let sock = server(&dir, vec![s.tap()]);
    let (_raw, _) = connect(
        &sock,
        Attach {
            session: SessionId(1),
            stream: Stream::Raw { from: Seq(0) },
        },
    );
    let (_screen, _) = connect(&sock, screen(1, None));
    let t = Instant::now();
    assert!(child.wait().unwrap().success());
    let p = s.tap().wait(|p| p.closed);
    assert_eq!(p.end, gen_bytes(n).len() as u64);
    println!(
        "stalled clients: {n} lines drained in {:.0} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    assert!(s.screen_text().contains(&format!("{n:07}")));
    fs::remove_dir_all(&dir).ok();
}

/// MK-029: 초기값은 known revision이어도 보내고 모드 변경도 통지한다.
#[test]
fn negotiated_input_modes_initial_change_and_reconnect() {
    use krill_proto::term_modes::{Modes, TERM_MODES};
    let dir = tmp("term-modes");
    let (mut child, session) = spawn(
        None,
        1,
        SMALL,
        "printf '\\033[>5u\\033[?1;1002;1006hready'; read line; printf '\\033[<u\\033[?1;1002;1006ldone'; read line",
    );
    let tap = session.tap();
    tap.wait(|p| p.end >= 5);
    let sock = server(&dir, vec![tap.clone()]);
    let (mut c, first) = connect_features(&sock, screen(1, None), FEATURES);
    assert_eq!(first.0, kind::ATTACHED);
    c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let (k, body) = frame(&mut c);
    let modes = Modes::decode(k, &body).unwrap();
    assert_eq!(modes.kitty, 5);
    assert!(modes.contains(Modes::APP_CURSOR | Modes::MOUSE_DRAG | Modes::SGR_MOUSE));
    let mut view = View::default();
    read_snapshot(&mut c, &mut view);
    let known = view.known(INSTANCE);
    let (mut reconnect, _) = connect_features(&sock, screen(1, known), FEATURES);
    assert_eq!(frame(&mut reconnect).0, TERM_MODES);
    reconnect
        .set_read_timeout(Some(Duration::from_millis(30)))
        .unwrap();
    assert!(
        ipc::read_frame(&mut reconnect).is_err(),
        "same revision skips screen only"
    );
    tap.write_input(b"\n", None).unwrap();
    loop {
        let (k, body) = frame(&mut c);
        if k == TERM_MODES {
            assert_eq!(
                Modes::decode(k, &body).unwrap(),
                Modes {
                    kitty: 0,
                    flags: Modes::ALTERNATE_SCROLL
                }
            );
            break;
        }
    }
    // 기존 클라이언트에 새 kind를 보내지 않는다(N-1).
    let (mut legacy, _) = connect(&sock, screen(1, None));
    read_snapshot(&mut legacy, &mut View::default());
    child.kill().unwrap();
    child.wait().unwrap();
    fs::remove_dir_all(&dir).ok();
}

/// MK-033: OSC 52 쓰기는 붙은 뒤의 것만 `FEAT_CLIPBOARD` 협상한 화면 연결에 간다.
#[test]
fn osc52_write_reaches_negotiated_clients_only_after_attach() {
    use krill_proto::clipboard::{CLIPBOARD, Clipboard, FEAT_CLIPBOARD};
    let dir = tmp("osc52");
    // "old" → 붙기 전, "new" → 붙은 뒤.
    let (mut child, session) = spawn(
        None,
        1,
        SMALL,
        "printf '\\033]52;c;b2xk\\007ready'; read line; printf '\\033]52;c;bmV3\\007done'; read line",
    );
    let tap = session.tap();
    tap.wait(|p| p.end >= 17);
    let sock = server(&dir, vec![tap.clone()]);
    let plain = FEATURES & !krill_proto::term_modes::FEAT_TERM_MODES;
    let (mut c, first) = connect_features(&sock, screen(1, None), plain);
    assert_eq!(first.0, kind::ATTACHED);
    read_snapshot(&mut c, &mut View::default());
    let (mut legacy, _) = connect_features(&sock, screen(1, None), plain & !FEAT_CLIPBOARD);
    read_snapshot(&mut legacy, &mut View::default());
    c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    tap.write_input(b"\n", None).unwrap();
    let got = loop {
        match frame(&mut c) {
            (CLIPBOARD, b) => break Clipboard::decode(CLIPBOARD, &b).unwrap(),
            (k, _) => assert_eq!(k, kind::UPDATE),
        }
    };
    assert_eq!(got.text, "new");
    legacy
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    while let Ok(Some((k, _))) = ipc::read_frame(&mut legacy) {
        assert_eq!(
            k,
            kind::UPDATE,
            "협상하지 않은 클라이언트에 새 kind를 보내지 않는다"
        );
    }
    child.kill().unwrap();
    child.wait().unwrap();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn negotiated_history_after_csi_3j_redraw_serves_hot_and_deduplicates_cold() {
    use krill_proto::screen::{HistoryViewRequest, HistoryViewRows, HistoryViewStatus};
    let dir = tmp("csi3j-hot-cold");
    let st = store(
        &dir,
        Limits {
            seal_bytes: 16 << 10,
            ..Limits::default()
        },
    );
    let script = "/usr/bin/perl -e '$|=1; for $i (1..1500) { printf \"line-%04d\\n\", $i } select(undef,undef,undef,0.05); print \"\\e[2J\\e[3J\\e[H\"; for $i (1..24) { printf \"screen-%02d\\n\", $i }'";
    let (mut child, session) = spawn(Some(&st), 72, SMALL, script);
    assert!(child.wait().unwrap().success());
    let tap = session.tap();
    tap.wait(|progress| progress.closed);
    let sock = server(&dir, vec![tap]);
    let (mut connection, _) = connect(&sock, screen(72, None));
    connection
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    read_snapshot(&mut connection, &mut View::default());
    let ask = |connection: &mut UnixStream, request: HistoryViewRequest| {
        connection.write_all(&request.encode().unwrap()).unwrap();
        loop {
            match frame(connection) {
                (kind::HISTORY_VIEW_ROWS, body) => {
                    return HistoryViewRows::decode(kind::HISTORY_VIEW_ROWS, &body).unwrap();
                }
                (kind::UPDATE | kind::CLOSED, _) => {}
                (kind, _) => unreachable!("unexpected frame {kind:#04x}"),
            }
        }
    };
    let row_text = |row: &[krill_proto::screen::Cell]| -> String {
        row.iter()
            .filter(|c| !c.flags.contains(krill_proto::screen::Flags::SPACER))
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_owned()
    };

    // First scroll request after attaching to a CSI 3 J TUI session hits Source::Hot directly.
    let hot = ask(
        &mut connection,
        HistoryViewRequest {
            req: 1,
            view: 0,
            skip: 0,
            count: 4,
        },
    );
    assert_eq!(hot.status, HistoryViewStatus::Ready);
    assert_eq!(hot.identity.as_ref().map(|id| id.source), Some(Source::Hot));
    assert_eq!(hot.total, 1_000);
    assert_eq!(
        hot.rows.iter().map(|r| row_text(r)).collect::<Vec<_>>(),
        vec!["screen-01", "line-1477", "line-1476", "line-1475"]
    );

    // Scrolling past the 1,000 hot rows seamlessly deduplicates hot coverage in Source::Cold.
    let cold = ask(
        &mut connection,
        HistoryViewRequest {
            req: 2,
            view: hot.view,
            skip: 1_000,
            count: 4,
        },
    );
    assert_eq!(cold.status, HistoryViewStatus::Ready);
    assert!(matches!(
        cold.identity.as_ref().map(|id| id.source),
        Some(Source::Cold { .. })
    ));
    assert_eq!(
        cold.rows.iter().map(|r| row_text(r)).collect::<Vec<_>>(),
        vec!["line-0478", "line-0477", "line-0476", "line-0475"]
    );
    fs::remove_dir_all(&dir).ok();
}
