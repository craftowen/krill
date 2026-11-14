//! Project worktree destinations. Relative paths are anchored to the main checkout.

use std::io;
use std::path::{Component, Path, PathBuf};

fn is_dot_git_segment(part: &std::ffi::OsStr) -> bool {
    part.to_str().is_none_or(|s| {
        let trimmed = s.trim_end_matches(['.', ' ']);
        trimmed.eq_ignore_ascii_case(".git") || trimmed.eq_ignore_ascii_case("git~1")
    })
}

pub fn normalize(raw: &str) -> io::Result<Option<String>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.len() > 4096
        || raw.chars().any(char::is_control)
        || (raw.starts_with('~') && raw != "~" && !raw.starts_with("~/") && !raw.starts_with("~\\"))
        || Path::new(raw)
            .components()
            .any(|part| matches!(part, Component::Normal(s) if is_dot_git_segment(s)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid worktree directory: use a path outside .git, optionally starting with ~/",
        ));
    }
    Ok(Some(raw.to_owned()))
}

pub fn resolve(raw: &str, repository: &Path, home: Option<&Path>) -> io::Result<Option<PathBuf>> {
    let Some(raw) = normalize(raw)? else {
        return Ok(None);
    };
    let path = if raw == "~" || raw.starts_with("~/") || raw.starts_with("~\\") {
        let home =
            home.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        let rel = raw
            .strip_prefix("~/")
            .or_else(|| raw.strip_prefix("~\\"))
            .filter(|_| raw != "~")
            .unwrap_or("");
        home.join(rel)
    } else {
        repository.join(raw)
    };
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_support_default_absolute_relative_and_home_paths() {
        let repo = Path::new("/projects/repo");
        let home = Some(Path::new("/home/me"));
        for (raw, expected) in [
            (" ", None),
            (".worktrees", Some("/projects/repo/.worktrees")),
            ("../repo-tasks", Some("/projects/repo/../repo-tasks")),
            ("/tasks", Some("/tasks")),
            (" ~/작업 폴더 ", Some("/home/me/작업 폴더")),
            ("~", Some("/home/me/")),
            (r"~\worktrees", Some("/home/me/worktrees")),
        ] {
            assert_eq!(
                resolve(raw, repo, home).unwrap(),
                expected.map(PathBuf::from)
            );
        }
        for raw in [
            "~someone/tasks",
            ".git/tasks",
            ".GIT/tasks",
            ".Git/tasks",
            "/repo/.git",
            "/repo/.GIT.",
            "GIT~1/tasks",
            "bad\0path",
            "bad\npath",
        ] {
            assert!(resolve(raw, repo, home).is_err(), "{raw:?}");
        }
        assert!(resolve("~/tasks", repo, None).is_err());
    }

    proptest::proptest! {
        #[test]
        fn path_input_never_panics(raw in ".{0,5000}") {
            resolve(&raw, Path::new("/repo"), Some(Path::new("/home"))).ok();
        }
    }
}
