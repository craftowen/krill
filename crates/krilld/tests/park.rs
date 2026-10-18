//! MK-035 숨긴 터미널 강등을 실제 PTY·UDS·화면 클라이언트로 검사한다.
//! - 터미널 20개(200×50)에 핫 스크롤백을 채운 뒤 16개의 화면 구독을 끊으면, [`PARK_GRACE`] 뒤 보이는 4개만
//!   핫 스크롤백을 쥔다. 이 프로세스의 `phys_footprint`로 돌려준 메모리를 잰다(krilld와 같은 jemalloc + decay 0).
//! - 숨긴 세션으로 전환(화면 구독 → 스냅샷 수신 완료)은 100ms 안이고, 화면은 강등 전과 같다.
//!
//! 재현: `cargo test --release -p krilld --test park -- --nocapture`. 측정값은 docs/spikes/MK-035.md.

#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::collections::HashMap;
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use krill_proto::SessionId;
use krill_proto::daemon::client_hello;
use krill_proto::holder::WinSize;
use krill_proto::ipc::{self, ClientRole, FEATURES, PROTO_MAX, PROTO_MIN, ServerHello};
use krill_proto::screen::{Attach, Msg, Stream, Update, View, kind};
use krilld::attach::{self, Tap};
use krilld::ipc::Listener;
use krilld::store::{Limits, Store};
use krilld::{Appearance, HOT_LINES, PARK_GRACE, Session};
use rustix::termios::Winsize;

#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// MK-009에서 데몬 예산을 넘긴 크기.
const SIZE: WinSize = WinSize {
    cols: 200,
    rows: 50,
};
const TERMINALS: u64 = 20;
const SHOWN: u64 = 4;
const INSTANCE: u64 = 35;

