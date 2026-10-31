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

/// Whether `cwd` is an internal `claude-mem` observer/plugin directory that should be hidden
/// from user-facing session history.
#[must_use]
pub fn is_claude_mem_cwd(cwd: &str) -> bool {
    let mut prev = "";
    for seg in cwd.split(['/', '\\']).filter(|s| !s.is_empty()) {
        if seg.eq_ignore_ascii_case(".claude-mem")
            || seg.eq_ignore_ascii_case("observer-sessions")
            || (prev.eq_ignore_ascii_case("thedotmack") && seg.eq_ignore_ascii_case("claude-mem"))
        {
            return true;
        }
        prev = seg;
    }
    false
}

/// Whether a directory name under `<claude_config_dir>/projects/` is an encoded `claude-mem`
/// internal project directory (for example `-Users-macmini--claude-mem-observer-sessions`).
#[must_use]
pub fn is_claude_mem_project_dir(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".claude-mem"
        || lower.starts_with(".claude-mem-")
        || lower.contains("--claude-mem")
        || lower == "observer-sessions"
        || lower.ends_with("-observer-sessions")
        || lower.contains("-observer-sessions-")
}

/// Normalize separators and ASCII case for syntactically Windows paths. Unresolved
/// or foreign Unicode aliases (for example Σ/ς) deliberately remain distinct: only
/// the native filesystem can decide their identity. POSIX case and
/// Unicode bytes remain distinct, including when the client runs on Windows.
#[must_use]
pub fn normalize_path_for_comparison(path: &str) -> String {
    let mut normalized = if windows_path(path) {
        let normalized = path.replace('\\', "/").to_ascii_lowercase();
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
    #[cfg(windows)]
    if windows_path(candidate)
        && windows_path(root)
        && let Some(inside) = native_windows_scope(candidate, root)
    {
        return inside;
    }
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

// Existing directory identities override lexical case folding, including on Windows
// directories with case sensitivity enabled. Missing leaf names use their nearest
// existing ancestor. Errors opening the root leave conservative lexical fallback.
#[cfg(windows)]
fn native_windows_scope(candidate: &str, root: &str) -> Option<bool> {
    let root = same_file::Handle::from_path(root).ok()?;
    for ancestor in std::path::Path::new(candidate).ancestors() {
        if let Ok(resolved) = ancestor.canonicalize() {
            return Some(resolved.ancestors().any(|parent| {
                same_file::Handle::from_path(parent).is_ok_and(|handle| handle == root)
            }));
        }
    }
    None
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

    #[test]
    fn claude_mem_cwd_and_project_dir_classification() {
        assert!(is_claude_mem_cwd(
            "/Users/macmini/.claude-mem/observer-sessions"
        ));
        assert!(is_claude_mem_cwd(
            "/Users/macmini/.claude-mem/observer-sessions/1003"
        ));
        assert!(is_claude_mem_cwd(
            r"C:\Users\macmini\.claude-mem\observer-sessions"
        ));
        assert!(is_claude_mem_cwd("/custom/data/observer-sessions"));
        assert!(is_claude_mem_cwd(
            "/Users/macmini/.claude/plugins/cache/thedotmack/claude-mem/13.39.0"
        ));
        assert!(!is_claude_mem_cwd("/Users/macmini/Krill/worktrees/krill"));

        assert!(is_claude_mem_project_dir(
            "-Users-macmini--claude-mem-observer-sessions"
        ));
        assert!(is_claude_mem_project_dir("-custom-data-observer-sessions"));
        assert!(is_claude_mem_project_dir(
            "-custom-data-observer-sessions-42"
        ));
        assert!(!is_claude_mem_project_dir("-Users-macmini-Krill-worktrees"));
    }

    #[test]
    fn unresolved_windows_unicode_is_conservative_and_posix_bytes_stay_distinct() {
        assert!(!path_inside_or_equal(
            "C:/__krill_unresolved__/i\u{307}/child",
            "C:/__krill_unresolved__/İ"
        ));
        assert!(!path_inside_or_equal(
            "C:/__krill_unresolved__/ς/child",
            "C:/__krill_unresolved__/Σ"
        ));
        assert!(!path_inside_or_equal(
            "C:/__krill_unresolved__/é/child",
            "C:/__krill_unresolved__/e\u{301}"
        ));
        assert!(!path_inside_or_equal("/remote/Σ/child", "/remote/ς"));
        assert!(!path_inside_or_equal("/remote/name ", "/remote/name"));
    }

    #[cfg(windows)]
    #[test]
    fn existing_windows_unicode_and_case_sensitive_names_use_native_identity() {
        let dir = std::env::temp_dir().join(format!("krill-vault-native-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (a, b) in [
            ("Σ", "ς"),
            ("İ", "i\u{307}"),
            ("é", "e\u{301}"),
            ("Case", "case"),
        ] {
            let left = dir.join(a);
            let right = dir.join(b);
            std::fs::create_dir_all(left.join("child")).unwrap();
            std::fs::create_dir_all(&right).unwrap();
            let identical = same_file::is_same_file(&left, &right).unwrap();
            assert_eq!(
                path_inside_or_equal(
                    &left.join("child").display().to_string(),
                    &right.display().to_string()
                ),
                identical
            );
        }
        std::fs::create_dir_all(dir.join("Outside/sub")).unwrap();
        assert!(!path_inside_or_equal(
            &dir.join("Case/../Outside/sub").display().to_string(),
            &dir.join("Case").display().to_string()
        ));
        std::fs::remove_dir_all(dir).unwrap();
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
