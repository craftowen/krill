//! MK-038: `krill hook <agent>`는 어떤 경우에도 `{}`를 먼저 내고 0으로, 50ms 예산 안에 끝난다(fail-open). krilld가
//! 받으면 spool에 쓰지 않고, 못 받으면(없음·죽은 소켓·hook 미지원·ACK 없음) spool에 붙인다. krilld 자리에는
//! `krill_proto`로 만든 가짜 서버를 둔다(실제 krilld의 hook 수신은 MK-039).
//!
//! 지연 측정(200회 p50·p95)은 `bench_200`(`--run-ignored only`, 비공개 MK-038 스파이크 기록).

#![cfg_attr(
    unix,
    expect(
        clippy::unwrap_used,
        reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
    )
)]
#![cfg(unix)]

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::hook::{self, FEAT_HOOK, HookEvent, Input, MAX_PAYLOAD, SPOOL_FILE};
use krill_proto::ipc::{self, ClientHello, PROTO_MAX, PROTO_MIN, ServerHello};

const MK: &str = env!("CARGO_BIN_EXE_krill");
/// 테스트 판정 상한. 예산은 50ms지만 병렬 테스트 부하에서 프로세스 기동이 늦어질 수 있어 넉넉히 둔다. 실제 지연은
/// `bench_200`이 잰다.
const SLACK: Duration = Duration::from_millis(500);
const STOP: &[u8] = br#"{"hook_event_name":"Stop","session_id":"s1"}"#;

/// doctor.rs와 같은 이유(OWE-484): 파이프 생성과 spawn을 한 스레드씩 한다.
static SPAWN: Mutex<()> = Mutex::new(());

fn state_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mk038-{}-{name}", std::process::id()));
    fs::remove_dir_all(&d).ok();
    fs::create_dir_all(d.join("run")).unwrap();
    d
}

fn socket(state: &Path) -> PathBuf {
    state.join("run/krilld.sock")
}

fn spawn(state: &Path, stdin: Stdio) -> Child {
    let _g = SPAWN.lock().unwrap();
    Command::new(MK)
        .args(["hook", "claude"])
        .env("KRILL_STATE_DIR", state)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

struct Run {
    stdout: Vec<u8>,
    ok: bool,
    elapsed: Duration,
    /// spawn부터 stdout 첫 바이트까지.
    first_byte: Duration,
}

const PIPE_PREFILL: usize = 16 * 1024;

/// `input`을 파이프에 미리 채워 두고(작은 입력은 EOF까지 닫은 채) `krill hook`을 띄운다(`None`이면 끝날 때까지 열어 둔다).
fn hook(state: &Path, input: Option<&[u8]>) -> Run {
    let (rx, mut tx) = std::io::pipe().unwrap();
    // 과대 입력은 앞 청크를 커널 파이프 버퍼에 먼저 채우고 쓰기 스레드가 깨어난 뒤에 자식 프로세스를 띄운다(OWE-533).
    // `None`이면 끝날 때까지 쥐고 있는다(map 클로저로 옮기면 None일 때 바로 닫힌다).
    let (writer, _open) = match input {
        Some(b) if b.len() <= PIPE_PREFILL => {
            tx.write_all(b).unwrap();
            drop(tx);
            (None, None)
        }
        Some(b) => {
            tx.write_all(&b[..PIPE_PREFILL]).unwrap();
            let rest = b[PIPE_PREFILL..].to_vec();
            let (ready_tx, ready_rx) = mpsc::sync_channel(0);
            let w = thread::spawn(move || {
                ready_tx.send(()).unwrap();
                tx.write_all(&rest).ok();
            });
            ready_rx.recv().unwrap();
            (Some(w), None)
        }
        None => (None, Some(tx)),
    };
    let start = Instant::now();
    let mut child = spawn(state, Stdio::from(rx));
    let mut out = child.stdout.take().unwrap();
    let mut first = [0u8; 1];
    out.read_exact(&mut first).unwrap();
    let first_byte = start.elapsed();
    let mut stdout = first.to_vec();
    out.read_to_end(&mut stdout).unwrap();
    let status = child.wait().unwrap();
    let elapsed = start.elapsed();
    if let Some(w) = writer {
        w.join().unwrap();
    }
    Run {
        stdout,
        ok: status.success(),
        elapsed,
        first_byte,
    }
}

fn spooled(state: &Path) -> Vec<HookEvent> {
    fs::read(state.join(SPOOL_FILE)).map_or_else(|_| Vec::new(), |b| hook::read_spool(&b))
}

fn assert_fail_open(r: &Run) {
    assert_eq!(r.stdout, b"{}\n");
    assert!(r.ok);
    assert!(r.elapsed < SLACK, "took {:?}", r.elapsed);
}

#[derive(Clone, Copy)]
enum Fake {
    /// hook을 받고 ACK한다(MK-039 이후의 krilld).
    Ack,
    /// 받기만 하고 ACK하지 않는다(멈춘 krilld).
    NoAck,
    /// `FEAT_HOOK`을 걸지 않는다(지금 krilld).
    Old,
    /// accept하고 아무것도 보내지 않는다(Hello 전에 멈춘 krilld).
    Mute,
}

/// 연결마다 받은 이벤트를 채널로 보낸다.
fn fake_krilld(socket: &Path, mode: Fake) -> Receiver<HookEvent> {
    let listener = UnixListener::bind(socket).unwrap();
    let (tx, rx) = mpsc::channel();
    let features = if let Fake::Old = mode { 1 } else { FEAT_HOOK };
    let hello = ServerHello {
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        features,
        build: "fake".to_owned(),
        instance_id: 1,
    }
    .encode()
    .unwrap();
    let ack = hook::ack();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    thread::spawn(move || {
        ready_tx.send(()).unwrap();
        for s in listener.incoming() {
            let mut s = s.unwrap();
            if let Fake::Mute = mode {
                thread::spawn(move || {
                    let _keep = s;
                    thread::sleep(Duration::from_secs(2));
                });
                continue;
            }
            // 예산이 다 된 hook은 도중에 닫는다. 그런 연결은 그냥 버린다.
            s.write_all(&hello).ok();
            let Ok(Some((k, body))) = ipc::read_frame(&mut s) else {
                continue;
            };
            ClientHello::decode(k, &body).unwrap();
            let Ok(Some((k, body))) = ipc::read_frame(&mut s) else {
                continue; // FEAT_HOOK 없으면 hook은 보내지 않고 닫는다
            };
            let ev = HookEvent::decode(k, &body).unwrap();
            if let Fake::Ack = mode {
                s.write_all(&ack).ok();
                tx.send(ev).unwrap();
            } else {
                tx.send(ev).unwrap();
                thread::spawn(move || {
                    let _keep = s;
                    thread::sleep(Duration::from_secs(2));
                });
            }
        }
    });
    ready_rx.recv().unwrap();
    rx
}

#[test]
fn no_daemon_spools() {
    let state = state_dir("none");
    let r = hook(&state, Some(STOP));
    assert_fail_open(&r);
    let ev = spooled(&state);
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].agent, "claude");
    assert_eq!(ev[0].input, Input::Complete);
    assert_eq!(ev[0].payload, STOP);
    assert_eq!(ev[0].ppid, std::process::id());
    fs::remove_dir_all(&state).ok();
}

