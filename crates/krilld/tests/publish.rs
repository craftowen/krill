//! MK-022: krilld 바이너리 100개를 같은 소켓 경로로 동시에 띄우면 하나만 남고, 소켓 경로는 그 하나에 붙는다.
//! 살아남은 krilld를 SIGKILL한 뒤(죽은 소켓·남은 `.tmp`) 다시 100개를 띄워도 같다.

#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::fs;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::ipc::{self, ServerHello};

const N: usize = 100;

/// 띄운 krilld들. 테스트가 실패해도 남기지 않는다.
struct Kids(Vec<Child>);

impl Drop for Kids {
    fn drop(&mut self) {
        for k in &mut self.0 {
            k.kill().ok();
            k.wait().ok();
        }
    }
}

/// `N`개를 동시에 띄우고, 진 프로세스가 모두 0으로 끝난 뒤 남은 하나(만 담은 [`Kids`])를 돌려준다.
fn launch(sock: &Path) -> Kids {
    let mut kids = Kids(Vec::new());
    for _ in 0..N {
        let k = Command::new(env!("CARGO_BIN_EXE_krilld")).arg(sock).spawn();
        kids.0.push(k.unwrap());
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut losers = 0;
    while losers < N - 1 {
        assert!(
            Instant::now() < deadline,
            "only {losers} of {} exited",
            N - 1
        );
        let mut i = 0;
        while i < kids.0.len() {
            if let Some(st) = kids.0[i].try_wait().unwrap() {
                assert!(st.success(), "loser exit {st}");
                kids.0.swap_remove(i).wait().unwrap(); // try_wait가 이미 거둔 상태를 돌려준다.
                losers += 1;
            } else {
                i += 1;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    // 진 프로세스가 다 끝난 뒤에도 남은 하나가 계속 사는지 잠시 본다.
    thread::sleep(Duration::from_millis(200));
    assert!(kids.0[0].try_wait().unwrap().is_none(), "daemon exited");
    kids
}

/// 게시된 소켓이 살아남은 krilld 것인지 `ServerHello::instance_id`(pid)로 확인한다.
fn served_by(sock: &Path) -> u64 {
    let mut c = UnixStream::connect(sock).unwrap();
    let (kind, body) = ipc::read_frame(&mut c).unwrap().unwrap();
    ServerHello::decode(kind, &body).unwrap().instance_id
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

#[test]
fn hundred_concurrent_starts_leave_one_daemon() {
    let dir = std::env::temp_dir().join(format!("mk022-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let sock = dir.join("k.sock");

    let d = launch(&sock);
    assert_eq!(served_by(&sock), u64::from(d.0[0].id()));
    assert_eq!(names(&dir), ["k.sock", "k.sock.lock"]);

    drop(d); // SIGKILL: 죽은 소켓이 남는다. 여기에 bind와 rename 사이에 죽은 흔적(.tmp)을 더한다.
    fs::write(dir.join("k.sock.tmp"), b"").unwrap();
    let d = launch(&sock);
    assert_eq!(served_by(&sock), u64::from(d.0[0].id()));
    assert_eq!(names(&dir), ["k.sock", "k.sock.lock"]);

    drop(d);
    fs::remove_dir_all(&dir).ok();
}
