//! OWE-836: Windows 세션 생존 e2e. 실제 `krill-holder.exe`·`krilld.exe`(ConPTY)에 붙어 불변식 1번을 잰다.
//!
//! 1. [`app_taskkill_loop_keeps_session_and_bytes`]: 앱과 같은 경로(`krill_app::layout` + `krill_app::pane`)로 붙는
//!    헤드리스 앱 프로세스가 세션에 출력을 일으킨 직후 `taskkill /F` → 다시 실행해 같은 세션에 붙기를 반복한다.
//!    각 실행은 해시 검사용 원본 스트림도 이어 받아 파일에 붙이고(앞 실행이 받은 길이부터), 끝에서 `Raw { from: 0 }`
//!    재생과 SHA-256·바이트 수가 같아야 한다(Gap 0, 누락 줄 0).
//! 2. [`krilld_taskkill_during_output`]: 출력이 흐르는 중 krilld를 `taskkill /F` → holder가 새 krilld를 띄우고 세션이
//!    산다. 손실은 ARCHITECTURE §4.1대로 생길 수 있어 단언하지 않고 잰다(재생 Gap·누락 줄).
//! 3. [`holder_taskkill_keeps_orphan_serving`]: holder를 `taskkill /F` → krilld가 고아 세션을 계속 서빙하고 새 세션은
//!    거절한다. 이어 krilld까지 죽이면 무엇이 남는지 기록한다(새 holder `Adopt`는 미구현).
//! 4. [`big_output_and_alt_screen_hash`]: 수 MB 출력(`type`)과 대체 화면 TUI에서 실시간 원본과 재생 원본의 SHA-256이
//!    같고, 생성한 줄이 빠짐없이 화면 출력에 있다.
//!
//! `ConPTY`는 자식이 쓴 바이트를 그대로 넘기지 않고 다시 그린 VT를 내보낸다. 그래서 바이트 해시는 "krilld가 PTY에서
//! 읽은 원본"(실시간 구독 ↔ 링·세그먼트 재생)끼리 비교하고, 생성기와의 비교는 줄 단위(번호·검사 꼬리표)로 한다.
//!
//! 비밀번호 로그온(DPAPI 세그먼트 키)이 아니면 krilld가 디스크 기록을 끄므로 링(256KiB) 밖 재생이 Gap이 된다. 그래서
//! 모두 `#[ignore]`이고, `KRILL_E2E_BIN`(holder·krilld 디렉터리, 기본은 `krill-app.exe` 옆)·`KRILL_E2E_DIR`(작업
//! 루트, 지정하면 지우지 않는다)·횟수 환경변수로 돌린다.
//!
//! 재현: `cargo build --release --workspace --bins` 뒤
//! `cargo test --release -p krill-app --test win_survival_e2e -- --ignored --nocapture --test-threads=1`
//! (측정값은 `OWE836` 줄).
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
    clippy::print_stdout,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{env, thread};

use krill_app::layout::Layout;
use krill_app::pane::{self, Pane, Program};
use krill_proto::SessionId;
use krill_proto::control::{CaptureFormat, Create, Reply, Request};
use krill_proto::daemon::{self, Paths};
use krill_proto::input::{Input, InputAck, InputId, InputResult, Now};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::os::net::UnixStream;
use krill_proto::screen::{self, Attach, Attached, GapMsg, Msg as _, Seq, Stream, kind};
use sha2::{Digest as _, Sha256};

const PHASE: &str = "OWE836_PHASE";
const WAIT: Duration = Duration::from_secs(60);
const CMD: &[u8] = br"C:\Windows\System32\cmd.exe";

fn env_num(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn say(line: &str) {
    println!("OWE836 {line}");
    std::io::stdout().flush().unwrap();
}

fn bin_dir() -> PathBuf {
    env::var_os("KRILL_E2E_BIN").map_or_else(
        || {
            Path::new(env!("CARGO_BIN_EXE_krill-app"))
                .parent()
                .unwrap()
                .to_path_buf()
        },
        PathBuf::from,
    )
}

fn paths(state: &Path) -> Paths {
    let bin = bin_dir();
    Paths {
        socket: state.join("k.sock"),
        holder: bin.join("krill-holder.exe"),
        krilld: bin.join("krilld.exe"),
    }
}

/// 세션 셸: `AutoRun` 없는 cmd(사용자 설정을 타지 않는다).
fn shell() -> Program {
    Program::Custom {
        program: CMD.to_vec(),
        args: vec![b"/D".to_vec()],
    }
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// `(p50, max)` 밀리초.
fn spread(samples: &mut [Duration]) -> (f64, f64) {
    samples.sort();
    let p50 = samples.get(samples.len() / 2).copied().unwrap_or_default();
    let max = samples.last().copied().unwrap_or_default();
    (ms(p50), ms(max))
}

// ---- 프로세스 ----

fn taskkill(pid: u32, tree: bool) -> bool {
    let mut c = Command::new("taskkill.exe");
    c.arg("/F");
    if tree {
        c.arg("/T");
    }
    c.args(["/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn alive(pid: u32) -> bool {
    let o = Command::new("tasklist.exe")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\""))
}

fn parent(pid: u32) -> u32 {
    let q = format!("(Get-CimInstance Win32_Process -Filter 'ProcessId = {pid}').ParentProcessId");
    let o = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &q])
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().parse().unwrap()
}

