//! OWE-486: 실제 `krill-holder` + `krilld` 바이너리로 데몬이 세션을 만드는 경로 전체.
//! 클라이언트 세션 생성 → PTY 출력이 krilld 세션에 들어옴(원본 스트림) → krilld `kill -9` → holder가 다시 띄운 krilld가
//! 같은 세션을 이어 받음(같은 id·pid, 출력 계속, 디스크에서 처음부터 재생) → 닫기(holder v3 `Terminate`: `setsid`로
//! 빠져나간 후손까지 끝남) → 목록에서 사라짐. holder 없이 뜬 krilld는 세션 생성을 거절한다.
//!
//! `krill-holder`는 `krilld` 옆(같은 target 디렉터리)에 있어야 한다: `cargo nextest run --workspace`.

#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use krill_proto::SessionId;
use krill_proto::control::{Create, Failure, Info, Program, Reply, Request};
use krill_proto::daemon::client_hello;
use krill_proto::ipc::{self, ClientRole, ServerHello};
use krill_proto::screen::{Attach, Attached, Msg, Seq, Stream, decode_output, kind};
use rustix::process::{Pid, Signal, kill_process, test_kill_process};

const KRILLD: &str = env!("CARGO_BIN_EXE_krilld");
const WAIT: Duration = Duration::from_secs(10);

