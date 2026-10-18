//! MK-064 (OWE-429): 생존 e2e ②③ + 장애 주입 CI(업데이트·krilld 크래시·출력 폭주·24시간 메모리 가속).
//!
//! 1. [`survival_e2e_2_update_preserves_session_output_screen_and_zero_orphan_daemons`]:
//!    업데이트(vN → vN+1) 중 세션·PID 생존, 결정적 생성기 바이트·SHA-256 무손실(Gap 0), 화면 복원,
//!    고아 데몬 0(소켓 프로세스 = holder + 새 krilld 2개), 끊긴 동안 보류 입력 전달.
//! 2. [`survival_e2e_3_krilld_crash_and_total_daemon_crash_recovery`]:
//!    - krilld `kill -9` 크래시: 세션 생존(SIGHUP 0), 감시 연결 1초 안 끊김 감지·5초 안 자동 재연결,
//!      끊긴 동안 보류 입력 정확히 1회 전달, 봉인 전 꼬리 명시적 Gap 표시 후 출력 연속.
//!    - holder+krilld 동시 `kill -9`(데몬 전체 소실): 새 데몬 자동 기동 후 디스크 세그먼트에서
//!      마지막 화면·스크롤백 읽기 전용 복원 및 세션 재개.
//! 3. [`fault_injection_output_flood_isolation_and_disk_cap`]:
//!    세션 A 대량 출력 폭주 중 세션 B 키 입력→화면 에코 지연(p99 < 33ms 목표 / 100ms 차단선) 및
//!    세션 디스크 상한(≤ 32MiB)·토큰 버킷 Gap 기록 검증.
//! 4. [`fault_injection_accelerated_24h_memory_soak`]:
//!    10세션(120×40) × 24에포크(24시간 가속) 연속 출력·세그먼트 봉인·재접속 부하에서
//!    데몬 `phys_footprint` 상한(≤ 60MiB, 전체 ≤ 200MiB)과 워밍업 이후 24에포크 증가량(≤ 20MiB 차단선) 검증.
//! 5. [`release_gui_survival_e2e_2_and_3_three_rounds`] (`#[ignore]`):
//!    실제 release `krill-app` GUI 프로세스로 생존 e2e ②(업데이트)·③(krilld kill -9 자동 복구) 3회 연속 검증.

#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::collections::HashMap;
use std::io::Write as _;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use krill_proto::SessionId;
use krill_proto::control::{Create, Info, Program, Reply, Request};
use krill_proto::daemon::{self, Paths, client_hello, reconnect_until, wait_closed, watch_hello};
use krill_proto::input::{Incarnation, Input, InputAck, Journal, Now};
use krill_proto::ipc::{self, ClientHello, ClientRole, ServerHello};
use krill_proto::screen::{
    Attach, Attached, HistoryRequest, HistoryRows, Msg, Seq, Source, Stream, Update, View,
    decode_output, kind,
};
use krilld::store::{Limits, Piece, Store};
use rustix::process::{Pid, Signal, kill_process, test_kill_process};
use sha2::{Digest as _, Sha256};

const KRILLD: &str = env!("CARGO_BIN_EXE_krilld");
const WAIT: Duration = Duration::from_secs(20);
const NEWER_BUILD: &str = "999.0.0";
const BATCH: usize = 4_000;

fn env_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn pid(raw: u32) -> Pid {
    Pid::from_raw(i32::try_from(raw).unwrap()).unwrap()
}

fn alive(raw: u32) -> bool {
    test_kill_process(pid(raw)).is_ok()
}