fn wait_dead(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while alive(pid) {
        if Instant::now() > deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
    true
}

/// 테스트 하나의 상태 디렉터리와 정리할 프로세스(holder 먼저: krilld가 먼저 죽으면 holder가 다시 띄운다).
struct Rig {
    dir: PathBuf,
    paths: Paths,
    pids: Vec<u32>,
    keep: bool,
}

impl Rig {
    fn new(tag: &str) -> Self {
        let keep = env::var_os("KRILL_E2E_DIR").is_some();
        let root = env::var_os("KRILL_E2E_DIR").map_or_else(env::temp_dir, PathBuf::from);
        let dir = root.join(format!("{tag}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let paths = paths(&dir);
        for p in [&paths.holder, &paths.krilld] {
            assert!(
                p.is_file(),
                "{} missing (cargo build --workspace --bins)",
                p.display()
            );
        }
        say(&format!(
            "RIG {tag} dir={} bin={}",
            dir.display(),
            bin_dir().display()
        ));
        Self {
            dir,
            paths,
            pids: Vec::new(),
            keep,
        }
    }

    /// 데몬을 띄우고(이미 있으면 붙고) `(holder, krilld)`를 정리 대상에 넣는다.
    /// 이 프로세스에는 `KRILL_STATE_DIR`을 줄 수 없어(`set_var` 금지) 자식이 띄운다. holder가 그 환경을 물려받는다.
    fn launch(&mut self) -> (u32, u32) {
        let (holder, krilld, _) = self.launch_full();
        (holder, krilld)
    }

    /// [`Rig::launch`]와 같되 이번에 holder를 새로 띄웠는지도 준다.
    fn launch_full(&mut self) -> (u32, u32, bool) {
        let (mut child, rx) = spawn("launch", &self.dir, &[]);
        let words = next(&rx, "KRILLD");
        assert!(child.wait().unwrap().success());
        let krilld: u32 = words[1].parse().unwrap();
        let holder = parent(krilld);
        self.pids.extend([holder, krilld]);
        (holder, krilld, words[2] == "true")
    }

    fn control(&self) -> UnixStream {
        control(&self.paths)
    }

    /// 명령줄에 이 상태 디렉터리가 들어간 프로세스(`name`이 비면 전부) 중 정리 대상에 없는 것.
    fn strays(&self, name: &str) -> Vec<u32> {
        let q = format!(
            "Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like '*{}*' -and $_.Name -like '*{name}' }} | ForEach-Object {{ \"$($_.ProcessId) $($_.Name)\" }}",
            self.dir.display()
        );
        let o = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &q])
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| {
                // 이 조회를 하는 powershell 자신도 명령줄에 상태 디렉터리가 들어 있다.
                let (pid, name) = l.trim().split_once(' ')?;
                (name != "powershell.exe").then(|| pid.parse().ok())?
            })
            .filter(|p| !self.pids.contains(p) && *p != std::process::id())
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        for &p in &self.pids {
            taskkill(p, true);
        }
        // 재결합 시도 등으로 이 상태 디렉터리를 쥔 채 남은 holder·krilld.
        for p in self.strays("") {
            taskkill(p, true);
        }
        if !self.keep {
            thread::sleep(Duration::from_millis(300));
            fs::remove_dir_all(&self.dir).ok();
        }
    }
}

// ---- krilld 연결 ----

fn control(paths: &Paths) -> UnixStream {
    daemon::handshake(
        &paths.socket,
        &daemon::client_hello(ClientRole::Cli, "owe836"),
        WAIT,
    )
    .unwrap()
    .stream
}

fn request(s: &mut UnixStream, r: &Request) -> Reply {
    s.write_all(&r.encode().unwrap()).unwrap();
    let (k, body) = ipc::read_frame(s).unwrap().unwrap();
    Reply::decode(k, &body).unwrap()
}

fn create(s: &mut UnixStream, cwd: &Path) -> (SessionId, u32) {
    let r = request(
        s,
        &Request::Create(Create {
            account: None,
            program: shell(),
            cwd: cwd.as_os_str().as_encoded_bytes().to_vec(),
            cols: 120,
            rows: 40,
        }),
    );
    let Reply::Created { session, pid } = r else {
        unreachable!("create failed: {r:?}");
    };
    (session, pid)
}

/// 보이는 화면 평문.
fn capture(s: &mut UnixStream, session: SessionId) -> String {
    let r = request(
        s,
        &Request::Capture {
            session,
            format: CaptureFormat::Plain,
        },
    );
    let Reply::Captured { bytes, .. } = r else {
        unreachable!("capture failed: {r:?}");
    };
    String::from_utf8_lossy(&bytes).into_owned()
}

/// 화면에 `done`이 참인 평문이 나올 때까지 기다린다.
fn wait_screen(s: &mut UnixStream, session: SessionId, what: &str, done: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let text = capture(s, session);
        if done(&text) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {text}"
        );
        thread::sleep(Duration::from_millis(30));
    }
}

fn has_line(text: &str, line: &str) -> bool {
    text.lines().any(|l| l.trim_end() == line)
}

/// 화면 스트림에 붙은 입력 연결.
/// 화면 프레임을 읽지 않으므로 오래 쥐면 krilld가 쓰기 기한(10초)으로 끊는다 — 입력마다 새로 연다.
fn typist(paths: &Paths, session: SessionId) -> UnixStream {
    let mut s = control(paths);
    s.write_all(
        &Attach {
            session,
            stream: Stream::Screen { known: None },
        }
        .encode()
        .unwrap(),
    )
    .unwrap();
    let (k, _) = ipc::read_frame(&mut s).unwrap().unwrap();
    assert_eq!(k, kind::ATTACHED);
    s
}

