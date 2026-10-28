//! Windows 명령 노출·native hook 계약, `%USERPROFILE%` 기준 hook·statusline 설치와 doctor 진단(OWE-835).
//! 모든 자식은 임시 `USERPROFILE`·상태 디렉터리만 쓴다.
#![cfg(windows)]
#![cfg_attr(
    windows,
    expect(clippy::unwrap_used, reason = "integration test helpers")
)]

use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use serde_json::{Value, json};

const KRILL: &str = env!("CARGO_BIN_EXE_krill");

fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("owe835-{}-{tag}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(dir.join(".claude")).unwrap();
    dir
}

/// 임시 홈에서만 `krill`을 돌린다(`HOME`·`CLAUDE_CONFIG_DIR`을 지워 `USERPROFILE` 경로를 확인).
fn krill(exe: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(exe)
        .args(args)
        .env("USERPROFILE", home)
        .env("KRILL_STATE_DIR", home.join("state"))
        .env_remove("HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// Claude Code가 hook·statusLine을 돌리는 Git Bash. 없으면 셸 실행 단계만 건너뛴다.
fn git_bash() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            let git = krill_proto::agent::which("git", &std::env::var_os("PATH")?)?;
            Some(git.parent()?.parent()?.join("bin").join("bash.exe"))
        })
        .filter(|p| p.is_file())
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

// PATH deliberately contains no Git, Bash or PowerShell. Exec-form hook invocations
// use an absolute executable path and inherited native pipes only.
fn hook_spawn(exe: &Path, home: &Path) -> Child {
    Command::new(exe)
        .args(["hook", "claude"])
        .env("USERPROFILE", home)
        .env("KRILL_STATE_DIR", home.join("state"))
        .env("MK_PANE_ID", "1")
        .env("PATH", "")
        .env_remove("CLAUDE_CODE_GIT_BASH_PATH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn hook_call(exe: &Path, home: &Path, payload: &[u8]) -> Output {
    let mut child = hook_spawn(exe, home);
    child.stdin.take().unwrap().write_all(payload).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn native_hook_waits_for_delayed_payload_and_eof() {
    let home = temp_home("delayed-input");
    let mut child = hook_spawn(Path::new(KRILL), &home);
    // Synchronize with the hook's initial response before delaying the writer:
    // process startup time cannot accidentally make the old 10ms budget pass.
    let mut response = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut response)
        .unwrap();
    assert_eq!(response.trim(), "{}");
    let mut input = child.stdin.take().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(25));
    input.write_all(br#"{"hook_event_name":"#).unwrap();
    input.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(25));
    input.write_all(br#""Stop"}"#).unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let events = krill_proto::hook::read_spool(&fs::read(home.join("state/hook.spool")).unwrap());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].input, krill_proto::hook::Input::Complete);
    assert_eq!(events[0].payload, br#"{"hook_event_name":"Stop"}"#);
    assert_eq!(events[0].ppid, std::process::id());
    fs::remove_dir_all(home).ok();
}

#[test]
fn native_hook_bounds_open_stdin_without_reporting_partial_input_complete() {
    for (tag, payload) in [
        ("open-empty", b"".as_slice()),
        ("open-partial", b"{".as_slice()),
    ] {
        let home = temp_home(tag);
        let mut child = hook_spawn(Path::new(KRILL), &home);
        let mut response = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut response)
            .unwrap();
        assert_eq!(response.trim(), "{}");
        let mut input = child.stdin.take().unwrap();
        input.write_all(payload).unwrap();
        input.flush().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let exited = child.try_wait().unwrap().is_some();
        if !exited {
            child.kill().unwrap();
        }
        let output = child.wait_with_output().unwrap();
        drop(input);
        assert!(exited, "hook waited for the writer to close stdin");
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let events =
            krill_proto::hook::read_spool(&fs::read(home.join("state/hook.spool")).unwrap());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input, krill_proto::hook::Input::TimedOut);
        assert!(events[0].payload.is_empty());
        fs::remove_dir_all(home).ok();
    }
}

#[test]
fn help_hides_computer_commands() {
    let output = Command::new(KRILL).arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(!help.contains("krill computer"));
}

