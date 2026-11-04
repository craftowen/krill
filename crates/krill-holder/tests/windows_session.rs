//! Windows holder 세션 수명 실기 검사: 실제 `krill-holder.exe`와 번들 `ConPTY`로 Ctrl+C·셸 종료·종료 단계를 본다.
//!
//! holder는 출시 경로처럼 [`krill_holder::spawn_detached`](`DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`)로 띄운다.
//! 이 테스트 바이너리가 띄우는 쪽(`role_launcher`)과 가짜 krilld(`role_krilld`, holder가 다시 실행한다)를 겸한다.
//! 가짜 krilld는 진짜처럼 세션 핸들 8개를 모두 쥔 채 출력 파이프를 읽고, 결과를 `MKT_DIR/report.txt`로 넘긴다.
//! 번들 파일이 필요해 `#[ignore]`다:
//! `KRILL_TEST_CONPTY_DIR=<conpty.dll, x64\OpenConsole.exe 폴더> cargo nextest run -p krill-holder --test windows_session --run-ignored all`
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{Read as _, Write as _};
use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use krill_holder::link::{Link, Pty};
use krill_proto::SessionId;
use krill_proto::holder::{FromHolder, ToHolder, WinSize};

const HOLDER: &str = env!("CARGO_BIN_EXE_krill-holder");
const CMD: &str = r"C:\Windows\System32\cmd.exe";
const PWSH: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
const WAIT: Duration = Duration::from_secs(30);

fn put(p: &Path, s: &str) {
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, s).unwrap();
    fs::rename(tmp, p).unwrap();
}

fn node() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("node.exe"))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("node.exe가 PATH에 있어야 한다"))
}

/// OSC(창 제목 등)를 뺀 출력. 제목에 실린 명령줄이 표식으로 잘못 잡히지 않게 한다.
fn visible(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let (mut out, mut rest) = (String::new(), s.as_ref());
    while let Some((before, osc)) = rest.split_once("\x1b]") {
        out.push_str(before);
        rest = osc
            .split_once('\x07')
            .or_else(|| osc.split_once("\x1b\\"))
            .map_or("", |(_, tail)| tail);
    }
    out + rest
}

fn alive(pid: u32) -> bool {
    let o = Command::new("tasklist.exe")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\""))
}

fn taskkill(pid: u32, tree: bool) {
    let mut c = Command::new("taskkill.exe");
    c.arg("/F");
    if tree {
        c.arg("/T");
    }
    c.args(["/PID", &pid.to_string()]).output().ok();
}

fn std_handles_null() -> bool {
    [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ]
    .iter()
    .all(|h| h.is_null())
}

fn millis(from: Instant, to: Instant) -> u128 {
    to.saturating_duration_since(from).as_millis()
}

// ---------- 역할: 가짜 krilld ----------

struct Session {
    pid: u32,
    pty: Pty,
    out: Arc<Mutex<Vec<u8>>>,
    eof: Arc<Mutex<Option<Instant>>>,
}

impl Session {
    fn text(&self) -> String {
        visible(&self.out.lock().unwrap())
    }

    fn wait_text(&self, text: &str, within: Duration) -> bool {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if self.text().contains(text) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// `text`가 `quiet` 동안 더 나오지 않으면 참(최대 `within`).
    fn settles(&self, text: &str, quiet: Duration, within: Duration) -> bool {
        let end = Instant::now() + within;
        let (mut seen, mut since) = (self.text().matches(text).count(), Instant::now());
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
            let now = self.text().matches(text).count();
            if now != seen {
                (seen, since) = (now, Instant::now());
            } else if since.elapsed() >= quiet {
                return true;
            }
        }
        false
    }

    fn wait_eof(&self, within: Duration) -> bool {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if self.eof.lock().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn type_in(&self, text: &str) {
        fs::File::from(self.pty.input.try_clone().unwrap())
            .write_all(text.as_bytes())
            .unwrap();
    }
}

struct Krilld {
    link: Link,
    rx: mpsc::Receiver<(FromHolder, Vec<OwnedHandle>)>,
    req: u32,
    exited: HashMap<u64, (i32, Instant)>,
    dir: PathBuf,
    report: String,
}

impl Krilld {
    fn note(&mut self, line: &str) {
        self.report.push_str(line);
        self.report.push('\n');
        fs::write(self.dir.join("report.txt"), &self.report).unwrap();
    }