fn type_line(paths: &Paths, session: SessionId, text: &str) {
    let s = &mut typist(paths, session);
    s.write_all(
        &Input {
            id: InputId(1),
            bytes: format!("{text}\r").into_bytes(),
        }
        .encode()
        .unwrap(),
    )
    .unwrap();
    loop {
        let (k, body) = ipc::read_frame(s).unwrap().unwrap();
        if k == krill_proto::input::kind::INPUT_ACK {
            assert_eq!(
                InputAck::decode(k, &body).unwrap().result,
                InputResult::Applied
            );
            return;
        }
    }
}

fn attach_raw(paths: &Paths, session: SessionId, from: u64) -> (UnixStream, u64) {
    let mut s = control(paths);
    s.write_all(
        &Attach {
            session,
            stream: Stream::Raw { from: Seq(from) },
        }
        .encode()
        .unwrap(),
    )
    .unwrap();
    let (k, body) = ipc::read_frame(&mut s).unwrap().unwrap();
    let at = Attached::decode(k, &body).unwrap();
    (s, at.seq.0)
}

/// `Raw { from: 0 }`으로 지금 끝까지 재생한다. `(바이트, Gap들)`.
fn replay(paths: &Paths, session: SessionId) -> (Vec<u8>, Vec<GapMsg>) {
    let (mut stream, end) = attach_raw(paths, session, 0);
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let (mut got, mut gaps, mut next) = (Vec::new(), Vec::new(), 0u64);
    while next < end {
        let (k, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
        match k {
            kind::OUTPUT => {
                let (seq, bytes) = screen::decode_output(k, &body).unwrap();
                if seq.0 != next {
                    gaps.push(GapMsg {
                        start: Seq(next),
                        len: seq.0.saturating_sub(next),
                    });
                }
                got.extend_from_slice(bytes);
                next = seq.0 + u64::try_from(bytes.len()).unwrap();
            }
            kind::GAP => {
                let gap = GapMsg::decode(k, &body).unwrap();
                next = gap.start.0 + gap.len;
                gaps.push(gap);
            }
            kind::CLOSED => break,
            _ => {}
        }
    }
    (got, gaps)
}

/// 실시간 원본 구독(앱처럼 끊기면 다시 붙어 받은 데까지 이어 받는다).
#[derive(Debug, Default)]
struct Live {
    bytes: Vec<u8>,
    next: u64,
    gaps: Vec<GapMsg>,
    reconnects: u32,
    closed: bool,
}

fn follow(paths: &Paths, session: SessionId) -> Arc<Mutex<Live>> {
    let live = Arc::new(Mutex::new(Live::default()));
    let (paths, out) = (paths.clone(), Arc::clone(&live));
    thread::spawn(move || {
        loop {
            let from = out.lock().unwrap().next;
            let Ok(conn) = daemon::handshake(
                &paths.socket,
                &daemon::client_hello(ClientRole::Cli, "owe836-live"),
                WAIT,
            ) else {
                thread::sleep(Duration::from_millis(50));
                continue;
            };
            let mut stream = conn.stream;
            stream.set_read_timeout(None).ok();
            let attach = Attach {
                session,
                stream: Stream::Raw { from: Seq(from) },
            };
            if stream.write_all(&attach.encode().unwrap()).is_err() {
                continue;
            }
            loop {
                let Ok(Some((k, body))) = ipc::read_frame(&mut stream) else {
                    out.lock().unwrap().reconnects += 1;
                    break;
                };
                let mut state = out.lock().unwrap();
                match k {
                    kind::OUTPUT => {
                        let (seq, bytes) = screen::decode_output(k, &body).unwrap();
                        if seq.0 != state.next {
                            let next = state.next;
                            state.gaps.push(GapMsg {
                                start: Seq(next),
                                len: seq.0.saturating_sub(next),
                            });
                        }
                        state.bytes.extend_from_slice(bytes);
                        state.next = seq.0 + u64::try_from(bytes.len()).unwrap();
                    }
                    kind::GAP => {
                        let gap = GapMsg::decode(k, &body).unwrap();
                        state.next = gap.start.0 + gap.len;
                        state.gaps.push(gap);
                    }
                    kind::CLOSED | kind::REFUSED => {
                        state.closed = true;
                        return;
                    }
                    _ => {}
                }
            }
        }
    });
    live
}

fn wait_live(live: &Mutex<Live>, what: &str, done: impl Fn(&[u8]) -> bool) {
    let deadline = Instant::now() + WAIT * 5;
    loop {
        if done(&live.lock().unwrap().bytes) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

// ---- 출력 해석 ----

/// VT 제어열(CSI·OSC·2바이트 ESC)을 뺀 글자.
fn plain(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] != 0x1b {
            out.push(raw[i]);
            i += 1;
            continue;
        }
        match raw.get(i + 1) {
            Some(b'[') => {
                i += 2;
                while i < raw.len() && !(0x40..=0x7e).contains(&raw[i]) {
                    i += 1;
                }
                i += 1;
            }
            Some(b']') => {
                i += 2;
                while i < raw.len() && raw[i] != 0x07 && raw[i] != 0x1b {
                    i += 1;
                }
                i += if raw.get(i) == Some(&0x1b) { 2 } else { 1 };
            }
            _ => i += 2,
        }
    }
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// 생성기 줄 `{p}{n:07}-{tag:08x}`의 검사 꼬리표.
fn tag(p: u8, n: u64) -> u32 {
    let x = n.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(p);
    u32::try_from(x >> 32).unwrap()
}

fn gen_line(p: u8, n: u64) -> String {
    format!("{}{n:07}-{:08x}", char::from(p), tag(p, n))
}

/// 평문에서 꼬리표가 맞는 `p` 줄 번호들과 꼬리표가 틀린 줄 수.
fn numbers(text: &[u8], p: u8) -> (BTreeSet<u64>, usize) {
    let (mut seen, mut bad) = (BTreeSet::new(), 0);
    for (i, _) in text.iter().enumerate().filter(|&(_, &b)| b == p) {
        let Some(w) = text.get(i + 1..i + 17) else {
            continue;
        };
        let (digits, rest) = w.split_at(7);
        if !digits.iter().all(u8::is_ascii_digit) || rest[0] != b'-' {
            continue;
        }
        let n: u64 = std::str::from_utf8(digits).unwrap().parse().unwrap();
        let hex = std::str::from_utf8(&rest[1..]).ok();
        if hex.and_then(|h| u32::from_str_radix(h, 16).ok()) == Some(tag(p, n)) {
            seen.insert(n);
        } else {
            bad += 1;
        }
    }
    (seen, bad)
}

/// 생성기의 끝 표시 `{p}END-{n}`이 있으면 n.
fn end_count(text: &[u8], p: u8) -> Option<u64> {
    let mark = [p, b'E', b'N', b'D', b'-'];
    let at = text.windows(5).position(|w| w == mark)? + 5;
    let digits: Vec<u8> = text[at..]
        .iter()
        .copied()
        .take_while(u8::is_ascii_digit)
        .collect();
    std::str::from_utf8(&digits).ok()?.parse().ok()
}

fn missing(seen: &BTreeSet<u64>, total: u64) -> u64 {
    total - seen.range(1..=total).count() as u64
}

/// 시나리오 1 평문 한 번 훑기: 라운드별 `I{r:04}-L{n}` 번호들과 `D{r:04}` 줄(끝 표시)이 있었는지.
fn rounds_seen(text: &[u8], rounds: usize) -> (Vec<BTreeSet<u64>>, Vec<bool>) {
    let (mut seen, mut done) = (vec![BTreeSet::new(); rounds], vec![false; rounds]);
    let num = |from: usize| -> (u64, usize) {
        let len = text[from..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        let n = std::str::from_utf8(&text[from..from + len]).unwrap();
        (n.parse().unwrap_or(u64::MAX), len)
    };
    for (i, &b) in text.iter().enumerate() {
        if b != b'I' && b != b'D' {
            continue;
        }
        let (r, len) = num(i + 1);
        let Some(r) = usize::try_from(r).ok().filter(|&r| len == 4 && r < rounds) else {
            continue;
        };
        let at = i + 5;
        if b == b'I' && text.get(at..at + 2) == Some(b"-L") {
            let (n, len) = num(at + 2);
            if len > 0 {
                seen[r].insert(n);
            }
        } else if b == b'D' && matches!(text.get(at), Some(b'\r' | b'\n')) {
            done[r] = true;
        }
    }
    (seen, done)
}

// ---- 자식 프로세스(헤드리스 앱·생성기) ----

/// 자식 프로세스 본체(`PHASE`가 없으면 아무것도 안 한다).
#[test]
fn child() {
    match env::var(PHASE).as_deref() {
        Ok("app") => app_phase(),
        Ok("gen") => gen_phase(),
        Ok("launch") => {
            let paths = paths(&daemon::state_dir().unwrap());
            match daemon::connect(&paths, &daemon::client_hello(ClientRole::Cli, "owe836")) {
                Ok(c) => say(&format!("KRILLD {} {}", c.server.instance_id, c.launched)),
                Err(e) => say(&format!("LAUNCHFAIL {e:?}")),
            }
        }
        _ => {}
    }
}

/// 세션 셸 안에서 도는 줄 생성기. `OWE836_GEN=p,줄 수(0이면 cwd의 stop 파일까지),묶음,쉼 ms`.
fn gen_phase() {
    let spec = env::var("OWE836_GEN").unwrap();
    let f: Vec<&str> = spec.split(',').collect();
    let p = f[0].as_bytes()[0];
    let (total, burst, rest): (u64, u64, u64) = (
        f[1].parse().unwrap(),
        f[2].parse().unwrap(),
        f[3].parse().unwrap(),
    );
    let mut out = std::io::stdout().lock();
    let mut buf = String::new();
    let mut n = 0;
    loop {
        n += 1;
        buf.push_str(&gen_line(p, n));
        buf.push('\n');
        if n % burst == 0 {
            out.write_all(buf.as_bytes()).unwrap();
            out.flush().unwrap();
            buf.clear();
            thread::sleep(Duration::from_millis(rest));
            if total == 0 && Path::new("stop").exists() {
                break;
            }
        }
        if n == total {
            break;
        }
    }
    writeln!(buf, "{}END-{n}", char::from(p)).unwrap();
    out.write_all(buf.as_bytes()).unwrap();
    out.flush().unwrap();
}

/// 앱 한 실행: 칸 하나를 세션에 붙인 상태(`pane_e2e`와 같은 경로).
struct App {
    pane: Pane<pane::Writer>,
    frames: Receiver<(u8, Vec<u8>)>,
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

impl App {
    fn open(paths: &Paths) -> (Self, SessionId, bool) {
        let layout_path = krill_app::layout::default_path().unwrap();
        let mut layout = Layout::load(&layout_path).unwrap_or_default();
        let id = layout.visible()[0];
        let pane::Found::Live(session, created) =
            pane::ensure_session(paths, layout.session(id), &shell()).unwrap()
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
        (Self { pane, frames }, session, created)
    }

    fn until(&mut self, what: &str, done: impl Fn(&[String]) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(&texts(&self.pane)) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "timed out waiting for {what}: {:?}",
                texts(&self.pane)
            );
            if let Ok((k, body)) = self.frames.recv_timeout(left) {
                self.pane.frame(k, &body).unwrap();
            }
        }
    }
}

/// 해시 검사용 원본을 `file` 끝(= 앞 실행들이 받은 길이)부터 이어 붙인다. 이음이 어긋나면 `gaps.txt`에 남기고 멈춘다.
fn raw_to_file(paths: &Paths, session: SessionId, file: PathBuf) {
    let from = fs::metadata(&file).map_or(0, |m| m.len());
    let (mut s, _) = attach_raw(paths, session, from);
    thread::spawn(move || {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .unwrap();
        let mut next = from;
        let gap = |what: String| {
            fs::write(file.with_file_name("gaps.txt"), what).unwrap();
        };
        while let Ok(Some((k, body))) = ipc::read_frame(&mut s) {
            match k {
                kind::OUTPUT => {
                    let (seq, bytes) = screen::decode_output(k, &body).unwrap();
                    if seq.0 > next {
                        return gap(format!("seq {} after {next}", seq.0));
                    }
                    let skip = usize::try_from(next - seq.0).unwrap();
                    if let Some(new) = bytes.get(skip..) {
                        f.write_all(new).unwrap();
                        next += u64::try_from(new.len()).unwrap();
                    }
                }
                kind::GAP => return gap(format!("{:?}", GapMsg::decode(k, &body).unwrap())),
                _ => {}
            }
        }
    });
}

fn app_phase() {
    let state = daemon::state_dir().unwrap();
    let paths = paths(&state);
    let iter: usize = env_num("OWE836_ITER", 0);
    let lines = env_num("OWE836_LINES", 200);
    let t0 = Instant::now();
    // 앱처럼 감시 연결을 먼저 연다(첫 실행이면 holder·krilld를 띄운다).
    let watch = daemon::connect(&paths, &daemon::watch_hello(ClientRole::App, "e2e")).unwrap();
    say(&format!("KRILLD {}", watch.server.instance_id));
    let (mut app, session, created) = App::open(&paths);
    say(&format!("OPEN {} {created}", session.0));
    raw_to_file(&paths, session, state.join("app-raw.bin"));
    if iter > 0 {
        let prev = format!("D{:04}", iter - 1);
        app.until(&prev, |rows| rows.contains(&prev));
        say(&format!("REATTACH ms {:.1}", ms(t0.elapsed())));
    }
    let cmd = format!("(for /L %n in (1,1,{lines}) do @echo I{iter:04}-L%n)&echo D{iter:04}");
    assert_eq!(
        app.pane.input(format!("{cmd}\r").as_bytes(), Now::get()),
        None
    );
    let echoed = format!("&echo D{iter:04}");
    app.until("input echo", |rows| {
        rows.iter().any(|r| r.ends_with(&echoed))
    });
    say("READY"); // 감시 연결(watch)은 앱처럼 죽을 때까지 쥔다.
    loop {
        thread::sleep(Duration::from_secs(60)); // taskkill /F를 기다린다
    }
}

fn progress_line(line: &str) -> Option<&str> {
    line.strip_prefix("test child ... ")
        .unwrap_or(line)
        .strip_prefix("OWE836 ")
}

fn spawn(phase: &str, state: &Path, envs: &[(&str, String)]) -> (Child, Receiver<String>) {
    let mut child = Command::new(env::current_exe().unwrap())
        .args(["child", "--exact", "--nocapture", "--test-threads=1"])
        .env(PHASE, phase)
        .env("KRILL_STATE_DIR", state)
        .envs(envs.iter().map(|(k, v)| (k, v)))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(l) = progress_line(&line)
                && tx.send(l.to_owned()).is_err()
            {
                return;
            }
        }
    });
    (child, rx)
}

