//! MK-031: 실제 release GUI 프로세스의 SIGKILL·재실행과 원본 seq 재생을 검사한다.
//! gpui 잠정 결정 의존은 이 테스트 모듈에만 둔다. 기본 검사에서는 명시적으로 제외한다.
//! 재현 명령과 검증 범위는 docs/testing/MK-031.md에 기록한다.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::FileTypeExt as _;
use std::os::unix::process::ExitStatusExt as _;

use krill_proto::SessionId;
use krill_proto::control::{Create, Info, Reply, Request};
use krill_proto::screen::{Attach, Attached, Msg, Seq, Stream, decode_output};

use super::*;

const BATCH: usize = 4_000;
const ROUNDS: usize = 3;

/// 앱·holder는 직접 띄운 자식이라 반드시 거둔다. 실패 경로에서도 테스트 세션을 남기지 않는다.
pub(super) struct Processes {
    pub(super) apps: Vec<Child>,
    pub(super) holder: Child,
    pub(super) krilld: Option<u32>,
    pub(super) sessions: Vec<u32>,
    pub(super) dir: PathBuf,
}

impl Drop for Processes {
    fn drop(&mut self) {
        for app in &mut self.apps {
            app.kill().ok();
            app.wait().ok();
        }
        self.holder.kill().ok(); // holder부터: krilld 재기동을 막는다.
        self.holder.wait().ok();
        for pid in self.krilld.iter().chain(&self.sessions) {
            Command::new("/bin/kill")
                .args(["-9", &pid.to_string()])
                .status()
                .ok();
        }
        fs::remove_dir_all(&self.dir).ok();
    }
}

pub(super) fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

pub(super) fn connection(paths: &Paths) -> daemon::Connection {
    // 검사 쪽이 실수로 헬퍼를 재기동하지 않도록 실제 소켓에만 Hello를 보낸다.
    let c = daemon::handshake(
        &paths.socket,
        &daemon::client_hello(ClientRole::App, "mk031-e2e"),
        WAIT,
    )
    .unwrap();
    c.stream.set_read_timeout(Some(WAIT)).unwrap();
    c
}

pub(super) fn request(paths: &Paths, req: &Request) -> Reply {
    let mut c = connection(paths);
    c.stream.write_all(&req.encode().unwrap()).unwrap();
    let (k, b) = ipc::read_frame(&mut c.stream).unwrap().unwrap();
    Reply::decode(k, &b).unwrap()
}

fn info(paths: &Paths, session: SessionId) -> Info {
    let Reply::Sessions(list) = request(paths, &Request::List) else {
        unreachable!("expected session list");
    };
    assert_eq!(list.len(), 1, "GUI created an unexpected session");
    let info = list[0];
    assert_eq!(info.session, session);
    assert_eq!(info.status, None, "session process exited");
    info
}

