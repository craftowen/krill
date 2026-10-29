//! Exercise the real Windows launcher with a local npm-style .cmd fixture; no provider is contacted.
#![cfg(windows)]

use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn batch_agent_preserves_arguments_selected_account_and_exit_status() {
    let root = std::env::temp_dir().join(format!("krill-agent 한글 O'wen-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    // A native Rust receiver follows the CRT argv rules used by node.exe. WSH's
    // cscript parser strips embedded quotes and does not model an npm launcher.
    std::fs::write(root.join("fake-agent.cmd"), "@echo off\r\n\"%KRILL_TEST_CAPTURE_EXE%\" --exact batch_argument_receiver --nocapture -- %*\r\nif not \"%errorlevel%\"==\"0\" exit /b %errorlevel%\r\nexit /b 23\r\n").unwrap();
    let args = [
        "space 한글",
        "it's \"quoted\"",
        "semi; $var & echo INJECTED",
        "trailing\\",
        "",
        "--settings",
        "C:\\한글 path\\settings.json",
        "--resume=conversation",
        "100%PATH%",
        "^caret !bang!",
        "(a) <b> |c",
    ];
    let account = root.join("selected account").into_os_string();
    let mut path = root.as_os_str().to_owned();
    path.push(";");
    path.push(std::env::var_os("PATH").unwrap());
    let output = Command::new(env!("CARGO_BIN_EXE_krilld"))
        .args(["--agent-exec", "fixture missing", "fake-agent"])
        .args(args)
        .env("PATH", path)
        .env("KRILL_TEST_CAPTURE_EXE", std::env::current_exe().unwrap())
        .env("KRILL_TEST_CAPTURE_FILE", root.join("captured.txt"))
        .env("KRILL_ACCOUNT_PROVIDER", "codex")
        .env("KRILL_ACCOUNT_DIR", &account)
        .env("codex_home", "wrong-account")
        .env("openai_api_key", "wrong-key")
        .env("MK_PANE_ID", "42")
        .env("KRILL_STATE_DIR", &root)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(23), "{output:?}");
    let mut want: Vec<_> = args
        .iter()
        .map(|arg| hex(std::ffi::OsStr::new(arg)))
        .collect();
    want.extend(
        [
            account,
            OsString::new(),
            OsString::new(),
            OsString::new(),
            "42".into(),
            root.as_os_str().to_owned(),
        ]
        .iter()
        .map(|s| hex(s)),
    );
    let text = std::fs::read_to_string(root.join("captured.txt")).unwrap();
    assert_eq!(text.lines().collect::<Vec<_>>(), want);
    std::fs::remove_dir_all(root).unwrap();
}

fn hex(s: &std::ffi::OsStr) -> String {
    s.encode_wide()
        .map(|ch| format!("{ch:x}"))
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn batch_argument_receiver() {
    let Some(path) = std::env::var_os("KRILL_TEST_CAPTURE_FILE") else {
        return;
    };
    let mut values: Vec<_> = std::env::args_os()
        .skip_while(|arg| arg != "--")
        .skip(1)
        .map(|arg| hex(&arg))
        .collect();
    values.extend(
        [
            "CODEX_HOME",
            "OPENAI_API_KEY",
            "KRILL_ACCOUNT_PROVIDER",
            "KRILL_ACCOUNT_DIR",
            "MK_PANE_ID",
            "KRILL_STATE_DIR",
        ]
        .map(|key| hex(&std::env::var_os(key).unwrap_or_default())),
    );
    std::fs::write(path, values.join("\n")).unwrap();
}

#[test]
fn missing_agent_prints_hint_and_does_not_start_a_daemon() {
    let output = Command::new(env!("CARGO_BIN_EXE_krilld"))
        .args([
            "--agent-exec",
            "fixture: install the missing agent",
            "krill-agent-does-not-exist-20261023",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(127));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("fixture: install the missing agent")
    );
}

#[test]
fn agent_launch_uses_native_helper_instead_of_powershell() {
    let profile = krill_proto::agent::Profile {
        name: "fixture".into(),
        launch: vec!["fake-agent".into()],
        yolo_args: vec!["--fixture-yolo".into()],
        install: "fixture install".into(),
        install_windows: String::new(),
        env: std::collections::BTreeMap::default(),
    };
    let launch = krilld::agent::launch(
        &profile,
        krill_proto::agent::Permission::Yolo,
        krill_proto::SessionId(42),
    )
    .unwrap();
    assert_eq!(launch.argv[1], krilld::agent::AGENT_THEN_SHELL);
    assert_eq!(
        launch.argv[3..],
        [b"fake-agent".as_slice(), b"--fixture-yolo"]
    );
    assert!(!launch.argv.iter().any(|arg| arg == b"-l" || arg == b"-c"));
    assert!(PathBuf::from(std::str::from_utf8(&launch.argv[0]).unwrap()).is_absolute());
}

/// Korean Windows consoles start at OEM code page 949. agy runs `powershell -NoProfile`
/// with piped stdout, which encodes with the console code page; CP949 bytes then poison
/// its conversation (`string_value contains invalid UTF-8`). The launcher must switch the
/// session console to UTF-8 before the agent starts.
#[test]
fn agent_console_keeps_noprofile_powershell_pipe_output_utf8_under_codepage_949() {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = std::env::temp_dir().join(format!("krill-agent-cp-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let script = root.join("emit.ps1");
    // ASCII source: the Korean text exists only inside PowerShell, as agy's tool output does.
    std::fs::write(
        &script,
        "Write-Output ([string][char]0xD55C + [char]0xAE00 + '-notion.md')\r\n",
    )
    .unwrap();
    // A fresh hidden console stands in for the session ConPTY at its Korean default.
    let output = Command::new("cmd.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .raw_arg(format!(
            "/d /s /c \"chcp 949 >nul && \"{}\" --agent-exec hint powershell -NoProfile -File \"{}\"\"",
            env!("CARGO_BIN_EXE_krilld"),
            script.display()
        ))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).as_deref(),
        Ok("한글-notion.md\r\n")
    );
    std::fs::remove_dir_all(root).ok();
}
