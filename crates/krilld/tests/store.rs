//! MK-024 세그먼트 저장소를 실제 PTY·프로세스로 검사한다.
//! - 1만 줄 넘는 출력: 콜드 페이지를 이어 붙이면 원본과 같다.
//! - krilld 크래시(kill -9): 디스크만으로 마지막 화면을 재구성하고, 다시 연 기록기는 Gap(길이 모름)으로 이어 쓴다.
//! - 부분 쓰기·쓰기 실패(파일 크기 한도 EFBIG): 온전한 데까지 잘라 닫고 Gap 뒤 계속 쓴다.
//!
//! 크래시·쓰기 실패는 이 테스트 바이너리를 자식 프로세스로 다시 실행해서 만든다(`#[ignore]`된 `child_*` 테스트).

#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use krill_proto::SessionId;
use krill_proto::holder::WinSize;
use krilld::store::{Limits, Store};
use krilld::{Appearance, Gap, Session};
use rustix::termios::Winsize;

const SIZE: WinSize = WinSize { cols: 80, rows: 40 };
const CHILD_DIR: &str = "MK024_CHILD_DIR";

fn tmp(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("mk024-it-{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    dir
}

/// `/bin/sh -c script`를 새 PTY에 붙여 띄우고 기록기를 단 세션을 시작한다.
fn spawn(store: &Arc<Store>, id: u64, script: &str) -> (Child, Session) {
    let ws = Winsize {
        ws_row: SIZE.rows,
        ws_col: SIZE.cols,
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
    let look = Arc::new(Appearance::load(PathBuf::from("/nonexistent/mk024/theme")));
    let rec = store.recorder(SessionId(id), SIZE).unwrap();
    let session = Session::start(SessionId(id), pty.controller, SIZE, look, Some(rec)).unwrap();
    (child, session)
}

/// 결정적 생성기: `line-NNNNNN 한글 ok` n줄(awk, 마지막 줄 형식이 `seq`처럼 바뀌지 않는다).
fn gen_script(n: usize, tail: &str) -> String {
    format!(
        "/usr/bin/awk 'BEGIN {{ for (i = 0; i < {n}; i++) printf \"line-%06d 한글 ok\\n\", i }}'; {tail}"
    )
}

fn want(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("line-{i:06} 한글 ok")).collect()
}

fn wait_screen(s: &Session, limit: Duration, needle: &str) -> String {
    let end = Instant::now() + limit;
    loop {
        let text = s.screen_text();
        if text.contains(needle) {
            return text;
        }
        assert!(Instant::now() < end, "시간 초과. 화면:\n{text}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// 이 테스트 바이너리의 `name` 테스트를 자식으로 실행한다. `wrap`은 그 앞에 붙일 셸 준비 명령.
fn child(name: &str, dir: &Path, wrap: &str) -> Child {
    Command::new("/bin/sh")
        .args(["-c", &format!("{wrap} exec \"$0\" \"$@\"")])
        .arg(env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .env(CHILD_DIR, dir)
        .spawn()
        .unwrap()
}

#[test]
fn cold_pages_beyond_10k_lines_match_original() {
    let root = tmp("pages");
    // 봉인을 작게 잡아 세그먼트 여러 개(워밍업 재생 경로)를 지나게 한다.
    let store = Store::open(
        &root,
        Limits {
            seal_bytes: 128 << 10,
            ..Limits::default()
        },
    )
    .unwrap();
    let n = 12_000;
    let (mut c, s) = spawn(&store, 1, &gen_script(n, "printf 'DONE'"));
    let live = wait_screen(&s, Duration::from_secs(20), "DONE");
    assert!(c.wait().unwrap().success());
    s.wait().unwrap();

    let hist = store.history(SessionId(1)).unwrap();
    assert!(hist.len() >= 3, "세그먼트 {}개", hist.len());
    let mut lines = Vec::new();
    let mut seq = 0;
    for i in 0..hist.len() {
        let page = hist.page(i).unwrap();
        assert_eq!(page.start_seq, seq, "페이지 {i}");
        assert!(page.gaps.is_empty());
        seq = page.end_seq;
        lines.extend(page.lines());
    }
    // 마지막 화면(40행)에 남은 39줄 + DONE 행을 뺀 전부가 콜드 페이지에 원본 순서대로 있다.
    let want = want(n);
    assert_eq!(lines.len(), n - (usize::from(SIZE.rows) - 1));
    assert_eq!(lines[..], want[..lines.len()]);
    // 디스크만으로 재구성한 화면 = 라이브 화면.
    assert_eq!(hist.restore().unwrap().screen_text(), live);
    fs::remove_dir_all(&root).ok();
}

/// OWE-534: 리사이즈가 세그먼트에 남아 디스크 재생 화면이 바뀐 크기로 그려진다.
#[test]
fn cold_replay_follows_resize() {
    let root = tmp("resize");
    let store = Store::open(&root, Limits::default()).unwrap();
    let (mut c, s) = spawn(
        &store,
        2,
        "printf 'ready\\n'; read x; stty size; printf 'DONE'",
    );
    wait_screen(&s, Duration::from_secs(10), "ready");
    let size = WinSize {
        cols: 100,
        rows: 30,
    };
    s.tap().resize(size).unwrap();
    s.tap().write_input(b"\n", None).unwrap();
    let live = wait_screen(&s, Duration::from_secs(10), "DONE");
    assert!(live.contains("30 100"), "stty size:\n{live}");
    assert!(c.wait().unwrap().success());
    s.wait().unwrap();

    let restored = store.history(SessionId(2)).unwrap().restore().unwrap();
    let screen = restored.screen();
    assert_eq!((screen.cols, screen.lines), (size.cols, size.rows));
    assert_eq!(restored.screen_text(), live);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn reconstructs_after_kill_9() {
    let root = tmp("kill9");
    let mut c = child("child_records_then_hangs", &root, "");
    let ready = root.join("ready");
    let end = Instant::now() + Duration::from_secs(30);
    while !ready.exists() {
        assert!(Instant::now() < end, "자식이 준비되지 않음");
        thread::sleep(Duration::from_millis(10));
    }
    let live = fs::read_to_string(&ready).unwrap();
    // SIGKILL: 버퍼를 비울 기회가 없다(CONVENTIONS P5·P6).
    c.kill().unwrap();
    c.wait().unwrap();

    let store = Store::open(&root, Limits::default()).unwrap();
    let id = SessionId(7);
    let hist = store.history(id).unwrap();
    let restored = hist.restore().unwrap();
    assert_eq!(restored.screen_text(), live);
    // 원본 1MiB 넘게 썼으니 세그먼트가 둘 이상이고, 이어 붙인 페이지 = 원본.
    assert!(hist.len() >= 2, "세그먼트 {}개", hist.len());
    let lines: Vec<String> = (0..hist.len())
        .flat_map(|i| hist.page(i).unwrap().lines())
        .collect();
    assert_eq!(lines[..], want(60_000)[..lines.len()]);

    // 다시 연 기록기: 봉인 없이 끝난 세그먼트 뒤는 길이 모르는 Gap으로 시작한다.
    let end_seq = restored.end_seq;
    let mut rec = store.recorder(id, SIZE).unwrap();
    rec.append(b"after restart\r\n");
    rec.seal();
    let hist = store.history(id).unwrap();
    let last = hist.page(hist.len() - 1).unwrap();
    assert_eq!(last.start_seq, end_seq);
    assert_eq!(
        last.gaps,
        vec![Gap {
            start: end_seq,
            len: 0
        }]
    );
    fs::remove_dir_all(&root).ok();
}

/// [`reconstructs_after_kill_9`]의 자식: 기록한 뒤 화면을 `ready`에 적고 죽기를 기다린다.
#[test]
#[ignore = "reconstructs_after_kill_9가 자식 프로세스로 실행한다"]
fn child_records_then_hangs() {
    let Some(root) = env::var_os(CHILD_DIR).map(PathBuf::from) else {
        return;
    };
    let store = Store::open(&root, Limits::default()).unwrap();
    // 60,000줄 × 약 24B ≈ 1.4MB: 봉인된 세그먼트 하나 + 열린 세그먼트 하나.
    let (_c, s) = spawn(
        &store,
        7,
        &gen_script(60_000, "printf 'PROMPT$ '; sleep 30"),
    );
    let screen = wait_screen(&s, Duration::from_secs(20), "PROMPT$ ");
    // 리더가 화면에 반영한 묶음은 곧바로 기록됐다(같은 스레드, 반영 뒤 append). 화면이 바뀌지 않게 잠시 더 본다.
    thread::sleep(Duration::from_millis(200));
    assert_eq!(s.screen_text(), screen);
    let tmp = root.join("ready.tmp");
    fs::write(&tmp, &screen).unwrap();
    fs::rename(&tmp, root.join("ready")).unwrap();
    thread::sleep(Duration::from_secs(60));
}

#[test]
fn write_failure_truncates_and_continues_with_gap() {
    let root = tmp("efbig");
    // 파일 크기 한도 4KiB(macOS sh의 ulimit -f 단위는 1,024B). 넘는 write는 부분 쓰기 뒤 EFBIG가 된다. SIGXFSZ는 무시(exec 뒤에도 유지).
    let mut c = child(
        "child_hits_file_size_limit",
        &root,
        "trap '' XFSZ; ulimit -f 4;",
    );
    assert!(c.wait().unwrap().success());
    fs::remove_dir_all(&root).ok();
}

#[test]
#[ignore = "write_failure_truncates_and_continues_with_gap가 자식 프로세스로 실행한다"]
fn child_hits_file_size_limit() {
    let Some(root) = env::var_os(CHILD_DIR).map(PathBuf::from) else {
        return;
    };
    let store = Store::open(&root, Limits::default()).unwrap();
    let id = SessionId(3);
    let mut rec = store.recorder(id, SIZE).unwrap();
    // 압축이 잘 안 되는 줄 1,000B씩.
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut record = || -> Vec<u8> {
        let mut v: Vec<u8> = (0..998)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                b'a' + u8::try_from(x % 26).unwrap()
            })
            .collect();
        v.extend_from_slice(b"\r\n");
        v
    };
    for _ in 0..12 {
        rec.append(&record());
    }
    rec.seal();
    let dir = store.session_dir(id);
    let hist = store.history(id).unwrap();
    assert!(hist.len() >= 2, "세그먼트 {}개", hist.len());
    let mut seq = 0;
    let mut gaps = Vec::new();
    for i in 0..hist.len() {
        let page = hist.page(i).unwrap();
        assert_eq!(page.start_seq, seq, "페이지 {i}: seq가 이어진다");
        seq = page.end_seq;
        gaps.extend(page.gaps);
    }
    assert_eq!(seq, 12 * 1_000, "버린 것까지 seq는 빠짐없이 센다");
    // 두 번째 세그먼트는 실패한 레코드 하나를 Gap으로 남기고 시작한다.
    let first_end = hist.page(0).unwrap().end_seq;
    assert_eq!(
        gaps.first(),
        Some(&Gap {
            start: first_end,
            len: 1_000
        })
    );
    // 모든 파일이 한도 안이고 부분 쓰기 흔적(잘린 청크) 없이 끝까지 읽힌다: 크기 = 한도 이하이고 디스크 쓰기 합과 같다.
    let mut on_disk = 0;
    for e in fs::read_dir(&dir).unwrap() {
        let len = e.unwrap().metadata().unwrap().len();
        assert!(len <= 4_096, "{len}");
        on_disk += len;
    }
    assert_eq!(on_disk, store.written());
}

/// 실측(10분): 비압축에 가까운 출력(base64 난수)을 실제 PTY로 흘리며 1분마다 디스크 쓰기를 적는다.
/// `cargo nextest run -p krilld --release --run-ignored only flood_measure --no-capture`, 시간은 `MK024_FLOOD_SECS`(기본 600).
#[test]
#[ignore = "10분 실측(MK-024 보고서)"]
fn flood_measure() {
    let secs: u64 = env::var("MK024_FLOOD_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let root = tmp("flood");
    let store = Store::open(&root, Limits::default()).unwrap();
    let (mut c, s) = spawn(&store, 9, "/bin/cat /dev/urandom | /usr/bin/base64 -b 76");
    let dir = store.session_dir(SessionId(9));
    let t0 = Instant::now();
    let mut last = 0;
    let mut minute = 0;
    while t0.elapsed() < Duration::from_secs(secs) {
        thread::sleep(Duration::from_secs(60).min(Duration::from_secs(secs)));
        minute += 1;
        let w = store.written();
        let disk: u64 = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        println!(
            "{minute}분: 쓰기 {}B, 누적 {w}B, 세션 디렉터리 {disk}B",
            w - last
        );
        last = w;
    }
    // sh 아래 cat·base64까지 끝내야 PTY가 닫힌다.
    Command::new("/usr/bin/pkill")
        .args(["-9", "-P", &c.id().to_string()])
        .status()
        .unwrap();
    c.kill().unwrap();
    c.wait().unwrap();
    s.wait().ok();
    fs::remove_dir_all(&root).ok();
}