fn next(rx: &Receiver<String>, tag: &str) -> Vec<String> {
    let line = rx.recv_timeout(WAIT * 2).unwrap();
    let words: Vec<String> = line.split(' ').map(str::to_owned).collect();
    assert_eq!(words[0], tag, "{line}");
    words
}

/// 세션 셸에서 이 테스트 바이너리를 줄 생성기로 실행하는 명령.
fn gen_command(spec: &str) -> String {
    format!(
        "set {PHASE}=gen&set OWE836_GEN={spec}&\"{}\" child --exact --nocapture --test-threads=1",
        env::current_exe().unwrap().display()
    )
}

// ---- 시나리오 ----

#[test]
#[ignore = "Windows 비밀번호 로그온 + release holder·krilld 필요(OWE-836)"]
fn app_taskkill_loop_keeps_session_and_bytes() {
    let kills = env_num("KRILL_E2E_APP_KILLS", 3);
    let lines = env_num("KRILL_E2E_APP_LINES", 200);
    let mut rig = Rig::new("app");
    let (mut first_krilld, mut session) = (0u32, None);
    let (mut rounds, mut reattach) = (Vec::new(), Vec::new());
    let t_all = Instant::now();
    for i in 0..kills {
        let t = Instant::now();
        let envs = [
            ("OWE836_ITER", i.to_string()),
            ("OWE836_LINES", lines.to_string()),
        ];
        let (mut child, rx) = spawn("app", &rig.dir, &envs);
        let krilld: u32 = next(&rx, "KRILLD")[1].parse().unwrap();
        if i == 0 {
            first_krilld = krilld;
            rig.pids.extend([parent(krilld), krilld]);
        }
        assert_eq!(krilld, first_krilld, "krilld restarted at round {i}");
        let open = next(&rx, "OPEN");
        let id: u64 = open[1].parse().unwrap();
        assert_eq!(open[2], (i == 0).to_string(), "round {i} created flag");
        assert_eq!(
            *session.get_or_insert(id),
            id,
            "round {i} attached to another session"
        );
        if i > 0 {
            reattach.push(Duration::from_secs_f64(
                next(&rx, "REATTACH")[2].parse::<f64>().unwrap() / 1e3,
            ));
        }
        next(&rx, "READY");
        assert!(taskkill(child.id(), false), "taskkill app round {i}");
        child.wait().unwrap();
        rounds.push(t.elapsed());
        if (i + 1) % 100 == 0 {
            say(&format!(
                "PROGRESS {} {:.1}s",
                i + 1,
                t_all.elapsed().as_secs_f64()
            ));
        }
    }
    let total = t_all.elapsed();
    let session = SessionId(session.unwrap());

    // 마지막 앱 대신 이 프로세스가 붙어 마지막 출력이 끝났는지 보고 원본을 끝까지 이어 받는다.
    let mut c = rig.control();
    let last = format!("D{:04}", kills - 1);
    wait_screen(&mut c, session, &last, |t| has_line(t, &last));
    thread::sleep(Duration::from_millis(500));
    let Reply::Sessions(list) = request(&mut c, &Request::List) else {
        unreachable!("list");
    };
    let info = list.iter().find(|s| s.session == session).unwrap();
    assert!(
        info.status.is_none() && alive(info.pid),
        "session process died"
    );
    let gaps_file = rig.dir.join("gaps.txt");
    assert!(
        !gaps_file.exists(),
        "app raw seam: {}",
        fs::read_to_string(&gaps_file).unwrap()
    );
    let (full, gaps) = replay(&rig.paths, session);
    let file = rig.dir.join("app-raw.bin");
    let mut pieces = fs::read(&file).unwrap();
    let had = pieces.len();
    if had < full.len() {
        let (mut s, _) = attach_raw(&rig.paths, session, u64::try_from(had).unwrap());
        s.set_read_timeout(Some(WAIT)).unwrap();
        while pieces.len() < full.len() {
            let (k, body) = ipc::read_frame(&mut s).unwrap().unwrap();
            assert_ne!(k, kind::GAP, "gap while catching up");
            if k == kind::OUTPUT {
                let (seq, bytes) = screen::decode_output(k, &body).unwrap();
                assert_eq!(seq.0, u64::try_from(pieces.len()).unwrap());
                pieces.extend_from_slice(bytes);
            }
        }
    }
    let (seen, done) = rounds_seen(&plain(&full), kills);
    let lost: u64 = seen.iter().map(|s| missing(s, lines as u64)).sum();
    let marks = done.iter().filter(|&&d| d).count();
    let (r50, rmax) = spread(&mut rounds);
    let (a50, amax) = spread(&mut reattach);
    say(&format!(
        "RESULT app_kill rounds={kills} lines/round={lines} bytes={} app_bytes_before_catchup={had} \
         sha_full={} sha_app={} gaps={} missing_lines={lost} markers={marks}/{kills} \
         round_p50_ms={r50:.1} round_max_ms={rmax:.1} reattach_p50_ms={a50:.1} reattach_max_ms={amax:.1} \
         total_s={:.1} session_pid={} krilld={first_krilld}",
        full.len(),
        sha(&full),
        sha(&pieces),
        gaps.len(),
        total.as_secs_f64(),
        info.pid,
    ));
    assert!(gaps.is_empty(), "replay gaps {gaps:?}");
    assert_eq!(
        sha(&pieces),
        sha(&full),
        "app-received bytes differ from replay"
    );
    assert_eq!(lost, 0, "missing lines");
    assert_eq!(marks, kills, "missing round markers");
}

