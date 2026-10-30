//! OWE-490: 실제 `krill-holder`·`krilld`(앱 옆)에 앱과 같은 경로(`krill_app::layout` + `krill_app::pane`)로 붙는 헤드리스
//! 앱 프로세스를 띄워 확인한다.
//!
//! 1. 첫 실행: 데몬 기동 → 칸 세션 생성·레이아웃 저장 → 화면 스트림 → 키 입력이 PTY 에코로 화면에 온다.
//! 2. 그 프로세스를 `kill -9` → 다시 실행: 레이아웃의 같은 세션에 다시 붙고 첫 스냅샷에 이전 출력이 있다.
//! 3. 1,500줄 출력 뒤 위로 끝까지 스크롤: 핫 → 콜드 페이지를 받아 이어 붙인 히스토리 + 화면에 `L1`~`L1500`이
//!    빠짐·겹침 없이 한 번씩 있다.
//!    다시 실행한 앱이 칸 크기를 100×30으로 맞추면(`Pane::fit`) PTY의 `stty size`가 `30 100`이다(OWE-534).
//! 4. 재부팅(MK-032): holder·krilld를 `kill -9` → 다시 실행: 데몬을 새로 띄우고, 레이아웃의 옛 세션은 목록에 없으니
//!    디스크 기록의 마지막 화면(`L1500`)과 스크롤백을 콜드 화면으로 받는다. "다시 시작"(칸 세션 떼기)이면 새 세션.
//!
//! 테스트 세션은 로그인 셸 대신 `HISTFILE=/dev/null`인 `/bin/sh`다(사용자 셸 기록·설정을 건드리지 않게).
//! 상태 디렉터리는 임시 디렉터리(`KRILL_STATE_DIR`, 자식 프로세스에만 줌).
//!
//! 재현: `cargo nextest run -p krill-app --test pane_e2e --no-capture` (측정값은 `MK490` 줄).
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    clippy::print_stdout,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use krill_app::layout::Layout;
use krill_app::pane::{self, Pane, Program};
use krill_proto::daemon::{self, Paths};
use krill_proto::input::Now;
use krill_proto::ipc::{self, ClientRole};

const APP: &str = env!("CARGO_BIN_EXE_krill-app");
const PHASE: &str = "MK490_E2E_PHASE";
const WAIT: Duration = Duration::from_secs(30);
const LINES: usize = 1_500;

#[path = "support/release_survival.rs"]
mod release_survival;

#[path = "support/memory_gate.rs"]
mod memory_gate;

fn paths(state: &Path) -> Paths {
    let bin = Path::new(APP).parent().unwrap();
    Paths {
        socket: state.join("run/krilld.sock"),
        holder: bin.join("krill-holder"),
        krilld: bin.join("krilld"),
    }
}

fn program() -> Program {
    let s = |v: &str| v.as_bytes().to_vec();
    Program::Custom {
        program: s("/usr/bin/env"),
        args: vec![s("HISTFILE=/dev/null"), s("PS1=$ "), s("/bin/sh")],
    }
}

