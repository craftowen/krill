//! MK-071: 실제 `krill-holder` + `krilld`에서 Resource Manager 조회값이 `footprint(1)` ±10% 안이고, 데몬 재시작 요청 뒤
//! 새 krilld가 같은 세션(같은 id·루트 pid)을 이어 받는다.
//!
//! `krill-holder`는 `krilld` 옆(같은 target 디렉터리)에 있어야 한다: `cargo nextest run --workspace`.

#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use krill_proto::control::{Create, Info, Program, Reply, Request};
use krill_proto::daemon::client_hello;
use krill_proto::ipc::{self, ClientRole, ServerHello};
use krill_proto::resources::{Action, Resources};
use krill_proto::screen::Msg;
use rustix::process::{Pid, Signal, kill_process, test_kill_process};

const KRILLD: &str = env!("CARGO_BIN_EXE_krilld");
const WAIT: Duration = Duration::from_secs(10);

fn pid(p: u32) -> Pid {
    Pid::from_raw(i32::try_from(p).unwrap()).unwrap()
}

struct Daemon {
    holder: Child,
    krilld: Vec<u32>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.holder.kill().ok();
        self.holder.wait().ok();
        for &k in &self.krilld {
            kill_process(pid(k), Signal::KILL).ok();
        }
    }
}

fn connect(sock: &Path) -> Option<(UnixStream, ServerHello)> {
    let mut s = UnixStream::connect(sock).ok()?;
    s.set_read_timeout(Some(WAIT)).unwrap();
    let (k, b) = ipc::read_frame(&mut s).ok()??;
    let hello = ServerHello::decode(k, &b).unwrap();
    std::io::Write::write_all(
        &mut s,
        &client_hello(ClientRole::App, "it").encode().unwrap(),
    )
    .unwrap();
    Some((s, hello))
}

fn wait_daemon(sock: &Path, not: u64) -> (UnixStream, ServerHello) {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(c) = connect(sock).filter(|(_, h)| h.instance_id != not) {
            return c;
        }
        assert!(Instant::now() < deadline, "krilld did not come up");
        thread::sleep(Duration::from_millis(20));
    }
}

fn request(c: &mut UnixStream, r: &Request) -> Reply {
    std::io::Write::write_all(c, &r.encode().unwrap()).unwrap();
    let (k, b) = ipc::read_frame(c).unwrap().unwrap();
    Reply::decode(k, &b).unwrap()
}

fn resources(c: &mut UnixStream, action: Action) -> Resources {
    match request(c, &Request::Resources(action)) {
        Reply::Resources(r) => r,
        other => panic!("resources: {other:?}"),
    }
}

/// `footprint -f bytes <pid>`의 `Footprint: N B`.
fn footprint_tool(pid: u32) -> u64 {
    let out = Command::new("/usr/bin/footprint")
        .args(["-f", "bytes", "--noCategories", &pid.to_string()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    text.split("Footprint:")
        .nth(1)
        .and_then(|t| t.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("footprint output: {text}"))
}

/// `pid`의 직계 자식(`pgrep -P`).
fn children(pid: u32) -> Vec<u32> {
    let out = Command::new("/usr/bin/pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect()
}

fn within_10pct(what: &str, shown: u64, tool: u64) {
    eprintln!("MK-071 {what}: shown={shown} footprint(1)={tool}");
    assert!(
        shown.abs_diff(tool) * 10 <= tool,
        "{what}: shown {shown} vs footprint(1) {tool}"
    );
}

#[test]
fn resources_match_footprint_and_restart_keeps_sessions() {
    if !Path::new("/usr/bin/footprint").exists() {
        return;
    }
    let dir = env::temp_dir().join(format!("mk071-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("run/k.sock");
    let holder = Command::new(Path::new(KRILLD).parent().unwrap().join("krill-holder"))
        .arg(KRILLD)
        .arg(&sock)
        .env("KRILL_STATE_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let holder_pid = holder.id();
    let mut daemon = Daemon {
        holder,
        krilld: Vec::new(),
    };
    let (mut c, hello) = wait_daemon(&sock, 0);
    let krilld = u32::try_from(hello.instance_id).unwrap();
    daemon.krilld.push(krilld);
    // 셸 + 자식 sleep: 측정 사이에 프로세스가 바뀌지 않는다.
    let Reply::Created { session, pid: root } = request(
        &mut c,
        &Request::Create(Create {
            account: None,
            program: Program::Custom {
                program: b"/bin/sh".to_vec(),
                args: vec![b"-c".to_vec(), b"/bin/sleep 300; :".to_vec()],
            },
            cwd: Vec::new(),
            cols: 80,
            rows: 24,
        }),
    ) else {
        panic!("create failed")
    };
    let deadline = Instant::now() + WAIT;
    while children(root).is_empty() {
        assert!(Instant::now() < deadline, "sleep did not start");
        thread::sleep(Duration::from_millis(20));
    }
    fs::write(dir.join("probe"), vec![1u8; 100_000]).unwrap();
    let r = resources(&mut c, Action::Query);
    let usage = r.sessions.iter().find(|s| s.session == session).unwrap();
    assert_eq!(usage.processes, 2, "sh + sleep");
    let tree: u64 = [root]
        .into_iter()
        .chain(children(root))
        .map(footprint_tool)
        .sum();
    within_10pct("session", usage.footprint, tree);
    within_10pct("krilld", r.daemon, footprint_tool(krilld));
    within_10pct("holder", r.holder.unwrap(), footprint_tool(holder_pid));
    within_10pct(
        "client",
        r.client.unwrap(),
        footprint_tool(std::process::id()),
    );
    eprintln!("MK-071 disk: shown={}", r.disk);
    assert!(r.disk >= 100_000, "state dir includes the probe file");

    // 확인은 클라이언트 몫이다. 요청이 오면 답한 뒤 holder가 새 krilld를 띄운다.
    resources(&mut c, Action::RestartDaemon);
    let (mut c, after) = wait_daemon(&sock, hello.instance_id);
    daemon
        .krilld
        .push(u32::try_from(after.instance_id).unwrap());
    let deadline = Instant::now() + WAIT;
    while test_kill_process(pid(krilld)).is_ok() {
        assert!(Instant::now() < deadline, "old krilld still running");
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        request(&mut c, &Request::List),
        Reply::Sessions(vec![Info {
            session,
            pid: root,
            status: None
        }])
    );
    assert!(test_kill_process(pid(root)).is_ok(), "session survived");
    assert_eq!(request(&mut c, &Request::Close { session }), Reply::Closed);
    drop(daemon);
    fs::remove_dir_all(dir).ok();
}