fn spawn(store: &Arc<Store>, id: u64) -> (Child, Session) {
    let ws = Winsize {
        ws_row: SIZE.rows,
        ws_col: SIZE.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = rustix_openpty::openpty(None, Some(&ws)).unwrap();
    // 핫 상한(1,000행)을 넘게, 열 폭을 채운 줄을 낸 뒤 살아 있는다.
    let script = format!(
        "/usr/bin/perl -e '$|=1; for $i (1..{}) {{ printf \"%07d %s\\n\", $i, \"한글x\" x 38 }}'; exec sleep 60",
        HOT_LINES * 2
    );
    let child = Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(Stdio::from(pty.user.try_clone().unwrap()))
        .stdout(Stdio::from(pty.user.try_clone().unwrap()))
        .stderr(Stdio::from(pty.user))
        .spawn()
        .unwrap();
    let look = Arc::new(Appearance::load(PathBuf::from("/nonexistent/mk035/theme")));
    let rec = Some(store.recorder(SessionId(id), SIZE).unwrap());
    let session = Session::start(SessionId(id), pty.controller, SIZE, look, rec).unwrap();
    (child, session)
}

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

/// 화면 구독을 열고 스냅샷을 다 받을 때까지의 시간을 잰다. 이후 프레임은 뒤에서 버린다(끊으면 구독 끝).
fn show(sock: &Path, id: u64) -> (UnixStream, View, Duration) {
    let t0 = Instant::now();
    let mut s = UnixStream::connect(sock).unwrap();
    ipc::read_frame(&mut s).unwrap().unwrap();
    let mut hello = client_hello(ClientRole::App, "it");
    hello.features = FEATURES & !krill_proto::term_modes::FEAT_TERM_MODES;
    s.write_all(&hello.encode().unwrap()).unwrap();
    let attach = Attach {
        session: SessionId(id),
        stream: Stream::Screen { known: None },
    };
    s.write_all(&attach.encode().unwrap()).unwrap();
    assert_eq!(ipc::read_frame(&mut s).unwrap().unwrap().0, kind::ATTACHED);
    let mut view = View::default();
    loop {
        let (k, b) = ipc::read_frame(&mut s).unwrap().unwrap();
        assert_eq!(k, kind::UPDATE);
        let u = Update::decode(k, &b).unwrap();
        let done = u.done;
        view.apply(u).unwrap();
        if done {
            break;
        }
    }
    let took = t0.elapsed();
    let mut drain = s.try_clone().unwrap();
    thread::spawn(move || while let Ok(Some(_)) = ipc::read_frame(&mut drain) {});
    (s, view, took)
}

fn footprint() -> u64 {
    let report = Command::new("/usr/bin/footprint")
        .args(["-p", &std::process::id().to_string(), "-f", "bytes"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&report.stdout);
    assert!(text.contains("Footprint: "), "footprint 출력:\n{text}");
    text.split("Footprint: ")
        .nth(1)
        .and_then(|t| t.split(' ').next())
        .and_then(|v| v.parse().ok())
        .unwrap()
}

fn hot(tap: &Tap) -> usize {
    tap.history(0, 0).0
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < until, "시간 초과: {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "macOS 전용: /usr/bin/footprint")]
fn only_shown_terminals_keep_hot_rows_and_switch_rebuilds_fast() {
    krilld::alloc::return_freed_pages().unwrap();
    let dir = env::temp_dir().join(format!("mk035-park-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let st = Store::open(&dir, Limits::default()).unwrap();
    let (mut children, sessions): (Vec<_>, Vec<_>) =
        (1..=TERMINALS).map(|id| spawn(&st, id)).unzip();
    let taps: Vec<Tap> = sessions.iter().map(Session::tap).collect();
    let sock = server(&dir, taps.clone());

    // 20개 모두 보이는 동안 핫 스크롤백을 채운다.
    let mut clients: Vec<_> = (1..=TERMINALS).map(|id| show(&sock, id).0).collect();
    // 줄마다 "%07d " + "한글x"×38 + "\r\n" = 276B(198열이라 줄바꿈 없음).
    let total = u64::try_from(HOT_LINES * 2 * 276).unwrap();
    wait_until("출력 끝", || taps.iter().all(|t| t.pulse().end >= total));
    assert!(taps.iter().all(|t| hot(t) == HOT_LINES));
    let full = footprint();

    // 탭 전환: 앞 4개만 보인다.
    let shown = usize::try_from(SHOWN).unwrap();
    for c in clients.drain(shown..) {
        c.shutdown(Shutdown::Both).unwrap();
    }
    let before: Vec<_> = taps.iter().map(|t| t.capture().0).collect();
    thread::sleep(PARK_GRACE + Duration::from_millis(1_500));
    wait_until("숨긴 16개 강등", || {
        taps.iter().skip(shown).all(Tap::is_parked)
    });
    let parked = footprint();
    let hot_rows: Vec<_> = taps.iter().map(hot).collect();
    assert!(
        hot_rows[..shown].iter().all(|&n| n == HOT_LINES),
        "{hot_rows:?}"
    );
    assert!(hot_rows[shown..].iter().all(|&n| n == 0), "{hot_rows:?}");
    assert!(taps[..shown].iter().all(|t| !t.is_parked()));
    let freed = full.saturating_sub(parked);
    // 강등 대상의 셀 계산값: 16 × 1,000행 × 200열 × 24B(alacritty Cell). 절반 이상 돌아와야 한다.
    let cells = (TERMINALS - SHOWN) * 1_000 * u64::from(SIZE.cols) * 24;
    println!(
        "footprint: 20개 핫 {full} B, 4개만 핫 {parked} B, 돌려줌 {freed} B (셀 계산 {cells} B)"
    );
    assert!(
        freed > cells / 2,
        "돌려준 메모리 {freed} B < {} B",
        cells / 2
    );

    // 숨긴 세션으로 하나씩 전환한다: 새로 보이는 것은 구독, 보던 것 하나는 끊는다.
    let mut took = Vec::new();
    for (i, tap) in taps.iter().enumerate().skip(shown) {
        let id = tap.id().0;
        let (c, view, t) = show(&sock, id);
        took.push(t.as_secs_f64() * 1e3);
        assert!(!tap.is_parked());
        assert_eq!(view.screen, before[i], "세션 {id} 화면");
        clients.remove(0).shutdown(Shutdown::Both).unwrap();
        clients.push(c);
    }
    took.sort_by(f64::total_cmp);
    let p95 = took[(took.len() - 1) * 95 / 100];
    let max = took[took.len() - 1];
    println!(
        "전환(구독 → 스냅샷 완료) {}회: p95 {p95:.2}ms, 최대 {max:.2}ms",
        took.len()
    );
    // 완료 조건 100ms는 release 기준이다(docs/spikes/MK-035.md). 디버그 빌드(check.sh)는 부하에 따라 수백 ms가 나와
    // (load avg 105에서 209ms 실측) 콜드 재생 같은 구조 회귀만 잡는 상한을 둔다.
    let limit = if cfg!(debug_assertions) {
        1_000.0
    } else {
        100.0
    };
    assert!(p95 < limit, "전환 p95 {p95:.2}ms (상한 {limit}ms)");

    for c in &mut children {
        c.kill().ok();
        c.wait().ok();
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn recording_disabled_preserves_hidden_hot_history() {
    let dir = env::temp_dir().join(format!("mk587-private-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("history.toml"), "recording = false").unwrap();
    let store = Store::open(&dir, Limits::default()).unwrap();
    let (mut child, session) = spawn(&store, 1);
    let tap = session.tap();
    wait_until("private history", || hot(&tap) == HOT_LINES);
    thread::sleep(PARK_GRACE + Duration::from_millis(500));
    let parked = tap.is_parked();
    let rows = hot(&tap);
    child.kill().ok();
    child.wait().unwrap();
    session.wait().unwrap();
    fs::remove_dir_all(dir).unwrap();
    assert!(!parked);
    assert_eq!(rows, HOT_LINES);
}
