//! MK-062: 실제 `krill-holder` + `krilld` 바이너리로 업그레이드 흐름 전체.
//! 출력 중인 세션 → `Request::Upgrade`(다른 경로의 "새" 바이너리) → holder exec 핸드오프 + 새 krilld 즉시 기동 →
//! 같은 세션·pid, 출력 무손실(처음부터 재생한 `tick` 번호가 빠짐·중복 없이 이어짐), 데몬 세대 하나(소켓을 쥔 프로세스는
//! holder와 새 krilld 둘뿐, 구 krilld는 끝남). 다운그레이드·같은 빌드는 거절, 구 앱(N-1, `FEAT_UPGRADE` 없음)도 붙는다.
//!
//! `MK062_UPGRADES=<n>`이면 업그레이드를 n번 반복하고 공백(요청 → 새 krilld Hello) 분포를 찍는다(기본 2).

#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use krill_proto::SessionId;
use krill_proto::control::{
    CaptureFormat, Create, FEAT_UPGRADE, Failure, Info, Program, Reply, Request, Upgrade, Upgraded,
};
use krill_proto::daemon::{self, Paths, client_hello};
use krill_proto::ipc::{self, ClientHello, ClientRole, ServerHello};
use krill_proto::screen::{Attach, Attached, Msg, Seq, Stream, decode_output, kind};
use rustix::process::{Pid, Signal, kill_process, test_kill_process};

const KRILLD: &str = env!("CARGO_BIN_EXE_krilld");
const WAIT: Duration = Duration::from_secs(10);
/// krilld 빌드보다 늘 새 빌드.
const NEWER: &str = "999.0.0";

fn pid(p: u32) -> Pid {
    Pid::from_raw(i32::try_from(p).unwrap()).unwrap()
}

fn alive(p: u32) -> bool {
    test_kill_process(pid(p)).is_ok()
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

fn connect_as(sock: &Path, hello: &ClientHello) -> Option<(UnixStream, ServerHello)> {
    let mut s = UnixStream::connect(sock).ok()?;
    s.set_read_timeout(Some(WAIT)).unwrap();
    let (k, b) = ipc::read_frame(&mut s).ok()??;
    let server = ServerHello::decode(k, &b).unwrap();
    s.write_all(&hello.encode().unwrap()).ok()?;
    Some((s, server))
}

fn connect(sock: &Path) -> Option<(UnixStream, ServerHello)> {
    connect_as(sock, &client_hello(ClientRole::App, "it"))
}

/// `instance_id`가 `not`이 아닌 krilld가 Hello를 보낼 때까지 1ms 간격으로 붙어 본다.
fn wait_daemon(sock: &Path, not: u64) -> (UnixStream, ServerHello) {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(c) = connect(sock).filter(|(_, h)| h.instance_id != not) {
            return c;
        }
        assert!(Instant::now() < deadline, "krilld did not come up");
        thread::sleep(Duration::from_millis(1));
    }
}

fn request(c: &mut UnixStream, r: &Request) -> Reply {
    c.write_all(&r.encode().unwrap()).unwrap();
    let (k, b) = ipc::read_frame(c).unwrap().unwrap();
    Reply::decode(k, &b).unwrap()
}

fn create(c: &mut UnixStream, dir: &Path, program: &[u8], args: &[&[u8]]) -> (SessionId, u32) {
    let Reply::Created { session, pid } = request(
        c,
        &Request::Create(Create {
            account: None,
            program: Program::Custom {
                program: program.to_vec(),
                args: args.iter().map(|a| a.to_vec()).collect(),
            },
            cwd: dir.as_os_str().as_encoded_bytes().to_vec(),
            cols: 80,
            rows: 24,
        }),
    ) else {
        unreachable!("create failed");
    };
    (session, pid)
}

/// 지금 화면의 평문.
fn screen(c: &mut UnixStream, session: SessionId) -> String {
    let r = request(
        c,
        &Request::Capture {
            session,
            format: CaptureFormat::Plain,
        },
    );
    let Reply::Captured { bytes, .. } = r else {
        unreachable!("capture: {r:?}")
    };
    String::from_utf8(bytes).unwrap()
}