    /// `Exited`는 따로 적고, 그 밖의 알림은 돌려준다.
    fn pump(&mut self, within: Duration) -> Option<(FromHolder, Vec<OwnedHandle>)> {
        let (m, fds) = self.rx.recv_timeout(within).ok()?;
        if let FromHolder::Exited { id, status } = m {
            self.exited.insert(id.0, (status, Instant::now()));
            return None;
        }
        Some((m, fds))
    }

    fn ask(&mut self, m: impl FnOnce(u32) -> ToHolder) -> (FromHolder, Vec<OwnedHandle>) {
        self.req += 1;
        let req = self.req;
        self.link.send(&m(req).encode().unwrap(), &[]).unwrap();
        let end = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < end, "holder did not answer req {req}");
            if let Some((m, fds)) = self.pump(Duration::from_millis(100))
                && matches!(m, FromHolder::Done { req: r } | FromHolder::Failed { req: r, .. } | FromHolder::Spawned { req: r, .. } if r == req)
            {
                return (m, fds);
            }
        }
    }

    fn wait_exit(&mut self, id: u64, within: Duration) -> Option<(i32, Instant)> {
        let end = Instant::now() + within;
        while !self.exited.contains_key(&id) && Instant::now() < end {
            self.pump(Duration::from_millis(20));
        }
        self.exited.get(&id).copied()
    }

    fn spawn(&mut self, id: u64, argv: &[&str]) -> Session {
        let env = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .filter(|(k, _)| !k.starts_with("MKT_") && !k.starts_with("NEXTEST"))
            .map(|(k, v)| (k.into_bytes(), v.into_bytes()))
            .collect();
        let cwd = self.dir.to_str().unwrap().as_bytes().to_vec();
        let (m, fds) = self.ask(|req| ToHolder::Spawn {
            req,
            id: SessionId(id),
            argv: argv.iter().map(|s| s.as_bytes().to_vec()).collect(),
            env,
            cwd,
            size: WinSize {
                cols: 100,
                rows: 30,
            },
        });
        let FromHolder::Spawned { pid, .. } = m else {
            panic!("spawn {argv:?}: {m:?}")
        };
        let pty = Pty::from_handles(fds).unwrap();
        let (out, eof) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(None)));
        let mut pipe = fs::File::from(pty.output.try_clone().unwrap());
        let (o, e) = (Arc::clone(&out), Arc::clone(&eof));
        std::thread::spawn(move || {
            let mut buf = [0; 8192];
            while let Ok(n @ 1..) = pipe.read(&mut buf) {
                o.lock().unwrap().extend_from_slice(&buf[..n]);
            }
            *e.lock().unwrap() = Some(Instant::now());
        });
        Session { pid, pty, out, eof }
    }

    fn signal(&mut self, id: u64, signal: Option<i32>) -> FromHolder {
        let id = SessionId(id);
        self.ask(|req| match signal {
            Some(signal) => ToHolder::Kill { req, id, signal },
            None => ToHolder::Terminate { req, id },
        })
        .0
    }

    fn release(&mut self, id: u64) {
        let r = self
            .ask(|req| ToHolder::Release {
                req,
                id: SessionId(id),
            })
            .0;
        assert!(matches!(r, FromHolder::Done { .. }), "release {id}: {r:?}");
    }

    /// 시나리오 뒤 정리: 남았으면 끝내고 놓는다.
    fn finish(&mut self, id: u64) {
        if !self.exited.contains_key(&id) {
            self.signal(id, Some(9));
            self.wait_exit(id, WAIT);
        }
        self.release(id);
    }
}

/// 출시 경로로 뜬 holder 아래 셸에서 `ping -t`가 Ctrl+C(`\x03`)로 멈추는지.
fn ctrl_c(k: &mut Krilld) {
    let shells: [(u64, &[&str], &str, &str); 2] = [
        (10, &[CMD], ">", "echo ctrlc-%OS%\r"),
        (11, &[PWSH, "-NoProfile"], "PS ", "echo \"ctrlc-$env:OS\"\r"),
    ];
    for (id, argv, prompt, echo) in shells {
        let s = k.spawn(id, argv);
        let ready = s.wait_text(prompt, WAIT);
        s.type_in("ping -t 127.0.0.1\r");
        let pinging = s.wait_text("TTL=", WAIT);
        s.type_in("\x03");
        // ping -t는 1초마다 응답을 찍는다. 2초 동안 더 없으면 멈춘 것이다.
        let stopped = s.settles("TTL=", Duration::from_secs(2), Duration::from_secs(10));
        s.type_in(echo);
        let prompt = s.wait_text("ctrlc-Windows_NT", Duration::from_secs(10));
        k.note(&format!(
            "ctrl_c {id} ready={ready} pinging={pinging} stopped={stopped} prompt={prompt}"
        ));
        if !(stopped && prompt) {
            let text: Vec<char> = s.text().chars().collect();
            let tail: String = text[text.len().saturating_sub(600)..].iter().collect();
            k.note(&format!("ctrl_c {id} tail={tail:?}"));
        }
        k.finish(id);
    }
}

