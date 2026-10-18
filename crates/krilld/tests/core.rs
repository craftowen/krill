//! 실제 PTY와 자식 프로세스로 검사한다.
//! - MK-019: 세션 단위 panic 격리, DEC 2026 sync 마감.
//! - MK-020: 클라이언트가 하나도 없어도 krilld가 질의에 답한다(내장 기본 테마 → 저장된 테마), 모드 2031 알림.

#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{fs, thread};

use krill_proto::SessionId;
use krill_proto::holder::WinSize;
use krill_proto::theme::{DEFAULT, Rgb};
use krilld::{Appearance, Session};
use rustix::termios::Winsize;

const SIZE: WinSize = WinSize { cols: 80, rows: 40 };

/// 테마를 저장하지 않는 테스트용(저장 경로에 쓰지 않는다).
fn no_theme() -> Arc<Appearance> {
    Arc::new(Appearance::load(PathBuf::from("/nonexistent/mk020/theme")))
}

/// `/bin/sh -c script`를 새 PTY에 붙여 띄우고 그 master로 세션을 시작한다.
fn spawn(id: u64, script: &str) -> (Child, Session) {
    spawn_with(id, script, no_theme())
}

fn spawn_with(id: u64, script: &str, look: Arc<Appearance>) -> (Child, Session) {
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
    let session = Session::start(SessionId(id), pty.controller, SIZE, look, None).unwrap();
    (child, session)
}

/// `pred`가 참이 될 때까지 1ms 간격으로 화면을 본다. 참이 된 시각의 화면을 돌려준다.
fn wait_screen(s: &Session, limit: Duration, pred: impl Fn(&str) -> bool) -> (Instant, String) {
    let end = Instant::now() + limit;
    loop {
        let text = s.screen_text();
        if pred(&text) {
            return (Instant::now(), text);
        }
        assert!(Instant::now() < end, "시간 초과. 화면:\n{text}");
        thread::sleep(Duration::from_millis(1));
    }
}

/// OWE-474: kitty 키보드 스택 overflow 출력(`CSI > 1 u` × 4097)은 좁은 포크에서 고쳐
/// panic도 Gap도 없이 그냥 지나간다. 이전에는 이 출력이 Term을 panic시켜 Gap 1개를 남겼다
/// (`engine_panic_spares_other_sessions`였음). panic 격리 기제 자체는
/// `session::tests::guarded_rebuilds_engine_after_panic`이 주입 panic으로 확인한다.
#[test]
fn kitty_keyboard_overflow_leaves_no_gap() {
    // A: 이전에는 panic하던 묶음. 이제는 끝까지 읽혀 A-after가 보인다.
    let (mut a_child, a) = spawn(
        1,
        "printf 'A-before\\n'; sleep 0.2; printf '\\033[>1u%.0s' $(/usr/bin/seq 1 4097); \
         sleep 0.2; printf 'A-after\\n'; sleep 0.5",
    );
    // B: A가 읽히는 동안에도 계속 출력한다.
    let (mut b_child, b) = spawn(
        2,
        "i=0; while [ $i -lt 30 ]; do printf 'B-%d\\n' $i; i=$((i+1)); sleep 0.03; done; sleep 0.5",
    );

    wait_screen(&a, Duration::from_secs(10), |t| t.contains("A-after"));
    let (_, b_text) = wait_screen(&b, Duration::from_secs(10), |t| t.contains("B-29"));

    // A: Gap 없음, 자식 정상 종료.
    assert!(a.gaps().is_empty(), "A gaps: {:?}", a.gaps());
    assert!(a_child.wait().unwrap().success());
    // B: Gap 없음, 줄 손실 없음, 자식 정상 종료.
    assert!(b.gaps().is_empty());
    for i in 0..30 {
        assert!(
            b_text.lines().any(|l| l.trim_end() == format!("B-{i}")),
            "B-{i} 없음:\n{b_text}"
        );
    }
    assert!(b_child.wait().unwrap().success());

    // 둘 다 EOF로 리더가 정상 종료.
    a.wait().unwrap();
    b.wait().unwrap();
}