fn upgrade_req(build: &str, holder: &Path, krilld: &Path) -> Request {
    Request::Upgrade(Upgrade {
        build: build.to_owned(),
        holder: holder.as_os_str().as_encoded_bytes().to_vec(),
        krilld: krilld.as_os_str().as_encoded_bytes().to_vec(),
    })
}

/// 원본 스트림을 처음부터 받아 `tick` 번호가 `upto`를 넘을 때까지 모은 번호들.
fn ticks_until(sock: &Path, id: SessionId, upto: u64) -> Vec<u64> {
    let (mut c, _) = connect(sock).unwrap();
    let a = Attach {
        session: id,
        stream: Stream::Raw { from: Seq(0) },
    };
    c.write_all(&a.encode().unwrap()).unwrap();
    let (k, b) = ipc::read_frame(&mut c).unwrap().unwrap();
    Attached::decode(k, &b).unwrap();
    let mut text = Vec::new();
    loop {
        let (k, b) = ipc::read_frame(&mut c).unwrap().unwrap();
        // 봉인 없이 끝난 krilld 뒤에는 `Gap(0)`(길이 모름)이 남는다. 업그레이드는 봉인한 뒤 끝나야 한다.
        assert_ne!(k, kind::GAP, "gap in replay");
        if k == kind::OUTPUT {
            text.extend_from_slice(decode_output(k, &b).unwrap().1);
        }
        // 마지막 줄은 덜 왔을 수 있어 완성된 줄만 센다.
        let done = String::from_utf8_lossy(&text);
        let Some((whole, _)) = done.rsplit_once('\n') else {
            continue;
        };
        let ticks: Vec<u64> = whole
            .lines()
            .filter_map(|l| l.trim_end().strip_prefix("tick ")?.parse().ok())
            .collect();
        if ticks.last().is_some_and(|&t| t > upto) {
            return ticks;
        }
    }
}

/// 이 소켓 경로를 인자로 가진 프로세스(holder·krilld) pid.
fn daemons(sock: &Path) -> Vec<u32> {
    let out = Command::new("/usr/bin/pgrep")
        .arg("-f")
        .arg(sock)
        .output()
        .unwrap();
    let mut v: Vec<u32> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim().parse().unwrap())
        .collect();
    v.sort_unstable();
    v
}

