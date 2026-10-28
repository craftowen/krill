//! Real Windows batch trampoline argv/exit regression, independent of agent launch.
#![cfg(windows)]

use std::ffi::OsStr;
use std::process::Command;

#[test]
fn custom_batch_trampoline_preserves_cmd_metacharacters_and_exit_code() {
    let root = std::env::temp_dir().join(format!("krill-shell 한글 O'wen-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let script = root.join("custom-shell.CmD");
    std::fs::write(&script, "@echo off\r\n\"%SystemRoot%\\System32\\cscript.exe\" //nologo \"%~dp0capture.js\" %*\r\nexit /b %errorlevel%\r\n").unwrap();
    std::fs::write(root.join("capture.js"), r"
function hex(s) { var a=[]; for (var i=0; i<s.length; i++) a.push(s.charCodeAt(i).toString(16)); return a.join(','); }
for (var i=0; i<WScript.Arguments.length; i++) WScript.Echo(hex(WScript.Arguments.Item(i)));
WScript.Quit(23);
").unwrap();
    let args = [
        "space 한글",
        "it's \"quoted\"",
        "& echo INJECTED",
        "100%PATH%!",
        "trailing\\",
        "",
    ];
    let output = Command::new(env!("CARGO_BIN_EXE_krilld"))
        .arg("--shell-exec")
        .arg(&script)
        .args(args)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(23), "{output:?}");
    let expected: Vec<_> = args
        .iter()
        .map(|arg| {
            OsStr::new(arg)
                .to_str()
                .unwrap()
                .encode_utf16()
                .map(|ch| format!("{ch:x}"))
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect();
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        expected
    );
    std::fs::remove_dir_all(root).unwrap();
}
