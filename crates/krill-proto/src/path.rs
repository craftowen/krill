//! Lexical identity for host paths, independent of the client's operating system.
//!
//! Windows drive-absolute and UNC paths compare without verbatim prefixes, ASCII
//! case or separator differences. Non-ASCII spelling is preserved: full Windows
//! Unicode case tables and per-directory case-sensitive flags require host IO.
//! Other paths retain native `Path` semantics, including Unix case sensitivity.

use std::path::{Path, PathBuf};

fn windows_text(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let text = text.replace('\\', "/");
    if text.starts_with("//./") {
        return None;
    }
    let text = if let Some(rest) = text.strip_prefix("//?/") {
        if let Some((_, share)) = rest
            .split_once('/')
            .filter(|(kind, _)| kind.eq_ignore_ascii_case("UNC"))
        {
            format!("//{share}")
        } else if drive_absolute(rest) {
            rest.to_owned()
        } else {
            return None; // Device namespaces must not alias ordinary files.
        }
    } else {
        text
    };
    let is_unc = text.strip_prefix("//").is_some_and(|rest| {
        let mut parts = rest.split('/').filter(|s| !s.is_empty());
        parts.next().is_some() && parts.next().is_some()
    });
    if !drive_absolute(&text) && !is_unc {
        return None;
    }
    let mut parts = text.split('/').filter(|s| !s.is_empty());
    let first = parts.next()?;
    let mut out = if is_unc {
        format!("\\\\{first}")
    } else {
        first.to_owned()
    };
    for part in parts {
        out.push('\\');
        out.push_str(part);
    }
    if drive_absolute(&text) && out.ends_with(':') {
        out.push('\\');
    }
    Some(out)
}

fn drive_absolute(text: &str) -> bool {
    matches!(text.as_bytes(), [drive, b':', b'/', ..] if drive.is_ascii_alphabetic())
}

/// Readable Windows spelling without verbatim prefixes; keeps case for display
/// and spawning. Other paths are returned unchanged. No filesystem IO occurs.
#[must_use]
pub fn normalize(path: &Path) -> PathBuf {
    windows_text(path).map_or_else(|| path.to_path_buf(), PathBuf::from)
}

/// Comparison key only. Keep the original path for display and spawning.
#[must_use]
pub fn identity(path: &Path) -> PathBuf {
    windows_text(path).map_or_else(
        || path.to_path_buf(),
        |s| PathBuf::from(s.to_ascii_lowercase()),
    )
}

/// Whether two host paths identify the same location lexically.
#[must_use]
pub fn equivalent(path: &Path, other: &Path) -> bool {
    identity(path) == identity(other)
}

/// Whether `path` equals or is nested under `root`, at a component boundary.
#[must_use]
pub fn within(path: &Path, root: &Path) -> bool {
    match (windows_text(path), windows_text(root)) {
        (Some(path), Some(root)) => {
            if path.split('\\').chain(root.split('\\')).any(|p| p == "..") {
                return false;
            }
            let path = path.to_ascii_lowercase();
            let root = root.to_ascii_lowercase();
            path == root
                || path
                    .strip_prefix(root.trim_end_matches('\\'))
                    .is_some_and(|rest| rest.starts_with('\\'))
        }
        (None, None) => path.starts_with(root),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_identity_is_host_independent_and_preserves_display_spelling() {
        for (a, b) in [
            (r"\\?\C:\Users\Owen\간단작업\", "c:/users/owen/간단작업"),
            (r"\\?\UNC\Server\Share\프로젝트", "//server/share/프로젝트/"),
            (r"C:\", "c:/"),
            (r"\\server\share\", r"\\SERVER\SHARE"),
        ] {
            assert!(equivalent(Path::new(a), Path::new(b)), "{a} / {b}");
            assert!(within(Path::new(a), Path::new(b)));
        }
        assert_eq!(
            normalize(Path::new(r"\\?\C:\Users\간단작업")),
            Path::new(r"C:\Users\간단작업")
        );
        assert_eq!(normalize(Path::new("C:/")), Path::new(r"C:\"));
        assert!(within(
            Path::new(r"\\?\C:\작업\child"),
            Path::new("c:/작업")
        ));
        assert!(within(Path::new("C:/child"), Path::new(r"c:\")));
        assert!(!within(
            Path::new("C:/project-other"),
            Path::new("c:/project")
        ));
        assert!(!within(Path::new("D:/project"), Path::new("c:/")));
        assert!(!within(
            Path::new(r"\\server\share-other"),
            Path::new(r"\\server\share")
        ));
    }

    #[test]
    fn unix_and_device_namespaces_do_not_gain_windows_semantics() {
        for path in [
            "/Repo/한글",
            r"relative\file",
            "C:relative",
            r"\\?\Volume{abc}\file",
            r"\\.\pipe\foo",
        ] {
            assert_eq!(identity(Path::new(path)), Path::new(path));
        }
        assert!(!equivalent(Path::new("/Repo"), Path::new("/repo")));
        assert!(!equivalent(Path::new("C:/Ä"), Path::new("C:/ä")));
        assert!(within(Path::new("/repo/한글"), Path::new("/repo")));
        assert!(within(Path::new("/repo"), Path::new("/")));
        assert!(!within(Path::new("/repository"), Path::new("/repo")));
    }
}