fn tmp(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("mk486-{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn pid(p: u32) -> Pid {
    Pid::from_raw(i32::try_from(p).unwrap()).unwrap()
}

fn alive(p: u32) -> bool {
    test_kill_process(pid(p)).is_ok()
}

/// 테스트가 실패해도 holder·krilld를 남기지 않는다(둘 다 죽으면 master가 모두 닫혀 세션도 SIGHUP으로 끝난다).
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

/// 붙어서 Hello를 주고받는다. 소켓이 아직 없거나 죽은 소켓이면 `None`.
fn connect(sock: &Path) -> Option<(UnixStream, ServerHello)> {
    let mut s = UnixStream::connect(sock).ok()?;
    s.set_read_timeout(Some(WAIT)).unwrap();
    let (k, b) = ipc::read_frame(&mut s).ok()??;
    let hello = ServerHello::decode(k, &b).unwrap();
    std::io::Write::write_all(
        &mut s,
        &client_hello(ClientRole::Cli, "it").encode().unwrap(),
    )
    .unwrap();
    Some((s, hello))
}

/// `instance_id`가 `not`이 아닌 krilld가 게시될 때까지 기다린다.
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

fn list(c: &mut UnixStream) -> Vec<Info> {
    let r = request(c, &Request::List);
    let Reply::Sessions(v) = r else {
        unreachable!("list: {r:?}")
    };
    v
}

/// 원본 스트림을 `from`부터 받아 `done`이 참이 될 때까지 모은 텍스트.
fn raw_until(sock: &Path, id: SessionId, from: Seq, done: impl Fn(&str) -> bool) -> String {
    let (mut c, _) = connect(sock).unwrap();
    let a = Attach {
        session: id,
        stream: Stream::Raw { from },
    };
    std::io::Write::write_all(&mut c, &a.encode().unwrap()).unwrap();
    let (k, b) = ipc::read_frame(&mut c).unwrap().unwrap();
    Attached::decode(k, &b).unwrap();
    let mut text = Vec::new();
    while !done(&String::from_utf8_lossy(&text)) {
        let (k, b) = ipc::read_frame(&mut c).unwrap().unwrap();
        if k == kind::OUTPUT {
            text.extend_from_slice(decode_output(k, &b).unwrap().1);
        }
    }
    String::from_utf8(text).unwrap()
}

/// 텍스트에 찍힌 가장 큰 `tick N`.
fn last_tick(text: &str) -> u64 {
    text.split("tick ")
        .filter_map(|t| t.split_whitespace().next()?.parse().ok())
        .max()
        .unwrap_or(0)
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !f() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn create_survive_krilld_kill_and_close() {
    let holder_exe = Path::new(KRILLD).parent().unwrap().join("krill-holder");
    assert!(
        holder_exe.is_file(),
        "krill-holder is not built next to krilld (run the whole workspace: cargo nextest run --workspace)"
    );
    let dir = tmp("e2e");
    let sock = dir.join("run/k.sock");
    let escaped = dir.join("escaped.pid");
    let holder = Command::new(&holder_exe)
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
    let first = u32::try_from(hello.instance_id).unwrap();
    daemon.krilld.push(first);

    // 없는 실행 파일은 holder의 exec 실패(ENOENT), 없는 세션 닫기는 NoSession.
    let bad = Request::Create(Create {
        account: None,
        program: Program::Custom {
            program: b"/nonexistent/mk486".to_vec(),
            args: Vec::new(),
        },
        cwd: Vec::new(),
        cols: 80,
        rows: 24,
    });
    assert_eq!(
        request(&mut c, &bad),
        Reply::Failed(Failure::Os(libc::ENOENT))
    );
    assert_eq!(
        request(
            &mut c,
            &Request::Close {
                session: SessionId(999)
            }
        ),
        Reply::Failed(Failure::NoSession)
    );

    // 세션: 50ms마다 한 줄 + setsid로 프로세스 그룹을 빠져나간 후손 하나(Terminate만 이것까지 끝낸다).
    let script = format!(
        "/usr/bin/perl -MPOSIX -e 'POSIX::setsid(); open(my $f, \">\", \"{}\"); print $f $$; close $f; sleep 60' & \
         i=0; while :; do i=$((i+1)); echo \"tick $i\"; sleep 0.05; done",
        escaped.display()
    );
    let create = Request::Create(Create {
        account: None,
        program: Program::Custom {
            program: b"/bin/sh".to_vec(),
            args: vec![b"-c".to_vec(), script.into_bytes()],
        },
        cwd: dir.as_os_str().as_encoded_bytes().to_vec(),
        cols: 80,
        rows: 24,
    });
    let Reply::Created { session, pid: root } = request(&mut c, &create) else {
        panic!("create failed");
    };
    assert_eq!(
        list(&mut c),
        [Info {
            session,
            pid: root,
            status: None
        }]
    );

    // PTY 출력이 krilld 세션에 들어온다.
    let before = last_tick(&raw_until(&sock, session, Seq(0), |t| last_tick(t) >= 5));
    wait_until("escaped descendant pid file", || {
        fs::read_to_string(&escaped).is_ok_and(|s| !s.is_empty())
    });
    let esc: u32 = fs::read_to_string(&escaped)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(esc));

    // krilld kill -9: holder가 새 krilld에 세션을 다시 넘긴다.
    kill_process(pid(first), Signal::KILL).unwrap();
    let (mut c, hello) = wait_daemon(&sock, hello.instance_id);
    daemon
        .krilld
        .push(u32::try_from(hello.instance_id).unwrap());
    assert!(alive(root), "session process died with krilld");
    assert_eq!(
        list(&mut c),
        [Info {
            session,
            pid: root,
            status: None
        }]
    );
    // 새 krilld의 세션에 출력이 계속 들어오고, 처음부터 읽으면 디스크 세그먼트가 이어 준다.
    let after = raw_until(&sock, session, Seq(0), |t| last_tick(t) > before + 3);
    assert!(
        after.starts_with("tick 1\r\n"),
        "replay from disk: {after:.40}"
    );

    // 닫기: Terminate가 세션과 빠져나간 후손을 끝내고 krilld가 세션을 지운다.
    assert_eq!(request(&mut c, &Request::Close { session }), Reply::Closed);
    wait_until("session removed", || list(&mut c).is_empty());
    wait_until("session process gone", || !alive(root));
    wait_until("escaped descendant gone", || !alive(esc));
    assert_eq!(
        request(&mut c, &Request::Close { session }),
        Reply::Failed(Failure::NoSession)
    );

    drop(daemon);
    fs::remove_dir_all(&dir).ok();
}

/// holder 없이 뜬 krilld(stdin이 소켓이 아님)는 세션을 만들지 않는다.
#[test]
fn without_holder_create_is_refused() {
    let dir = tmp("solo");
    let sock = dir.join("run/k.sock");
    let mut k = Command::new(KRILLD)
        .arg(&sock)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let (mut c, _) = wait_daemon(&sock, 0);
    let create = Request::Create(Create {
        account: None,
        program: Program::Login,
        cwd: Vec::new(),
        cols: 80,
        rows: 24,
    });
    assert_eq!(request(&mut c, &create), Reply::Failed(Failure::NoHolder));
    assert_eq!(list(&mut c), []);
    k.kill().ok();
    k.wait().ok();
    fs::remove_dir_all(&dir).ok();
}

/// MK-049: `SIGCHLD`를 받는 holder의 루트 종료와 `NOTE_EXIT`로 보는 전경 에이전트 종료를
/// 실제 바이너리·PTY·kill -9·States 왕복으로 검증한다. 셸 생존·작업 중지는 종료와 구분한다.
#[test]
fn mk049_sigkill_reports_exited_within_one_second() {
    use krill_proto::control::{AgentState, AgentStatus, Source};

    let dir = tmp("mk049");
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
    let mut daemon = Daemon {
        holder,
        krilld: Vec::new(),
    };
    let (mut c, hello) = wait_daemon(&sock, 0);
    daemon
        .krilld
        .push(u32::try_from(hello.instance_id).unwrap());
    let state = |c: &mut UnixStream, id| -> AgentStatus {
        let Reply::States(rows) = request(c, &Request::States) else {
            panic!("states")
        };
        rows.into_iter().find(|r| r.session == id).unwrap().status
    };

    for foreground in [false, true] {
        let child_file = dir.join(if foreground {
            "foreground.pid"
        } else {
            "root.pid"
        });
        let perl = format!(
            "open(my $f, \">\", \"{}\") or die; print $f $$; close $f; $|=1; print qq(\\e]0;✳ Claude Code\\a); sleep 60;",
            child_file.display()
        );
        let (program, args) = if foreground {
            (
                b"/bin/zsh".to_vec(),
                vec![
                    b"-f".to_vec(),
                    b"-i".to_vec(),
                    b"-c".to_vec(),
                    format!(
                        "setopt monitor; /usr/bin/perl -e '{perl}'; print AGENT_EXITED; read keep"
                    )
                    .into_bytes(),
                ],
            )
        } else {
            (
                b"/usr/bin/perl".to_vec(),
                vec![b"-e".to_vec(), perl.into_bytes()],
            )
        };
        let Reply::Created { session, pid: root } = request(
            &mut c,
            &Request::Create(Create {
                account: None,
                program: Program::Custom { program, args },
                cwd: Vec::new(),
                cols: 80,
                rows: 24,
            }),
        ) else {
            panic!("create")
        };
        wait_until("agent started", || {
            child_file.is_file() && state(&mut c, session).state == AgentState::Done
        });
        let agent: u32 = fs::read_to_string(&child_file).unwrap().parse().unwrap();
        if foreground {
            assert_ne!(agent, root);
            // 전경 pgid가 바뀌어도 멈춘 프로세스는 아직 살아 있다.
            kill_process(pid(agent), Signal::STOP).unwrap();
            thread::sleep(Duration::from_millis(350));
            assert_ne!(state(&mut c, session).state, AgentState::Exited);
            kill_process(pid(agent), Signal::CONT).unwrap();
        }
        // 감시 주기를 한 번 지나 등록된 뒤 SIGKILL한다(등록 전 빠른 종료는 holder가 맡는다).
        thread::sleep(Duration::from_millis(150));
        let killed = Instant::now();
        kill_process(pid(agent), Signal::KILL).unwrap();
        loop {
            let s = state(&mut c, session);
            if s.state == AgentState::Exited {
                assert_eq!(s.source, Some(Source::Process));
                break;
            }
            assert!(
                killed.elapsed() < Duration::from_secs(1),
                "exited too late: {s:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            killed.elapsed() < Duration::from_secs(1),
            "States response exceeded deadline"
        );
        eprintln!(
            "MK-049 foreground={foreground}: SIGKILL → exited {:?}",
            killed.elapsed()
        );
        if foreground {
            assert!(alive(root), "agent death killed its shell");
            assert_eq!(
                list(&mut c)
                    .iter()
                    .find(|i| i.session == session)
                    .unwrap()
                    .status,
                None
            );
            // 옛 제목이 다음 감시 주기에 exited를 덮어쓰면 안 된다.
            thread::sleep(Duration::from_millis(350));
            assert_eq!(state(&mut c, session).state, AgentState::Exited);
        }
        assert_eq!(request(&mut c, &Request::Close { session }), Reply::Closed);
    }
    drop(daemon);
    fs::remove_dir_all(&dir).ok();
}

/// MK-068: 실제 1분 주기 3회 초과 → holder 재기동, 같은 세션·PID·PTY 출력 유지.
/// release 바이너리로 실행: cargo test --locked --release -p krilld --test holder mk068 -- --ignored --nocapture
#[test]
#[ignore = "실제 3분을 기다리는 release 자기 감시 e2e; MK-068에서 별도 실행"]
fn mk068_footprint_restart_preserves_session_and_minute_metrics() {
    let dir = tmp("mk068");
    let sock = dir.join("run/k.sock");
    let holder = Command::new(Path::new(KRILLD).parent().unwrap().join("krill-holder"))
        .arg(KRILLD)
        .arg(&sock)
        .env("KRILL_STATE_DIR", &dir)
        .env("KRILL_FOOTPRINT_LIMIT_BYTES", "1") // 실제 footprint가 항상 상한을 넘게 주입한다.
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
    let Reply::Created { session, pid: root } = request(
        &mut c,
        &Request::Create(Create {
            account: None,
            program: Program::Custom {
                program: b"/bin/sh".to_vec(),
                args: vec![
                    b"-c".to_vec(),
                    b"i=0; while :; do i=$((i+1)); echo tick $i; sleep 0.1; done".to_vec(),
                ],
            },
            cwd: dir.as_os_str().as_encoded_bytes().to_vec(),
            cols: 80,
            rows: 24,
        }),
    ) else {
        panic!("create failed")
    };
    let before = last_tick(&raw_until(&sock, session, Seq(0), |t| last_tick(t) >= 3));
    let started = Instant::now();
    // 측정 2회까지는 운영하며, 다음 1분에 3회째 초과를 확인하고 재기동한다.
    while !fs::read_to_string(dir.join("monitor.log"))
        .is_ok_and(|s| s.contains("restart_requested"))
    {
        assert!(
            started.elapsed() < Duration::from_secs(210),
            "monitor did not restart"
        );
        assert!(alive(root), "session died during monitoring");
        thread::sleep(Duration::from_millis(100));
    }
    let (mut c, after) = wait_daemon(&sock, hello.instance_id);
    daemon
        .krilld
        .push(u32::try_from(after.instance_id).unwrap());
    assert_eq!(
        list(&mut c),
        [Info {
            session,
            pid: root,
            status: None
        }]
    );
    assert!(alive(root));
    let text = raw_until(&sock, session, Seq(0), |t| last_tick(t) > before + 3);
    assert!(last_tick(&text) > before);
    let log = fs::read_to_string(dir.join("monitor.log")).unwrap();
    let metrics: Vec<_> = log
        .lines()
        .filter(|line| line.contains(" disk_write "))
        .collect();
    assert_eq!(metrics.len(), 3, "{log}");
    eprintln!("{log}");
    for line in metrics {
        let elapsed: u64 = line
            .split_whitespace()
            .find_map(|f| f.strip_prefix("elapsed_ms="))
            .unwrap()
            .parse()
            .unwrap();
        assert!(elapsed >= 60_000, "short sample: {line}");
        assert!(line.contains(" bytes=") && line.contains(" total_bytes="));
    }
    assert!(dir.join("monitor-restart").is_file());
    eprintln!(
        "MK-068 release: 3 minute samples, holder restart after {:?}, session {session:?} pid={root} preserved",
        started.elapsed()
    );
    assert_eq!(request(&mut c, &Request::Close { session }), Reply::Closed);
    drop(daemon);
    fs::remove_dir_all(dir).ok();
}

/// OWE-488: Create의 프로필 id → 런처 argv·env → 실제 holder PTY, 권한 스냅샷의 재기동 보존.
#[test]
fn agent_profile_permissions_are_fixed_at_spawn_and_survive_restart() {
    use krill_proto::agent::Permission;
    use krill_proto::control::AgentLaunch;

    let dir = tmp("agent-launch");
    let sock = dir.join("run/k.sock");
    fs::write(dir.join("agents.toml"), r#"
permission = "manual"
[agent.fixture]
name = "Fixture"
launch = ["/bin/sh", "-c", 'printf "ARG=%s\n" "$@"; printf "INTERNAL=%s/%s\n" "${MK_PANE_ID-unset}" "${KRILL_PRIVATE-unset}"; while :; do sleep 1; done', "fixture"]
yolo_args = ["--fixture-yolo", "quoted ; $(false)"]
[agent.missing]
name = "Missing fixture"
launch = ["/nonexistent/mk488"]
install = "fixture-install"
"#).unwrap();
    let holder = Command::new(Path::new(KRILLD).parent().unwrap().join("krill-holder"))
        .arg(KRILLD)
        .arg(&sock)
        .env("KRILL_STATE_DIR", &dir)
        .env("KRILL_PRIVATE", "must-not-leak")
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
    let create = |id: &str| {
        Request::Create(Create {
            account: None,
            program: Program::Agent { id: id.into() },
            cwd: Vec::new(),
            cols: 80,
            rows: 24,
        })
    };
    assert_eq!(
        request(&mut c, &create("unknown")),
        Reply::Failed(Failure::UnknownAgent)
    );
    let Reply::AgentSettings(settings) =
        request(&mut c, &Request::AgentSettings { permission: None })
    else {
        panic!("settings")
    };
    assert_eq!(settings.permission, Permission::Manual);
    assert!(settings.profiles.iter().any(|p| p.id == "fixture"));
    let Reply::Created {
        session: manual,
        pid: manual_pid,
    } = request(&mut c, &create("fixture"))
    else {
        panic!("manual create")
    };
    // 내부 값은 넘기지 않고 pane 환경(OWE-503)만 넣는다.
    let text = raw_until(&sock, manual, Seq(0), |s| {
        s.contains(&format!("INTERNAL={}/unset", manual.0))
    });
    assert!(!text.contains("--fixture-yolo"), "{text}");
    let Reply::AgentSettings(settings) = request(
        &mut c,
        &Request::AgentSettings {
            permission: Some(Permission::Yolo),
        },
    ) else {
        panic!("toggle")
    };
    assert_eq!(settings.permission, Permission::Yolo);
    let Reply::Created {
        session: yolo,
        pid: yolo_pid,
    } = request(&mut c, &create("fixture"))
    else {
        panic!("yolo create")
    };
    let text = raw_until(&sock, yolo, Seq(0), |s| {
        s.contains(&format!("INTERNAL={}/unset", yolo.0))
    });
    assert!(
        text.contains("ARG=--fixture-yolo\r\nARG=quoted ; $(false)"),
        "{text}"
    );
    // 메타데이터는 응답 전에 저장되고 토글 변경으로 기존 실행 권한이 바뀌지 않는다.
    assert_eq!(
        request(&mut c, &Request::AgentLaunch { session: manual }),
        Reply::AgentLaunch(Some(AgentLaunch {
            profile: "fixture".into(),
            permission: Permission::Manual
        }))
    );
    assert_eq!(
        request(&mut c, &Request::AgentLaunch { session: yolo }),
        Reply::AgentLaunch(Some(AgentLaunch {
            profile: "fixture".into(),
            permission: Permission::Yolo
        }))
    );
    kill_process(pid(u32::try_from(hello.instance_id).unwrap()), Signal::KILL).unwrap();
    let (mut c, restarted) = wait_daemon(&sock, hello.instance_id);
    daemon
        .krilld
        .push(u32::try_from(restarted.instance_id).unwrap());
    assert!(alive(manual_pid) && alive(yolo_pid));
    let Reply::AgentSettings(settings) =
        request(&mut c, &Request::AgentSettings { permission: None })
    else {
        panic!("restart settings")
    };
    assert_eq!(settings.permission, Permission::Yolo);
    request(
        &mut c,
        &Request::AgentSettings {
            permission: Some(Permission::Manual),
        },
    );
    assert_eq!(
        request(&mut c, &Request::AgentLaunch { session: yolo }),
        Reply::AgentLaunch(Some(AgentLaunch {
            profile: "fixture".into(),
            permission: Permission::Yolo
        }))
    );
    assert_eq!(
        request(&mut c, &Request::AgentLaunch { session: manual }),
        Reply::AgentLaunch(Some(AgentLaunch {
            profile: "fixture".into(),
            permission: Permission::Manual
        }))
    );
    let Reply::Created {
        session: missing, ..
    } = request(&mut c, &create("missing"))
    else {
        panic!("missing preflight create")
    };
    let text = raw_until(&sock, missing, Seq(0), |s| s.contains("fixture-install"));
    assert!(text.contains("not installed"), "{text}");
    for session in [manual, yolo, missing] {
        assert_eq!(request(&mut c, &Request::Close { session }), Reply::Closed);
    }
    wait_until("agent sessions removed", || list(&mut c).is_empty());
    assert!(!dir.join(format!("agent-launches/{}.json", yolo.0)).exists());
    drop(daemon);
    fs::remove_dir_all(dir).unwrap();
}
