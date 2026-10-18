//! Windows 명령 노출·미지원 hook 계약, `%USERPROFILE%` 기준 hook·statusline 설치와 doctor 진단(OWE-835).
//! 모든 자식은 임시 `USERPROFILE`·상태 디렉터리만 쓴다.
#![cfg(windows)]
#![cfg_attr(
    windows,
    expect(clippy::unwrap_used, reason = "integration test helpers")
)]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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

fn find_command(value: &Value, needle: &str) -> Option<String> {
    match value {
        Value::String(s) if s.contains(needle) => Some(s.clone()),
        Value::Array(items) => items.iter().find_map(|v| find_command(v, needle)),
        Value::Object(map) => map.values().find_map(|v| find_command(v, needle)),
        _ => None,
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn help_hides_computer_commands() {
    let output = Command::new(KRILL).arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(!help.contains("krill computer"));
}

#[test]
fn unsupported_hook_preserves_agent_response_and_success() {
    for (agent, event, expected) in [
        ("claude", "PreToolUse", "{}"),
        ("agy", "PreToolUse", r#"{"decision":"ask"}"#),
        ("antigravity", "Stop", r#"{"decision":""}"#),
        ("agy", "SessionStart", "{}"),
    ] {
        let output = Command::new(KRILL)
            .args(["hook", agent, event])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("not supported")
        );
    }
}

#[test]
fn unsupported_hook_uses_event_environment_and_argument_precedence() {
    for (args, expected) in [
        (vec!["hook", "agy"], r#"{"decision":"ask"}"#),
        (vec!["hook", "agy", "Stop"], r#"{"decision":""}"#),
    ] {
        let output = Command::new(KRILL)
            .args(args)
            .env("MK_HOOK_EVENT", "PreToolUse")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    }
}

#[test]
fn claude_hook_installs_under_userprofile_and_quotes_spaced_path_for_git_bash() {
    let home = temp_home("hook");
    let bin = home.join("Krill 한글 dir");
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
    let command = find_command(&installed, "hook claude").unwrap();
    assert!(
        command.contains(&format!("'{}'", exe.display())),
        "{command}"
    );

    if let Some(bash) = git_bash() {
        for pane in [None, Some("pane-1")] {
            let mut shell = Command::new(&bash);
            shell.arg("-c").arg(&command).env_remove("MK_PANE_ID");
            if let Some(pane) = pane {
                shell.env("MK_PANE_ID", pane);
            }
            let out = shell.output().unwrap();
            assert!(out.status.success(), "{pane:?}: {out:?}");
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{}");
            // 판 안에서는 공백·한글 경로의 krill.exe까지 실제로 닿아야 한다(미지원 hook 경고).
            assert_eq!(
                String::from_utf8_lossy(&out.stderr).contains("not supported"),
                pane.is_some(),
                "{out:?}"
            );
        }
    } else {
        eprintln!("Git Bash not found; skipped hook command execution");
    }

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
fn doctor_json_probes_state_writes_and_skips_daemon() {
    let home = temp_home("doctor");
    fs::create_dir_all(home.join("state").join("run")).unwrap();
    let out = krill(Path::new(KRILL), &home, &["doctor", "--json", "--no-start"]);
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
    // 데몬 기동은 아직 Windows 미지원이라 고장(fail)이 아니라 건너뜀이다.
    assert_eq!(check("daemon.connect")["status"], "skip");
    assert!(fs::read_dir(home.join("state")).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".krill-doctor")
    }));
    fs::remove_dir_all(&home).ok();
}
