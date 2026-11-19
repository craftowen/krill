//! ppid 체인 판정([`walk`]). 가짜 트리라 모든 플랫폼(Windows 포함)에서 돈다.

use super::*;

const A: SessionId = SessionId(1);
const B: SessionId = SessionId(2);
/// 시작 시각을 못 읽는 프로세스(Windows의 다른 사용자·서비스 프로세스).
const DENIED: u64 = u64::MAX;

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
        match self.0.get(&pid) {
            Some(&(_, DENIED)) => Err(io::ErrorKind::PermissionDenied.into()),
            Some(&(_, start)) => Ok(start),
            None => Err(io::ErrorKind::NotFound.into()),
        }
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
        (70, 999, 0), // 부모가 이미 끝남: launchd에 입양된 고아와 같다
    ]);
    let r = roots(&[(A, 20, 200)]);
    assert_eq!(walk(40, &r, &info), Scope::Session(A), "grandchild");
    assert_eq!(walk(20, &r, &info), Scope::Session(A), "root itself");
    assert_eq!(walk(60, &r, &info), Scope::Full, "external shell");
    assert_eq!(walk(10, &r, &info), Scope::Full, "holder");
    assert_eq!(walk(70, &r, &info), Scope::Full, "parent exited");
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

/// E2E(v0.2.51, 재부팅한 Windows VM): wininit(548)의 죽은 부모 PID 472를 services(688) 아래 svchost가
/// 재사용했다. 서비스 프로세스는 표준 사용자가 열 수 없어 시작 시각 비교가 안 되고, 그대로 믿고 오르면
/// 472 → 688 → 548 → 472로 돌다 [`MAX_DEPTH`]를 넘겨 SSH의 `krill ls`가 막혔다.
#[test]
fn unreadable_service_parent_ends_the_chain() {
    let info = tree(&[
        (900, 800, 9_000),  // krill.exe
        (800, 700, 8_000),  // pwsh
        (700, 600, 7_000),  // sshd-session(사용자)
        (600, 688, DENIED), // sshd(SYSTEM)
        (688, 548, DENIED), // services
        (548, 472, DENIED), // wininit
        (472, 688, DENIED), // 472를 재사용한 svchost(NETWORK SERVICE)
    ]);
    assert_eq!(walk(900, &Roots::default(), &info), Scope::Full, "ssh");
    assert_eq!(walk(548, &Roots::default(), &info), Scope::Restricted);
}

/// 탐색기(30)의 부모 userinit(20)은 로그온 직후 끝나 PID가 재사용될 수 있다. 재사용한 프로세스가 세션 A의
/// 후손이어도 탐색기가 띄운 앱을 세션 A로 귀속하지 않는다.
#[test]
fn reused_parent_pid_is_not_followed() {
    let info = tree(&[
        (10, 1, 100),  // 세션 A 루트
        (20, 10, 500), // 죽은 userinit의 PID를 재사용한 세션 A의 자식
        (30, 20, 300), // explorer
        (40, 30, 400), // 탐색기에서 띄운 앱
    ]);
    let r = roots(&[(A, 10, 100)]);
    assert_eq!(walk(40, &r, &info), Scope::Full);
    assert_eq!(walk(20, &r, &info), Scope::Session(A), "the reuser itself");
}

/// 확인한 링크만 따르므로 접근 불가 조상은 세션 루트 위에 있어도 판정을 바꾸지 않는다.
#[test]
fn unreadable_ancestor_above_root_keeps_session() {
    let info = tree(&[
        (10, 4, DENIED), // 서비스
        (20, 10, 200),   // 세션 B 루트
        (30, 20, 300),
    ]);
    let r = roots(&[(B, 20, 200)]);
    assert_eq!(walk(30, &r, &info), Scope::Session(B));
}

#[test]
fn every_verified_peer_is_full() {
    // Orca와 같은 소유자 모델: 세션 안 피어도 외부 셸과 같은 권한, 계보를 못 확인한 피어만 막는다.
    assert!(Scope::Full.full());
    assert!(Scope::Session(A).full());
    assert!(!Scope::Restricted.full());
}

#[test]
fn remove_forgets_session() {
    let mut r = roots(&[(A, 20, 200), (B, 21, 210)]);
    r.remove(A);
    let info = tree(&[(20, 1, 200), (21, 1, 210)]);
    assert_eq!(walk(20, &r, &info), Scope::Full);
    assert_eq!(walk(21, &r, &info), Scope::Session(B));
}

/// 실제 프로세스: 이 테스트 프로세스는 어느 세션의 루트 아래도 아니므로 전체 권한이다. Windows VM에서는
/// SSH(sshd 서비스) 아래에서 돌아 위 E2E와 같은 경로를 탄다.
#[test]
fn this_process_is_full() {
    let mut chain = Vec::new();
    let mut pid = std::process::id();
    while pid > 1 && chain.len() < 16 {
        let ppid = sys::parent_pid(pid).unwrap_or(0);
        chain.push((
            pid,
            sys::process_name(pid).ok(),
            sys::start_us(pid).ok(),
            ppid,
        ));
        pid = ppid;
    }
    assert_eq!(
        walk(std::process::id(), &Roots::default(), &Sys),
        Scope::Full,
        "{chain:?}"
    );
}
