//! `std::os::unix`의 `fs`·`net`·`ffi` 대역(M11 Windows 포팅). 하위 크레이트는 import 경로만 바꾼다.
//!
//! - Unix: `std::os::unix` 재수출이라 동작이 바이트 단위로 같다.
//! - Windows: 쓰기 측 `mode`(`OpenOptionsExt`·`DirBuilderExt`)는 no-op이다. 비공개 권한은 `%LOCALAPPDATA%`
//!   ACL 상속에 맡긴다(추정, 명시 DACL은 후속 작업). `net`은 `AF_UNIX`(`uds_windows`), `ffi`는 OS 인코딩 바이트
//!   (`as_encoded_bytes`, 올바른 유니코드면 UTF-8과 같다).
//! - 넣지 않는 것: 읽기 측(실행 비트·소유자, `PermissionsExt`·`MetadataExt`), `Permissions::from_mode`(Windows는
//!   빈 `Permissions`를 만들 수 없다), `symlink`(파일·디렉터리 구분과 권한이 다르다). 호출처가 cfg로 분기한다.

/// Native user home. Windows providers use `USERPROFILE`, including when Git Bash sets a different `HOME`.
pub fn home_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
}

/// Background CLI work must not allocate a Windows console, including npm `.cmd` launchers.
pub fn background_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    configure_background(&mut command);
    command
}

/// Keep a configured command's arguments/environment while hiding its Windows console.
pub fn configure_background(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// [`std::fs::canonicalize`] without the Win32 verbatim prefix (`\\?\C:\x` → `C:\x`), so it agrees with
/// shell-reported cwds and stays readable on other hosts' sidebars. Identity wrapper on Unix.
pub fn canonicalize(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    std::fs::canonicalize(path).map(|path| shell_path(&path))
}

/// Use ordinary DOS/UNC paths for shell working directories and arguments. A Windows
/// verbatim prefix from canonicalization makes `cmd.exe` treat a local drive as UNC
/// and silently change to the Windows directory. Other device namespaces are preserved.
/// This only changes the representation and does not resolve or access the path.
pub fn shell_path(path: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        let ordinary = crate::daemon::executable_path(path);
        // Preserve names that only the verbatim namespace can represent, and
        // long paths that would otherwise acquire the Win32 MAX_PATH limit.
        if ordinary.as_os_str().encode_wide().count() >= 260
            || path.components().any(|part| {
                let name = match part {
                    std::path::Component::Normal(name) => name,
                    std::path::Component::CurDir | std::path::Component::ParentDir => return true,
                    _ => return false,
                };
                let text = name.to_string_lossy();
                let stem = text.split('.').next().unwrap_or_default();
                let upper = stem.to_ascii_uppercase();
                text.ends_with(['.', ' '])
                    || text.contains(['/', '<', '>', ':', '"', '|', '?', '*'])
                    || text.chars().any(char::is_control)
                    || matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                    || ["COM", "LPT"].iter().any(|prefix| {
                        upper.strip_prefix(prefix).is_some_and(|n| {
                            matches!(
                                n,
                                "1" | "2"
                                    | "3"
                                    | "4"
                                    | "5"
                                    | "6"
                                    | "7"
                                    | "8"
                                    | "9"
                                    | "¹"
                                    | "²"
                                    | "³"
                            )
                        })
                    })
            })
        {
            return path.to_owned();
        }
        ordinary
    }
    #[cfg(not(windows))]
    path.to_owned()
}

/// Append standard per-user Windows CLI install folders (`%USERPROFILE%\.local\bin`,
/// `%LOCALAPPDATA%\agy\bin`, `%APPDATA%\npm`, etc.) that exist on disk when Explorer's
/// inherited `PATH` omits them (e.g. CLIs installed via PowerShell `$PROFILE` or before relogin).
#[must_use]
pub fn augment_windows_path(path: Option<&std::ffi::OsStr>) -> Option<std::ffi::OsString> {
    let user_profile = std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from);
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| user_profile.as_ref().map(|h| h.join(r"AppData\Local")));
    let app_data = std::env::var_os("APPDATA")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| user_profile.as_ref().map(|h| h.join(r"AppData\Roaming")));
    augment_windows_path_with(
        path,
        user_profile.as_deref(),
        local_app_data.as_deref(),
        app_data.as_deref(),
    )
}