fn ppid(raw: u32) -> u32 {
    let out = Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &raw.to_string()])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn command_of(raw: u32) -> String {
    let out = Command::new("/bin/ps")
        .args(["-o", "command=", "-p", &raw.to_string()])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

fn daemons(sock: &Path) -> Vec<u32> {
    let out = Command::new("/usr/bin/pgrep")
        .arg("-f")
        .arg(sock)
        .output()
        .unwrap();
    let mut pids: Vec<u32> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    pids.sort_unstable();
    pids
}

fn footprint_bytes(target_pid: u32) -> u64 {
    let mut found = None;
    for _ in 0..3 {
        let out = Command::new("/usr/bin/footprint")
            .args(["-p", &target_pid.to_string(), "-f", "bytes"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(val) = text
            .split("Footprint: ")
            .nth(1)
            .and_then(|t| t.split(' ').next())
            .and_then(|v| v.parse().ok())
        {
            found = Some(val);
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    found.unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        write!(out, "{b:02x}").unwrap();
    }
    out
}

fn percentile_ms(samples: &mut [Duration], pct: usize) -> f64 {
    samples.sort_unstable();
    let idx = (samples.len() * pct).div_ceil(100).saturating_sub(1);
    samples[idx].as_secs_f64() * 1_000.0
}

struct Rig {
    dir: PathBuf,
    sock: PathBuf,
    holder: Option<Child>,
    pids: Vec<u32>,
    apps: Vec<Child>,
}

impl Rig {
    fn new(tag: &str) -> Self {
        let target = Path::new(KRILLD).parent().unwrap();
        assert!(
            target.join("krill-holder").is_file(),
            "krill-holder must be built next to krilld"
        );
        let dir = PathBuf::from(format!("/tmp/mk064-{tag}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("run/k.sock");
        Self {
            dir,
            sock,
            holder: None,
            pids: Vec::new(),
            apps: Vec::new(),
        }
    }

    fn install_bins(&self, sub: &str) -> Paths {
        let target = Path::new(KRILLD).parent().unwrap();
        let dst = self.dir.join(sub);
        fs::create_dir_all(&dst).unwrap();
        for name in ["krill-holder", "krilld"] {
            let from = target.join(name);
            let to = dst.join(name);
            if fs::hard_link(&from, &to).is_err() {
                fs::copy(&from, &to).unwrap();
            }
        }
        Paths {
            socket: self.sock.clone(),
            holder: dst.join("krill-holder"),
            krilld: dst.join("krilld"),
        }
    }

    fn start_holder(&mut self, paths: &Paths) -> (u32, UnixStream, ServerHello) {
        let child = Command::new(&paths.holder)
            .arg(&paths.krilld)
            .arg(&paths.socket)
            .env("KRILL_STATE_DIR", &self.dir)
            .env("HOME", &self.dir)
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let holder_pid = child.id();
        self.pids.push(holder_pid);
        self.holder = Some(child);
        let (c, hello) = wait_daemon(&self.sock, 0);
        let krilld_pid = u32::try_from(hello.instance_id).unwrap();
        self.pids.push(krilld_pid);
        (holder_pid, c, hello)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        for app in &mut self.apps {
            app.kill().ok();
            app.wait().ok();
        }
        // holder부터: krilld가 먼저 죽으면 holder가 다시 띄워 Drop이 모르는 pid가 남는다(OWE-604).
        if let Some(h) = &mut self.holder {
            h.kill().ok();
            h.wait().ok();
        }
        for &p in &self.pids {
            kill_process(pid(p), Signal::KILL).ok();
        }
        for p in daemons(&self.sock) {
            kill_process(pid(p), Signal::KILL).ok();
        }
        // holder가 죽었으므로 재기동은 없다. 남아 있으면 다음 폴링에서 잡힌다.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !daemons(&self.sock).is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        fs::remove_dir_all(&self.dir).ok();
    }
}

/// 이 소켓 경로를 인자로 가진 holder 프로세스가 0개인지 확인한다.
/// 테스트 본문 끝에서 `Rig`를 명시 drop한 뒤 부른다(Drop 안에서는 단언하지 않는다).
fn assert_no_holders(sock: &Path) {
    let left = daemons(sock);
    let holders: Vec<String> = left
        .iter()
        .map(|p| (*p, command_of(*p)))
        .filter(|(_, cmd)| cmd.contains("krill-holder"))
        .map(|(p, cmd)| format!("{p}: {}", cmd.trim()))
        .collect();
    assert!(
        holders.is_empty(),
        "residual holder on {}: {holders:?} (all on socket: {left:?})",
        sock.display()
    );
}

fn connect_as(sock: &Path, hello: &ClientHello) -> Option<(UnixStream, ServerHello)> {
    let mut s = UnixStream::connect(sock).ok()?;
    s.set_read_timeout(Some(WAIT)).unwrap();
    s.set_write_timeout(Some(WAIT)).unwrap();
    let (k, b) = ipc::read_frame(&mut s).ok()??;
    let server = ServerHello::decode(k, &b).unwrap();
    s.write_all(&hello.encode().unwrap()).ok()?;
    Some((s, server))
}

fn connect(sock: &Path) -> Option<(UnixStream, ServerHello)> {
    connect_as(sock, &client_hello(ClientRole::App, "mk064"))
}

fn wait_daemon(sock: &Path, not: u64) -> (UnixStream, ServerHello) {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(c) = connect(sock).filter(|(_, h)| h.instance_id != not) {
            return c;
        }
        assert!(Instant::now() < deadline, "krilld did not come up");
        thread::sleep(Duration::from_millis(5));
    }
}

fn request(c: &mut UnixStream, r: &Request) -> Reply {
    c.write_all(&r.encode().unwrap()).unwrap();
    let (k, b) = ipc::read_frame(c).unwrap().unwrap();
    Reply::decode(k, &b).unwrap()
}

fn create_custom(
    c: &mut UnixStream,
    dir: &Path,
    cols: u16,
    rows: u16,
    program: &[u8],
    args: &[&[u8]],
) -> (SessionId, u32) {
    let Reply::Created { session, pid } = request(
        c,
        &Request::Create(Create {
            account: None,
            program: Program::Custom {
                program: program.to_vec(),
                args: args.iter().map(|a| a.to_vec()).collect(),
            },
            cwd: dir.as_os_str().as_encoded_bytes().to_vec(),
            cols,
            rows,
        }),
    ) else {
        unreachable!("session creation failed");
    };
    (session, pid)
}

fn phase_bytes(phase: usize) -> Vec<u8> {
    let start = (phase - 1) * BATCH + 1;
    let mut bytes: Vec<u8> = (start..start + BATCH)
        .flat_map(|i| format!("{i:07} 한글 \x1b[3{}m색\x1b[0m line\r\n", i % 8).into_bytes())
        .collect();
    bytes.extend_from_slice(format!("MK031-PHASE-{phase}\r\n").as_bytes());
    bytes
}

fn write_phase_generator(dir: &Path) -> (PathBuf, String) {
    let generator = dir.join("generator.pl");
    fs::write(
        &generator,
        format!(
            "$|=1; print \"MK031-READY\\r\\n\"; for $phase (1..3) {{\n\
             while (!-e \"$ARGV[0]/go-$phase\") {{ select undef,undef,undef,0.01; }}\n\
             for $i (($phase-1)*{BATCH}+1..$phase*{BATCH}) {{\n\
             printf \"%07d 한글 \\e[3%dm색\\e[0m line\\r\\n\", $i, $i%8; }}\n\
             print \"MK031-PHASE-$phase\\r\\n\";\n\
             open $done, '>', \"$ARGV[0]/done-$phase\" or die $!; close $done; }}\n\
             while (1) {{ select undef,undef,undef,1; }}\n"
        ),
    )
    .unwrap();
    let script = format!(
        "stty raw -echo; exec /usr/bin/perl '{}' '{}'",
        generator.display(),
        dir.display()
    );
    (generator, script)
}

fn attach_raw(sock: &Path, session: SessionId, from: u64) -> (UnixStream, Attached) {
    let (mut s, _) = connect(sock).unwrap();
    s.write_all(
        &Attach {
            session,
            stream: Stream::Raw { from: Seq(from) },
        }
        .encode()
        .unwrap(),
    )
    .unwrap();
    let (k, b) = ipc::read_frame(&mut s).unwrap().unwrap();
    let at = Attached::decode(k, &b).unwrap();
    (s, at)
}

fn receive_exact_raw(stream: &mut UnixStream, got: &mut Vec<u8>, end: usize) {
    let deadline = Instant::now() + WAIT;
    while got.len() < end {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "raw stream deadline expired");
        stream.set_read_timeout(Some(left)).unwrap();
        let (k, b) = ipc::read_frame(stream).unwrap().unwrap();
        assert_ne!(k, kind::GAP, "unexpected Gap frame during lossless replay");
        let (seq, bytes) = decode_output(k, &b).unwrap();
        assert_eq!(seq.0, u64::try_from(got.len()).unwrap(), "output seq gap");
        got.extend_from_slice(bytes);
    }
    assert_eq!(got.len(), end, "unexpected extra output");
}

struct ScreenClient {
    stream: UnixStream,
    frames: Receiver<(u8, Vec<u8>)>,
    view: View,
}

impl ScreenClient {
    fn attach(sock: &Path, session: SessionId) -> Self {
        let (mut s, _) = connect(sock).unwrap();
        s.write_all(
            &Attach {
                session,
                stream: Stream::Screen { known: None },
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
        let (k, b) = ipc::read_frame(&mut s).unwrap().unwrap();
        Attached::decode(k, &b).unwrap();
        let mut reader = s.try_clone().unwrap();
        reader.set_read_timeout(None).ok();
        let (tx, frames) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(f)) = ipc::read_frame(&mut reader) {
                if tx.send(f).is_err() {
                    return;
                }
            }
        });
        Self {
            stream: s,
            frames,
            view: View::default(),
        }
    }

    fn wait_text(&mut self, needle: &str, timeout: Duration) -> Vec<InputAck> {
        let deadline = Instant::now() + timeout;
        let mut acks = Vec::new();
        while !self.contains(needle) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "timed out waiting for {needle:?} on screen; rows: {:?}",
                self.rows_text()
            );
            let (k, b) = self.frames.recv_timeout(left).unwrap();
            if k == kind::UPDATE {
                self.view.apply(Update::decode(k, &b).unwrap()).unwrap();
            } else if k == krill_proto::input::kind::INPUT_ACK {
                acks.push(InputAck::decode(k, &b).unwrap());
            }
        }
        acks
    }

    fn send_inputs(&mut self, inputs: &[Input]) {
        for f in inputs {
            self.stream.write_all(&f.encode().unwrap()).unwrap();
        }
    }

    fn collect_acks(&mut self, journal: &mut Journal, want: usize) {
        let deadline = Instant::now() + WAIT;
        let mut got = 0;
        while got < want {
            let left = deadline.saturating_duration_since(Instant::now());
            let (k, b) = self.frames.recv_timeout(left).unwrap();
            if k == kind::UPDATE {
                self.view.apply(Update::decode(k, &b).unwrap()).unwrap();
            } else if k == krill_proto::input::kind::INPUT_ACK {
                assert!(journal.ack(&InputAck::decode(k, &b).unwrap()).is_none());
                got += 1;
            }
        }
    }

    fn rows_text(&self) -> Vec<String> {
        self.view
            .screen
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

    fn contains(&self, needle: &str) -> bool {
        self.rows_text().iter().any(|r| r.contains(needle))
    }
}

impl Drop for ScreenClient {
    fn drop(&mut self) {
        self.stream.shutdown(Shutdown::Both).ok();
    }
}

/// 생존 e2e ②: 업데이트(v1 → v2 → v1) 중 세션·PID 생존, 결정적 출력 SHA-256 무손실(Gap 0),
/// 화면 재구성, 끊긴 동안 보류 입력 전달, 고아 데몬 0(소켓 프로세스 = holder + 새 krilld 2개).
#[test]
fn survival_e2e_2_update_preserves_session_output_screen_and_zero_orphan_daemons() {
    let mut rig = Rig::new("e2e2-upd");
    let installs = [rig.install_bins("v1"), rig.install_bins("v2")];
    let (holder_pid, mut c, mut hello) = rig.start_holder(&installs[0]);

    let (_, gen_script) = write_phase_generator(&rig.dir);
    let (gen_session, gen_pid) = create_custom(
        &mut c,
        &rig.dir,
        80,
        24,
        b"/bin/sh",
        &[b"-c", gen_script.as_bytes()],
    );
    rig.pids.push(gen_pid);

    let echo_out = rig.dir.join("echo.txt");
    let echo_script = format!(
        "stty raw -echo; /usr/bin/perl -e '$|=1; open my $f, \">\", \"{}\" or die; \
         print \"MK064-SCREEN-BEFORE\\r\\n\"; \
         while (<STDIN>) {{ print $f $_; flush $f; chomp; print \"ECHO:$_\\r\\n\"; }}'",
        echo_out.display()
    );
    let (ui_session, ui_pid) = create_custom(
        &mut c,
        &rig.dir,
        80,
        24,
        b"/bin/sh",
        &[b"-c", echo_script.as_bytes()],
    );
    rig.pids.push(ui_pid);

    let mut ui = ScreenClient::attach(&rig.sock, ui_session);
    ui.wait_text("MK064-SCREEN-BEFORE", WAIT);
    let mut journal = Journal::new();
    let (init_held, init_notices) = journal.relink(Incarnation::Same, Now::get());
    assert!(init_held.is_empty() && init_notices.is_empty());
    let pre = journal.input(b"pre-upgrade\n", Now::get()).unwrap();
    ui.send_inputs(&pre);
    ui.collect_acks(&mut journal, pre.len());
    ui.wait_text("ECHO:pre-upgrade", WAIT);

    // Phase 1: 업그레이드 전 출력(4,000줄) 수신 및 바이트 검증.
    let (mut raw_stream, _) = attach_raw(&rig.sock, gen_session, 0);
    let mut got = Vec::new();
    let mut want = b"MK031-READY\r\n".to_vec();
    receive_exact_raw(&mut raw_stream, &mut got, want.len());
    fs::write(rig.dir.join("go-1"), []).unwrap();
    want.extend(phase_bytes(1));
    receive_exact_raw(&mut raw_stream, &mut got, want.len());
    assert_eq!(got, want);
    let resume_seq = u64::try_from(got.len()).unwrap();
    drop(raw_stream);
    drop(ui);
    assert!(journal.unlink().is_none());

    // 업그레이드 중 보류 입력(화면 연결이 끊긴 상태).
    assert!(
        journal
            .input(b"held-during-upgrade\n", Now::get())
            .unwrap()
            .is_empty()
    );

    // Phase 2를 흘리면서 업그레이드 수행(v1 -> v2, 추가 회차 시 교차).
    fs::write(rig.dir.join("go-2"), []).unwrap();
    let rounds = env_usize("MK064_UPGRADE_ROUNDS", 2);
    let watch = watch_hello(ClientRole::App, "krill-app");
    let mut upgrade_gaps = Vec::new();
    for round in 0..rounds {
        let next_paths = &installs[(round + 1) % 2];
        let old_krilld = u32::try_from(hello.instance_id).unwrap();
        let watch_conn = daemon::handshake(&rig.sock, &watch, WAIT).unwrap();
        let mut watch_stream = watch_conn.stream;
        let watcher = thread::spawn(move || {
            wait_closed(&mut watch_stream);
            Instant::now()
        });
        let t0 = Instant::now();
        let next_server = daemon::upgrade_as(next_paths, &watch, NEWER_BUILD)
            .unwrap()
            .unwrap();
        upgrade_gaps.push(t0.elapsed());
        let closed_at = watcher.join().unwrap();
        assert!(closed_at >= t0);
        let reconnected = reconnect_until(next_paths, &watch);
        assert!(!reconnected.launched, "upgrade must not spawn a new holder");
        assert_eq!(reconnected.server.instance_id, next_server.instance_id);
        assert_ne!(next_server.instance_id, hello.instance_id);

        (c, hello) = wait_daemon(&rig.sock, hello.instance_id);
        let new_krilld = u32::try_from(hello.instance_id).unwrap();
        rig.pids.push(new_krilld);

        let deadline = Instant::now() + WAIT;
        while alive(old_krilld) {
            assert!(Instant::now() < deadline, "old krilld still running");
            thread::sleep(Duration::from_millis(5));
        }
        let mut expected_daemons = vec![holder_pid, new_krilld];
        expected_daemons.sort_unstable();
        assert_eq!(
            daemons(&rig.sock),
            expected_daemons,
            "orphan daemon detected after upgrade"
        );
        assert!(
            command_of(holder_pid).starts_with(next_paths.holder.to_str().unwrap()),
            "holder did not exec into new binary"
        );
        assert!(
            command_of(new_krilld).starts_with(next_paths.krilld.to_str().unwrap()),
            "krilld did not launch from new binary"
        );
    }

    let deadline = Instant::now() + WAIT;
    while !rig.dir.join("done-2").exists() {
        assert!(Instant::now() < deadline, "phase 2 did not finish");
        thread::sleep(Duration::from_millis(10));
    }
    want.extend(phase_bytes(2));

    // 세션 ID·PID 생존 확인.
    assert!(alive(gen_pid) && alive(ui_pid), "session died on upgrade");
    let Reply::Sessions(list) = request(&mut c, &Request::List) else {
        unreachable!("expected session list");
    };
    assert_eq!(
        list,
        vec![
            Info {
                session: gen_session,
                pid: gen_pid,
                status: None
            },
            Info {
                session: ui_session,
                pid: ui_pid,
                status: None
            },
        ]
    );

    // Phase 2(업그레이드 중) + Phase 3(업그레이드 후) 바이트·SHA-256 완전 일치(Gap 0).
    let (mut raw_stream, at) = attach_raw(&rig.sock, gen_session, resume_seq);
    assert!(at.seq.0 >= u64::try_from(want.len()).unwrap());
    receive_exact_raw(&mut raw_stream, &mut got, want.len());
    fs::write(rig.dir.join("go-3"), []).unwrap();
    want.extend(phase_bytes(3));
    receive_exact_raw(&mut raw_stream, &mut got, want.len());
    let got_hash = sha256_hex(&got);
    let want_hash = sha256_hex(&want);
    assert_eq!(got_hash, want_hash, "SHA-256 mismatch across upgrade");

    // 화면 재구성 + 끊긴 동안 보류 입력 전달 확인.
    let mut ui = ScreenClient::attach(&rig.sock, ui_session);
    ui.wait_text("MK064-SCREEN-BEFORE", WAIT);
    let (held_frames, notices) = journal.relink(Incarnation::Same, Now::get());
    assert!(notices.is_empty());
    assert_eq!(held_frames.len(), 1);
    ui.send_inputs(&held_frames);
    ui.collect_acks(&mut journal, 1);
    ui.wait_text("ECHO:held-during-upgrade", WAIT);
    let post = journal.input(b"post-upgrade\n", Now::get()).unwrap();
    ui.send_inputs(&post);
    ui.collect_acks(&mut journal, post.len());
    ui.wait_text("ECHO:post-upgrade", WAIT);

    let received_lines = fs::read_to_string(&echo_out).unwrap();
    assert_eq!(
        received_lines.lines().collect::<Vec<_>>(),
        ["pre-upgrade", "held-during-upgrade", "post-upgrade"]
    );
    eprintln!(
        "MK-064 e2e-2 update x{rounds}: gap p50 {:.2}ms max {:.2}ms, bytes {}, sha256 {got_hash}, daemons=2 PASS",
        percentile_ms(&mut upgrade_gaps, 50),
        percentile_ms(&mut upgrade_gaps, 100),
        got.len()
    );
}

/// 생존 e2e ③: (A) krilld `kill -9` 크래시 시 세션 생존·자동 재연결·보류 입력 1회 전달·Gap 표시 복구,
/// (B) holder+krilld 동시 `kill -9`(데몬 전체 크래시) 시 디스크 세그먼트 읽기 전용 복원 및 재개.
#[test]
fn survival_e2e_3_krilld_crash_and_total_daemon_crash_recovery() {
    let mut rig = Rig::new("e2e3-crash");
    let paths = rig.install_bins("bin");
    let (holder_pid, mut c, hello) = rig.start_holder(&paths);
    let first_krilld = u32::try_from(hello.instance_id).unwrap();

    let out_file = rig.dir.join("inputs.txt");
    let script = format!(
        "stty raw -echo; /usr/bin/perl -e '$|=1; open my $f, \">\", \"{}\" or die; \
         for $i (1..40) {{ printf \"PRE-LINE-%02d 한글\\r\\n\", $i; }} \
         print \"MK064-PRE-CRASH\\r\\n\"; \
         while (<STDIN>) {{ print $f $_; flush $f; chomp; print \"ACK:$_\\r\\n\"; }}'",
        out_file.display()
    );
    let (session, session_pid) = create_custom(
        &mut c,
        &rig.dir,
        80,
        24,
        b"/bin/sh",
        &[b"-c", script.as_bytes()],
    );
    rig.pids.push(session_pid);

    let mut ui = ScreenClient::attach(&rig.sock, session);
    ui.wait_text("MK064-PRE-CRASH", WAIT);
    let mut journal = Journal::new();
    let (init_held, init_notices) = journal.relink(Incarnation::Same, Now::get());
    assert!(init_held.is_empty() && init_notices.is_empty());
    let before = journal.input(b"line-before-crash\n", Now::get()).unwrap();
    ui.send_inputs(&before);
    ui.collect_acks(&mut journal, before.len());
    ui.wait_text("ACK:line-before-crash", WAIT);
    drop(ui);
    assert!(journal.unlink().is_none());

    // STABLE(1s) 이상 살린 뒤 krilld kill -9 → 첫 크래시로 즉시 재기동.
    thread::sleep(Duration::from_millis(1_100));
    let watch = watch_hello(ClientRole::App, "krill-app");
    let watch_conn = daemon::handshake(&rig.sock, &watch, WAIT).unwrap();
    let mut watch_stream = watch_conn.stream;
    let watcher = thread::spawn(move || {
        wait_closed(&mut watch_stream);
        Instant::now()
    });
    let kill_at = Instant::now();
    kill_process(pid(first_krilld), Signal::KILL).unwrap();

    // 끊긴 동안 보류 입력 10줄 기록.
    for i in 1..=10 {
        let line = format!("held-{i:02}\n");
        assert!(
            journal
                .input(line.as_bytes(), Now::get())
                .unwrap()
                .is_empty()
        );
    }

    let detected_at = watcher.join().unwrap();
    let reconnected = reconnect_until(&paths, &watch);
    let recovered_at = Instant::now();
    let detect_ms = (detected_at - kill_at).as_secs_f64() * 1_000.0;
    let recover_ms = (recovered_at - kill_at).as_secs_f64() * 1_000.0;

    let second_krilld = u32::try_from(reconnected.server.instance_id).unwrap();
    rig.pids.push(second_krilld);
    assert!(!reconnected.launched, "must not launch a second holder");
    assert_ne!(second_krilld, first_krilld);
    assert_eq!(ppid(second_krilld), holder_pid);
    assert!(alive(session_pid), "session died when krilld crashed");

    let (detect_limit, recover_limit) = if cfg!(debug_assertions) {
        (5_000.0, 15_000.0)
    } else {
        (1_000.0, 5_000.0)
    };
    assert!(detect_ms < detect_limit, "detect_ms {detect_ms:.2}");
    assert!(recover_ms < recover_limit, "recover_ms {recover_ms:.2}");

    // 재기동된 krilld에 다시 붙어 화면 재구성 + 보류 입력 전달 확인.
    let mut ui = ScreenClient::attach(&rig.sock, session);
    ui.wait_text("MK064-PRE-CRASH", WAIT);
    let (held_frames, notices) = journal.relink(Incarnation::Same, Now::get());
    assert!(notices.is_empty());
    assert_eq!(held_frames.len(), 10);
    ui.send_inputs(&held_frames);
    ui.collect_acks(&mut journal, 10);
    ui.wait_text("ACK:held-10", WAIT);
    drop(ui);

    // 원본 스트림 Seq(0) 재생: 크래시 전(디스크) + 크래시 후(링) 출력이 이어지고,
    // 디스크 세그먼트(Store::raw) 및 미기록 꼬리 재구독(from > end)에서 명시적 Gap(len=0)이 나온다.
    let (mut raw_stream, at) = attach_raw(&rig.sock, session, 0);
    raw_stream.set_read_timeout(Some(WAIT)).unwrap();
    let mut replay_text = Vec::new();
    while !String::from_utf8_lossy(&replay_text).contains("ACK:held-10") {
        let (k, b) = ipc::read_frame(&mut raw_stream).unwrap().unwrap();
        if k == kind::OUTPUT {
            replay_text.extend_from_slice(decode_output(k, &b).unwrap().1);
        }
    }
    let replay_str = String::from_utf8_lossy(&replay_text);
    assert!(replay_str.contains("MK064-PRE-CRASH"));
    assert!(replay_str.contains("ACK:held-01") && replay_str.contains("ACK:held-10"));
    drop(raw_stream);

    // 크래시로 디스크에 못 쓴 꼬리 뒤(from > end)에서 재구독하면 즉시 Gap(len=0) 프레임을 보낸다.
    let (mut ahead_stream, _) = attach_raw(&rig.sock, session, at.seq.0 + 64);
    ahead_stream.set_read_timeout(Some(WAIT)).unwrap();
    let (gk, gb) = ipc::read_frame(&mut ahead_stream).unwrap().unwrap();
    assert_eq!(
        gk,
        kind::GAP,
        "ahead-of-end attach after crash must emit GAP"
    );
    let gap_msg = krill_proto::screen::GapMsg::decode(gk, &gb).unwrap();
    assert_eq!(gap_msg.len, 0);
    drop(ahead_stream);

    // 디스크 세그먼트 스트림(Store::raw)에도 비봉인 SIGKILL 경계에 Piece::Gap { len: 0 }이 기록돼 있다.
    let store = Store::open(&rig.dir, Limits::default()).unwrap();
    let mut disk_saw_gap = false;
    store
        .raw(session, 0, at.seq.0, |piece| {
            if matches!(piece, Piece::Gap { len: 0, .. }) {
                disk_saw_gap = true;
            }
            Ok(())
        })
        .unwrap();
    assert!(
        disk_saw_gap,
        "unsealed krilld SIGKILL must record Gap(0) on disk"
    );

    // 입력 중복·누락 0 확인.
    let got_inputs = fs::read_to_string(&out_file).unwrap();
    let mut want_inputs = vec!["line-before-crash".to_owned()];
    want_inputs.extend((1..=10).map(|i| format!("held-{i:02}")));
    assert_eq!(got_inputs.lines().collect::<Vec<_>>(), want_inputs);

    // ③-B: holder + krilld 동시 SIGKILL(데몬 전체 소실) → 새 데몬 자동 기동 + 디스크 콜드 복원 + 재개.
    if let Some(h) = &mut rig.holder {
        h.kill().unwrap();
        h.wait().unwrap();
    }
    kill_process(pid(second_krilld), Signal::KILL).ok();
    let deadline = Instant::now() + WAIT;
    while alive(session_pid) {
        assert!(
            Instant::now() < deadline,
            "session still alive after holder+krilld kill"
        );
        thread::sleep(Duration::from_millis(10));
    }

    // 앱이 다시 연결하면 새 데몬을 자동 기동하고 목록은 비어 있다.
    let fresh_conn = daemon::connect(&paths, &watch).unwrap();
    assert!(
        fresh_conn.launched,
        "expected fresh daemon launch after total loss"
    );
    let third_krilld = u32::try_from(fresh_conn.server.instance_id).unwrap();
    rig.pids.push(ppid(third_krilld));
    rig.pids.push(third_krilld);

    let (mut ctrl, _) = connect(&rig.sock).unwrap();
    assert_eq!(
        request(&mut ctrl, &Request::List),
        Reply::Sessions(Vec::new())
    );

    // 디스크 세그먼트에서 읽기 전용 마지막 화면·스크롤백 복원(ARCHITECTURE §4.1).
    let store = Store::open(&rig.dir, Limits::default()).unwrap();
    let hist = store.history(session).unwrap();
    let restored = hist.restore().unwrap();
    let restored_screen = restored.screen_text();
    assert!(
        restored_screen.contains("ACK:held-10"),
        "cold restored screen missing last output: {restored_screen:?}"
    );
    let cold_lines: Vec<String> = (0..hist.len())
        .flat_map(|i| hist.page(i).unwrap().lines())
        .collect();
    assert!(
        cold_lines.iter().any(|l| l.contains("PRE-LINE-01"))
            || restored_screen.contains("MK064-PRE-CRASH"),
        "cold history missing pre-crash lines"
    );

    // 새 데몬에서 세션 재개(다시 시작) 성공 확인.
    let (resumed_id, resumed_pid) = create_custom(
        &mut ctrl,
        &rig.dir,
        80,
        24,
        b"/bin/sh",
        &[b"-c", b"echo MK064-RESUMED-OK; sleep 30"],
    );
    rig.pids.push(resumed_pid);
    let mut resumed_ui = ScreenClient::attach(&rig.sock, resumed_id);
    resumed_ui.wait_text("MK064-RESUMED-OK", WAIT);
    eprintln!(
        "MK-064 e2e-3 crash: detect {detect_ms:.2}ms recover {recover_ms:.2}ms, held=10/10, cold-restore+resume PASS"
    );
    let sock = rig.sock.clone();
    drop(rig);
    assert_no_holders(&sock);
}

/// 장애 주입: 세션 A 출력 폭주 중 세션 B 키 입력→화면 에코 지연(p99 < 33ms 목표 / 100ms 차단선)과
/// 디스크 세션 상한(≤ 32MB)·토큰 버킷 Gap 격리 검증.
#[test]
fn fault_injection_output_flood_isolation_and_disk_cap() {
    let mut rig = Rig::new("flood");
    let paths = rig.install_bins("bin");
    let (_, mut c, _) = rig.start_holder(&paths);

    let flood_mib = env_u64("KRILL_FLOOD_MIB", 40);
    let flood_bytes = env_u64("MK064_FLOOD_BYTES", flood_mib << 20);
    let echo_samples = env_usize("MK064_ECHO_SAMPLES", 40);

    // 세션 B(대화형): 입력 줄을 즉시 PTY로 에코한다.
    let (interactive, interactive_pid) = create_custom(
        &mut c,
        &rig.dir,
        80,
        24,
        b"/bin/sh",
        &[
            b"-c",
            b"stty raw -echo; exec /usr/bin/perl -e '$|=1; print \"B-READY\\r\\n\"; while (<STDIN>) { chomp; print \"ECHO:$_\\r\\n\"; }'",
        ],
    );
    rig.pids.push(interactive_pid);
    let mut b_ui = ScreenClient::attach(&rig.sock, interactive);
    b_ui.wait_text("B-READY", WAIT);

    // 세션 A(폭주): 1MiB 비압축성 인쇄 가능 줄 블록을 만들어 세션 B 측정이 끝나고 `flood_bytes`를 넘을 때까지 쏟아낸다.
    let stop_flood = rig.dir.join("stop-flood");
    let flood_done = rig.dir.join("flood-done");
    let flood_script = format!(
        "stty raw -echo; exec /usr/bin/perl -e '\
         $|=1; srand(1); $block = \"\"; \
         for (1..8192) {{ \
           $line = \"\"; \
           for (1..126) {{ $line .= chr(33 + int(rand(90))); }} \
           $block .= $line . \"\\r\\n\"; \
         }} \
         $target={flood_bytes}; $written=0; \
         while ($written < $target || !-e \"{}\") {{ \
           $n = syswrite(STDOUT, $block); \
           $written += ($n // 0); \
         }} \
         open my $f, \">\", \"{}\" or die; print $f $written; close $f; sleep 30;'",
        stop_flood.display(),
        flood_done.display()
    );
    let (flood, flood_pid) = create_custom(
        &mut c,
        &rig.dir,
        120,
        40,
        b"/bin/sh",
        &[b"-c", flood_script.as_bytes()],
    );
    rig.pids.push(flood_pid);

    // 세션 A에도 화면 구독자를 붙여 폭주 중 델타 송신이 함께 돌게 한다.
    let _a_ui = ScreenClient::attach(&rig.sock, flood);
    let deadline = Instant::now() + WAIT;
    while {
        let (_, at) = attach_raw(&rig.sock, flood, 0);
        at.seq.0 < (1 << 20)
    } {
        assert!(Instant::now() < deadline, "flood session did not ramp up");
        thread::sleep(Duration::from_millis(5));
    }

    // 폭주가 한창인 동안 세션 B에 키 입력을 보내 화면 에코 왕복 지연을 잰다.
    let mut journal = Journal::new();
    let (init_held, init_notices) = journal.relink(Incarnation::Same, Now::get());
    assert!(init_held.is_empty() && init_notices.is_empty());
    let mut echo_latencies = Vec::with_capacity(echo_samples);
    for idx in 1..=echo_samples {
        let token = format!("PING-{idx:04}");
        let msg = format!("{token}\n");
        let frames = journal.input(msg.as_bytes(), Now::get()).unwrap();
        let t0 = Instant::now();
        b_ui.send_inputs(&frames);
        let acks = b_ui.wait_text(&format!("ECHO:{token}"), WAIT);
        for ack in acks {
            assert!(journal.ack(&ack).is_none());
        }
        echo_latencies.push(t0.elapsed());
        thread::sleep(Duration::from_millis(5));
    }
    fs::write(&stop_flood, []).unwrap();

    // 폭주 완료 대기 및 디스크 상한(≤ 32MiB)·Gap 기록 확인.
    let flood_wait = Instant::now() + Duration::from_secs(180);
    while !flood_done.exists() {
        assert!(Instant::now() < flood_wait, "flood generator timed out");
        thread::sleep(Duration::from_millis(20));
    }
    let (_, at_end) = attach_raw(&rig.sock, flood, 0);
    assert!(
        at_end.seq.0 >= flood_bytes,
        "flood seq {} < target {flood_bytes}",
        at_end.seq.0
    );

    let session_dir = rig.dir.join(format!("sessions/{}.noindex", flood.0));
    let disk_bytes: u64 = fs::read_dir(&session_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum();
    let cap_bytes: u64 = 32 << 20;
    assert!(
        disk_bytes <= cap_bytes,
        "flood session disk bytes {disk_bytes} exceeded 32MiB cap ({cap_bytes})"
    );
    let store = Store::open(&rig.dir, Limits::default()).unwrap();
    let mut flood_saw_gap = false;
    store
        .raw(flood, 0, at_end.seq.0, |piece| {
            if matches!(piece, Piece::Gap { .. }) {
                flood_saw_gap = true;
            }
            Ok(())
        })
        .unwrap();
    assert!(
        flood_saw_gap,
        "flood exceeding 32MB burst/cap must record Gap on flooded session"
    );

    let p50 = percentile_ms(&mut echo_latencies, 50);
    let p95 = percentile_ms(&mut echo_latencies, 95);
    let p99 = percentile_ms(&mut echo_latencies, 99);
    let max = percentile_ms(&mut echo_latencies, 100);
    let p99_limit = if cfg!(debug_assertions) && env::var_os("MK064_STRICT").is_none() {
        2_000.0
    } else {
        100.0
    };
    eprintln!(
        "MK-064 flood isolation: flood_seq={}B disk={}B (<={cap_bytes}B, gap={flood_saw_gap}), echo x{echo_samples} p50={p50:.2}ms p95={p95:.2}ms p99={p99:.2}ms max={max:.2}ms (limit {p99_limit}ms)",
        at_end.seq.0, disk_bytes
    );
    assert!(
        p99 < p99_limit,
        "flood isolation echo p99 {p99:.2}ms >= {p99_limit}ms"
    );
    let sock = rig.sock.clone();
    drop(rig);
    assert_no_holders(&sock);
}

/// 장애 주입: 24시간 메모리 가속 측정(10세션 120×40 × 24에포크 연속 출력·세그먼트 봉인·재접속·히스토리 조회).
/// 워밍업(에포크 2 + 숨김 세션 강등) 이후 에포크 24까지의 데몬 `phys_footprint` 증가량이 차단선(≤ 20MiB) 안이고,
/// 데몬 합계(≤ 60MiB) 및 전체 합계(≤ 200MiB) 예산 안임을 검사한다.
#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "macOS 전용: /usr/bin/footprint")]
fn fault_injection_accelerated_24h_memory_soak() {
    let mut rig = Rig::new("soak24");
    let paths = rig.install_bins("bin");
    let (holder_pid, mut c, hello) = rig.start_holder(&paths);
    let krilld_pid = u32::try_from(hello.instance_id).unwrap();

    let epochs = env_usize("MK064_SOAK_EPOCHS", 24);
    let warmup_lines = env_usize("MK064_WARMUP_LINES", 5_500);
    let lines_per_epoch = env_usize("MK064_EPOCH_LINES", 600);

    // 10개 터미널 세션(120×40): 워밍업(에포크 1~2) 동안 첫 1MiB 세그먼트를 봉인하고,
    // 에포크 3~24 동안 연속 출력·추가 세그먼트 봉인을 이어 간다.
    let created_at = Instant::now();
    let mut sessions = Vec::with_capacity(10);
    for idx in 0..10usize {
        let script = format!(
            "stty raw -echo; exec /usr/bin/perl -e '\
             $|=1; open my $r, \">\", \"{0}/ready-{idx}\" or die; close $r; \
             for $e (1..{epochs}) {{ \
               while (!-e \"{0}/epoch-$e\") {{ select undef,undef,undef,0.01; }} \
               $n = ($e <= 2) ? {warmup_lines} : {lines_per_epoch}; \
               for $l (1..$n) {{ \
                 printf \"S%02d-E%02d-L%05d 한글 메모리 가속 소크 \\e[3%dm색상\\e[0m %s\\r\\n\", {idx}, $e, $l, $l%8, \"x\" x 45; \
               }} \
               print \"SOAK-{idx}-E$e-DONE\\r\\n\"; \
               open my $f, \">\", \"{0}/done-{idx}-$e\" or die; close $f; \
             }} \
             while (1) {{ select undef,undef,undef,1; }}'",
            rig.dir.display()
        );
        let (sid, spid) = create_custom(
            &mut c,
            &rig.dir,
            120,
            40,
            b"/bin/sh",
            &[b"-c", script.as_bytes()],
        );
        rig.pids.push(spid);
        sessions.push((sid, spid));
    }
    let deadline = Instant::now() + WAIT;
    for idx in 0..sessions.len() {
        while !rig.dir.join(format!("ready-{idx}")).exists() {
            assert!(Instant::now() < deadline, "session {idx} ready timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    // 앱의 활성 탭(세션 0)에 화면 구독자를 붙이고, 나머지 9개 세션은 백그라운드 탭(구독자 0 → 강등)으로 둔다.
    let mut active_view = ScreenClient::attach(&rig.sock, sessions[0].0);
    let mut samples_kib: Vec<(usize, u64, u64)> = Vec::new();
    for epoch in 1..=epochs {
        fs::write(rig.dir.join(format!("epoch-{epoch}")), []).unwrap();
        let deadline = Instant::now() + WAIT;
        for idx in 0..sessions.len() {
            while !rig.dir.join(format!("done-{idx}-{epoch}")).exists() {
                assert!(
                    Instant::now() < deadline,
                    "session {idx} epoch {epoch} timed out"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
        // 매 에포크마다 활성 탭 세션을 재구독 + 핫/콜드 히스토리 1페이지를 번갈아 조회해 IPC·디코더 churn을 발생시킨다.
        active_view = ScreenClient::attach(&rig.sock, sessions[0].0);
        active_view.wait_text(&format!("SOAK-0-E{epoch}-DONE"), WAIT);
        let source = if epoch % 2 == 0 {
            Source::Cold { page: 0 }
        } else {
            Source::Hot
        };
        active_view
            .stream
            .write_all(
                &HistoryRequest {
                    req: u32::try_from(epoch).unwrap(),
                    source,
                    skip: 0,
                    count: 64,
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let (k, b) = active_view.frames.recv_timeout(left).unwrap();
            if k == kind::UPDATE {
                active_view
                    .view
                    .apply(Update::decode(k, &b).unwrap())
                    .unwrap();
            } else if k == kind::HISTORY_ROWS {
                let rows = HistoryRows::decode(k, &b).unwrap();
                assert!(!rows.rows.is_empty());
                break;
            }
        }

        if epoch == 2 {
            // 워밍업 완료 시점: 백그라운드 9개 세션의 PARK_GRACE(10s) 강등이 끝날 때까지 기다려 기준선을 잡는다.
            let park_wait = (krilld::PARK_GRACE + Duration::from_millis(500))
                .saturating_sub(created_at.elapsed());
            if !park_wait.is_zero() {
                thread::sleep(park_wait);
            }
        }
        if matches!(epoch, 2 | 6 | 12 | 18) || epoch == epochs {
            let krilld_kib = footprint_bytes(krilld_pid) / 1024;
            let holder_kib = footprint_bytes(holder_pid) / 1024;
            samples_kib.push((epoch, krilld_kib, holder_kib));
        }
    }
    drop(active_view);

    for &(_, spid) in &sessions {
        assert!(alive(spid), "session died during 24-epoch soak");
    }
    let (_, base_krilld, base_holder) = samples_kib[0];
    let &(_, end_krilld, end_holder) = samples_kib.last().unwrap();
    let daemon_total_kib = end_krilld + end_holder;
    let drift_kib = i64::try_from(daemon_total_kib).unwrap()
        - i64::try_from(base_krilld + base_holder).unwrap();
    eprintln!(
        "MK-064 24-epoch soak (10x 120x40, warmup={warmup_lines}, epoch={lines_per_epoch}): samples(epoch, krilld KiB, holder KiB)={samples_kib:?}, daemon_total={daemon_total_kib}KiB, drift(e{epochs}-e2)={drift_kib:+}KiB"
    );
    assert!(
        daemon_total_kib <= 60 * 1024,
        "daemon footprint {daemon_total_kib}KiB > 60MiB budget"
    );
    assert!(
        drift_kib <= 20 * 1024,
        "24-epoch memory drift {drift_kib}KiB > 20MiB blocking limit"
    );
    let sock = rig.sock.clone();
    drop(rig);
    assert_no_holders(&sock);
}

/// 실제 release `krill-app` GUI를 띄워 생존 e2e ②(데몬 업그레이드 vN→vN+1)와
/// 생존 e2e ③(krilld `kill -9` 크래시 자동 재연결)을 3회 연속 검증한다.
#[test]
#[ignore = "actual macOS GUI; build release krill-app/krilld/krill-holder and run --release --ignored"]
fn release_gui_survival_e2e_2_and_3_three_rounds() {
    assert!(
        std::hint::black_box(!cfg!(debug_assertions)),
        "requires the release profile"
    );
    let bin_dir = Path::new(KRILLD).parent().unwrap();
    let app_bin = bin_dir.join("krill-app");
    assert!(
        app_bin.is_file(),
        "missing release krill-app binary next to krilld: {}",
        app_bin.display()
    );
    let rounds = env_usize("MK064_GUI_ROUNDS", 3);
    for round in 1..=rounds {
        gui_survival_round(round, &app_bin);
    }
}

fn wait_gui_attached(app: &mut Child) {
    let deadline = Instant::now() + WAIT;
    loop {
        assert_eq!(app.try_wait().unwrap(), None, "GUI exited before attach");
        let out = Command::new("/usr/sbin/lsof")
            .args(["-a", "-p", &app.id().to_string(), "-U", "-Ffn"])
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&out.stdout);
        let mut endpoints = HashMap::new();
        for ep in output.lines().filter_map(|l| l.strip_prefix('n')) {
            *endpoints.entry(ep.to_owned()).or_insert(0usize) += 1;
        }
        if out.status.success() && endpoints.values().any(|&n| n >= 3) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for GUI pane socket attachment"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn write_single_pane_layout(path: &Path, session: SessionId) {
    let mut bytes = vec![2, 1, 0, 0, 0, 0];
    bytes.extend_from_slice(&240.0f32.to_le_bytes());
    bytes.extend_from_slice(&280.0f32.to_le_bytes());
    bytes.extend_from_slice(&[1, 1, 0, u8::try_from(session.0).unwrap()]);
    fs::write(path, bytes).unwrap();
}

fn gui_survival_round(round: usize, app_bin: &Path) {
    let mut rig = Rig::new(&format!("gui-r{round}"));
    let v1 = rig.install_bins("v1");
    let v2 = rig.install_bins("v2");
    let (holder_pid, mut c, hello) = rig.start_holder(&v1);
    let v1_krilld = u32::try_from(hello.instance_id).unwrap();

    let (_, gen_script) = write_phase_generator(&rig.dir);
    let (session, session_pid) = create_custom(
        &mut c,
        &rig.dir,
        80,
        24,
        b"/bin/sh",
        &[b"-c", gen_script.as_bytes()],
    );
    rig.pids.push(session_pid);
    write_single_pane_layout(&rig.dir.join("layout.bin"), session);

    // 실제 release GUI 실행 및 소켓 연결(>=3 fd) 대기.
    let log = fs::File::create(rig.dir.join("app.log")).unwrap();
    let app = Command::new(app_bin)
        .env("KRILL_STATE_DIR", &rig.dir)
        .env("HOME", &rig.dir)
        .current_dir(&rig.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    let app_pid = app.id();
    rig.apps.push(app);
    wait_gui_attached(rig.apps.last_mut().unwrap());

    // Phase 1: 업그레이드 전 출력 수신.
    let (mut raw_stream, _) = attach_raw(&rig.sock, session, 0);
    let mut got = Vec::new();
    let mut want = b"MK031-READY\r\n".to_vec();
    receive_exact_raw(&mut raw_stream, &mut got, want.len());
    fs::write(rig.dir.join("go-1"), []).unwrap();
    want.extend(phase_bytes(1));
    receive_exact_raw(&mut raw_stream, &mut got, want.len());
    let resume_seq = u64::try_from(got.len()).unwrap();
    drop(raw_stream);

    // 생존 e2e ②: GUI가 떠 있는 동안 v1 -> v2 데몬 업그레이드 + Phase 2 출력.
    fs::write(rig.dir.join("go-2"), []).unwrap();
    let watch = watch_hello(ClientRole::App, "krill-app");
    let upgraded = daemon::upgrade_as(&v2, &watch, NEWER_BUILD)
        .unwrap()
        .unwrap();
    let v2_krilld = u32::try_from(upgraded.instance_id).unwrap();
    rig.pids.push(v2_krilld);
    assert_ne!(v2_krilld, v1_krilld);

    let deadline = Instant::now() + WAIT;
    while alive(v1_krilld) || !rig.dir.join("done-2").exists() {
        assert!(
            Instant::now() < deadline,
            "v1 krilld exit / phase 2 timeout"
        );
        thread::sleep(Duration::from_millis(10));
    }
    want.extend(phase_bytes(2));

    // 고아 데몬 0 + GUI 자동 재연결 확인.
    let mut want_daemons = vec![holder_pid, v2_krilld];
    want_daemons.sort_unstable();
    assert_eq!(daemons(&rig.sock), want_daemons);
    wait_gui_attached(rig.apps.last_mut().unwrap());

    // 업그레이드 구간 무손실(Gap 0) 확인.
    let (mut raw_stream, _) = attach_raw(&rig.sock, session, resume_seq);
    receive_exact_raw(&mut raw_stream, &mut got, want.len());
    drop(raw_stream);

    // 생존 e2e ③: GUI가 떠 있는 동안 v2 krilld를 SIGKILL(kill -9) → 즉시 재기동 + GUI 자동 재연결 + Phase 3 출력.
    thread::sleep(Duration::from_millis(1_100));
    kill_process(pid(v2_krilld), Signal::KILL).unwrap();
    let (mut c, hello3) = wait_daemon(&rig.sock, upgraded.instance_id);
    let v3_krilld = u32::try_from(hello3.instance_id).unwrap();
    rig.pids.push(v3_krilld);
    assert_ne!(v3_krilld, v2_krilld);
    assert_eq!(ppid(v3_krilld), holder_pid);
    wait_gui_attached(rig.apps.last_mut().unwrap());

    fs::write(rig.dir.join("go-3"), []).unwrap();
    let deadline = Instant::now() + WAIT;
    while !rig.dir.join("done-3").exists() {
        assert!(Instant::now() < deadline, "phase 3 timeout");
        thread::sleep(Duration::from_millis(10));
    }
    want.extend(phase_bytes(3));

    // Phase 2 끝까지 봉인되었거나 이어진 스트림에서 Phase 3까지 수신하고 SHA-256 일치 확인.
    let (mut raw_stream, _) = attach_raw(&rig.sock, session, 0);
    raw_stream.set_read_timeout(Some(WAIT)).unwrap();
    let mut full = Vec::new();
    while full.len() < want.len() {
        let (k, b) = ipc::read_frame(&mut raw_stream).unwrap().unwrap();
        if k == kind::OUTPUT {
            full.extend_from_slice(decode_output(k, &b).unwrap().1);
        }
    }
    let got_hash = sha256_hex(&full);
    let want_hash = sha256_hex(&want);
    assert_eq!(got_hash, want_hash);
    assert!(alive(session_pid));
    if let Reply::Sessions(list) = request(&mut c, &Request::List) {
        for info in list {
            rig.pids.push(info.pid);
            request(
                &mut c,
                &Request::Close {
                    session: info.session,
                },
            );
        }
    }
    eprintln!(
        "MK-064 GUI round {round}: app={app_pid} session={session_pid} holder={holder_pid} krilld={v1_krilld}->{v2_krilld}->{v3_krilld} bytes={} sha256={got_hash} PASS",
        full.len()
    );
    let sock = rig.sock.clone();
    drop(rig);
    assert_no_holders(&sock);
}
