//! GUI 로그인 도메인의 임시 잡만 조작한다. 전역 LaunchAgents·설정 파일은 쓰지 않는다.

use super::*;

struct Job {
    dir: PathBuf,
    target: String,
}

impl Job {
    fn print(&self) -> String {
        let out = Command::new("/bin/launchctl")
            .args(["print", &self.target])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        put(&self.dir.join("stop"), "");
        // AbandonProcessGroup을 쓰므로 테스트 대역 krilld는 따로 정리한다.
        if let Some(s) = get(&self.dir.join("krilld")) {
            rustix::process::kill_process(pid(s.split(' ').next().unwrap()), Signal::KILL).ok();
        }
        Command::new("/bin/launchctl")
            .args(["bootout", &self.target])
            .output()
            .ok();
        fs::remove_dir_all(&self.dir).ok();
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 사용자 GUI 도메인이 없는 CI에서는 일반 `exec_handoff`만 실행한다.
/// 재현: `cargo test -p krill-holder --release --locked --test survive launchd_exec_handoff -- --ignored --nocapture`
#[test]
#[ignore = "로그인한 사용자 gui 도메인에 임시 launchd 잡을 만드는 수동 e2e"]
fn launchd_exec_handoff() {
    let dir = std::env::temp_dir().join(format!("mkh-launchd-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let uid = rustix::process::getuid().as_raw();
    let label = format!("dev.krill.test.handoff.{}", std::process::id());
    let job = Job {
        dir,
        target: format!("gui/{uid}/{label}"),
    };
    // release launchd 측정은 복사한 실행 파일의 최초 로드도 포함한다.
    fs::copy(HOLDER, job.dir.join("holder-next")).unwrap();
    let exe = std::env::current_exe().unwrap();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{holder}</string><string>{exe}</string>
<string>role_krilld</string><string>--exact</string><string>--nocapture</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><false/>
<key>AbandonProcessGroup</key><true/><key>ProcessType</key><string>Interactive</string>
<key>AssociatedBundleIdentifiers</key><array><string>dev.krill.test</string></array>
<key>EnvironmentVariables</key><dict>
<key>MKH_ROLE</key><string>krilld</string><key>MKH_DIR</key><string>{dir}</string>
<key>MKH_GENS</key><string>10</string><key>MKH_RATE</key><string>1000000</string>
<key>MKH_VERIFY</key><string>1</string><key>MKH_UPGRADES_RUN</key><string>10</string>
<key>MKH_EVERY_MS</key><string>100</string><key>MKH_STEP</key><string>1</string>
</dict><key>StandardOutPath</key><string>{dir}/stdout</string>
<key>StandardErrorPath</key><string>{dir}/stderr</string>
</dict></plist>"#,
        holder = xml(HOLDER),
        exe = xml(&exe.to_string_lossy()),
        dir = xml(&job.dir.to_string_lossy())
    );
    let path = job.dir.join("job.plist");
    fs::write(&path, plist).unwrap();
    let out = Command::new("/bin/launchctl")
        .args(["bootstrap", &format!("gui/{uid}")])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let krilld = wait_for("launchd krilld", 30, || get(&job.dir.join("krilld")));
    let fields: Vec<_> = krilld.split(' ').collect();
    let holder_pid: u32 = fields[3].parse().unwrap();
    let initial = job.print();
    assert!(initial.contains("state = running"), "{initial}");
    assert!(initial.contains("runs = 1"), "{initial}");
    for k in 0..10 {
        let p = wait_for("launchd handoff", 30, || {
            get(&job.dir.join(format!("handoff-{k}")))
        });
        assert_eq!(p.parse::<u32>().unwrap(), holder_pid);
        let status = job.print();
        for field in [
            "state = running".to_owned(),
            "runs = 1".to_owned(),
            format!("pid = {holder_pid}"),
        ] {
            assert!(status.lines().any(|l| l.trim() == field), "{status}");
        }
        // launchd의 ProgramArguments는 원래 경로지만 실행 이미지는 실제 교체돼 있어야 한다.
        let out = Command::new("/bin/ps")
            .args(["-p", &p, "-o", "comm="])
            .output()
            .unwrap();
        assert!(out.status.success());
        let image = String::from_utf8(out.stdout).unwrap();
        let want = if k % 2 == 0 {
            job.dir.join("holder-next")
        } else {
            HOLDER.into()
        };
        assert_eq!(image.trim(), want.to_str().unwrap());
        put(&job.dir.join(format!("ack-{k}")), "");
    }
    let preflight_ns: u64 = wait_for("preflight log", 30, || get(&job.dir.join("preflight")))
        .parse()
        .unwrap();
    let log = wait_for("handoff log", 30, || get(&job.dir.join("handoffs")));
    let mut times: Vec<_> = log
        .lines()
        .map(|l| l.split_once(' ').unwrap().0.parse::<u64>().unwrap())
        .collect();
    assert_eq!(times.len(), 10);
    println!(
        "launchd exec 10회: PID {holder_pid}, running/runs=1 유지, 사전 검증 {:.2}ms, 핸드오프 p50 {:.2}ms 최대 {:.2}ms",
        Duration::from_nanos(preflight_ns).as_secs_f64() * 1e3,
        pct(&mut times, 50),
        pct(&mut times, 100)
    );
    assert!(pct(&mut times, 100) <= 50.0);
    put(&job.dir.join("stop"), "");
    for id in 0..10 {
        let result = wait_for("출력 검증", 30, || {
            get(&job.dir.join(format!("verify-{id}")))
        });
        assert!(result.starts_with("ok "), "{id}: {result}");
        let st: i32 = wait_for("종료 상태", 30, || {
            get(&job.dir.join(format!("exit-{id}")))
        })
        .parse()
        .unwrap();
        assert!(libc::WIFEXITED(st) && libc::WEXITSTATUS(st) == 100 + id);
        println!("세션 {id}: {result}, exit {}", libc::WEXITSTATUS(st));
    }
    put(&job.dir.join("restore-listener"), "");
    rustix::process::kill_process(pid(fields[0]), Signal::KILL).unwrap();
    assert_eq!(
        wait_for("listener 복원", 30, || get(&job
            .dir
            .join("listener-restored"))),
        "ok"
    );
    println!("exec 10회 뒤 krilld 재기동: listener + master fd 10개 복원");
    let target = job.target.clone();
    let dir = job.dir.clone();
    drop(job);
    assert!(!dir.exists(), "임시 디렉터리 정리");
    assert!(
        !Command::new("/bin/launchctl")
            .args(["print", &target])
            .output()
            .unwrap()
            .status
            .success(),
        "임시 launchd 잡 정리"
    );
}
