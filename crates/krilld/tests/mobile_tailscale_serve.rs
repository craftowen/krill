#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.0.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

#[test]
fn foreground_serve_forces_cli_mode_without_terminal_environment()
-> Result<(), Box<dyn std::error::Error>> {
    let directory =
        std::env::temp_dir().join(format!("krill-tailscale-guard-{}", std::process::id()));
    fs::create_dir_all(&directory)?;
    let binary = directory.join("tailscale");
    let record = directory.join("child.json");
    fs::write(
        &binary,
        r"#!/usr/bin/env python3
import json,os,sys,time
with open(os.environ['KRILL_TEST_TAILSCALE_RECORD'],'w') as file:
    json.dump({'cli':os.environ.get('TAILSCALE_BE_CLI'),'args':sys.argv[1:],'pid':os.getpid(),'terminal':[k for k in ('TERM','SHLVL','TERM_PROGRAM','PS1') if k in os.environ]},file)
if os.environ.get('TAILSCALE_BE_CLI') != '1':
    print('The Tailscale GUI failed to start.')
    sys.exit(0)
while True: time.sleep(1)
",
    )?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
    let mut guard = Guard(
        Command::new(env!("CARGO_BIN_EXE_krilld"))
            .arg("--mobile-tailscale-serve-guard")
            .arg(&binary)
            .args(["18449", "58179"])
            .env("KRILL_TEST_TAILSCALE_RECORD", &record)
            .env("TAILSCALE_BE_CLI", "0")
            .env_remove("TERM")
            .env_remove("SHLVL")
            .env_remove("TERM_PROGRAM")
            .env_remove("PS1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let observation = loop {
        if let Ok(bytes) = fs::read(&record)
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
        {
            break value;
        }
        assert!(
            Instant::now() < deadline,
            "Serve child did not record its launch"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(observation["cli"], "1");
    assert_eq!(
        observation["args"],
        serde_json::json!(["serve", "--https=18449", "http://127.0.0.1:58179"])
    );
    assert_eq!(observation["terminal"], serde_json::json!([]));
    let pid = rustix::process::Pid::from_raw(i32::try_from(
        observation["pid"].as_u64().ok_or("missing child pid")?,
    )?)
    .ok_or("invalid child pid")?;
    assert!(
        guard.0.try_wait()?.is_none(),
        "Serve owner exited before EOF"
    );
    guard.0.stdin.take();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = guard.0.try_wait()? {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "Serve guard did not stop after EOF"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());
    assert!(
        rustix::process::test_kill_process(pid).is_err(),
        "Serve child survived owner EOF"
    );
    fs::remove_dir_all(directory)?;
    Ok(())
}