/// 셸이 GUI 앱과 새 콘솔 앱을 띄우고 끝나면 세션도 끝나고, 띄운 앱은 남는다(Windows Terminal과 같다).
fn leader_exit(k: &mut Krilld) {
    let pids = k.dir.join("left.pid");
    let script = format!(
        "$a = Start-Process notepad.exe -PassThru; $b = Start-Process ping.exe -ArgumentList '-n','300','127.0.0.1' -WindowStyle Hidden -PassThru; \"$($a.Id) $($b.Id)\" | Out-File -Encoding ascii '{}'; exit 3",
        pids.display()
    );
    let s = k.spawn(
        20,
        &[PWSH, "-NoProfile", "-NonInteractive", "-Command", &script],
    );
    let exit = k.wait_exit(20, Duration::from_secs(20));
    let left: Vec<u32> = fs::read_to_string(&pids)
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect();
    let leader = alive(s.pid);
    k.finish(20);
    std::thread::sleep(Duration::from_millis(500));
    let survived: Vec<bool> = left.iter().map(|p| alive(*p)).collect();
    k.note(&format!(
        "leader_exit status={:?} leader_alive={leader} left={} survived={survived:?}",
        exit.map(|e| e.0),
        left.len()
    ));
    for p in left {
        taskkill(p, false);
    }
}

/// `Terminate`는 콘솔을 먼저 끊어(hangup) 셸이 스스로 끝날 기회를 주고, 남은 것은 5초 뒤 Job째 끝낸다.
fn terminate(k: &mut Krilld) {
    let node = node();
    let node = node.to_str().unwrap();
    // 셸: 콘솔이 끊기면 스스로 0으로 끝난다(Job 강제 종료면 1).
    let shell = k.spawn(30, &[CMD]);
    shell.wait_text(">", WAIT);
    let sent = Instant::now();
    k.signal(30, None);
    let exit = k.wait_exit(30, Duration::from_secs(10));
    k.note(&format!(
        "terminate shell status={:?} ms={:?}",
        exit.map(|e| e.0),
        exit.map(|e| millis(sent, e.1))
    ));
    k.finish(30);
    // node: 콘솔 입력이 끊기면 정리 표시를 남기고 5로 끝난다.
    let bye = k.dir.join("bye");
    let js = "const f=process.argv[1],bye=()=>{require('fs').writeFileSync(f,'bye');process.exit(5)};process.stdin.on('end',bye);process.stdin.on('error',bye);process.stdin.resume();console.log('tidy-ready');setInterval(()=>{},1000)";
    let tidy = k.spawn(31, &[node, "-e", js, bye.to_str().unwrap()]);
    tidy.wait_text("tidy-ready", WAIT);
    let sent = Instant::now();
    k.signal(31, None);
    let exit = k.wait_exit(31, Duration::from_secs(10));
    k.note(&format!(
        "terminate tidy status={:?} ms={:?} bye={}",
        exit.map(|e| e.0),
        exit.map(|e| millis(sent, e.1)),
        bye.exists()
    ));
    k.finish(31);
    // 콘솔을 보지 않는 프로세스는 deadline(5초)에 Job째 끝난다. Kill 9는 바로 끝낸다.
    for (id, signal) in [(32, None), (33, Some(9))] {
        let deaf = k.spawn(
            id,
            &[
                node,
                "-e",
                "console.log('deaf-ready');setInterval(()=>{},1000)",
            ],
        );
        deaf.wait_text("deaf-ready", WAIT);
        let sent = Instant::now();
        k.signal(id, signal);
        let exit = k.wait_exit(id, Duration::from_secs(12));
        k.note(&format!(
            "terminate deaf {signal:?} status={:?} ms={:?} alive={}",
            exit.map(|e| e.0),
            exit.map(|e| millis(sent, e.1)),
            alive(deaf.pid)
        ));
        k.finish(id);
    }
}

