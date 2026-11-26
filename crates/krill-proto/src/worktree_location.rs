//! Project worktree destinations. Relative paths are anchored to the main checkout.

use std::io;
use std::path::{Component, Path, PathBuf};

fn is_dot_git_segment(s: &str) -> bool {
    let trimmed = s.trim_end_matches(['.', ' ']);
    trimmed.eq_ignore_ascii_case(".git") || trimmed.eq_ignore_ascii_case("git~1")
}

fn tilde_remainder(raw: &str) -> Option<&str> {
    if raw == "~" {
        Some("")
    } else if raw.starts_with("~/") || raw.starts_with("~\\") {
        raw.strip_prefix('~')
            .map(|rest| rest.trim_start_matches(['/', '\\']))
    } else {
        None
    }
}

fn has_root_or_drive_prefix(rel: &str) -> bool {
    matches!(rel.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic())
        || Path::new(rel)
            .components()
            .any(|c| matches!(c, Component::RootDir | Component::Prefix(_)))
}

pub fn normalize(raw: &str) -> io::Result<Option<String>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let bad_tilde =
        raw.starts_with('~') && tilde_remainder(raw).is_none_or(has_root_or_drive_prefix);
    if raw.len() > 4096
        || raw.chars().any(char::is_control)
        || bad_tilde
        || raw.split(['/', '\\']).any(is_dot_git_segment)
        || Path::new(raw).components().any(|part| {
            matches!(part, Component::Normal(s) if s.to_str().is_none_or(is_dot_git_segment))
        })
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
    let path = if let Some(rel) = tilde_remainder(&raw) {
        if has_root_or_drive_prefix(rel) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid worktree directory: use a path outside .git, optionally starting with ~/",
            ));
        }
        let home =
            home.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
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
            ("~//worktrees", Some("/home/me/worktrees")),
            (r"~\\worktrees", Some("/home/me/worktrees")),
            ("~///", Some("/home/me/")),
        ] {
            assert_eq!(
                resolve(raw, repo, home).unwrap(),
                expected.map(PathBuf::from)
            );
        }
        for raw in [
            "~someone/tasks",
            "~/D:/worktrees",
            "~//C:/worktrees",
            r"~\C:\worktrees",
            "~/c:worktrees",
            ".git/tasks",
            ".GIT/tasks",
            ".Git/tasks",
            "/repo/.git",
            "/repo/.GIT.",
            "GIT~1/tasks",
            r"~\.git",
            r"~\.git/tasks",
            r"~\.GIT.\tasks",
            r"~\GIT~1\tasks",
            r"foo\.git\bar",
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
