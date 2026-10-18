//! MK-039: 실제 `krill-holder`·`krilld`·`krill hook`(`krill` 옆)으로 에이전트 상태 저장소 전체.
//!
//! 세션 안 셸이 `krill hook claude`를 부른다. krilld를 SIGSTOP으로 멈춘 채 부르면 `krill hook`은 ACK를 못 받아 spool에
//! 쓴다. 그 krilld를 SIGKILL하면 holder가 새 krilld를 띄우고, 새 krilld는 기동하면서 spool을 수거한다. 이렇게
//! SessionStart·Stop을 재기동 중에 보내도 잃지 않는지, 소켓과 spool로 두 번 온 이벤트를 한 번만 반영하는지, 앱이
//! 다시 붙었을 때 1초 안에 같은 상태를 받는지 본다.
//!
//! 재현: `cargo nextest run -p krill --test status --no-capture`(측정값은 `mk039 reconnect:` 줄).

#![cfg_attr(
    unix,
    expect(
        clippy::unwrap_used,
        reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
    )
)]
#![cfg(unix)]

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::control::{
    AgentState, AgentStatus, Create, Program, Reply, Request, SessionStatus, Source,
};
use krill_proto::daemon::{client_hello, handshake};
use krill_proto::hook::SPOOL_FILE;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;

const MK: &str = env!("CARGO_BIN_EXE_krill");
const WAIT: Duration = Duration::from_secs(10);
/// 앱 재시작 뒤 같은 상태를 받기까지의 상한(완료 조건).
const SAME_STATE_MAX: Duration = Duration::from_secs(1);

fn signal(sig: &str, pid: u32) {
    Command::new("/bin/kill")
        .args([sig, &pid.to_string()])
        .status()
        .unwrap();
}

/// 테스트가 실패해도 holder·krilld를 남기지 않는다.
struct Daemon {
    holder: Child,
    krilld: u32,
    dir: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.holder.kill().ok();
        self.holder.wait().ok();
        signal("-9", self.krilld);
        fs::remove_dir_all(&self.dir).ok();
    }
}

/// `not`이 아닌 krilld에 붙는다.
fn connect(sock: &Path, not: u64) -> (UnixStream, u64) {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(c) = handshake(sock, &client_hello(ClientRole::App, "mk039"), WAIT)
            && c.server.instance_id != not
        {
            return (c.stream, c.server.instance_id);
        }
        assert!(Instant::now() < deadline, "krilld did not come up");
        thread::sleep(Duration::from_millis(20));
    }
}

fn request(c: &mut UnixStream, r: &Request) -> Reply {
    c.write_all(&r.encode().unwrap()).unwrap();
    let (k, b) = ipc::read_frame(c).unwrap().unwrap();
    Reply::decode(k, &b).unwrap()
}

#[expect(clippy::panic, reason = "통합 테스트 헬퍼의 응답 타입 검사")]
fn status(c: &mut UnixStream, session: SessionId) -> AgentStatus {
    let r = request(c, &Request::States);
    let Reply::States(v) = r else {
        panic!("states: {r:?}")
    };
    v.into_iter()
        .find(|s: &SessionStatus| s.session == session)
        .unwrap()
        .status
}

fn wait_file(p: &Path) {
    let deadline = Instant::now() + WAIT;
    while !p.exists() {
        assert!(Instant::now() < deadline, "timed out: {}", p.display());
        thread::sleep(Duration::from_millis(20));
    }
}

/// 세션 셸이 `go<n>`을 보면 hook `n`을 부르고 끝나면 `sent<n>`을 만든다.
fn fire(dir: &Path, n: u32) {
    fs::write(dir.join(format!("go{n}")), b"").unwrap();
    wait_file(&dir.join(format!("sent{n}")));
}

fn spool_len(dir: &Path) -> u64 {
    fs::metadata(dir.join(SPOOL_FILE)).map_or(0, |m| m.len())
}

/// holder가 첫 크래시로 보고 바로 다시 띄우도록(1초 넘게 산 krilld) 기다렸다가 멈춘다.
fn stop(d: &Daemon) {
    thread::sleep(Duration::from_millis(1_100));
    signal("-STOP", d.krilld);
}

fn assert_hook(s: &AgentStatus, state: AgentState) {
    assert_agent(s, state, "claude");
}

fn assert_agent(s: &AgentStatus, state: AgentState, agent: &str) {
    assert_eq!((s.state, s.source), (state, Some(Source::Hook)), "{s:?}");
    assert_eq!(s.agent, agent);
}