#[test]
#[ignore = "Windows 비밀번호 로그온 + release holder·krilld 필요(OWE-836)"]
fn krilld_taskkill_during_output() {
    let kills = env_num("KRILL_E2E_KRILLD_KILLS", 3);
    let mut rig = Rig::new("krilld");
    let (holder, mut krilld) = rig.launch();
    let mut c = rig.control();
    let (session, pid) = create(&mut c, &rig.dir);
    rig.pids.push(pid);
    let live = follow(&rig.paths, session);
    type_line(&rig.paths, session, &gen_command("K,0,50,10"));
    wait_live(&live, "generator start", |b| contains(b, b"K0000100-"));
    let mut recover = Vec::new();
    let t_all = Instant::now();
    for i in 0..kills {
        // holder는 1초 넘게 산 krilld가 죽으면 바로 다시 띄운다(그보다 빠른 반복은 백오프).
        thread::sleep(Duration::from_millis(1_500));
        let before = live.lock().unwrap().bytes.len();
        let at = Instant::now();
        assert!(taskkill(krilld, false), "taskkill krilld round {i}");
        let fresh = loop {
            if let Ok(n) = daemon::handshake(
                &rig.paths.socket,
                &daemon::client_hello(ClientRole::Cli, "owe836"),
                Duration::from_secs(5),
            ) && n.server.instance_id != u64::from(krilld)
            {
                break u32::try_from(n.server.instance_id).unwrap();
            }
            assert!(at.elapsed() < WAIT, "krilld did not come back (round {i})");
            thread::sleep(Duration::from_millis(20));
        };
        recover.push(at.elapsed());
        rig.pids.push(fresh);
        krilld = fresh;
        assert!(alive(pid), "session died at round {i}");
        wait_live(&live, "output after restart", |b| b.len() > before + 4_096);
    }
    let kill_secs = t_all.elapsed().as_secs_f64();
    assert_eq!(
        parent(krilld),
        holder,
        "new krilld is not the holder's child"
    );
    fs::write(rig.dir.join("stop"), b"").unwrap();
    wait_live(&live, "generator end", |b| {
        end_count(&plain(b), b'K').is_some()
    });
    thread::sleep(Duration::from_millis(500));
    let (full, gaps) = replay(&rig.paths, session);
    let l = live.lock().unwrap();
    let total = end_count(&plain(&l.bytes), b'K').unwrap();
    let (seen_replay, bad_replay) = numbers(&plain(&full), b'K');
    let (seen_live, bad_live) = numbers(&plain(&l.bytes), b'K');
    let (r50, rmax) = spread(&mut recover);
    say(&format!(
        "RESULT krilld_kill kills={kills} lines={total} replay_bytes={} live_bytes={} \
         replay_gaps={} replay_gap_bytes={} live_gaps={} live_reconnects={} \
         missing_replay={} missing_live={} bad_tag={}/{} sha_equal={} recover_p50_ms={r50:.1} recover_max_ms={rmax:.1} \
         kill_phase_s={kill_secs:.1} session_alive={} holder={holder}",
        full.len(),
        l.bytes.len(),
        gaps.len(),
        gaps.iter().map(|g| g.len).sum::<u64>(),
        l.gaps.len(),
        l.reconnects,
        missing(&seen_replay, total),
        missing(&seen_live, total),
        bad_replay,
        bad_live,
        sha(&full) == sha(&l.bytes),
        alive(pid),
    ));
    for g in &gaps {
        say(&format!("GAP replay start={} len={}", g.start.0, g.len));
    }
    assert!(alive(pid), "session died");
}