pub(super) fn launch_app(guard: &mut Processes) {
    let log = fs::File::create(guard.dir.join(format!("app-{}.log", guard.apps.len()))).unwrap();
    let child = Command::new(APP)
        .env("KRILL_STATE_DIR", &guard.dir)
        .env("HOME", &guard.dir)
        .current_dir(&guard.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    guard.apps.push(child);
    let app = guard.apps.last_mut().unwrap();
    // Shell::linked이 보관하는 화면 reader/writer와 sock 복제본이 적어도 세 fd다.
    // 감시 연결 하나만 성공한 앱을 준비 완료로 오인하지 않는다. 제품 코드는 건드리지 않는다.
    wait_for("GUI pane socket attachment", || {
        assert_eq!(app.try_wait().unwrap(), None, "GUI exited before attach");
        let out = Command::new("/usr/sbin/lsof")
            .args(["-a", "-p", &app.id().to_string(), "-U", "-Ffn"])
            .output()
            .unwrap();
        let output = String::from_utf8(out.stdout).unwrap();
        let mut endpoints = HashMap::new();
        for endpoint in output.lines().filter_map(|line| line.strip_prefix('n')) {
            *endpoints.entry(endpoint).or_insert(0usize) += 1;
        }
        out.status.success() && endpoints.values().any(|&copies| copies >= 3)
    });
}

fn raw(paths: &Paths, session: SessionId, from: u64) -> (UnixStream, Attached) {
    let mut c = connection(paths);
    c.stream
        .write_all(
            &Attach {
                session,
                stream: Stream::Raw { from: Seq(from) },
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    let (k, b) = ipc::read_frame(&mut c.stream).unwrap().unwrap();
    let at = Attached::decode(k, &b).unwrap();
    (c.stream, at)
}

fn receive(stream: &mut UnixStream, got: &mut Vec<u8>, end: usize) {
    let deadline = Instant::now() + WAIT;
    while got.len() < end {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "output deadline expired");
        stream.set_read_timeout(Some(left)).unwrap();
        let (k, b) = ipc::read_frame(stream).unwrap().unwrap();
        // GAP·CLOSED·REFUSED도 실패다. 중복·누락은 seq와 바이트 완전 일치 검사로 검출한다.
        let (seq, bytes) = decode_output(k, &b).unwrap();
        assert_eq!(seq.0, u64::try_from(got.len()).unwrap(), "output seq gap");
        got.extend_from_slice(bytes);
    }
    assert_eq!(got.len(), end, "unexpected extra output");
}

fn phase_bytes(phase: usize) -> Vec<u8> {
    let start = (phase - 1) * BATCH + 1;
    let mut bytes: Vec<u8> = (start..start + BATCH)
        .flat_map(|i| format!("{i:07} 한글 \x1b[3{}m색\x1b[0m line\r\n", i % 8).into_bytes())
        .collect();
    bytes.extend_from_slice(format!("MK031-PHASE-{phase}\r\n").as_bytes());
    bytes
}

fn sha256(bytes: &[u8], dir: &Path, name: &str) -> String {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    let out = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

#[test]
#[ignore = "actual macOS GUI; build all three release binaries and run --release --ignored"]
#[expect(
    clippy::assertions_on_constants,
    reason = "명시 실행된 debug 테스트를 통과로 오인하지 않도록 release 프로필을 강제한다"
)]
fn release_app_kill9_survives_and_replays_hash_three_times() {
    assert!(!cfg!(debug_assertions), "requires the release profile");
    assert_eq!(
        Path::new(APP).parent().unwrap().file_name().unwrap(),
        "release"
    );
    for round in 1..=ROUNDS {
        survival_round(round);
    }
}

/// 새 상태 디렉터리 `dir`에 release holder(→ krilld)를 띄우고 소켓이 열릴 때까지 기다린다. 반환: 경로, 정리 가드, krilld
/// instance(= PID).
pub(super) fn start_daemon(dir: PathBuf) -> (Paths, Processes, u64) {
    fs::create_dir(&dir).unwrap();
    let paths = paths(&dir);
    for binary in [
        APP,
        paths.holder.to_str().unwrap(),
        paths.krilld.to_str().unwrap(),
    ] {
        assert!(
            Path::new(binary).is_file(),
            "missing release binary: {binary}"
        );
    }
    let holder = Command::new(&paths.holder)
        .arg(daemon::DETACH_ARG)
        .arg(&paths.krilld)
        .arg(&paths.socket)
        .env("KRILL_STATE_DIR", &dir)
        .env("HOME", &dir)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut guard = Processes {
        apps: Vec::new(),
        holder,
        krilld: None,
        sessions: Vec::new(),
        dir,
    };
    wait_for("daemon socket", || {
        paths
            .socket
            .metadata()
            .is_ok_and(|m| m.file_type().is_socket())
    });
    let instance = connection(&paths).server.instance_id;
    guard.krilld = Some(u32::try_from(instance).unwrap());
    (paths, guard, instance)
}

fn survival_round(round: usize) {
    let dir = PathBuf::from(format!("/tmp/mk031-{}-{round}", std::process::id()));
    let (paths, mut guard, instance) = start_daemon(dir);
    // 생성기는 파일 게이트를 기다린다. GUI와 raw observer가 모두 없는 동안 phase 2 전체를 출력한다.
    // /bin/sh는 rc를 읽지 않고 stty 뒤 exec하므로 세션 PID는 바뀌지 않는다.
    let generator = guard.dir.join("generator.pl");
    fs::write(
        &generator,
        format!(
            "$|=1; print \"MK031-READY\\r\\n\"; for $phase (1..3) {{\n\
         while (!-e \"$ARGV[0]/go-$phase\") {{ select undef,undef,undef,0.02; }}\n\
         for $i (($phase-1)*{BATCH}+1..$phase*{BATCH}) {{\n\
         printf \"%07d 한글 \\e[3%dm색\\e[0m line\\r\\n\", $i, $i%8; }}\n\
         print \"MK031-PHASE-$phase\\r\\n\";\n\
         open $done, '>', \"$ARGV[0]/done-$phase\" or die $!; close $done; }}\n\
         while (1) {{ select undef,undef,undef,1; }}\n"
        ),
    )
    .unwrap();
    let script = format!(
        "stty raw -echo; exec /usr/bin/perl {} {}",
        generator.display(),
        guard.dir.display()
    );
    let Reply::Created { session, pid } = request(
        &paths,
        &Request::Create(Create {
            account: None,
            program: Program::Custom {
                program: b"/bin/sh".to_vec(),
                args: vec![b"-c".to_vec(), script.into_bytes()],
            },
            cwd: Vec::new(),
            cols: 80,
            rows: 24,
        }),
    ) else {
        unreachable!("generator session creation failed");
    };
    guard.sessions.push(pid);
    let mut layout = Layout::default();
    layout.set_session(layout.visible()[0], session);
    let layout_path = guard.dir.join("layout.bin");
    layout.save(&layout_path).unwrap();

    launch_app(&mut guard);
    assert_eq!(info(&paths, session).pid, pid);
    let (mut stream, _) = raw(&paths, session, 0);
    let mut got = Vec::new();
    let mut want = b"MK031-READY\r\n".to_vec();
    receive(&mut stream, &mut got, want.len());
    fs::write(guard.dir.join("go-1"), []).unwrap();
    want.extend(phase_bytes(1));
    receive(&mut stream, &mut got, want.len());
    assert_eq!(got, want);
    let resume = u64::try_from(got.len()).unwrap();
    drop(stream);

    let first = &mut guard.apps[0];
    first.kill().unwrap(); // std Child::kill = SIGKILL (kill -9).
    assert_eq!(first.wait().unwrap().signal(), Some(9));
    assert_eq!(info(&paths, session).pid, pid);
    fs::write(guard.dir.join("go-2"), []).unwrap();
    wait_for("output while GUI absent", || {
        guard.dir.join("done-2").exists()
    });
    want.extend(phase_bytes(2));
    assert_eq!(info(&paths, session).pid, pid);

    launch_app(&mut guard);
    let restored = Layout::load(&layout_path).unwrap();
    assert_eq!(restored.session(restored.visible()[0]), Some(session));
    assert_eq!(info(&paths, session).pid, pid);
    assert_eq!(
        connection(&paths).server.instance_id,
        instance,
        "daemon restarted"
    );
    let (mut stream, at) = raw(&paths, session, resume);
    assert!(
        at.seq.0 >= u64::try_from(want.len()).unwrap(),
        "phase 2 not drained into daemon"
    );
    receive(&mut stream, &mut got, want.len());
    fs::write(guard.dir.join("go-3"), []).unwrap();
    want.extend(phase_bytes(3));
    receive(&mut stream, &mut got, want.len());
    assert_eq!(
        got, want,
        "output bytes differ from deterministic generator"
    );
    let got_hash = sha256(&got, &guard.dir, "got.bin");
    let want_hash = sha256(&want, &guard.dir, "want.bin");
    assert_eq!(got_hash, want_hash);
    assert_eq!(info(&paths, session).pid, pid);
    println!(
        "MK031 round {round}/{ROUNDS}: app {} SIGKILL=9 -> app {}, session {} pid {pid} daemon {instance}, resume {resume}, bytes {}, sha256 {got_hash} PASS",
        guard.apps[0].id(),
        guard.apps[1].id(),
        session.0,
        got.len()
    );
    assert_eq!(request(&paths, &Request::Close { session }), Reply::Closed);
    guard.sessions.clear();
}
