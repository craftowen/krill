//! 2026-10-28 사건 회귀: `cargo test -p krilld --test upgrade`(nextest 아님)가 띄운 holder와, holder가 다시 띄운
//! krilld가 운영 로그(`~/Library/Logs/Krill`)에 썼다. `cargo test`의 표식(`CARGO_BIN_EXE_<이름>`)만 있어도 holder는
//! 임시 로그 폴더에 쓰고, 재기동한 krilld 세대마다 같은 표식을 물려받는다.

#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

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
