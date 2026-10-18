use krill_proto::os::net::UnixListener;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use rustix::process::{Pid, Signal, kill_process_group};

use super::*;

const A: SessionId = SessionId(1);
const B: SessionId = SessionId(2);
const C: SessionId = SessionId(3);

/// 가짜 프로세스 트리: pid → (ppid, 시작 시각).
struct Tree(HashMap<u32, (u32, u64)>);

impl Procs for Tree {
    fn parent(&self, pid: u32) -> io::Result<u32> {
        self.0
            .get(&pid)
            .map(|n| n.0)
            .ok_or(io::ErrorKind::NotFound.into())
    }
    fn start_us(&self, pid: u32) -> io::Result<u64> {
        self.0
            .get(&pid)
            .map(|n| n.1)
            .ok_or(io::ErrorKind::NotFound.into())
    }
}

fn tree(nodes: &[(u32, u32, u64)]) -> Tree {
    Tree(
        nodes
            .iter()
            .map(|&(pid, ppid, s)| (pid, (ppid, s)))
            .collect(),
    )
}

fn roots(list: &[(SessionId, u32, u64)]) -> Roots {
    Roots {
        pids: list
            .iter()
            .map(|&(id, pid, start)| (pid, (id, start)))
            .collect(),
        ..Roots::default()
    }
}

#[test]
fn walk_classifies_by_lineage() {
    // launchd(1) ← holder(10) ← 세션 A 루트(20) ← zsh(30) ← krill(40). 외부: Terminal(50) ← zsh(60).
    let info = tree(&[
        (10, 1, 0),
        (20, 10, 200),
        (30, 20, 300),
        (40, 30, 400),
        (50, 1, 0),
        (60, 50, 600),
        (70, 999, 0), // 부모 조회 실패
    ]);
    let r = roots(&[(A, 20, 200)]);
    assert_eq!(walk(40, &r, &info), Scope::Session(A), "grandchild");
    assert_eq!(walk(20, &r, &info), Scope::Session(A), "root itself");
    assert_eq!(walk(60, &r, &info), Scope::Full, "external shell");
    assert_eq!(walk(10, &r, &info), Scope::Full, "holder");
    assert_eq!(walk(70, &r, &info), Scope::Restricted, "broken chain");
    assert_eq!(walk(12345, &r, &info), Scope::Restricted, "peer gone");

    // 루트 PID가 재사용됨(시작 시각 다름) → 그 세션이 아니다.
    let reused = roots(&[(A, 20, 199)]);
    assert_eq!(walk(40, &reused, &info), Scope::Full);
}

#[test]
fn walk_gives_up_on_cycles() {
    let info = tree(&[(5, 6, 0), (6, 5, 0)]);
    assert_eq!(walk(5, &Roots::default(), &info), Scope::Restricted);
}

#[test]
fn allows_matrix() {
    let mut r = Roots::default();
    assert!(Scope::Full.allows(A, &r) && Scope::Full.allows(B, &r));
    assert!(Scope::Session(A).allows(A, &r));
    assert!(!Scope::Session(A).allows(B, &r));
    assert!(!Scope::Restricted.allows(A, &r));

    // A가 만든 B(그리고 B가 만든 C)는 A 범위 안에 들어가지만, 역방향·무관한 세션은 거부한다.
    r.record_creator(B, Scope::Session(A)).unwrap();
    r.record_creator(C, Scope::Session(B)).unwrap();
    assert!(Scope::Session(A).allows(B, &r));
    assert!(Scope::Session(A).allows(C, &r));
    assert!(Scope::Session(B).allows(C, &r));
    assert!(!Scope::Session(B).allows(A, &r));
    assert!(!Scope::Session(C).allows(A, &r));
    r.remove(B);
    assert!(Scope::Session(A).allows(C, &r), "reparented grandchild");
}

#[test]
fn remove_forgets_session() {
    let mut r = roots(&[(A, 20, 200), (B, 21, 210)]);
    r.remove(A);
    let info = tree(&[(20, 1, 200), (21, 1, 210)]);
    assert_eq!(walk(20, &r, &info), Scope::Full);
    assert_eq!(walk(21, &r, &info), Scope::Session(B));
}

