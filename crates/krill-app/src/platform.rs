//! gpui가 열지 않은 OS 기능. macOS는 `AppKit`·`IOKit`·`Carbon` 직접 호출, 그 밖은 같은 시그니처의 no-op이다.
#![cfg_attr(
    target_os = "macos",
    expect(
        unsafe_code,
        reason = "objc2 메시지 호출 및 IOKit IOPMAssertion·Carbon SecureEventInput C FFI(CONVENTIONS U1)"
    )
)]

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub mod menu_bar;
#[cfg(not(target_os = "macos"))]
mod windows;

pub mod permissions;

/// 실행 비트(`0o111`)가 하나라도 있는지. Windows는 실행 비트가 없어 늘 `true`다(파일 여부는 호출자가 본다).
pub fn has_exec_bits(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        true
    }
}

#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(not(target_os = "macos"))]
pub use windows::*;

/// Background CLI work must never allocate a Windows console. Piped stdout/stderr
/// remain available, so discovery and diagnostics keep their existing contracts.
pub fn background_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        let mut command = command;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        command
    }
    #[cfg(not(windows))]
    command
}

/// Refresh existing windows after a theme or attention-count change.
#[cfg(windows)]
pub fn sync_windows(cx: &mut gpui::App) {
    use gpui_component::ActiveTheme as _;
    let dark = cx.theme().mode == gpui_component::ThemeMode::Dark;
    for handle in cx.windows() {
        handle
            .update(cx, |_, window, _| sync_window(window, dark))
            .ok();
    }
}
