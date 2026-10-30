//! Real Windows batch trampoline argv/exit regression, independent of agent launch.
//!
//! Requires an installed native Rust toolchain: `RUSTC` or `rustc` on PATH. The
//! collector uses `std::env::args_os`, matching native/Node CRT argv semantics.
//! `WScript` has its own parser and cannot assert this contract.
#![cfg(windows)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt as _;
use std::process::Command;

#[test]
fn custom_batch_trampoline_preserves_cmd_metacharacters_and_exit_code() {
    let root = std::env::temp_dir().join(format!("krill-shell 한글 O'wen-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let collector = root.join("capture.rs");
    std::fs::write(&collector, include_str!("support/windows_argv.rs")).unwrap();
    let compiled = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .arg("--edition=2024")
        .arg("--crate-name=krill_argv_collector")
        .arg(&collector)
        .arg("-o")
        .arg(root.join("capture.exe"))
        .output()
        .expect("Windows shell regression requires installed native rustc");
    assert!(compiled.status.success(), "{compiled:?}");
    let script = root.join("custom-shell.CmD");
    std::fs::write(
        &script,
        "@echo off\r\n\"%~dp0capture.exe\" %*\r\nexit /b %errorlevel%\r\n",
    )
    .unwrap();
    let args = [
        "space 한글 🦀",
        "it's \"quoted\"",
        "& echo INJECTED>injected-marker",
        "100%PATH%!",
        "trailing\\",
        "",
    ];
    let output = Command::new(env!("CARGO_BIN_EXE_krilld"))
        .current_dir(&root)
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
                .encode_wide()
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
    assert!(root.join("collector-ran").is_file());
    assert!(!root.join("injected-marker").exists());
    std::fs::remove_file(root.join("collector-ran")).unwrap();
    for arg in ["bad\narg", "bad\rarg"] {
        let output = Command::new(env!("CARGO_BIN_EXE_krilld"))
            .current_dir(&root)
            .arg("--shell-exec")
            .arg(&script)
            .arg(arg)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert!(!root.join("collector-ran").exists());
        assert!(!root.join("injected-marker").exists());
    }
    std::fs::remove_dir_all(root).unwrap();
}
