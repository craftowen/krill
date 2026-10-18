//! MK-026: 실제 `krill-holder`·`krilld`(`krill` 옆)로 krilld를 SIGKILL한 뒤 앱과 같은 길(`watch_hello` 감시 연결 →
//! `wait_closed` → `reconnect_until`)로 끊김 감지 1초 안, 조작 없이 재연결 5초 안을 잰다.
//!
//! krilld가 연달아 죽어 holder가 백오프 중일 때 붙어도 holder를 하나 더 띄우지 않는지(OWE-491)도 본다.
//!
//! 재현: `cargo nextest run -p krill --test reconnect --no-capture` (측정값은 `mk026 reconnect:` 줄).

#![cfg_attr(
    unix,
    expect(
        clippy::unwrap_used,
        reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
    )
)]
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::daemon::{self, Connection, Paths, reconnect_until, wait_closed, watch_hello};
use krill_proto::ipc::{ClientHello, ClientRole};

const MK: &str = env!("CARGO_BIN_EXE_krill");
/// 반복 횟수. holder는 1초(STABLE) 넘게 산 krilld의 크래시를 첫 크래시로 보고 바로 다시 띄운다.
const ROUNDS: usize = 5;
const DETECT_MAX: Duration = Duration::from_secs(1);
const RECOVER_MAX: Duration = Duration::from_secs(5);

fn kill9(pid: u32) {
    Command::new("/bin/kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
}

fn ppid(pid: u32) -> u32 {
    let o = Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap().trim().parse().unwrap()
}

/// 테스트가 끝나면 holder와 마지막 krilld를 죽인다.
struct Cleanup {
    holder: u32,
    krilld: u32,
    dir: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        // holder 먼저: krilld가 먼저 죽으면 holder가 다시 띄운다.
        kill9(self.holder);
        kill9(self.krilld);
        fs::remove_dir_all(&self.dir).ok();
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1_000.0
}

/// `krill` 옆의 holder·krilld를 띄워 감시 연결로 붙는다.
fn launch(name: &str) -> (Paths, ClientHello, Connection, Cleanup) {
    let bin = Path::new(MK).parent().unwrap();
    for b in ["krill-holder", "krilld"] {
        assert!(
            bin.join(b).is_file(),
            "{b} is not built next to krill (run the whole workspace: cargo nextest run --workspace)"
        );
    }
    let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let paths = Paths {
        socket: dir.join("run/krilld.sock"),
        holder: bin.join("krill-holder"),
        krilld: bin.join("krilld"),
    };
    let hello = watch_hello(ClientRole::App, name);
    let conn = daemon::connect(&paths, &hello).unwrap();
    assert!(conn.launched);
    let krilld = u32::try_from(conn.server.instance_id).unwrap();
    let guard = Cleanup {
        holder: ppid(krilld),
        krilld,
        dir,
    };
    (paths, hello, conn, guard)
}

#[test]
fn krilld_sigkill_is_detected_and_recovered() {
    let (paths, hello, mut conn, mut guard) = launch("mk026");

    let (mut detect, mut recover) = (Vec::new(), Vec::new());
    for _ in 0..ROUNDS {
        // holder가 첫 크래시로 보도록 1초 넘게 산 뒤 죽인다.
        thread::sleep(Duration::from_millis(1_100));
        let mut stream = conn.stream;
        let watcher = thread::spawn(move || {
            wait_closed(&mut stream);
            Instant::now()
        });
        let t0 = Instant::now();
        kill9(guard.krilld);
        let closed = watcher.join().unwrap();
        // 앱은 EOF를 본 순간 "재연결 중"을 띄우고 재연결을 시작한다.
        conn = reconnect_until(&paths, &hello);
        let back = Instant::now();
        let new = u32::try_from(conn.server.instance_id).unwrap();
        assert_ne!(new, guard.krilld, "reconnected to the killed krilld");
        assert_eq!(ppid(new), guard.holder, "a second holder was launched");
        guard.krilld = new;
        detect.push(closed - t0);
        recover.push(back - t0);
    }
    let max = |v: &[Duration]| v.iter().copied().max().unwrap();
    println!(
        "mk026 reconnect: rounds {ROUNDS} detect ms {:?} (max {:.1}) recover ms {:?} (max {:.1})",
        detect.iter().map(|d| ms(*d).round()).collect::<Vec<_>>(),
        ms(max(&detect)),
        recover.iter().map(|d| ms(*d).round()).collect::<Vec<_>>(),
        ms(max(&recover)),
    );
    assert!(max(&detect) < DETECT_MAX, "detect {detect:?}");
    assert!(max(&recover) < RECOVER_MAX, "recover {recover:?}");
}

/// OWE-491: 1초(holder의 STABLE) 안에 죽이기를 되풀이해 재기동 지연을 0·250·500·1000ms로 늘린다. 소켓에 krilld가
/// 없는 동안 [`daemon::connect`]해도 holder를 새로 띄우지 않고 같은 holder가 다시 띄운 krilld에 붙어야 한다.
/// holder가 listen 소켓을 쥐기 전에는 `ConnectionRefused`라 둘째 holder가 떴다.
#[test]
fn crash_loop_does_not_launch_second_holder() {
    let (paths, hello, mut conn, mut guard) = launch("mk491");
    for _ in 0..4 {
        let mut stream = conn.stream;
        kill9(guard.krilld);
        wait_closed(&mut stream);
        conn = daemon::connect(&paths, &hello).unwrap();
        let new = u32::try_from(conn.server.instance_id).unwrap();
        assert!(!conn.launched, "a second holder was launched");
        assert_ne!(new, guard.krilld, "reconnected to the killed krilld");
        assert_eq!(ppid(new), guard.holder, "krilld of another holder");
        guard.krilld = new;
    }
}