/// Testable implementation of [`augment_windows_path`] with explicit root directories.
#[must_use]
pub fn augment_windows_path_with(
    path: Option<&std::ffi::OsStr>,
    user_profile: Option<&std::path::Path>,
    local_app_data: Option<&std::path::Path>,
    app_data: Option<&std::path::Path>,
) -> Option<std::ffi::OsString> {
    let mut entries: Vec<std::path::PathBuf> = path
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    let mut candidates = Vec::new();
    if let Some(home) = user_profile {
        candidates.extend([
            home.join(".local").join("bin"),
            home.join(".opencode").join("bin"),
            home.join(".cargo").join("bin"),
            home.join("scoop").join("shims"),
        ]);
    }
    if let Some(local) = local_app_data {
        candidates.extend([
            local.join("agy").join("bin"),
            local.join("Programs").join("Antigravity").join("bin"),
            local.join("Programs").join("cursor-agent"),
            local.join("cursor-agent"),
            local.join("Microsoft").join("WinGet").join("Links"),
        ]);
    }
    if let Some(roaming) = app_data {
        candidates.push(roaming.join("npm"));
    }
    let norm = |p: &std::path::Path| {
        p.to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    for cand in candidates {
        if !cand.is_dir() {
            continue;
        }
        let cand_norm = norm(&cand);
        if !entries.iter().any(|existing| norm(existing) == cand_norm) {
            entries.push(cand);
        }
    }
    if entries.is_empty() {
        return None;
    }
    std::env::join_paths(entries).ok().or_else(|| {
        path.filter(|p| !p.is_empty())
            .map(std::ffi::OsStr::to_owned)
    })
}

#[cfg(unix)]
pub use std::os::unix::{ffi, fs, net};

/// 쓰기 측 권한 확장 트레이트(no-op).
#[cfg(windows)]
pub mod fs {
    /// [`std::os::unix::fs::OpenOptionsExt`] 대역.
    pub trait OpenOptionsExt {
        /// Windows에서는 무시한다.
        fn mode(&mut self, mode: u32) -> &mut Self;
    }

    impl OpenOptionsExt for std::fs::OpenOptions {
        fn mode(&mut self, _mode: u32) -> &mut Self {
            self
        }
    }

    /// [`std::os::unix::fs::DirBuilderExt`] 대역.
    pub trait DirBuilderExt {
        /// Windows에서는 무시한다.
        fn mode(&mut self, mode: u32) -> &mut Self;
    }

    impl DirBuilderExt for std::fs::DirBuilder {
        fn mode(&mut self, _mode: u32) -> &mut Self {
            self
        }
    }
}

/// Windows 10 1803+ `AF_UNIX` 소켓.
#[cfg(windows)]
pub mod net {
    pub use uds_windows::{SocketAddr, UnixListener, UnixStream};
}

/// 경로·문자열 바이트 변환.
#[cfg(windows)]
pub mod ffi {
    use std::ffi::{OsStr, OsString};

    /// [`std::os::unix::ffi::OsStrExt`] 대역.
    pub trait OsStrExt {
        /// OS 인코딩 바이트(WTF-8).
        fn as_bytes(&self) -> &[u8];
        /// UTF-8이 아니면 `U+FFFD` 한 글자(어떤 실제 경로와도 겹치지 않게).
        fn from_bytes(bytes: &[u8]) -> &Self;
    }

    // ponytail: 짝 없는 서로게이트(UTF-8 아님) 경로는 왕복하지 못한다. 필요하면 와이어에 WTF-8 검증을 넣고
    // `from_encoded_bytes_unchecked`(unsafe, FFI 모듈)로 옮긴다.
    impl OsStrExt for OsStr {
        fn as_bytes(&self) -> &[u8] {
            self.as_encoded_bytes()
        }

        fn from_bytes(bytes: &[u8]) -> &Self {
            OsStr::new(std::str::from_utf8(bytes).unwrap_or("\u{FFFD}"))
        }
    }

    /// [`std::os::unix::ffi::OsStringExt`] 대역.
    pub trait OsStringExt {
        /// OS 인코딩 바이트(WTF-8).
        fn into_vec(self) -> Vec<u8>;
        /// UTF-8이 아닌 부분은 `U+FFFD`로 바꾼다.
        fn from_vec(vec: Vec<u8>) -> Self;
    }

    impl OsStringExt for OsString {
        fn into_vec(self) -> Vec<u8> {
            self.into_encoded_bytes()
        }

        fn from_vec(vec: Vec<u8>) -> Self {
            match String::from_utf8(vec) {
                Ok(s) => s.into(),
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned().into(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn augment_windows_path_appends_existing_cli_dirs_without_duplicating() {
        let root = std::env::temp_dir().join(format!("krill-win-path-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let home = root.join("user");
        let local = root.join("local");
        let roaming = root.join("roaming");
        let claude_bin = home.join(".local/bin");
        let agy_bin = local.join("agy/bin");
        let npm_bin = roaming.join("npm");
        let base_bin = root.join("system32");
        for dir in [&claude_bin, &agy_bin, &npm_bin, &base_bin] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let initial = std::env::join_paths([&base_bin, &claude_bin]).unwrap();
        let augmented =
            augment_windows_path_with(Some(&initial), Some(&home), Some(&local), Some(&roaming))
                .unwrap();
        let split: Vec<_> = std::env::split_paths(&augmented).collect();
        assert_eq!(split.first(), Some(&base_bin));
        assert_eq!(split.iter().filter(|p| *p == &claude_bin).count(), 1);
        assert!(split.contains(&agy_bin));
        assert!(split.contains(&npm_bin));
        std::fs::remove_dir_all(root).ok();
    }
}
