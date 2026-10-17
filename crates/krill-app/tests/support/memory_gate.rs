//! MK-066: 메모리 회귀 게이트. 실제 release 앱 + krilld + holder에 터미널 10개를 붙여 출력을 흘린 뒤
//! `phys_footprint` 합이 200MiB(ARCHITECTURE §출시 차단선)를 넘으면 실패한다. 세션 프로세스(에이전트 대역)는 예산 밖이다.
//! 병렬 부하에서 값이 흔들리므로 `scripts/memory-gate.sh`로만 돈다. gpui 잠정 결정 의존은 MK-031처럼 이 모듈에만 있다.
//! 재현·측정값은 docs/testing/MK-066.md.

use std::fmt::Write as _;
use std::fs;

use krill_proto::control::{Create, Reply, Request};

use super::release_survival::{Processes, launch_app, request, start_daemon, wait_for};
use super::*;

const TERMINALS: usize = 10;
/// 세션마다 내는 줄 수(약 125B/줄 → 약 2.5MB). 핫 스크롤백 1,000행을 다 채우고 콜드 저장소도 쓰게 한다.
const GEN_LINES: usize = 20_000;
const APP_LIMIT: u64 = 120 << 20;
const DAEMON_LIMIT: u64 = 60 << 20;
const LIMIT: u64 = 200 << 20;
/// MK-009처럼 출력이 끝난 뒤 이만큼 쉬고 잰다.
const IDLE: Duration = Duration::from_secs(10);

/// `footprint`의 "Footprint: N B". MK-009에서 빈 출력이 나온 적이 있어 세 번까지 다시 잰다.
fn footprint(pid: u32) -> u64 {
    let fp = (0..3).find_map(|_| {
        let out = Command::new("/usr/bin/footprint")
            .args(["-f", "bytes", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let bytes = text
            .split_once("Footprint: ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok());
        if bytes.is_none() {
            thread::sleep(Duration::from_millis(500));
        }
        bytes
    });
    assert!(fp.is_some(), "footprint failed for pid {pid}");
    fp.unwrap()
}

fn mib(bytes: u64) -> String {
    let kib = u32::try_from(bytes >> 10).unwrap();
    format!("{:.1}", f64::from(kib) / 1024.0)
}

fn measure(guard: &Processes) -> (u64, u64, u64, String) {
    let app = footprint(guard.apps[0].id());
    let krilld = footprint(guard.krilld.unwrap());
    let holder = footprint(guard.holder.id());
    let daemon = krilld + holder;
    let total = app + daemon;
    let mut line = String::new();
    for (name, fp) in [
        ("app", app),
        ("krilld", krilld),
        ("holder", holder),
        ("daemon", daemon),
    ] {
        write!(line, "{name} {} ", mib(fp)).unwrap();
    }
    (
        app,
        daemon,
        total,
        format!("{line}total {} MiB", mib(total)),
    )
}

#[test]
#[ignore = "actual macOS GUI; run via scripts/memory-gate.sh"]
#[expect(
    clippy::assertions_on_constants,
    reason = "debug 빌드 수치를 게이트 결과로 오인하지 않도록 release 프로필을 강제한다"
)]
fn release_ten_terminals_within_200mib() {
    assert!(!cfg!(debug_assertions), "requires the release profile");
    let dir = PathBuf::from(format!("/tmp/mk066-{}", std::process::id()));
    let (paths, mut guard, _) = start_daemon(dir);

    // 생성기는 go 파일을 기다렸다가(앱이 붙은 뒤) 한글·색·120열을 넘는 줄을 내고 done-<i>를 남긴 채 산다.
    let generator = guard.dir.join("generator.pl");
    fs::write(
        &generator,
        format!(
            "$|=1; while (!-e \"$ARGV[0]/go\") {{ select undef,undef,undef,0.02; }}\n\
             for $i (1..{GEN_LINES}) {{ printf \"%07d 한글 \\e[3%dm색\\e[0m %s\\r\\n\", $i, $i%8, 'x' x 110; }}\n\
             open $done, '>', \"$ARGV[0]/done-$ARGV[1]\" or die $!; close $done;\n\
             while (1) {{ select undef,undef,undef,1; }}\n"
        ),
    )
    .unwrap();
    let mut layout = Layout::default();
    for i in 0..TERMINALS {
        let script = format!(
            "stty raw -echo; exec /usr/bin/perl {} {} {i}",
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
                cols: 120,
                rows: 40,
            }),
        ) else {
            unreachable!("session creation failed");
        };
        guard.sessions.push(pid);
        if i > 0 {
            layout.new_tab();
        }
        layout.set_session(layout.visible()[0], session);
    }
    layout.activate(0);
    layout.save(&guard.dir.join("layout.bin")).unwrap();

    launch_app(&mut guard);
    fs::write(guard.dir.join("go"), []).unwrap();
    wait_for("generator output", || {
        (0..TERMINALS).all(|i| guard.dir.join(format!("done-{i}")).exists())
    });
    thread::sleep(IDLE);
    assert_eq!(guard.apps[0].try_wait().unwrap(), None, "GUI exited");

    let (app, daemon, total, line) = measure(&guard);
    println!(
        "MK066 {TERMINALS} terminals: {line} (limits: app {} / daemon {} / total {} MiB)",
        mib(APP_LIMIT),
        mib(DAEMON_LIMIT),
        mib(LIMIT)
    );
    assert!(
        app <= APP_LIMIT,
        "app memory gate: {line} (app > {} MiB)",
        mib(APP_LIMIT)
    );
    assert!(
        daemon <= DAEMON_LIMIT,
        "daemon memory gate: {line} (daemon > {} MiB)",
        mib(DAEMON_LIMIT)
    );
    assert!(total <= LIMIT, "memory gate: {line} > {} MiB", mib(LIMIT));
}