#[test]
fn daemon_acks_no_spool() {
    let state = state_dir("ack");
    let rx = fake_krilld(&socket(&state), Fake::Ack);
    let r = hook(&state, Some(STOP));
    assert_fail_open(&r);
    let got = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(got.payload, STOP);
    assert!(spooled(&state).is_empty());
    fs::remove_dir_all(&state).ok();
}

/// ACK가 예산 안에 안 오면 spool에도 쓴다. krilld는 같은 id로 중복을 없앤다(MK-039).
#[test]
fn no_ack_spools_same_id() {
    let state = state_dir("noack");
    let rx = fake_krilld(&socket(&state), Fake::NoAck);
    let r = hook(&state, Some(STOP));
    assert_fail_open(&r);
    let sent = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(spooled(&state), vec![sent]);
    fs::remove_dir_all(&state).ok();
}

#[test]
fn old_daemon_without_hook_feature_spools() {
    let state = state_dir("old");
    let rx = fake_krilld(&socket(&state), Fake::Old);
    let r = hook(&state, Some(STOP));
    assert_fail_open(&r);
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    assert_eq!(spooled(&state).len(), 1);
    fs::remove_dir_all(&state).ok();
}

#[test]
fn hung_daemon_spools_within_budget() {
    let state = state_dir("mute");
    let _rx = fake_krilld(&socket(&state), Fake::Mute);
    let r = hook(&state, Some(STOP));
    assert_fail_open(&r);
    assert_eq!(spooled(&state).len(), 1);
    fs::remove_dir_all(&state).ok();
}

#[test]
fn four_second_signature_verification_still_spools_hook_within_budget() {
    let state = state_dir("slow-signature");
    let listener = UnixListener::bind(socket(&state)).unwrap();
    let (accepted, peer) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        accepted.send(()).unwrap();
        thread::sleep(Duration::from_secs(4));
        drop(stream);
    });
    let r = hook(&state, Some(STOP));
    peer.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_fail_open(&r);
    let events = spooled(&state);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].agent, "claude");
    fs::remove_dir_all(state).unwrap();
}