/// 테스트마다 짧은 임시 디렉터리(소켓 경로 104바이트 제한 안).
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mkscope-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&d).ok();
        fs::create_dir_all(&d).unwrap();
        Self(d)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn audit_line_mode_and_rotation() {
    let dir = Dir::new("audit");
    let path = dir.0.join("audit.log");
    let audit = Audit::new(path.clone());
    let r = Roots::default();
    let peer = Peer {
        pid: Some(42),
        scope: Scope::Session(A),
    };
    assert!(!peer.authorize("send", B, &r, &audit));
    assert!(
        peer.authorize("send", A, &r, &audit),
        "allowed is not logged"
    );
    let gone = Peer {
        pid: None,
        scope: Scope::Restricted,
    };
    assert!(!gone.authorize("read", A, &r, &audit));
    let text = fs::read_to_string(&path).unwrap();
    let lines: Vec<_> = text.lines().map(|l| l.split_once(' ').unwrap().1).collect();
    assert_eq!(
        lines,
        [
            "deny op=send target=2 peer_pid=42 scope=session:1",
            "deny op=read target=1 peer_pid=? scope=restricted",
        ]
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    fs::write(&path, vec![b'x'; usize::try_from(AUDIT_MAX).unwrap()]).unwrap();
    assert!(!peer.authorize("send", B, &r, &audit));
    assert_eq!(
        fs::metadata(dir.0.join("audit.log.1")).unwrap().len(),
        AUDIT_MAX
    );
    assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1);
}

/// `sh -c 'sh -c "sleep 5 | nc -U <sock>"; :'` — nc가 바깥 sh의 손자로 소켓에 붙는다(`; :`가 exec 최적화를 막는다).
fn nested_nc(sock: &Path) -> Child {
    nested_nc_as(sock, Path::new("/usr/bin/nc"))
}

/// `nested_nc`의 nc 경로 지정판. ad-hoc 서명한 복사본으로 피어 서명을 바꾼다.
fn nested_nc_as(sock: &Path, nc: &Path) -> Child {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "/bin/sh -c 'sleep 5 | {} -U {}'; :",
            nc.display(),
            sock.display()
        ))
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// S13(MK-036 완료 조건): 세션 A 안에서 B로 보내는 요청은 거부되고 감사 로그에 남는다. 외부 셸은 B를 다룬다.
/// 실제 프로세스 계보와 실제 `LOCAL_PEERPID`·`proc_pidinfo`를 쓴다.
#[test]
fn real_processes_inside_vs_outside_session() {
    let dir = Dir::new("real");
    let sock = dir.0.join("s");
    let lis = UnixListener::bind(&sock).unwrap();
    let audit = Audit::new(dir.0.join("audit.log"));

    // 세션 A: 루트는 바깥 sh, 요청은 그 손자 nc에서 온다.
    let mut inside = nested_nc(&sock);
    let mut r = Roots::default();
    r.insert(A, inside.id()).unwrap();
    let (conn, _) = lis.accept().unwrap();
    let peer = Peer::of(&conn, &r);
    assert_eq!(peer.scope, Scope::Session(A));
    assert_ne!(
        peer.pid,
        Some(inside.id()),
        "peer is a descendant, not the root"
    );
    assert!(
        !peer.authorize("send", B, &r, &audit),
        "krill send B from A"
    );
    assert!(peer.authorize("send", A, &r, &audit), "own session");

    // 외부 셸: 같은 모양이지만 루트로 등록하지 않았다.
    let mut outside = nested_nc(&sock);
    let (conn2, _) = lis.accept().unwrap();
    let ext = Peer::of(&conn2, &r);
    assert_eq!(ext.scope, Scope::Full);
    assert!(ext.authorize("send", B, &r, &audit));

    // 이 테스트 프로세스 자신(외부)도 전체 권한.
    let _me = UnixStream::connect(&sock).unwrap();
    let (conn3, _) = lis.accept().unwrap();
    let me = Peer::of(&conn3, &r);
    assert_eq!((me.pid, me.scope), (Some(std::process::id()), Scope::Full));

    let log = fs::read_to_string(dir.0.join("audit.log")).unwrap();
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(log.contains(&format!(
        "deny op=send target=2 peer_pid={} scope=session:1",
        peer.pid.unwrap()
    )));

    drop((conn, conn2, conn3));
    for c in [&mut inside, &mut outside] {
        let pgid = Pid::from_raw(i32::try_from(c.id()).unwrap()).unwrap();
        kill_process_group(pgid, Signal::KILL).ok();
        c.wait().unwrap();
    }
}