fn texts(p: &Pane<pane::Writer>) -> Vec<String> {
    p.shown()
        .rows
        .iter()
        .map(|r| {
            r.iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// 앱 한 실행: 칸 하나를 세션에 붙인 상태.
struct App {
    pane: Pane<pane::Writer>,
    frames: Receiver<(u8, Vec<u8>)>,
}

impl App {
    fn open(paths: &Paths) -> (Self, bool, Instant) {
        let t0 = Instant::now();
        let layout_path = krill_app::layout::default_path().unwrap();
        let mut layout = Layout::load(&layout_path).unwrap_or_default();
        let id = layout.visible()[0];
        let pane::Found::Live(session, created) =
            pane::ensure_session(paths, layout.session(id), &program()).unwrap()
        else {
            unreachable!("expected a live session");
        };
        layout.set_session(id, session);
        layout.save(&layout_path).unwrap();
        let l = pane::attach(paths, session, None).unwrap();
        let mut pane = Pane::default();
        assert!(
            pane.link(session, l.instance, l.writer, l.features, Now::get())
                .is_empty()
        );
        let (tx, frames) = mpsc::channel();
        let mut reader = l.reader;
        thread::spawn(move || {
            while let Ok(Some(f)) = ipc::read_frame(&mut reader) {
                if tx.send(f).is_err() {
                    return;
                }
            }
        });
        (Self { pane, frames }, created, t0)
    }

    /// 프레임을 적용하며 `done`이 참이 될 때까지 기다린다.
    fn until(&mut self, what: &str, done: impl Fn(&Pane<pane::Writer>) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(&self.pane) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "timed out waiting for {what}: {:?}",
                texts(&self.pane)
            );
            if let Ok((k, b)) = self.frames.recv_timeout(left) {
                let events = self.pane.frame(k, &b).unwrap();
                if let Some(notice) = events.history_notice {
                    eprintln!("history notice while waiting for {what}: {notice}");
                }
            }
        }
    }

    fn has_row(&mut self, row: &str) {
        self.until(row, |p| texts(p).iter().any(|t| t == row));
    }

    fn type_text(&mut self, s: &str) {
        assert_eq!(self.pane.input(s.as_bytes(), Now::get()), None);
    }
}

fn say(line: &str) {
    println!("MK490 {line}");
    std::io::stdout().flush().unwrap();
}

/// 자식 프로세스 본체(`PHASE`가 없으면 아무것도 안 한다).
#[test]
fn child() {
    let Some(phase) = std::env::var_os(PHASE) else {
        return;
    };
    let state = daemon::state_dir().unwrap();
    let paths = paths(&state);
    if phase == "first" {
        let conn = daemon::connect(&paths, &daemon::watch_hello(ClientRole::App, "e2e")).unwrap();
        say(&format!("KRILLD {}", conn.server.instance_id));
        let (mut app, created, _) = App::open(&paths);
        let t = Instant::now();
        app.type_text("echo mk490-$((6*7))\r");
        app.has_row("mk490-42");
        say(&format!(
            "READY {} {created} echo_ms {:.1}",
            app.pane.session().unwrap().0,
            t.elapsed().as_secs_f64() * 1e3
        ));
        loop {
            thread::sleep(Duration::from_secs(60)); // kill -9를 기다린다
        }
    }

    if phase == "cold" {
        cold_phase(&paths);
        return;
    }

    // 다시 실행한 앱: 같은 세션, 첫 스냅샷에 이전 출력.
    let (mut app, created, t0) = App::open(&paths);
    app.until("first snapshot", |p| p.shown().lines > 0);
    let reattach = t0.elapsed();
    assert!(
        texts(&app.pane).iter().any(|t| t == "mk490-42"),
        "{:?}",
        texts(&app.pane)
    );
    say(&format!(
        "REATTACH {} {created} ms {:.1}",
        app.pane.session().unwrap().0,
        reattach.as_secs_f64() * 1e3
    ));

    // OWE-534: 칸 크기를 바꾸면 PTY 크기가 따라온다.
    assert!(app.pane.fit(100, 30).is_none());
    app.type_text("stty size\r");
    app.has_row("30 100");
    let s = app.pane.screen();
    assert_eq!((s.cols, s.lines), (100, 30));

    // 지연 히스토리: 핫 1,000행을 넘는 출력 뒤 끝까지 올라가며 모은다.
    app.type_text(&format!("printf 'L%d\\n' $(seq 1 {LINES})\r"));
    app.has_row(&format!("L{LINES}"));
    // 응답 없는 500ms가 히스토리의 끝은 아니다. 모든 페이지를 요청해 가장 오래된 행을 기다린다.
    app.pane.scroll(10_000);
    app.has_row("L1");
    // Collect one newly exposed row per step, including asynchronous sparse
    // cache misses. A retained viewport must not masquerade as a completed move.
    let mut all = texts(&app.pane);
    while app.pane.scroll_offset() > 0 {
        let target = app.pane.scroll_offset() - 1;
        app.pane.scroll(-1);
        app.until("next history row", |pane| pane.scroll_offset() == target);
        all.push(texts(&app.pane).pop().unwrap());
    }
    let numbers: Vec<usize> = all
        .iter()
        .filter_map(|t| t.strip_prefix('L')?.parse().ok())
        .collect();
    let want: Vec<usize> = (1..=LINES).collect();
    let dup = numbers.len().saturating_sub(want.len());
    say(&format!(
        "HISTORY rows {} numbered {} first {:?} last {:?}",
        all.len(),
        numbers.len(),
        numbers.first(),
        numbers.last()
    ));
    assert!(dup == 0 && numbers == want, "history mismatch: {numbers:?}");
}

/// 재부팅 뒤 앱: 레이아웃은 그대로, 세션은 콜드 화면, 다시 시작하면 새 세션.
fn cold_phase(paths: &Paths) {
    let t = Instant::now();
    let conn = daemon::connect(paths, &daemon::watch_hello(ClientRole::App, "e2e")).unwrap();
    say(&format!("KRILLD {}", conn.server.instance_id));
    let layout_path = krill_app::layout::default_path().unwrap();
    let mut layout = Layout::load(&layout_path).unwrap();
    let id = layout.visible()[0];
    let old = layout.session(id).unwrap();
    let pane::Found::Cold(session, cold) =
        pane::ensure_session(paths, Some(old), &program()).unwrap()
    else {
        unreachable!("expected the cold view of session {old:?}");
    };
    assert_eq!(session, old);
    let history = cold.history.len();
    let mut p: Pane<pane::Writer> = Pane::default();
    p.show_cold(session, cold).unwrap();
    let shown = t.elapsed();
    let last = format!("L{LINES}");
    assert!(texts(&p).contains(&last), "{:?}", texts(&p));
    // 스크롤백 끝까지 올라가면 화면 위의 L 행들이 보인다.
    assert!(p.scroll(10_000));
    let top: usize = texts(&p)
        .first()
        .and_then(|t| t.strip_prefix('L')?.parse().ok())
        .unwrap();
    assert!(top < LINES, "scrollback top {top}");
    assert_eq!(p.input(b"x", Now::get()), None, "cold view is read-only");

    layout.clear_session(id);
    let new = pane::resume_session(paths, Some(old), &program()).unwrap();
    assert_ne!(new, old);
    say(&format!(
        "COLD {} {} history {history} top L{top} ms {:.1}",
        old.0,
        new.0,
        shown.as_secs_f64() * 1e3
    ));
}

fn progress_line(line: &str) -> Option<&str> {
    // libtest의 pretty 형식은 첫 출력 앞에 줄바꿈 없이 `test child ... `를 쓴다.
    line.strip_prefix("test child ... ")
        .unwrap_or(line)
        .strip_prefix("MK490 ")
}

#[test]
fn progress_preserves_first_line_after_libtest_prefix() {
    // 실제 `child --exact --nocapture --test-threads=1`의 출력 형식.
    let output = "\nrunning 1 test\ntest child ... MK490 KRILLD 123\nMK490 READY 1 true\nok\n";
    let progress: Vec<_> = output.lines().filter_map(progress_line).collect();
    assert_eq!(progress, ["KRILLD 123", "READY 1 true"]);
}

fn spawn(phase: &str, state: &Path) -> (Child, Receiver<String>) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["child", "--exact", "--nocapture", "--test-threads=1"])
        .env(PHASE, phase)
        .env("KRILL_STATE_DIR", state)
        // `release_check.py`는 이 바이너리를 cargo·nextest 없이 직접 돌린다 — 자식 앱·holder·krilld에 테스트 표식이 없다.
        .env("KRILL_LOG_DIR", state.join("logs"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(l) = progress_line(&line) {
                println!("{line}");
                if tx.send(l.to_owned()).is_err() {
                    return;
                }
            }
        }
    });
    (child, rx)
}