/// krilld가 죽어 소켓 파일만 남았다(ECONNREFUSED): 재시도 후 spool.
#[test]
fn dead_socket_retries_then_spools() {
    let state = state_dir("dead");
    drop(UnixListener::bind(socket(&state)).unwrap());
    let r = hook(&state, Some(STOP));
    assert_fail_open(&r);
    assert_eq!(spooled(&state).len(), 1);
    fs::remove_dir_all(&state).ok();
}

/// 에이전트가 stdin을 닫지 않아도 기다리지 않는다.
#[test]
fn stdin_never_closes() {
    let state = state_dir("stdin");
    let r = hook(&state, None);
    assert_fail_open(&r);
    let ev = spooled(&state);
    assert_eq!(ev[0].input, Input::TimedOut);
    assert!(ev[0].payload.is_empty());
    fs::remove_dir_all(&state).ok();
}

#[test]
fn oversize_json_is_truncated() {
    let state = state_dir("big");
    let rx = fake_krilld(&socket(&state), Fake::Ack);
    let big = vec![b'x'; 4 << 20];
    let r = hook(&state, Some(&big));
    assert_fail_open(&r);
    let got = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(got.input, Input::Truncated);
    assert_eq!(got.payload.len(), MAX_PAYLOAD);
    fs::remove_dir_all(&state).ok();
}

#[test]
fn missing_agent_still_prints_braces() {
    let out = Command::new(MK).arg("hook").output().unwrap();
    assert_eq!(out.stdout, b"{}\n");
    assert!(out.status.success());
}

#[test]
fn non_utf8_arg_still_prints_braces() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let state = state_dir("non-utf8");
    let bad = OsStr::from_bytes(b"\xff\xfe");
    let out = Command::new(MK)
        .arg("hook")
        .arg(bad)
        .env("KRILL_STATE_DIR", &state)
        .output()
        .unwrap();
    assert_eq!(out.stdout, b"{}\n");
    assert!(out.status.success(), "status={:?}", out.status.code());

    let out2 = Command::new(MK)
        .args([OsStr::new("hook"), OsStr::new("claude"), bad])
        .env("KRILL_STATE_DIR", &state)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out2.stdout, b"{}\n");
    assert!(out2.status.success(), "status={:?}", out2.status.code());
    fs::remove_dir_all(&state).ok();
}

/// 이름, krilld 자리(없으면 소켓 없음), stdin(없으면 열어 둠).
type Case<'a> = (&'a str, Option<Fake>, Option<&'a [u8]>);

fn pct(v: &mut [Duration], p: usize) -> Duration {
    v.sort();
    v[(v.len() * p / 100).min(v.len() - 1)]
}

/// 200회 지연 측정. 시나리오마다 wall-clock(spawn→종료)과 `{}` 첫 바이트의 p50·p95·최대를 찍는다.
#[test]
#[ignore = "측정용: cargo nextest run --release -p krill --run-ignored only --no-capture bench_200"]
fn bench_200() {
    let big = vec![b'x'; 4 << 20];
    let cases: [Case; 7] = [
        ("ack", Some(Fake::Ack), Some(STOP)),
        ("no-daemon", None, Some(STOP)),
        ("old-daemon", Some(Fake::Old), Some(STOP)),
        ("hung-daemon", Some(Fake::Mute), Some(STOP)),
        ("stdin-open", Some(Fake::Ack), None),
        ("4MiB-json", Some(Fake::Ack), Some(&big)),
        ("stdin+hung", Some(Fake::Mute), None),
    ];
    for (name, mode, input) in cases {
        let state = state_dir(&format!("bench-{name}"));
        let _rx = mode.map(|m| fake_krilld(&socket(&state), m));
        hook(&state, Some(STOP)); // 첫 실행(바이너리 검사) 제외
        let (mut wall, mut first) = (Vec::new(), Vec::new());
        for _ in 0..200 {
            let r = hook(&state, input);
            assert_eq!(r.stdout, b"{}\n");
            wall.push(r.elapsed);
            first.push(r.first_byte);
        }
        println!(
            "{name:<12} wall p50 {:?} p95 {:?} max {:?} | first byte p50 {:?} p95 {:?} max {:?} | spooled {}",
            pct(&mut wall, 50),
            pct(&mut wall, 95),
            pct(&mut wall, 100),
            pct(&mut first, 50),
            pct(&mut first, 95),
            pct(&mut first, 100),
            spooled(&state).len(),
        );
        fs::remove_dir_all(&state).ok();
    }
}
