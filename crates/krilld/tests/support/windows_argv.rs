//! Native argv collector for windows_shell.rs (CRT-compatible Rust argv parsing).
use std::os::windows::ffi::OsStrExt as _;

fn main() -> std::process::ExitCode {
    std::fs::write("collector-ran", b"ran").unwrap();
    for arg in std::env::args_os().skip(1) {
        println!(
            "{}",
            arg.encode_wide()
                .map(|unit| format!("{unit:x}"))
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    std::process::ExitCode::from(23)
}