#[test]
fn hook_outside_panes_preserves_agent_response_and_success() {
    for (agent, event, expected) in [
        ("claude", "PreToolUse", "{}"),
        ("agy", "PreToolUse", r#"{"decision":"ask"}"#),
        ("antigravity", "Stop", r#"{"decision":""}"#),
        ("agy", "SessionStart", "{}"),
    ] {
        let output = Command::new(KRILL)
            .args(["hook", agent, event])
            .env_remove("MK_PANE_ID")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn hook_uses_event_environment_and_argument_precedence() {
    for (args, expected) in [
        (vec!["hook", "agy"], r#"{"decision":"ask"}"#),
        (vec!["hook", "agy", "Stop"], r#"{"decision":""}"#),
    ] {
        let output = Command::new(KRILL)
            .args(args)
            .env("MK_HOOK_EVENT", "PreToolUse")
            .env_remove("MK_PANE_ID")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    }
}

#[test]
fn claude_hook_exec_form_delivers_without_git_bash_and_preserves_user_settings() {
    let home = temp_home("hook");
    let bin = home.join("Krill 한글 '$` & dir");
    fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("krill.exe");
    fs::copy(KRILL, &exe).unwrap();
    let settings = home.join(".claude").join("settings.json");
    fs::write(&settings, "{\"theme\":\"dark\"}\n").unwrap();

    let out = krill(&exe, &home, &["hook", "install", "claude", "--yes"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let installed = read_json(&settings);
    assert_eq!(installed["theme"], "dark");
    let handler = &installed["hooks"]["PreToolUse"][0]["hooks"][0];
    assert_eq!(Path::new(handler["command"].as_str().unwrap()), exe);
    assert_eq!(handler["args"], json!(["hook", "claude"]));
    assert!(handler.get("shell").is_none());
    let out = hook_call(
        &exe,
        &home,
        b"{\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"Read\"}",
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{}");
    assert!(out.stderr.is_empty(), "{out:?}");
    let events = krill_proto::hook::read_spool(&fs::read(home.join("state/hook.spool")).unwrap());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].agent, "claude");
    assert_eq!(events[0].ppid, std::process::id());
    assert_eq!(events[0].input, krill_proto::hook::Input::Complete);
    let payload: Value = serde_json::from_slice(&events[0].payload).unwrap();
    assert_eq!(payload["tool_name"], "Read");

    let out = krill(&exe, &home, &["hook", "uninstall", "claude", "--yes"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(read_json(&settings), json!({"theme": "dark"}));
    fs::remove_dir_all(&home).ok();
}

#[test]
fn statusline_install_chains_user_command_through_git_bash_and_records_usage() {
    let Some(bash) = git_bash() else {
        eprintln!("Git Bash not found; skipped statusline chain");
        return;
    };
    let home = temp_home("statusline");
    let settings = home.join(".claude").join("settings.json");
    let original = json!({"statusLine": {"type": "command", "command": "echo user-line"}});
    fs::write(&settings, original.to_string()).unwrap();
    let exe = PathBuf::from(KRILL);

    let out = krill(&exe, &home, &["statusline", "install", "--yes"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let command = read_json(&settings)["statusLine"]["command"]
        .as_str()
        .unwrap()
        .to_owned();

    let payload = json!({
        "session_id": "s1",
        "rate_limits": {
            "five_hour": {"used_percentage": 42, "resets_at": "2099-01-01T00:00:00Z"},
            "seven_day": {"used_percentage": 67, "resets_at": "2099-01-08T00:00:00Z"},
        },
    })
    .to_string();
    let mut child = Command::new(&bash)
        .arg("-c")
        .arg(&command)
        .env("USERPROFILE", &home)
        .env("CLAUDE_CODE_GIT_BASH_PATH", &bash)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove(krill_proto::statusline::DEPTH_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "user-line");
    let cache = read_json(&krill_proto::statusline::cache_path(&home.join(".claude")));
    assert_eq!(cache["rate_limits"]["five_hour"]["used_percentage"], 42);

    let out = krill(&exe, &home, &["statusline", "uninstall", "--yes"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(read_json(&settings), original);
    fs::remove_dir_all(&home).ok();
}

#[test]
fn codex_command_hook_preserves_windows_executable_metacharacters() {
    use std::os::windows::process::CommandExt as _;
    let home = temp_home("codex-hook-command");
    let dir = home.join("한글 O'wen %PATH% $cash & safe");
    fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("krill.exe");
    fs::copy(KRILL, &exe).unwrap();
    let command =
        krill_proto::agent::hooks::command(&exe, krill_proto::agent::hooks::Agent::Codex).unwrap();
    let out = Command::new("cmd.exe")
        .args(["/d", "/s", "/c"])
        .raw_arg(format!("\"{command}\""))
        .env_remove("MK_PANE_ID")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{}");
    assert!(out.stderr.is_empty(), "{out:?}");
    fs::remove_dir_all(home).ok();
}

#[test]
fn agent_list_and_doctor_find_npm_cli_before_path_refresh() {
    let home = temp_home("npm-path-refresh");
    let roaming = home.join("AppData/Roaming");
    let npm = roaming.join("npm");
    fs::create_dir_all(&npm).unwrap();
    fs::write(
        npm.join("claude.cmd"),
        b"@echo off\r\necho cli-path-fixture\r\n",
    )
    .unwrap();
    for args in [
        vec!["agent", "list"],
        vec!["doctor", "--json", "--no-start"],
    ] {
        let out = Command::new(KRILL)
            .args(&args)
            .env("USERPROFILE", &home)
            .env("APPDATA", &roaming)
            .env("LOCALAPPDATA", home.join("AppData/Local"))
            .env("KRILL_STATE_DIR", home.join("state"))
            .env("PATH", "")
            .env_remove("HOME")
            .output()
            .unwrap();
        if args[0] == "agent" {
            assert!(out.status.success());
            let text = String::from_utf8_lossy(&out.stdout);
            let row = text
                .lines()
                .find(|line| line.starts_with("claude "))
                .unwrap();
            assert!(row.contains("cli-path-fixture"), "{row}");
            assert!(row.contains("claude.cmd"), "{row}");
        } else {
            let report: Value = serde_json::from_slice(&out.stdout).unwrap();
            let found = report["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|check| check["id"] == "agent.any")
                .unwrap();
            assert_eq!(found["status"], "ok", "{found}");
        }
    }
    fs::remove_dir_all(home).ok();
}

#[test]
fn doctor_json_no_start_reports_missing_daemon_without_launching() {
    let home = temp_home("doctor");
    fs::create_dir_all(home.join("state").join("run")).unwrap();
    // 실행 가능한 헬퍼가 있어도 --no-start는 기동하지 않고 소켓만 확인한다.
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for name in ["krill.exe", "krill-holder.exe", "krilld.exe"] {
        fs::copy(KRILL, bin.join(name)).unwrap();
    }
    let out = krill(
        &bin.join("krill.exe"),
        &home,
        &["doctor", "--json", "--no-start"],
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    let check = |id: &str| {
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .cloned()
            .unwrap()
    };
    let state = check("state.writable");
    assert_eq!(state["status"], "ok", "{state}");
    assert!(
        state["detail"]
            .as_str()
            .unwrap()
            .contains("owner check skipped"),
        "{state}"
    );
    assert_eq!(check("bin.helpers")["status"], "ok");
    assert_eq!(check("daemon.connect")["status"], "fail");
    assert!(!out.status.success());
    assert!(!home.join("state/run/krilld.sock.launch").exists());
    assert!(fs::read_dir(home.join("state")).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".krill-doctor")
    }));
    fs::remove_dir_all(&home).ok();
}

#[test]
fn native_hook_sends_the_event_and_waits_for_ack_without_spooling() {
    use krill_proto::{hook, ipc, os::net::UnixListener};
    let home = temp_home("native-ack");
    fs::create_dir_all(home.join("state/run")).unwrap();
    let listener = UnixListener::bind(home.join("state/run/krilld.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => panic!("hook connection: {error}"),
            }
        };
        // Winsock accepted sockets inherit the listener's non-blocking mode.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        stream
            .write_all(
                &ipc::ServerHello {
                    proto_min: ipc::PROTO_MIN,
                    proto_max: ipc::PROTO_MAX,
                    features: hook::FEAT_HOOK,
                    build: "OWE-917-test".into(),
                    instance_id: 1,
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
        assert_eq!(
            ipc::ClientHello::decode(kind, &body).unwrap().role,
            ipc::ClientRole::Hook
        );
        let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
        let event = hook::HookEvent::decode(kind, &body).unwrap();
        stream
            .write_all(&ipc::encode_frame(hook::kind::ACK, &[]).unwrap())
            .unwrap();
        event
    });
    let output = hook_call(Path::new(KRILL), &home, br#"{"hook_event_name":"Stop"}"#);
    assert!(output.status.success(), "{output:?}");
    let event = server.join().unwrap();
    assert_eq!(event.ppid, std::process::id());
    assert_eq!(event.payload, br#"{"hook_event_name":"Stop"}"#);
    assert!(!home.join("state/hook.spool").exists());
    fs::remove_dir_all(home).ok();
}