fn kill9(pid: u32) {
    Command::new("/bin/kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
}

fn ppid(pid: u32) -> u32 {
    let o = Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap().trim().parse().unwrap()
}

/// 테스트가 끝나면 holder와 krilld를 죽이고 상태 디렉터리를 지운다.
struct Cleanup {
    pids: Vec<u32>,
    dir: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for &p in &self.pids {
            kill9(p); // holder 먼저: krilld가 먼저 죽으면 holder가 다시 띄운다.
        }
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn next(rx: &Receiver<String>, tag: &str) -> Vec<String> {
    let line = rx.recv_timeout(WAIT * 2).unwrap();
    let words: Vec<String> = line.split(' ').map(str::to_owned).collect();
    assert_eq!(words[0], tag, "{line}");
    words
}

#[test]
fn app_kill9_reattaches_same_session() {
    let bin = Path::new(APP).parent().unwrap();
    for b in ["krill-holder", "krilld"] {
        assert!(
            bin.join(b).is_file(),
            "{b} is not built next to krill-app (run the whole workspace: cargo nextest run --workspace)"
        );
    }
    // 소켓 경로가 104바이트를 넘지 않게 짧은 임시 경로.
    let dir = PathBuf::from(format!("/tmp/mk490-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let mut guard = Cleanup {
        pids: Vec::new(),
        dir: dir.clone(),
    };

    let (mut first, rx) = spawn("first", &dir);
    let krilld: u32 = next(&rx, "KRILLD")[1].parse().unwrap();
    guard.pids = vec![ppid(krilld), krilld];
    let ready = next(&rx, "READY");
    assert_eq!(ready[2], "true", "first run creates the session");
    kill9(first.id());
    first.wait().unwrap();

    let (mut second, rx) = spawn("second", &dir);
    let again = next(&rx, "REATTACH");
    assert_eq!(again[1], ready[1], "reattached to another session");
    assert_eq!(again[2], "false", "second run must not create a session");
    next(&rx, "HISTORY");
    assert!(second.wait().unwrap().success());

    // 재부팅: holder·krilld가 함께 사라진다.
    for p in std::mem::take(&mut guard.pids) {
        kill9(p);
        let deadline = Instant::now() + WAIT;
        while Command::new("/bin/kill")
            .args(["-0", &p.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            assert!(Instant::now() < deadline, "{p} did not die");
            thread::sleep(Duration::from_millis(20));
        }
    }
    let (mut cold, rx) = spawn("cold", &dir);
    let krilld: u32 = next(&rx, "KRILLD")[1].parse().unwrap();
    guard.pids = vec![ppid(krilld), krilld];
    let c = next(&rx, "COLD");
    assert_eq!(c[1], ready[1], "cold view of the old session");
    assert!(cold.wait().unwrap().success());
}
