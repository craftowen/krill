//! 2026-10-28 사건 회귀: `cargo test -p krilld --test upgrade`(nextest 아님)가 띄운 holder와, holder가 다시 띄운
//! krilld가 운영 로그(`~/Library/Logs/Krill`)에 썼다. `cargo test`의 표식(`CARGO_BIN_EXE_<이름>`)만 있어도 holder는
//! 임시 로그 폴더에 쓰고, 재기동한 krilld 세대마다 같은 표식을 물려받는다.

use std::os::fd::AsFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use krill_holder::link::Link;
use krill_proto::holder::{FromHolder, ToHolder};

const HOLDER: &str = env!("CARGO_BIN_EXE_krill-holder");

#[test]
fn restarted_krilld_keeps_the_test_marker_and_logs_stay_out_of_home() {
    let dir = env::temp_dir().join(format!("krill-holder-test-logs-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    // 가짜 krilld: 물려받은 테스트 표식 개수만 남기고(값·다른 환경은 쓰지 않는다) 곧바로 죽어 holder가 다시 띄우게 한다.
    let mut holder = Command::new(HOLDER)
        .args([
            "/bin/sh",
            "-c",
            r#"n=$(/usr/bin/env | /usr/bin/grep -c '^CARGO_BIN_EXE_'); echo "$n" > "$MKH_TEST_LOGS/gen-$$"; exit 1"#,
        ])
        .env_remove("NEXTEST")
        .env_remove("KRILL_LOG_DIR")
        .env("CARGO_BIN_EXE_krill-holder", HOLDER)
        .env("MKH_TEST_LOGS", &dir)
        .env("HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let generations = |dir: &Path| -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| {
                let path = e.ok()?.path();
                let name = path.file_name()?.to_str()?;
                name.starts_with("gen-")
                    .then(|| fs::read_to_string(&path).ok())
                    .flatten()
                    .filter(|s| s.ends_with('\n'))
            })
            .collect()
    };
    let end = Instant::now() + Duration::from_secs(30);
    while generations(&dir).len() < 2 && Instant::now() < end {
        thread::sleep(Duration::from_millis(20));
    }
    holder.kill().ok();
    holder.wait().ok();
    let seen = generations(&dir);
    let wrote_home = home.join("Library/Logs/Krill").exists();
    let holder_log = fs::read_to_string(env::temp_dir().join("krill-test-logs/krill-holder.log"))
        .unwrap_or_default();
    fs::remove_dir_all(&dir).ok();
    assert!(seen.len() >= 2, "krilld was not restarted");
    for count in &seen {
        assert_ne!(count.trim(), "0", "restarted krilld lost the test marker");
    }
    assert!(!wrote_home, "holder wrote the production log directory");
    let pid = format!(" pid={} ", holder.id());
    assert!(
        holder_log.lines().any(|l| l.contains(&pid)),
        "holder did not log to the test directory"
    );
}

#[test]
fn role_restart_log_krilld() {
    if env::var("MKH_LOG_ROLE").as_deref() != Ok("krilld") {
        return;
    }
    let dir = PathBuf::from(env::var("MKH_LOG_STATE_DIR").unwrap());
    let stdin = std::io::stdin().as_fd().try_clone_to_owned().unwrap();
    let mut link = Link::new(stdin);
    loop {
        let p = link.recv().unwrap().unwrap();
        if matches!(FromHolder::decode(p.frame()).unwrap(), FromHolder::Ready) {
            break;
        }
    }
    if !dir.join("gen1").exists() {
        fs::write(dir.join("gen1"), b"1").unwrap();
        let exe = env::current_exe().unwrap();
        let m = ToHolder::UpgradeKrilld {
            req: 1,
            path: exe.as_os_str().as_bytes().to_vec(),
        };
        link.send(&m.encode().unwrap(), &[]).unwrap();
        loop {
            let p = link.recv().unwrap().unwrap();
            if matches!(
                FromHolder::decode(p.frame()).unwrap(),
                FromHolder::Done { req: 1 }
            ) {
                break;
            }
        }
        return;
    }
    if !dir.join("gen2").exists() {
        fs::write(dir.join("gen2"), b"2").unwrap();
        #[expect(clippy::exit, reason = "simulate an unplanned krilld crash exit")]
        std::process::exit(1);
    }
    fs::write(dir.join("gen3"), b"3").unwrap();
    thread::sleep(Duration::from_secs(30));
}

#[test]
fn handover_restart_logs_info_while_crash_restart_logs_warn() {
    let dir = env::temp_dir().join(format!("krill-holder-restart-log-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let log_dir = dir.join("logs");
    fs::create_dir_all(&log_dir).unwrap();
    let exe = env::current_exe().unwrap();

    let mut holder = Command::new(HOLDER)
        .arg(&exe)
        .args([
            "--exact",
            "test_logs::role_restart_log_krilld",
            "--nocapture",
        ])
        .env("KRILL_LOG_DIR", &log_dir)
        .env("MKH_LOG_ROLE", "krilld")
        .env("MKH_LOG_STATE_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let end = Instant::now() + Duration::from_secs(30);
    while !dir.join("gen3").exists() && Instant::now() < end {
        thread::sleep(Duration::from_millis(20));
    }
    let spawned_gen3 = dir.join("gen3").exists();
    holder.kill().ok();
    holder.wait().ok();

    let log = fs::read_to_string(log_dir.join("krill-holder.log")).unwrap_or_default();
    fs::remove_dir_all(&dir).ok();

    assert!(
        spawned_gen3,
        "holder did not advance through handover and crash restarts: {log}"
    );
    let scheduled: Vec<_> = log
        .lines()
        .filter(|l| l.contains("daemon restart scheduled"))
        .collect();
    assert!(
        scheduled
            .iter()
            .any(|l| l.contains(" INFO ") && l.contains("streak=0") && l.contains("give_up=false")),
        "planned handover restart must log at INFO: {scheduled:?}"
    );
    assert!(
        scheduled
            .iter()
            .any(|l| l.contains(" WARN ") && l.contains("streak=1") && l.contains("give_up=false")),
        "unplanned crash restart must log at WARN: {scheduled:?}"
    );
    assert!(
        !scheduled
            .iter()
            .any(|l| l.contains(" WARN ") && l.contains("streak=0")),
        "planned handover restart must not log WARN: {scheduled:?}"
    );
}