#[test]
fn prune_dead_drops_gone_children_and_reparents_grandchildren() {
    const D: SessionId = SessionId(4);
    const E: SessionId = SessionId(5);
    let dir = std::env::temp_dir().join(format!("mk612-prune-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session-creators");
    let mut r = Roots::open(Some(path.clone()));
    r.record_creator(B, Scope::Session(A)).unwrap();
    r.record_creator(C, Scope::Session(B)).unwrap();
    r.record_creator(D, Scope::Session(A)).unwrap();
    // B는 죽었고 A·C·D·E만 살아 있다: B 항목은 버리고 C는 A 밑으로 이어 준다.
    r.record_creator(E, Scope::Session(D)).unwrap();
    r.prune_dead(&[A, C, D, E]);
    assert!(!Scope::Session(A).allows(B, &r), "dead child must be gone");
    assert!(
        Scope::Session(A).allows(C, &r),
        "grandchild reparented to A"
    );
    assert!(Scope::Session(A).allows(D, &r));
    assert!(Scope::Session(D).allows(E, &r));
    let back = Roots::open(Some(path));
    assert!(!Scope::Session(A).allows(B, &back), "prune must persist");
    assert!(Scope::Session(A).allows(C, &back));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn creator_ancestry_and_reparenting_survive_restart_with_private_file() {
    let dir = std::env::temp_dir().join(format!("mk595-creators-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session-creators");
    let mut r = Roots::open(Some(path.clone()));
    r.record_creator(B, Scope::Session(A)).unwrap();
    r.record_creator(C, Scope::Session(B)).unwrap();
    let mut r = Roots::open(Some(path.clone()));
    assert!(Scope::Session(A).allows(C, &r));
    assert!(!Scope::Session(C).allows(A, &r));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    r.remove(B);
    let mut r = Roots::open(Some(path.clone()));
    assert!(Scope::Session(A).allows(C, &r));
    r.record_creator(C, Scope::Full).unwrap();
    assert!(!Scope::Session(A).allows(C, &Roots::open(Some(path))));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_creator_save_does_not_grant_unpersisted_scope() {
    let dir = std::env::temp_dir().join(format!("mk595-creators-fail-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session-creators");
    let mut r = Roots::open(Some(path.clone()));
    fs::create_dir(path.with_extension("tmp")).unwrap();
    assert!(r.record_creator(B, Scope::Session(A)).is_err());
    assert!(!Scope::Session(A).allows(B, &r));
    fs::remove_dir(path.with_extension("tmp")).unwrap();
    r.record_creator(B, Scope::Session(A)).unwrap();
    assert!(Scope::Session(A).allows(B, &Roots::open(Some(path))));
    fs::remove_dir_all(dir).unwrap();
}

// ---- MK-096 피어 코드서명 검증 ----

fn valid_sign(id: &str) -> CodeSign {
    CodeSign {
        valid: true,
        identifier: Some(id.to_owned()),
        team: None,
    }
}

#[test]
fn signing_policy_matrix() {
    let off = SigningPolicy::default();
    assert!(!off.enforced(), "default is opt-in (off)");
    assert!(off.allows(None));
    assert!(off.allows(Some(&CodeSign::invalid())));
    assert!(off.allows(Some(&valid_sign("com.example.anything"))));

    let required = SigningPolicy {
        require_signed: true,
        ..Default::default()
    };
    assert!(required.enforced());
    assert!(!required.allows(None), "token lookup failure is unsigned");
    assert!(!required.allows(Some(&CodeSign::invalid())));
    assert!(required.allows(Some(&valid_sign("com.example.anything"))));

    let named = SigningPolicy {
        require_identifier: Some("com.example.app".to_owned()),
        ..Default::default()
    };
    assert!(named.enforced(), "identifier alone forces verification");
    assert!(named.allows(Some(&valid_sign("com.example.app"))));
    assert!(!named.allows(Some(&valid_sign("com.example.evil"))));
    assert!(
        !named.allows(Some(&CodeSign {
            valid: true,
            identifier: None,
            team: None,
        })),
        "valid flag without identifier is not enough"
    );

    let team = SigningPolicy {
        require_team: Some("TEAM123456".to_owned()),
        ..Default::default()
    };
    assert!(team.enforced());
    assert!(team.allows(Some(&CodeSign {
        valid: true,
        identifier: Some("com.example.app".to_owned()),
        team: Some("TEAM123456".to_owned()),
    })));
    assert!(!team.allows(Some(&CodeSign {
        valid: true,
        identifier: Some("com.example.app".to_owned()),
        team: Some("OTHER".to_owned()),
    })));
}

#[test]
fn signing_policy_load_defaults_and_file() {
    assert_eq!(SigningPolicy::load(None), SigningPolicy::default());
    let dir = Dir::new("signing-load");
    assert_eq!(
        SigningPolicy::load(Some(&dir.0)),
        SigningPolicy::default(),
        "missing file is off"
    );
    fs::write(dir.0.join(SIGNING_POLICY_FILE), "require_signed = true\n").unwrap();
    assert!(SigningPolicy::load(Some(&dir.0)).enforced());
    fs::write(
        dir.0.join(SIGNING_POLICY_FILE),
        "require_identifier = \"com.example.app\"\nrequire_team = \"TEAM123456\"\n",
    )
    .unwrap();
    let policy = SigningPolicy::load(Some(&dir.0));
    assert_eq!(
        policy.require_identifier.as_deref(),
        Some("com.example.app")
    );
    assert_eq!(policy.require_team.as_deref(), Some("TEAM123456"));
    fs::write(dir.0.join(SIGNING_POLICY_FILE), "require_signed = tru").unwrap();
    let bad = SigningPolicy::load(Some(&dir.0));
    assert_eq!(
        bad,
        SigningPolicy::deny_all(),
        "malformed file fails closed"
    );
    assert!(!bad.allows(Some(&valid_sign("com.example.app"))));
    fs::write(dir.0.join(SIGNING_POLICY_FILE), "require_sign = true\n").unwrap();
    assert_eq!(
        SigningPolicy::load(Some(&dir.0)),
        SigningPolicy::deny_all(),
        "unknown field typo fails closed"
    );
    fs::write(
        dir.0.join(SIGNING_POLICY_FILE),
        "require_identifier = \"\"\n",
    )
    .unwrap();
    assert!(
        !SigningPolicy::load(Some(&dir.0)).enforced(),
        "empty identifier is unset"
    );
}

#[test]
fn gate_signing_downgrades_only_unverified_full() {
    let dir = Dir::new("signing-gate");
    let audit = Audit::new(dir.0.join("audit.log"));
    let required = SigningPolicy {
        require_signed: true,
        ..Default::default()
    };
    let mut denied = Peer {
        pid: Some(7),
        scope: Scope::Full,
    };
    assert!(denied.gate_signing(&required, None, &audit));
    assert_eq!(denied.scope, Scope::Restricted);
    assert!(!denied.authorize("send", B, &Roots::default(), &audit));

    let mut allowed = Peer {
        pid: Some(7),
        scope: Scope::Full,
    };
    assert!(!allowed.gate_signing(&required, Some(&valid_sign("x")), &audit));
    assert_eq!(allowed.scope, Scope::Full);

    let mut session = Peer {
        pid: Some(7),
        scope: Scope::Session(A),
    };
    assert!(
        !session.gate_signing(&required, None, &audit),
        "session peers keep their scope"
    );
    assert_eq!(session.scope, Scope::Session(A));

    let mut off = Peer {
        pid: Some(7),
        scope: Scope::Full,
    };
    assert!(
        !off.gate_signing(&SigningPolicy::default(), None, &audit),
        "policy off keeps existing behavior"
    );

    let log = fs::read_to_string(dir.0.join("audit.log")).unwrap();
    assert_eq!(log.lines().count(), 2, "{log}");
    assert!(
        log.contains("deny op=peer-sign target=0 peer_pid=7 scope=full"),
        "{log}"
    );
    assert!(
        log.contains("deny op=send target=2 peer_pid=7 scope=restricted"),
        "{log}"
    );
}

/// 없는 프로세스의 토큰·짧은 토큰은 미서명으로 본다(검증 실패 경로).
#[test]
fn bogus_token_is_invalid() {
    assert_eq!(sys::verify_audit_token(&[0u8; 32]), CodeSign::invalid());
    assert_eq!(sys::verify_audit_token(&[1u8; 7]), CodeSign::invalid());
}

/// ad-hoc 서명한 nc 복사본. `identifier`를 지정해 피어 서명을 바꾼다.
fn adhoc_nc(dir: &Path, tag: &str, identifier: &str) -> PathBuf {
    let dst = dir.join(format!("nc-{tag}"));
    fs::copy("/usr/bin/nc", &dst).unwrap();
    let out = Command::new("/usr/bin/codesign")
        .args(["-f", "-s", "-", "-i", identifier])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dst
}

/// 자식이 붙을 때까지 최대 15초 기다린다. 서명이 깨진 바이너리는 커널이 죽여 영원히 안 오므로 타임아웃으로
/// 실패한다.
fn accept_timeout(lis: &UnixListener) -> UnixStream {
    lis.set_nonblocking(true).unwrap();
    let start = std::time::Instant::now();
    loop {
        match lis.accept() {
            Ok((conn, _)) => {
                conn.set_nonblocking(false).ok();
                lis.set_nonblocking(false).ok();
                return conn;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                assert!(
                    start.elapsed() < std::time::Duration::from_secs(15),
                    "peer did not connect"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => panic!("accept: {e}"),
        }
    }
}

fn kill_group(child: &mut Child) {
    let pgid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    kill_process_group(pgid, Signal::KILL).ok();
    child.wait().unwrap();
}

/// MK-096 완료 조건(설정 켜짐): ad-hoc 서명 피어는 전체 권한을 유지한다.
#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "macOS 전용: 코드서명(codesign)")]
fn real_adhoc_peer_passes_when_signing_required() {
    let dir = Dir::new("sign-real-pass");
    let sock = dir.0.join("s");
    let lis = UnixListener::bind(&sock).unwrap();
    let audit = Audit::new(dir.0.join("audit.log"));
    let nc = adhoc_nc(&dir.0, "pass", "com.example.mk-test-pass");
    let mut child = nested_nc_as(&sock, &nc);
    let conn = accept_timeout(&lis);
    let mut peer = Peer::of(&conn, &Roots::default());
    assert_eq!(peer.scope, Scope::Full);
    let token = sys::peer_token(conn.as_fd()).unwrap();
    let sign = sys::verify_audit_token(&token);
    assert!(sign.valid, "ad-hoc peer must verify");
    assert_eq!(sign.identifier.as_deref(), Some("com.example.mk-test-pass"));
    let policy = SigningPolicy {
        require_signed: true,
        ..Default::default()
    };
    assert!(!peer.gate_signing(&policy, Some(&sign), &audit));
    assert_eq!(peer.scope, Scope::Full);
    assert!(peer.authorize("send", B, &Roots::default(), &audit));
    drop(conn);
    kill_group(&mut child);
}

/// MK-096 완료 조건(설정 켜짐): 요구 식별자와 다른 서명의 피어는 전체 권한을 잃는다.
#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "macOS 전용: 코드서명(codesign)")]
fn real_identifier_mismatch_is_denied() {
    let dir = Dir::new("sign-real-mismatch");
    let sock = dir.0.join("s");
    let lis = UnixListener::bind(&sock).unwrap();
    let audit = Audit::new(dir.0.join("audit.log"));
    let nc = adhoc_nc(&dir.0, "evil", "com.example.mk-test-evil");
    let mut child = nested_nc_as(&sock, &nc);
    let conn = accept_timeout(&lis);
    let mut peer = Peer::of(&conn, &Roots::default());
    assert_eq!(peer.scope, Scope::Full);
    let token = sys::peer_token(conn.as_fd()).unwrap();
    let sign = sys::verify_audit_token(&token);
    assert_eq!(sign.identifier.as_deref(), Some("com.example.mk-test-evil"));
    let policy = SigningPolicy {
        require_identifier: Some("com.example.minikrill".to_owned()),
        ..Default::default()
    };
    assert!(peer.gate_signing(&policy, Some(&sign), &audit));
    assert_eq!(peer.scope, Scope::Restricted);
    assert!(!peer.authorize("send", B, &Roots::default(), &audit));
    let log = fs::read_to_string(dir.0.join("audit.log")).unwrap();
    assert!(log.contains("deny op=peer-sign target=0"), "{log}");
    drop(conn);
    kill_group(&mut child);
}

/// MK-096(설정 꺼짐): 서명 여부와 무관하게 기존처럼 전체 권한이다.
#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "macOS 전용: 코드서명(codesign)")]
fn real_policy_off_keeps_full() {
    let dir = Dir::new("sign-real-off");
    let sock = dir.0.join("s");
    let lis = UnixListener::bind(&sock).unwrap();
    let audit = Audit::new(dir.0.join("audit.log"));
    let nc = adhoc_nc(&dir.0, "off", "com.example.mk-test-off");
    let mut child = nested_nc_as(&sock, &nc);
    let conn = accept_timeout(&lis);
    let mut peer = Peer::of(&conn, &Roots::default());
    assert_eq!(peer.scope, Scope::Full);
    assert!(
        !peer.gate_signing(&SigningPolicy::default(), None, &audit),
        "policy off keeps existing behavior"
    );
    assert!(peer.authorize("send", B, &Roots::default(), &audit));
    drop(conn);
    kill_group(&mut child);
}
