//! Comparison keys for transcript paths. Original names remain untouched.

fn windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with("\\\\")
        || path.starts_with("//")
        || (bytes.first().is_some_and(u8::is_ascii_alphabetic)
            && bytes.get(1) == Some(&b':')
            && bytes.get(2).is_some_and(|b| matches!(b, b'/' | b'\\')))
}

/// Recognize POSIX, drive-rooted and UNC paths independently of the app's OS.
#[must_use]
pub fn is_absolute_history_path(path: &str) -> bool {
    path.starts_with('/') || windows_path(path)
}

/// Normalize Windows spelling only for syntactically Windows paths. POSIX case and
/// Unicode bytes remain distinct, including when the client runs on Windows.
#[must_use]
pub fn normalize_path_for_comparison(path: &str) -> String {
    let path = path.trim();
    let mut normalized = if windows_path(path) {
        let normalized = path.replace('\\', "/").to_lowercase();
        if let Some(rest) = normalized.strip_prefix("//?/unc/") {
            format!("//{rest}")
        } else {
            normalized
                .strip_prefix("//?/")
                .unwrap_or(&normalized)
                .to_owned()
        }
    } else {
        path.to_owned()
    };
    while normalized.len() > 1
        && normalized.ends_with('/')
        && !(normalized.len() == 3 && normalized.as_bytes().get(1) == Some(&b':'))
    {
        normalized.pop();
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt as _;
        for (alias, root) in [("/private/tmp", "/tmp"), ("/private/var", "/var")] {
            if let Some(rest) = normalized.strip_prefix(alias)
                && (rest.is_empty() || rest.starts_with('/'))
                && std::fs::metadata(alias)
                    .ok()
                    .zip(std::fs::metadata(root).ok())
                    .is_some_and(|(a, b)| a.dev() == b.dev() && a.ino() == b.ino())
            {
                return format!("{root}{rest}");
            }
        }
    }
    normalized
}

/// Match a root or a descendant at a directory boundary. On macOS the filesystem
/// itself resolves NFC/NFD and symlink aliases; this does not conflate distinct names
/// on case-sensitive remote hosts or volumes.
#[must_use]
pub fn path_inside_or_equal(candidate: &str, root: &str) -> bool {
    let candidate_key = normalize_path_for_comparison(candidate);
    let root_key = normalize_path_for_comparison(root);
    if root_key.is_empty() {
        return false;
    }
    if candidate_key == root_key
        || (root_key == "/" && candidate_key.starts_with('/'))
        || candidate_key
            .strip_prefix(root_key.as_str())
            .is_some_and(|s| root_key.ends_with('/') || s.starts_with('/'))
    {
        return true;
    }
    #[cfg(target_os = "macos")]
    if !windows_path(candidate) && !windows_path(root) {
        use std::os::unix::fs::MetadataExt as _;
        if let Ok(root_meta) = std::fs::metadata(root) {
            for ancestor in std::path::Path::new(candidate).ancestors() {
                if std::fs::metadata(ancestor).is_ok_and(|meta| {
                    meta.dev() == root_meta.dev() && meta.ino() == root_meta.ino()
                }) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_unc_and_posix_boundaries_preserve_identity() {
        for root in [r"C:\work\app", "c:/work/app/", r"\\?\C:\work\app"] {
            assert!(path_inside_or_equal("c:/WORK/app", root));
            assert!(path_inside_or_equal(r"C:\Work\app\sub", root));
            assert!(!path_inside_or_equal("c:/work/app-old", root));
        }
        assert!(path_inside_or_equal(
            r"\\?\UNC\server\share\app\sub",
            "//server/share/app"
        ));
        assert!(!path_inside_or_equal("/work/App", "/work/app"));
        assert!(!path_inside_or_equal("/work/가", "/work/\u{1100}\u{1161}"));
        assert!(path_inside_or_equal("/work/app", "/"));
        assert!(path_inside_or_equal("c:/work/app", r"C:\"));
        assert!(!path_inside_or_equal("d:/work/app", r"C:\"));
        assert!(!path_inside_or_equal("/work/app-old", "/work/app"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn existing_macos_unicode_names_use_filesystem_identity() {
        let dir = std::env::temp_dir().join(format!("krill-vault-unicode-{}", std::process::id()));
        let composed = dir.join("가/sub");
        let decomposed = dir.join("\u{1100}\u{1161}");
        std::fs::create_dir_all(&composed).unwrap();
        assert!(path_inside_or_equal(
            &composed.display().to_string(),
            &decomposed.display().to_string()
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
