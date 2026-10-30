use super::{Agent, command};
use std::path::Path;
use std::process::{Command, Stdio};

fn invoke(
    executable: &Path,
    capture: &Path,
    pane: bool,
    exit_code: i32,
) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt as _;
    let hook = command(executable, Agent::Codex)
        .ok_or_else(|| std::io::Error::other("fixture executable path is not UTF-8"))?;
    let input = capture.with_extension("stdin.json");
    std::fs::write(
        &input,
        br#"{"hook_event_name":"Stop","session_id":"fixture"}"#,
    )?;
    let mut child = Command::new("cmd.exe");
    child.args(["/d", "/s", "/c"]);
    child.raw_arg(format!("\"{hook}\""));
    child.env_remove("MK_PANE_ID");
    if pane {
        child.env("MK_PANE_ID", "fixture-pane");
    }
    let child = child
        .env("KRILL_HOOK_CAPTURE", capture)
        .env("KRILL_HOOK_FIXTURE_EXIT", exit_code.to_string())
        .stdin(std::fs::File::open(input)?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.wait_with_output()
}

#[test]
fn default_windows_hook_shell_preserves_exe_stdin_guard_and_exit_status() {
    let root = std::env::temp_dir().join(format!(
        "krill hook '$ & %USERNAME% 한글-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("fixture.cs");
    std::fs::write(
        &source,
        r#"
using System;
using System.IO;
class Fixture {
    static int Main(string[] args) {
        if (args.Length != 2 || args[0] != "hook" || args[1] != "codex") return 91;
        using (var input = new MemoryStream()) {
            Console.OpenStandardInput().CopyTo(input);
            File.WriteAllBytes(Environment.GetEnvironmentVariable("KRILL_HOOK_CAPTURE"), input.ToArray());
        }
        return int.Parse(Environment.GetEnvironmentVariable("KRILL_HOOK_FIXTURE_EXIT"));
    }
}
"#,
    )
    .unwrap();
    let executable = root.join("krill.exe");
    // .NET Framework ships with Windows; executing compiled tests needs no Rust toolchain.
    let system_root = std::env::var_os("SystemRoot").unwrap();
    let compiler = ["Framework64", "Framework"]
        .into_iter()
        .map(|architecture| {
            Path::new(&system_root)
                .join("Microsoft.NET")
                .join(architecture)
                .join("v4.0.30319/csc.exe")
        })
        .find(|path| path.is_file())
        .expect("Windows .NET Framework C# compiler");
    let compiled = Command::new(compiler)
        .args(["/nologo", "/target:exe"])
        .arg(format!("/out:{}", executable.display()))
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    let capture = root.join("captured.json");
    let inactive = invoke(&executable, &capture, false, 0).unwrap();
    assert!(
        inactive.status.success(),
        "{}",
        String::from_utf8_lossy(&inactive.stderr)
    );
    assert_eq!(String::from_utf8(inactive.stdout).unwrap().trim(), "{}");
    assert!(!capture.exists());
    let active = invoke(&executable, &capture, true, 7).unwrap();
    assert_eq!(
        active.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&active.stderr)
    );
    assert_eq!(
        std::fs::read(&capture).unwrap(),
        br#"{"hook_event_name":"Stop","session_id":"fixture"}"#
    );
    std::fs::remove_file(&capture).unwrap();
    let missing = invoke(&root.join("missing.exe"), &capture, true, 0).unwrap();
    assert!(!missing.status.success());
    assert!(!capture.exists());
    std::fs::remove_dir_all(root).unwrap();
}