/// `krill-holder`(krilld 자식)를 `dir`의 상태 디렉터리로 띄우고 붙는다.
fn start(name: &str) -> (Daemon, UnixStream, u64) {
    let bin = Path::new(MK).parent().unwrap();
    for b in ["krill-holder", "krilld"] {
        assert!(
            bin.join(b).is_file(),
            "{b} is not built next to krill (run the whole workspace: cargo nextest run --workspace)"
        );
    }
    let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    // krill hook이 찾는 기본 위치(`<KRILL_STATE_DIR>/run/krilld.sock`).
    let sock = dir.join("run/krilld.sock");
    let holder = Command::new(bin.join("krill-holder"))
        .arg(bin.join("krilld"))
        .arg(&sock)
        .env("KRILL_STATE_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (c, first) = connect(&sock, 0);
    let d = Daemon {
        holder,
        krilld: u32::try_from(first).unwrap(),
        dir,
    };
    (d, c, first)
}

#[expect(clippy::panic, reason = "통합 테스트 헬퍼의 응답 타입 검사")]
fn create(c: &mut UnixStream, script: String) -> SessionId {
    let create = Request::Create(Create {
        account: None,
        program: Program::Custom {
            program: b"/bin/sh".to_vec(),
            args: vec![b"-c".to_vec(), script.into_bytes()],
        },
        cwd: Vec::new(),
        cols: 80,
        rows: 24,
    });
    let Reply::Created { session, .. } = request(c, &create) else {
        panic!("create failed");
    };
    session
}

fn drain_spool(dir: &Path) {
    let deadline = Instant::now() + WAIT;
    while spool_len(dir) > 0 {
        assert!(Instant::now() < deadline, "spool was not drained");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn hook_states_survive_krilld_restart() {
    let (mut d, mut c, first) = start("mk039");
    let dir = d.dir.clone();
    let sock = dir.join("run/krilld.sock");

    for (n, payload) in [
        (
            1,
            r#"{"hook_event_name":"SessionStart","source":"startup"}"#,
        ),
        (2, r#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#),
        (3, r#"{"hook_event_name":"Stop"}"#),
        (4, r#"{"hook_event_name":"PermissionRequest"}"#),
        (5, r#"{"hook_event_name":"Stop"}"#),
    ] {
        fs::write(dir.join(format!("in{n}")), payload).unwrap();
    }

    // 세션 환경의 KRILL_STATE_DIR(OWE-503)와 같은 값이지만 MK-039 경로를 그대로 둔다.
    // OWE-533: 파이프 왼쪽 프로세스 스케줄 지연으로 STDIN_WAIT(10ms)를 넘기지 않게 미리 써 둔 파일을 표준 입력으로 연다.
    let script = format!(
        r#"D='{dir}'; MK='{MK}'
fire() {{ while [ ! -f "$D/go$1" ]; do sleep 0.02; done; KRILL_STATE_DIR="$D" "$MK" hook claude <"$D/in$1" >/dev/null; touch "$D/sent$1"; }}
fire 1
fire 2
fire 3
fire 4
fire 5
exec sleep 600"#,
        dir = dir.display()
    );
    let session = create(&mut c, script);
    assert_eq!(status(&mut c, session), AgentStatus::UNKNOWN);

    // 1. krilld가 멈춘 동안 SessionStart → spool → 새 krilld가 기동하면서 수거(첫 질의에 이미 반영).
    stop(&d);
    fire(&dir, 1);
    assert!(spool_len(&dir) > 0, "SessionStart was not spooled");
    signal("-9", d.krilld);
    let (mut c, id) = connect(&sock, first);
    d.krilld = u32::try_from(id).unwrap();
    assert_hook(&status(&mut c, session), AgentState::Done);
    assert_eq!(spool_len(&dir), 0);

    // 2. 살아 있는 krilld: 소켓으로 받고 ACK(spool 없음). ACK 전에 반영하므로 hook이 끝나면 이미 보인다.
    fire(&dir, 2);
    assert_eq!(spool_len(&dir), 0, "delivered over the socket");
    let working = status(&mut c, session);
    assert_hook(&working, AgentState::Working);

    // 3. 다시 멈춘 동안 Stop. 수거 뒤 같은 프레임을 spool에 한 번 더 넣어(소켓·spool 양쪽 도착) 중복 제거를 본다.
    stop(&d);
    fire(&dir, 3);
    let spooled = fs::read(dir.join(SPOOL_FILE)).unwrap();
    assert!(!spooled.is_empty(), "Stop was not spooled");
    signal("-9", d.krilld);
    let (mut c, id) = connect(&sock, id);
    d.krilld = u32::try_from(id).unwrap();
    let done = status(&mut c, session);
    assert_hook(&done, AgentState::Done);
    assert!(done.since_unix_ns > working.since_unix_ns);
    fs::OpenOptions::new()
        .append(true)
        .open(dir.join(SPOOL_FILE))
        .unwrap()
        .write_all(&spooled)
        .unwrap();
    drain_spool(&dir);
    assert_eq!(
        status(&mut c, session),
        done,
        "duplicate Stop changed the state"
    );

    // 4. OWE-533: 살아 있는 krilld가 일시 과부하(SIGSTOP)로 소켓 예산(30ms) 안에 응답하지 못해 spool로 가더라도,
    // 재기동 없이 깨어난 뒤(SIGCONT) SPOOL_POLL 수거로 이벤트가 유실 없이 순서대로 반영된다.
    signal("-STOP", d.krilld);
    fire(&dir, 4);
    assert!(spool_len(&dir) > 0, "PermissionRequest was not spooled");
    signal("-CONT", d.krilld);
    drain_spool(&dir);
    let waiting = status(&mut c, session);
    assert_hook(&waiting, AgentState::Waiting);
    assert!(waiting.since_unix_ns > done.since_unix_ns);

    signal("-STOP", d.krilld);
    fire(&dir, 5);
    assert!(spool_len(&dir) > 0, "Stop was not spooled");
    signal("-CONT", d.krilld);
    drain_spool(&dir);
    let done = status(&mut c, session);
    assert_hook(&done, AgentState::Done);
    assert!(done.since_unix_ns > waiting.since_unix_ns);

    // 5. 앱 재시작: 연결을 끊고 새로 붙어 같은 상태를 받기까지.
    drop(c);
    let t0 = Instant::now();
    let (mut c, live_id) = connect(&sock, 0);
    assert_eq!(live_id, id, "krilld restarted unexpectedly");
    let again = status(&mut c, session);
    let took = t0.elapsed();
    println!(
        "mk039 reconnect: same state after {:.2} ms",
        took.as_secs_f64() * 1_000.0
    );
    assert_eq!(again, done);
    assert!(took < SAME_STATE_MAX, "{took:?}");

    drop(d);
}

/// OWE-503: 설치기가 쓰는 전역 hook 명령(`MK_PANE_ID` 가드)을 세션 안에서 그대로 부른다. krilld가 넣은 pane 환경만으로
/// 가드가 열리고 `krill hook`이 상태 디렉터리를 찾아 Cursor 네이티브 이벤트가 pane 상태를 바꾼다.
#[test]
fn global_hook_guard_drives_pane_state() {
    use krill_proto::agent::hooks::{self, Agent};

    let (d, mut c, _) = start("mk503");
    let dir = d.dir.clone();
    let agent = Agent::Cursor;
    let guard = hooks::command(Path::new(MK), agent).unwrap();
    fs::write(dir.join(format!("{}.sh", agent.id())), guard).unwrap();
    let steps = [
        (
            "cursor",
            r#"{"hook_event_name":"beforeSubmitPrompt"}"#,
            AgentState::Working,
        ),
        ("cursor", r#"{"hook_event_name":"stop"}"#, AgentState::Done),
        (
            "cursor",
            r#"{"hook_event_name":"sessionEnd"}"#,
            AgentState::Exited,
        ),
    ];
    let mut script = format!(
        r#"D='{}'
printf '%s' "$MK_PANE_ID" >"$D/pane"
fire() {{ while [ ! -f "$D/go$1" ]; do sleep 0.02; done; /bin/sh "$D/$2.sh" <"$D/in$1" >/dev/null; touch "$D/sent$1"; }}
"#,
        dir.display()
    );
    for (n, (agent, payload, _)) in steps.iter().enumerate() {
        fs::write(dir.join(format!("in{n}")), payload).unwrap();
        writeln!(script, "fire {n} {agent}").unwrap();
    }
    script.push_str("exec sleep 600");
    let session = create(&mut c, script);
    wait_file(&dir.join("pane"));
    assert_eq!(
        fs::read_to_string(dir.join("pane")).unwrap(),
        session.0.to_string()
    );
    for (n, (agent, _, state)) in steps.iter().enumerate() {
        fire(&dir, u32::try_from(n).unwrap());
        assert_agent(&status(&mut c, session), *state, agent);
    }
    assert_eq!(spool_len(&dir), 0, "delivered over the socket");
    drop(d);
}
