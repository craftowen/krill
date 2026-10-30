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

#[cfg(any(windows, test))]
mod locale;
#[cfg(windows)]
pub(crate) use locale::preferred_locale;

pub mod computer;
#[cfg_attr(
    windows,
    expect(
        unsafe_code,
        reason = "Handle-relative Win32 filesystem boundary (CONVENTIONS U1)"
    )
)]
pub(crate) mod explorer_transfer;
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

/// Windows 단일 실행 표지 이름. 같은 상태 폴더(대소문자·구분자·끝 구분자 무시)끼리만 같다.
pub fn instance_key(state_dir: Option<&std::path::Path>) -> String {
    use sha2::Digest as _;
    let path = state_dir
        .map(krill_proto::path::identity)
        .unwrap_or_default();
    let digest = sha2::Sha256::digest(path.as_os_str().as_encoded_bytes());
    let head = u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ]);
    format!("Krill-App-{head:016x}")
}

/// Background CLI work must never allocate a Windows console. Piped stdout/stderr
/// remain available, so discovery and diagnostics keep their existing contracts.
pub fn background_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    krill_proto::os::background_command(program)
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn instance_key_is_scoped_to_the_state_dir() {
        let key = super::instance_key(Some(Path::new(r"C:\Users\a\AppData\Local\Krill")));
        assert!(key.starts_with("Krill-App-") && key.len() == "Krill-App-".len() + 16);
        assert_eq!(
            key,
            super::instance_key(Some(Path::new("c:/users/a/appdata/local/krill/")))
        );
        assert_ne!(
            key,
            super::instance_key(Some(Path::new(r"C:\Users\a\AppData\Local\Krill-test")))
        );
        assert_ne!(key, super::instance_key(None));
    }
}