#[test]
#[ignore = "Windows 비밀번호 로그온 + release holder·krilld 필요(OWE-836)"]
fn holder_taskkill_keeps_orphan_serving() {
    let mut rig = Rig::new("holder");
    let (holder, krilld) = rig.launch();
    let mut c = rig.control();
    let (session, pid) = create(&mut c, &rig.dir);
    rig.pids.push(pid);
    let live = follow(&rig.paths, session);
    type_line(&rig.paths, session, &gen_command("H,0,50,10"));
    wait_live(&live, "generator start", |b| contains(b, b"H0000100-"));
    thread::sleep(Duration::from_millis(500));
    assert!(taskkill(holder, false), "taskkill holder");
    assert!(wait_dead(holder, WAIT), "holder still alive");
    let after = live.lock().unwrap().bytes.len();
    wait_live(&live, "orphan output", |b| b.len() > after + 16_384);
    assert!(
        alive(krilld) && alive(pid),
        "krilld or session died with holder"
    );
    let rejected = request(
        &mut c,
        &Request::Create(Create {
            account: None,
            program: shell(),
            cwd: Vec::new(),
            cols: 80,
            rows: 25,
        }),
    );
    // 앱식 재접속: Windows는 Adopt 전까지 새 holder를 띄우지 않고 고아 krilld에 그대로 붙는다(daemon::ready_handshake).
    let t_re = Instant::now();
    let (mut child, rx) = spawn("launch", &rig.dir, &[]);
    let reconnect = rx.recv_timeout(WAIT * 2).unwrap().replace(' ', ":");
    child.wait().unwrap();
    let reconnect_ms = ms(t_re.elapsed());
    let strays = rig.strays("");
    fs::write(rig.dir.join("stop"), b"").unwrap();
    wait_live(&live, "generator end", |b| {
        end_count(&plain(b), b'H').is_some()
    });
    // 입력도 고아 세션에 닿는다.
    type_line(
        &rig.paths,
        session,
        "echo ORPHAN_%COMPUTERNAME:~0,0%INPUT_OK",
    );
    wait_screen(&mut c, session, "orphan input", |s| {
        has_line(s, "ORPHAN_INPUT_OK")
    });
    thread::sleep(Duration::from_millis(300));
    let (full, gaps) = replay(&rig.paths, session);
    let live_bytes = live.lock().unwrap().bytes.clone();
    let total = end_count(&plain(&live_bytes), b'H').unwrap();
    let (seen, bad) = numbers(&plain(&full), b'H');
    say(&format!(
        "RESULT holder_kill lines={total} bytes={} gaps={} missing={} bad_tag={bad} sha_equal={} \
         create_after={rejected:?} reconnect={reconnect} reconnect_ms={reconnect_ms:.0} strays={strays:?} \
         krilld_alive={} session_alive={}",
        full.len(),
        gaps.len(),
        missing(&seen, total),
        sha(&full) == sha(&live_bytes[..full.len().min(live_bytes.len())]),
        alive(krilld),
        alive(pid),
    ));
    assert!(
        matches!(rejected, Reply::Failed(_)),
        "orphan create {rejected:?}"
    );
    assert!(
        gaps.is_empty() && missing(&seen, total) == 0,
        "orphan output lost"
    );
    drop(c);
    for &p in &strays {
        taskkill(p, true);
    }

    // 이어서 krilld도 죽인다(holder·krilld 동시 소실). 새 holder Adopt는 미구현이라 결과만 기록한다.
    assert!(taskkill(krilld, false));
    assert!(wait_dead(krilld, WAIT));
    let session_dead = wait_dead(pid, Duration::from_secs(10));
    let (_, _, fresh_launched) = rig.launch_full();
    let mut c = rig.control();
    let listed = request(&mut c, &Request::List);
    // 디스크 기록의 마지막 화면(읽기 전용 콜드 화면, MK-032).
    let cold = pane::load_cold(&rig.paths, session).map(|v| {
        let mut p: Pane<pane::Writer> = Pane::default();
        p.show_cold(session, v).unwrap();
        let rows: Vec<String> = texts(&p).into_iter().filter(|t| !t.is_empty()).collect();
        let tail = rows[rows.len().saturating_sub(3)..].join("|");
        (rows.iter().any(|t| t == "ORPHAN_INPUT_OK"), tail)
    });
    say(&format!(
        "RESULT holder_then_krilld_kill session_dead_10s={session_dead} fresh_launched={fresh_launched} \
         list={listed:?} cold_has_last_screen={cold:?}"
    ));
}

