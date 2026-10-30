#![forbid(unsafe_code)]
//! MK-099 · MK-131: 원격 SSH 터널 코드는 `krill`와 `krill-app`이 공유할 수 있도록 `krill-hosts::ssh`로 이동했다.
pub use krill_hosts::ssh::*;

/// Validate remote absolute syntax without interpreting the path on this host.
pub(crate) fn is_remote_absolute_path(path: &[u8]) -> bool {
    !path.contains(&0)
        && (path.starts_with(b"/")
            || matches!(path, [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
            || path.strip_prefix(b"\\\\").is_some_and(|rest| {
                rest.split(|byte| *byte == b'\\')
                    .filter(|part| !part.is_empty())
                    .take(2)
                    .count()
                    == 2
            }))
}

/// Match a native host path at directory boundaries, including Windows separators.
pub(crate) fn is_path_within(path: &str, root: &str) -> bool {
    let windows = matches!(root.as_bytes(), [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
        || root.starts_with("\\\\");
    path == root
        || path.strip_prefix(root).is_some_and(|rest| {
            root.ends_with('/')
                || (windows && root.ends_with('\\'))
                || rest.starts_with('/')
                || (windows && rest.starts_with('\\'))
        })
}

#[cfg(test)]
mod path_tests {
    use super::is_remote_absolute_path;

    #[test]
    fn worktree_boundaries_keep_native_windows_descendants() {
        for (path, root) in [
            (r"C:\repo\child", r"C:\repo"),
            (r"\\server\share\repo\child", r"\\server\share\repo"),
            ("/repo/child", "/repo"),
            (r"C:\repo", r"C:\"),
            ("/repo", "/"),
        ] {
            assert!(super::is_path_within(path, root));
        }
        assert!(!super::is_path_within(r"C:\repo-other\child", r"C:\repo"));
        assert!(!super::is_path_within(r"/repo\literal", "/repo"));
    }

    #[test]
    fn absolute_paths_use_remote_platform_syntax() {
        for path in [
            "/srv/repo",
            "C:\\work\\repo",
            "c:/work/repo",
            "\\\\server\\share\\repo",
            "//server/share",
        ] {
            assert!(is_remote_absolute_path(path.as_bytes()), "{path}");
        }
        for path in ["", "repo", "C:repo", "\\repo", "\\\\server", "/repo\0bad"] {
            assert!(!is_remote_absolute_path(path.as_bytes()), "{path:?}");
        }
    }
}