/// 짧게 끝나는 세션의 마지막 출력이 남는지(A9 재현 시도).
fn short(k: &mut Krilld) {
    let node = node();
    let node = node.to_str().unwrap();
    let mut lost = Vec::new();
    for i in 0..30 {
        let marker = format!("k{i}mark");
        let s = k.spawn(
            100 + i,
            &[node, "-e", &format!("process.stdout.write('{marker}\\n')")],
        );
        k.wait_exit(100 + i, WAIT);
        s.wait_eof(Duration::from_secs(5));
        if !s.text().contains(&marker) {
            lost.push(marker);
        }
        k.finish(100 + i);
    }
    for i in 0..5 {
        let js = "for(let n=1;n<=2000;n++)console.log('line'+n+'-end')";
        let s = k.spawn(200 + i, &[node, "-e", js]);
        k.wait_exit(200 + i, WAIT);
        s.wait_eof(Duration::from_secs(5));
        if !s.text().contains("line2000-end") {
            lost.push(format!("burst{i}"));
        }
        k.finish(200 + i);
    }
    k.note(&format!("short lost={lost:?}"));
}

#[test]
fn role_krilld() {
    let Some(dir) = std::env::var_os("MKT_DIR").map(PathBuf::from) else {
        return;
    };
    let link = Link::inherited().unwrap();
    let mut reader = link.try_clone().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(Some(p)) = reader.recv() {
            let m = FromHolder::decode(p.frame()).unwrap();
            if tx.send((m, p.fds)).is_err() {
                break;
            }
        }
    });
    while rx.recv().unwrap().0 != FromHolder::Ready {}
    if !dir.join("started").exists() {
        put(&dir.join("started"), "");
        let mut k = Krilld {
            link,
            rx,
            req: 0,
            exited: HashMap::new(),
            dir: dir.clone(),
            report: String::new(),
        };
        k.note(&format!("krilld std_null={}", std_handles_null()));
        match std::env::var("MKT_SCENARIO").unwrap().as_str() {
            "ctrl_c" => ctrl_c(&mut k),
            "leader_exit" => leader_exit(&mut k),
            "terminate" => terminate(&mut k),
            "short" => short(&mut k),
            other => panic!("unknown scenario {other}"),
        }
        put(&dir.join("done"), "");
    }
    // holder가 재기동한 krilld도 여기서 머문다. 정리는 오케스트레이터가 holder 트리째 한다.
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// 출시 경로(`--bootstrap`)와 같은 [`krill_holder::spawn_detached`]로 holder를 띄운다(Job 탈출만 뺀다).
#[test]
fn role_launcher() {
    let Some(dir) = std::env::var_os("MKT_DIR").map(PathBuf::from) else {
        return;
    };
    let me = OsString::from(std::env::current_exe().unwrap());
    let args = |name: &str| {
        [
            me.clone(),
            "--exact".into(),
            name.into(),
            "--nocapture".into(),
        ]
    };
    let holder = dir.join(r"install\krill-holder.exe");
    let holder =
        krill_holder::spawn_detached(holder.as_os_str(), &args("role_krilld"), false).unwrap();
    put(&dir.join("holder.pid"), &holder.pid.to_string());
    // 같은 생성 플래그로 뜬 자식의 표준 핸들(A10).
    krill_holder::spawn_detached(&me, &args("role_probe")[1..], false).unwrap();
}

#[test]
fn role_probe() {
    if let Some(dir) = std::env::var_os("MKT_DIR").map(PathBuf::from) {
        put(
            &dir.join("probe.txt"),
            &format!("detached std_null={}", std_handles_null()),
        );
    }
}

// ---------- 오케스트레이터 ----------

struct Rig {
    dir: PathBuf,
    holder: u32,
}

impl Drop for Rig {
    fn drop(&mut self) {
        taskkill(self.holder, true);
        std::thread::sleep(Duration::from_millis(500));
        fs::remove_dir_all(&self.dir).ok();
    }
}