fn command_of(p: u32) -> String {
    let out = Command::new("/bin/ps")
        .args(["-o", "command=", "-p", &p.to_string()])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

/// Mach-O thin(`MH_MAGIC_64`)의 `LC_MAIN.entryoff` 파일 오프셋. 진입 명령어를 확정적으로
/// 깨뜨려(profile·레이아웃 무관) 자식 실행을 시그널 종료로 만들기 위한 테스트 전용 파서다.
fn macho_entry_offset(bytes: &[u8]) -> Option<usize> {
    // `entry_point_command { cmd, cmdsize, entryoff: u64, stacksize: u64 }`.
    const LC_MAIN: u32 = 0x8000_0028;
    if bytes.len() < 32 {
        return None;
    }
    if u32::from_le_bytes(bytes[0..4].try_into().ok()?) != 0xfeed_facf {
        return None;
    }
    let ncmds = u32::from_le_bytes(bytes[16..20].try_into().ok()?) as usize;
    let mut off = 32usize;
    for _ in 0..ncmds {
        if off + 8 > bytes.len() {
            return None;
        }
        let cmd = u32::from_le_bytes(bytes[off..off + 4].try_into().ok()?);
        let cmdsize = u32::from_le_bytes(bytes[off + 4..off + 8].try_into().ok()?) as usize;
        if cmdsize < 8 || off + cmdsize > bytes.len() {
            return None;
        }
        if cmd == LC_MAIN && cmdsize >= 24 {
            let entry = usize::try_from(u64::from_le_bytes(
                bytes[off + 8..off + 16].try_into().ok()?,
            ))
            .ok()?;
            return (entry + 4 <= bytes.len()).then_some(entry);
        }
        off += cmdsize;
    }
    None
}

/// target의 holder·krilld를 다른 디렉터리에 놓은 "새 빌드"(하드 링크, 안 되면 복사).
fn install(dir: &Path) -> (PathBuf, PathBuf) {
    let target = Path::new(KRILLD).parent().unwrap();
    fs::create_dir_all(dir).unwrap();
    let mut out = Vec::new();
    for name in ["krill-holder", "krilld"] {
        let to = dir.join(name);
        if fs::hard_link(target.join(name), &to).is_err() {
            fs::copy(target.join(name), &to).unwrap();
        }
        out.push(to);
    }
    let krilld = out.pop().unwrap();
    (out.pop().unwrap(), krilld)
}

#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "macOS 전용: Mach-O 손상 holder 주입"
)]
fn upgrade_keeps_sessions_lossless_and_one_generation() {
    let target = Path::new(KRILLD).parent().unwrap();
    assert!(
        target.join("krill-holder").is_file(),
        "krill-holder is not built next to krilld (run the whole workspace: cargo nextest run --workspace)"
    );
    let dir = env::temp_dir().join(format!("mk062-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let sock = dir.join("run/k.sock");
    let installs = [
        (target.join("krill-holder"), PathBuf::from(KRILLD)),
        install(&dir.join("new")),
    ];
    let holder = Command::new(&installs[0].0)
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
    let (mut c, mut hello) = wait_daemon(&sock, 0);
    daemon
        .krilld
        .push(u32::try_from(hello.instance_id).unwrap());
    assert_ne!(hello.features & FEAT_UPGRADE, 0);

    // 5ms마다 한 줄. 업그레이드 동안에도 계속 나온다.
    let (session, root) = create(
        &mut c,
        &dir,
        b"/usr/bin/perl",
        &[
            b"-e",
            b"$|=1; for ($i=1;;$i++) { print \"tick $i\\n\"; select(undef,undef,undef,0.005) }",
        ],
    );
    ticks_until(&sock, session, 5);
    // 한 번 찍고 멈춘 세션: 새 krilld가 디스크에서 화면을 되살려야 보인다.
    let (quiet, quiet_pid) = create(
        &mut c,
        &dir,
        b"/bin/sh",
        &[b"-c", b"echo MK062-MARK; exec sleep 600"],
    );
    let deadline = Instant::now() + WAIT;
    while !screen(&mut c, quiet).contains("MK062-MARK") {
        assert!(Instant::now() < deadline, "marker not on screen");
        thread::sleep(Duration::from_millis(10));
    }
    let both = Reply::Sessions(vec![
        Info {
            session,
            pid: root,
            status: None,
        },
        Info {
            session: quiet,
            pid: quiet_pid,
            status: None,
        },
    ]);

    // 다운그레이드·같은 빌드는 아무것도 하지 않는다. 앱 쪽 판단(`daemon::upgrade`)도 같은 빌드면 요청하지 않는다.
    let (new_holder, new_krilld) = &installs[1];
    for build in ["0.0.1", env!("CARGO_PKG_VERSION")] {
        assert_eq!(
            request(&mut c, &upgrade_req(build, new_holder, new_krilld)),
            Reply::Upgrade(Upgraded::NotNewer)
        );
    }
    assert_eq!(
        request(&mut c, &upgrade_req("x", new_holder, new_krilld)),
        Reply::Failed(Failure::BadRequest)
    );
    assert_eq!(
        request(
            &mut c,
            &upgrade_req(NEWER, &dir.join("missing"), new_krilld)
        ),
        Reply::Failed(Failure::Os(libc::ENOENT))
    );
    let noexec = dir.join("noexec");
    fs::write(&noexec, b"#!/bin/sh\n").unwrap();
    assert_eq!(
        request(&mut c, &upgrade_req(NEWER, &noexec, new_krilld)),
        Reply::Failed(Failure::Os(libc::EACCES))
    );
    // 서명이 깨진 Mach-O 바이너리는 preflight 자식 프로세스가 시그널로 죽어 EPERM으로 거절돼,
    // 실제 holder가 execve 중 SIGKILL로 죽지 않는다(MK-065). 자식의 죽음은 서명 페이지 검사(SIGKILL)든
    // 깨진 진입 명령어 실행(SIGILL)이든 `status.code() == None`으로 같게 보인다.
    //
    // 교차 검수(OWE-674/675): 고정 오프셋 1바이트 뒤집기만으로는 부족하다. ad-hoc 서명 페이지는
    // 건드리는 쪽만 죽이므로, 뒤집힌 바이트가 cold 페이지에 떨어지면 자식 실행(인자 없음)은
    // 정상 종료(코드 1)해 preflight를 통과하고, 진짜 holder가 STATE_ENV로 exec한 뒤 그 페이지를
    // 건드리는 순간 커널 SIGKILL로 죽는다(release에서 `Failed(NoHolder)` + holder 좀비로 재현,
    // 세션 프로세스·출력은 krilld가 쥔 fd 덕에 겉보기로 살아남는다). 진입점(`LC_MAIN.entryoff`)에
    // arm64 UDF(`0x00000000`, 영구 미정의 → 실행 즉시 SIGILL)를 함께 묻어 어떤 프로필·레이아웃에서도
    // 자식이 시그널로 죽게 한다. 서명 검사가 먼저 걸리면 SIGKILL — 어느 쪽이든 EPERM이다.
    let corrupt = dir.join("corrupt-holder");
    let mut bytes = fs::read(new_holder).unwrap();
    *bytes.get_mut(16384).unwrap() ^= 0xff;
    let Some(entry) = macho_entry_offset(&bytes) else {
        panic!("corrupt-holder: LC_MAIN entryoff를 찾지 못했다");
    };
    bytes[entry..entry + 4].copy_from_slice(&[0, 0, 0, 0]);
    fs::write(&corrupt, &bytes).unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&corrupt, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(
        request(&mut c, &upgrade_req(NEWER, &corrupt, new_krilld)),
        Reply::Failed(Failure::Os(libc::EPERM))
    );
    let paths = Paths {
        socket: sock.clone(),
        holder: new_holder.clone(),
        krilld: new_krilld.clone(),
    };
    let app = daemon::watch_hello(ClientRole::App, "it");
    assert_eq!(daemon::upgrade(&paths, &app).unwrap(), None);
    assert_eq!(daemon::upgrade_as(&paths, &app, "0.0.1").unwrap(), None);
    // 구 앱(N-1): `FEAT_UPGRADE`를 모르는 Hello로 붙어도 제어가 되고, 업그레이드 요청은 협상 밖이라 거절된다.
    let old_app = ClientHello {
        features: client_hello(ClientRole::App, "old").features & !FEAT_UPGRADE,
        ..client_hello(ClientRole::App, "old")
    };
    let (mut old, _) = connect_as(&sock, &old_app).unwrap();
    assert_eq!(request(&mut old, &Request::List), both);
    assert_eq!(
        request(&mut old, &upgrade_req(NEWER, new_holder, new_krilld)),
        Reply::Failed(Failure::BadRequest)
    );
    drop(old);
    assert_eq!(daemons(&sock).len(), 2, "before: {:?}", daemons(&sock));

    let rounds: usize = env::var("MK062_UPGRADES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let mut gaps = Vec::new();
    let mut seen = 0;
    for round in 0..rounds {
        let (to_holder, to_krilld) = &installs[(round + 1) % 2];
        let old_krilld = u32::try_from(hello.instance_id).unwrap();
        let to = Paths {
            socket: sock.clone(),
            holder: to_holder.clone(),
            krilld: to_krilld.clone(),
        };
        // 앱이 쓰는 경로: 요청하고 새 krilld가 Hello를 보낼 때까지 기다린다.
        let started = Instant::now();
        let next = daemon::upgrade_as(&to, &app, NEWER).unwrap().unwrap();
        gaps.push(started.elapsed());
        assert_ne!(next.instance_id, hello.instance_id);
        (c, hello) = wait_daemon(&sock, hello.instance_id);
        assert_eq!(hello.instance_id, next.instance_id);
        let new_krilld = u32::try_from(hello.instance_id).unwrap();
        daemon.krilld.push(new_krilld);

        // 세대 하나: 구 krilld는 끝나고, 소켓을 인자로 가진 프로세스는 같은 holder와 새 krilld뿐이다.
        let deadline = Instant::now() + WAIT;
        while alive(old_krilld) {
            assert!(Instant::now() < deadline, "old krilld still running");
            thread::sleep(Duration::from_millis(5));
        }
        let mut want = vec![holder_pid, new_krilld];
        want.sort_unstable();
        assert_eq!(daemons(&sock), want);
        assert!(command_of(holder_pid).starts_with(to_holder.to_str().unwrap()));
        assert!(command_of(new_krilld).starts_with(to_krilld.to_str().unwrap()));

        // 같은 세션·pid가 살아 있고 출력은 처음부터 빠짐·중복 없이 이어진다.
        assert!(alive(root), "session died during upgrade");
        assert_eq!(request(&mut c, &Request::List), both);
        let text = screen(&mut c, quiet);
        assert!(text.contains("MK062-MARK"), "screen not rebuilt: {text:?}");
        let ticks = ticks_until(&sock, session, seen + 40);
        let want: Vec<u64> = (1..=ticks.len() as u64).collect();
        assert!(ticks == want, "lost or duplicated output: {ticks:?}");
        seen = *ticks.last().unwrap();
    }
    gaps.sort_unstable();
    eprintln!(
        "MK-062 upgrade x{rounds}: gap p50 {:?} p95 {:?} max {:?} (daemon::upgrade request -> new krilld hello), lines {seen}",
        gaps[gaps.len() / 2],
        gaps[(gaps.len() * 95).div_ceil(100) - 1],
        gaps[gaps.len() - 1]
    );
    // 기본 실행은 병렬 부하에서도 깨지지 않을 상한만 본다. 300ms 기준은 docs/spikes/MK-062.md의 측정으로 판정한다.
    assert!(gaps[gaps.len() - 1] < Duration::from_secs(3));

    assert_eq!(request(&mut c, &Request::Close { session }), Reply::Closed);
    assert_eq!(
        request(&mut c, &Request::Close { session: quiet }),
        Reply::Closed
    );
    drop(daemon);
    fs::remove_dir_all(&dir).ok();
}

/// OWE-589: 새 실행 파일이 즉시 죽거나 exec조차 실패해도 이전 경로로 되돌리고 PTY를 유지한다.
#[test]
fn failed_upgrade_rolls_back_and_preserves_session() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = env::temp_dir().join(format!("mk589-rollback-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let sock = dir.join("run/k.sock");
    let (new_holder, bad) = install(&dir.join("new"));
    // hard link를 덮어써 target 바이너리를 바꾸지 않도록 먼저 지운다.
    fs::remove_file(&bad).unwrap();
    fs::write(&bad, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o700)).unwrap();
    let holder = Command::new(Path::new(KRILLD).parent().unwrap().join("krill-holder"))
        .arg(KRILLD)
        .arg(&sock)
        .env("KRILL_STATE_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = Daemon {
        holder,
        krilld: Vec::new(),
    };
    let (mut c, mut hello) = wait_daemon(&sock, 0);
    daemon
        .krilld
        .push(u32::try_from(hello.instance_id).unwrap());
    let (session, root) = create(
        &mut c,
        &dir,
        b"/bin/sh",
        &[b"-c", b"echo ROLLBACK; exec sleep 60"],
    );
    let deadline = Instant::now() + WAIT;
    while !screen(&mut c, session).contains("ROLLBACK") {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    // 즉시 죽는 바이너리로 업그레이드하면 N회 연속 실패 뒤 이전 경로로 되돌리고 세션은 산다.
    assert_eq!(
        request(&mut c, &upgrade_req(NEWER, &new_holder, &bad)),
        Reply::Upgrade(Upgraded::Started)
    );
    (c, hello) = wait_daemon(&sock, hello.instance_id);
    daemon
        .krilld
        .push(u32::try_from(hello.instance_id).unwrap());
    assert!(command_of(u32::try_from(hello.instance_id).unwrap()).starts_with(KRILLD));
    assert!(alive(root), "session died during rollback");
    assert!(screen(&mut c, session).contains("ROLLBACK"));
    assert_eq!(
        request(&mut c, &Request::List),
        Reply::Sessions(vec![Info {
            session,
            pid: root,
            status: None
        }])
    );
    // 실행 권한이 없으면 preflight가 EACCES로 거절한다: 데몬·세션 그대로, 재기동 없음.
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        request(&mut c, &upgrade_req(NEWER, &new_holder, &bad)),
        Reply::Failed(Failure::Os(libc::EACCES))
    );
    assert_eq!(connect(&sock).unwrap().1.instance_id, hello.instance_id);
    assert!(alive(root), "session died after rejected upgrade");
    assert!(screen(&mut c, session).contains("ROLLBACK"));
    assert_eq!(request(&mut c, &Request::Close { session }), Reply::Closed);
    drop(daemon);
    fs::remove_dir_all(dir).ok();
}

/// OWE-684: exec 핸드오프 뒤 holder가 Done 없이 죽으면(좀비·SIGKILL) 업그레이드는
/// 실패로 보고하고(성공 오인 금지) 구 krilld가 master fd를 쥔 채 계속 서빙한다.
/// cold 페이지 손상 흉내: 진짜 핸드오프(`MK_HOLDER_STATE` 있음)에서만 스스로 SIGKILL하고,
/// preflight(인자 없음·환경 비움)는 정상 종료해 통과한다.
#[test]
fn upgrade_holder_death_without_done_is_failure_and_sessions_survive() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = env::temp_dir().join(format!("mk684-holderloss-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    // `run/`은 holder·krilld가 0700으로 만든다. 미리 만들면(0755) 바인드가 거부된다.
    fs::create_dir_all(&dir).unwrap();
    // `krill doctor`·`krill ls`가 찾는 기본 소켓 이름(`run/krilld.sock`)을 쓴다.
    let sock = dir.join("run/krilld.sock");
    let killer = dir.join("killer-holder");
    fs::write(
        &killer,
        "#!/bin/sh\nif [ -n \"$MK_HOLDER_STATE\" ]; then kill -9 $$; fi\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&killer, fs::Permissions::from_mode(0o755)).unwrap();
    let holder = Command::new(Path::new(KRILLD).parent().unwrap().join("krill-holder"))
        .arg(KRILLD)
        .arg(&sock)
        .env("KRILL_STATE_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = Daemon {
        holder,
        krilld: Vec::new(),
    };
    let (mut c, hello) = wait_daemon(&sock, 0);
    daemon
        .krilld
        .push(u32::try_from(hello.instance_id).unwrap());
    let (quiet, quiet_pid) = create(
        &mut c,
        &dir,
        b"/bin/sh",
        &[b"-c", b"echo MK684-MARK; exec sleep 600"],
    );
    let deadline = Instant::now() + WAIT;
    while !screen(&mut c, quiet).contains("MK684-MARK") {
        assert!(Instant::now() < deadline, "marker not on screen");
        thread::sleep(Duration::from_millis(10));
    }
    // 앱과 같은 감시 연결: holder 상실 통보를 여기서 받는다.
    let mut watch = UnixStream::connect(&sock).unwrap();
    watch.set_read_timeout(Some(WAIT)).unwrap();
    let (k, b) = ipc::read_frame(&mut watch).unwrap().unwrap();
    ServerHello::decode(k, &b).unwrap();
    watch
        .write_all(
            &daemon::watch_hello(ClientRole::App, "mk684")
                .encode()
                .unwrap(),
        )
        .unwrap();
    let (k, b) = ipc::read_frame(&mut watch).unwrap().unwrap();
    assert_eq!(
        daemon::HolderStatus::decode(k, &b).unwrap(),
        daemon::HolderStatus::Linked
    );
    // preflight는 통과하지만 진짜 holder가 Done 없이 죽는다 → 실패로 보고한다.
    assert_eq!(
        request(&mut c, &upgrade_req(NEWER, &killer, Path::new(KRILLD))),
        Reply::Failed(Failure::NoHolder)
    );
    // 성공으로 오인하지 않았다: 같은 세대의 구 krilld가 계속 서빙한다.
    assert_eq!(connect(&sock).unwrap().1.instance_id, hello.instance_id);
    assert!(alive(quiet_pid), "session died with holder");
    assert!(screen(&mut c, quiet).contains("MK684-MARK"));
    // 죽은 holder(좀비)를 거두고 나면 소켓에는 krilld만 남지만 스스로 재시작하지 않는다.
    daemon.holder.wait().ok();
    thread::sleep(Duration::from_secs(1));
    assert_eq!(connect(&sock).unwrap().1.instance_id, hello.instance_id);
    assert_eq!(daemons(&sock), daemon.krilld);
    // 감시 연결에는 Orphan이 간다(앱 기존 재연결·알림 경로).
    let (k, b) = ipc::read_frame(&mut watch).unwrap().unwrap();
    assert_eq!(
        daemon::HolderStatus::decode(k, &b).unwrap(),
        daemon::HolderStatus::Orphan
    );
    drop(watch);
    // 그 상태에서 `krill doctor --json --no-start`는 holder 상실을 경고한다(치유 없음).
    let krill = Path::new(KRILLD).with_file_name("krill");
    assert!(krill.is_file(), "krill is not built next to krilld");
    let out = Command::new(&krill)
        .args(["doctor", "--json", "--no-start"])
        .env("KRILL_STATE_DIR", &dir)
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let tail = text.find("\"holder.orphan\"").and_then(|at| text.get(at..));
    assert!(tail.is_some(), "{text}");
    assert!(
        tail.unwrap_or_default()
            .find("\"warn\"")
            .is_some_and(|i| i < 400),
        "{text}"
    );
    // `krill ls`는 세션을 보여주고 holder 상실 경고를 stderr에 남긴다.
    // 새 holder를 띄워 복구하므로 인계 뒤 세션 생존도 함께 확인한다.
    let out = Command::new(&krill)
        .arg("ls")
        .env("KRILL_STATE_DIR", &dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("holder"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&quiet.0.to_string()),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let (mut c2, hello2) = wait_daemon(&sock, hello.instance_id);
    let k2 = u32::try_from(hello2.instance_id).unwrap();
    daemon.krilld.push(k2);
    assert!(screen(&mut c2, quiet).contains("MK684-MARK"));
    assert!(alive(quiet_pid), "session died during recovery");
    // 복구에 뜬 새 holder를 찾아 정리한다(분리 기동이라 Drop이 모른다).
    let parent = Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &k2.to_string()])
        .output()
        .unwrap();
    let h2: u32 = String::from_utf8(parent.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    kill_process(pid(h2), Signal::KILL).ok();
    // 기본 모드 `krill doctor`도 치유(연결) 전에 관측해 holder 상실을 경고해야 한다.
    thread::sleep(Duration::from_secs(1));
    let out = Command::new(&krill)
        .args(["doctor", "--json"])
        .env("KRILL_STATE_DIR", &dir)
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let tail = text.find("\"holder.orphan\"").and_then(|at| text.get(at..));
    assert!(
        tail.and_then(|t| t.find("\"warn\""))
            .is_some_and(|i| i < 400),
        "{text}"
    );
    // doctor가 치유하며 띄운 krilld·holder 쌍도 정리한다.
    let (_c3, hello3) = wait_daemon(&sock, hello2.instance_id);
    let k3 = u32::try_from(hello3.instance_id).unwrap();
    daemon.krilld.push(k3);
    let parent = Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &k3.to_string()])
        .output()
        .unwrap();
    if let Ok(h3) = String::from_utf8(parent.stdout)
        .unwrap()
        .trim()
        .parse::<u32>()
    {
        kill_process(pid(h3), Signal::KILL).ok();
    }
    // 세션 프로세스(원 holder의 자식, init에 입양됨)도 정리한다.
    kill_process(pid(quiet_pid), Signal::KILL).ok();
    drop(daemon);
    fs::remove_dir_all(&dir).ok();
}

/// OWE-589: 이미 건강한 데몬이 있으면 새 holder는 빈 채로 끝나며 재기동하지 않는다.
#[test]
fn duplicate_holder_exits_without_restarting_healthy_daemon() {
    let dir = env::temp_dir().join(format!("mk589-duplicate-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let sock = dir.join("run/k.sock");
    let exe = Path::new(KRILLD).parent().unwrap().join("krill-holder");
    let launch = || {
        Command::new(&exe)
            .arg(KRILLD)
            .arg(&sock)
            .env("KRILL_STATE_DIR", &dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut first = Daemon {
        holder: launch(),
        krilld: Vec::new(),
    };
    let (mut c, hello) = wait_daemon(&sock, 0);
    first.krilld.push(u32::try_from(hello.instance_id).unwrap());
    let mut duplicate = Daemon {
        holder: launch(),
        krilld: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = duplicate.holder.try_wait().unwrap() {
            assert!(status.success(), "duplicate holder: {status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "duplicate holder keeps restarting krilld"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(request(&mut c, &Request::List), Reply::Sessions(Vec::new()));
    assert_eq!(connect(&sock).unwrap().1.instance_id, hello.instance_id);
    drop(duplicate);
    drop(first);
    fs::remove_dir_all(dir).ok();
}