#[test]
#[ignore = "Windows 비밀번호 로그온 + release holder·krilld 필요(OWE-836)"]
fn big_output_and_alt_screen_hash() {
    let lines = env_num("KRILL_E2E_BIG_LINES", 20_000) as u64;
    let mut rig = Rig::new("big");
    rig.launch();
    let mut c = rig.control();
    let (session, pid) = create(&mut c, &rig.dir);
    rig.pids.push(pid);
    // 생성 줄: 검사 꼬리표 + SGR 색. cmd `type`이 VT 처리를 켠 채 쓴다.
    let mut big = Vec::new();
    for n in 1..=lines {
        big.extend_from_slice(
            format!(
                "{} \x1b[3{}mcolor\x1b[0m line\r\n",
                gen_line(b'G', n),
                n % 8
            )
            .as_bytes(),
        );
    }
    big.extend_from_slice(format!("GEND-{lines}\r\n").as_bytes());
    fs::write(rig.dir.join("big.txt"), &big).unwrap();
    let mut tui1 = b"\x1b[?1049h\x1b[2J".to_vec();
    for f in 0..50 {
        for r in 1..=20 {
            tui1.extend_from_slice(format!("\x1b[{r};1HX{f:03}R{r:02}\x1b[K").as_bytes());
        }
    }
    tui1.extend_from_slice(b"\x1b[22;1HTUI-FINAL\x1b[K\x1b[23;1H");
    fs::write(rig.dir.join("tui1.txt"), &tui1).unwrap();
    fs::write(rig.dir.join("tui2.txt"), b"\x1b[?1049lTUI-END\r\n").unwrap();

    let live = follow(&rig.paths, session);
    let t0 = Instant::now();
    type_line(&rig.paths, session, "type big.txt");
    let end = format!("GEND-{lines}");
    wait_live(&live, "big output", |b| contains(&plain(b), end.as_bytes()));
    let secs = t0.elapsed().as_secs_f64();
    wait_screen(&mut c, session, "prompt after big", |s| {
        s.lines().any(|l| l.trim_end().ends_with('>'))
    });
    type_line(&rig.paths, session, "type tui1.txt");
    wait_screen(&mut c, session, "tui frame", |s| {
        has_line(s, "TUI-FINAL") && has_line(s, "X049R20")
    });
    let alt = capture(&mut c, session);
    type_line(&rig.paths, session, "type tui2.txt");
    wait_screen(&mut c, session, "primary screen", |s| {
        has_line(s, "TUI-END")
    });
    let primary = capture(&mut c, session);
    thread::sleep(Duration::from_millis(500));
    let (full, gaps) = replay(&rig.paths, session);
    let l = live.lock().unwrap();
    let (seen, bad) = numbers(&plain(&full), b'G');
    let lost = missing(&seen, lines);
    let alt_left = !primary.contains("TUI-FINAL") && primary.contains("type tui1.txt");
    say(&format!(
        "RESULT big_tui source_bytes={} lines={lines} pty_bytes={} live_bytes={} sha_replay={} sha_live={} \
         gaps={} live_gaps={} missing_lines={lost} bad_tag={bad} type_secs={secs:.1} alt_drawn={} alt_restored={alt_left}",
        big.len(),
        full.len(),
        l.bytes.len(),
        sha(&full),
        sha(&l.bytes),
        gaps.len(),
        l.gaps.len(),
        !alt.contains("type tui1.txt"),
    ));
    if lost > 0 {
        let first: Vec<u64> = (1..=lines).filter(|n| !seen.contains(n)).take(20).collect();
        say(&format!("MISSING first={first:?}"));
    }
    assert!(
        gaps.is_empty() && l.gaps.is_empty(),
        "gaps {gaps:?} {:?}",
        l.gaps
    );
    assert_eq!(sha(&full), sha(&l.bytes), "live and replay differ");
    assert_eq!(lost, 0, "lines lost");
    assert!(alt_left, "alt screen not restored: {primary}");
}