#[test]
fn sync_without_end_is_shown_after_deadline() {
    let start = Instant::now();
    let (mut child, s) = spawn(3, "printf '\\033[?2026hSYNC-BODY'; sleep 1");
    let (at, _) = wait_screen(&s, Duration::from_secs(5), |t| t.contains("SYNC-BODY"));
    let shown = at - start;
    eprintln!("sync 단독: 반영까지 {shown:?}(spawn 기준)");
    assert!(
        shown >= Duration::from_millis(150),
        "마감 전 반영: {shown:?}"
    );
    assert!(
        shown < Duration::from_millis(1_000),
        "마감 뒤 반영이 늦음: {shown:?}"
    );
    assert!(child.wait().unwrap().success());
}

#[test]
fn sync_stream_without_end_is_shown_while_streaming() {
    // ESU 없이 바이트가 계속 들어와도(poll이 타임아웃으로 깨지 않아도) 마감에 반영돼야 한다.
    let start = Instant::now();
    let (mut child, s) = spawn(
        4,
        "printf '\\033[?2026h'; i=0; while [ $i -lt 100 ]; do printf 'S%d ' $i; i=$((i+1)); \
         sleep 0.01; done; printf 'END'; sleep 0.5",
    );
    let (at, text) = wait_screen(&s, Duration::from_secs(10), |t| t.contains("S0 "));
    let shown = at - start;
    eprintln!("sync 스트림: 반영까지 {shown:?}(spawn 기준)");
    assert!(
        shown >= Duration::from_millis(150),
        "마감 전 반영: {shown:?}"
    );
    assert!(
        !text.contains("END"),
        "스트림이 끝난 뒤에야 반영됨:\n{text}"
    );
    assert!(child.wait().unwrap().success());
}

/// 자식이 raw 모드에서 질의를 보내고, 받은 입력(= krilld 응답)을 `out`에 그대로 적는다. 2초간 입력이 없으면 끝난다.
fn query_script(queries: &str, out: &Path) -> String {
    format!(
        "stty raw -echo min 0 time 20; printf '{queries}READY'; cat > '{}'",
        out.display()
    )
}

#[test]
fn answers_without_client_and_keeps_last_theme() {
    let dir = std::env::temp_dir().join(format!("mk020-core-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let theme_path = dir.join("theme");
    // DA1·DSR 6·OSC 10·OSC 11, 그리고 모드 2031 켜기. 앱(IPC 클라이언트)은 없다.
    let queries = r"\033[?2031h\033]10;?\033\\\033]11;?\007\033[6n\033[c";

    // 1) 테마를 한 번도 받지 않은 krilld: 내장 기본 테마로 답한다.
    let look = Arc::new(Appearance::load(theme_path.clone()));
    let out = dir.join("replies-1");
    let (mut child, s) = spawn_with(5, &query_script(queries, &out), Arc::clone(&look));
    wait_screen(&s, Duration::from_secs(10), |t| t.contains("READY"));
    // UI가 밝은 테마를 푸시: 디스크에 저장되고, 2031을 켠 이 세션에 알림이 간다.
    let mut light = DEFAULT;
    light.background = Rgb {
        r: 0xfa,
        g: 0xfa,
        b: 0xfa,
    };
    light.foreground = Rgb {
        r: 0x20,
        g: 0x20,
        b: 0x20,
    };
    assert!(look.set_theme(light).unwrap());
    s.theme_changed();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(&out).unwrap(),
        "\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\\x1b]11;rgb:1c1c/1c1c/1c1c\x07\x1b[1;1R\x1b[?6c\x1b[?997;2n"
    );
    s.wait().unwrap();

    // 2) krilld 재기동: 디스크의 마지막 테마로 답한다(앱은 여전히 없다).
    let look = Arc::new(Appearance::load(theme_path));
    let out = dir.join("replies-2");
    let (mut child, s) = spawn_with(6, &query_script(queries, &out), look);
    assert!(child.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(&out).unwrap(),
        "\x1b]10;rgb:2020/2020/2020\x1b\\\x1b]11;rgb:fafa/fafa/fafa\x07\x1b[1;1R\x1b[?6c"
    );
    s.wait().unwrap();
    fs::remove_dir_all(&dir).unwrap();
}