/// 시나리오 하나를 돌리고 `report.txt`와 `probe.txt`를 이어 돌려준다.
fn run(scenario: &str) -> String {
    let bundle = PathBuf::from(
        std::env::var_os("KRILL_TEST_CONPTY_DIR").unwrap_or_else(|| {
            panic!("set KRILL_TEST_CONPTY_DIR (conpty.dll, x64\\OpenConsole.exe)")
        }),
    );
    let dir = std::env::temp_dir().join(format!("krill-holder-{scenario}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let install = dir.join("install");
    fs::create_dir_all(install.join("x64")).unwrap();
    fs::create_dir_all(dir.join("state")).unwrap();
    fs::copy(HOLDER, install.join("krill-holder.exe")).unwrap();
    for file in ["conpty.dll", r"x64\OpenConsole.exe"] {
        fs::copy(bundle.join(file), install.join(file)).unwrap();
    }
    let launched = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "role_launcher", "--nocapture"])
        .env("MKT_DIR", &dir)
        .env("MKT_SCENARIO", scenario)
        .env("KRILL_STATE_DIR", dir.join("state"))
        .env("LOCALAPPDATA", dir.join("state"))
        .status()
        .unwrap();
    assert!(launched.success());
    let holder = fs::read_to_string(dir.join("holder.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let rig = Rig { dir, holder };
    let end = Instant::now() + Duration::from_secs(180);
    while !rig.dir.join("done").exists() || !rig.dir.join("probe.txt").exists() {
        let report = fs::read_to_string(rig.dir.join("report.txt")).unwrap_or_default();
        assert!(Instant::now() < end, "timeout:\n{report}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let report = fs::read_to_string(rig.dir.join("report.txt")).unwrap()
        + &fs::read_to_string(rig.dir.join("probe.txt")).unwrap();
    // A10: holder·krilld 생성 경로의 자식은 표준 핸들이 없어(NULL) 다른 핸들 값과 겹칠 수 없다.
    assert!(report.contains("krilld std_null=true"), "{report}");
    assert!(report.contains("detached std_null=true"), "{report}");
    report
}

#[test]
#[ignore = "requires packaged conpty.dll and x64/OpenConsole.exe; set KRILL_TEST_CONPTY_DIR"]
fn ctrl_c_stops_a_console_command_in_cmd_and_powershell() {
    let report = run("ctrl_c");
    for id in [10, 11] {
        assert!(
            report.contains(&format!(
                "ctrl_c {id} ready=true pinging=true stopped=true prompt=true"
            )),
            "{report}"
        );
    }
}

#[test]
#[ignore = "requires packaged conpty.dll and x64/OpenConsole.exe; set KRILL_TEST_CONPTY_DIR"]
fn shell_exit_ends_session_and_leaves_launched_apps_running() {
    let report = run("leader_exit");
    assert!(
        report
            .contains("leader_exit status=Some(3) leader_alive=false left=2 survived=[true, true]"),
        "{report}"
    );
}

#[test]
#[ignore = "requires packaged conpty.dll and x64/OpenConsole.exe; set KRILL_TEST_CONPTY_DIR"]
fn terminate_hangs_up_first_and_kills_the_rest_at_the_deadline() {
    let report = run("terminate");
    let field = |line: &str, key: &str| -> Option<u128> {
        let rest = report.lines().find(|l| l.starts_with(line))?;
        let v = rest.split(&format!("{key}=Some(")).nth(1)?;
        v.split(')').next()?.parse().ok()
    };
    // 셸은 콘솔이 끊기자 스스로(코드 0) 끝난다. 강제 종료였다면 1이다.
    assert_eq!(field("terminate shell", "status"), Some(0), "{report}");
    assert!(
        field("terminate shell", "ms").is_some_and(|ms| ms < 3_000),
        "{report}"
    );
    assert!(report.contains("bye=true"), "{report}");
    assert_eq!(field("terminate tidy", "status"), Some(5), "{report}");
    // 콘솔을 보지 않으면 deadline에, Kill 9는 바로.
    let deaf = field("terminate deaf None", "ms").unwrap_or_default();
    assert!((4_500..9_000).contains(&deaf), "{report}");
    assert!(
        field("terminate deaf Some(9)", "ms").is_some_and(|ms| ms < 1_500),
        "{report}"
    );
    assert!(!report.contains("alive=true"), "{report}");
}

#[test]
#[ignore = "requires packaged conpty.dll and x64/OpenConsole.exe; set KRILL_TEST_CONPTY_DIR"]
fn short_session_keeps_its_last_output() {
    let report = run("short");
    assert!(report.contains("short lost=[]"), "{report}");
}
