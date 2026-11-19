use super::*;
use std::fs;

#[test]
fn include_parser_ignores_patterns_and_limits_entries() {
    assert_eq!(
        include_entries("# comment\n.env\r\n\n!secret\n*.key\na?\n[a]\n{a,b}\n한글/x\n").unwrap(),
        [".env", "한글/x"]
    );
    assert!(include_entries(&".env\n".repeat(MAX_ENTRIES + 1)).is_err());
    assert!(include_entries(&"*.env\n".repeat(MAX_ENTRIES + 1)).is_err());
}

proptest::proptest! {
    #[test]
    fn manifest_parser_never_panics(s in ".{0,4096}") {
        drop(include_entries(&s));
    }
}

#[test]
#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), ignore = "macOS 전용: APFS clonefile")]
fn clone_and_fallback_preserve_independent_files_and_never_overwrite() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let dir = std::env::temp_dir().join(format!("mk-clone-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir(&dir).unwrap();
    let src = dir.join("src");
    let clone = dir.join("clone");
    let fallback = dir.join("fallback");
    fs::write(&src, vec![42; 4 * 1024 * 1024]).unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o600)).unwrap();
    // 성공 여부를 직접 확인한다. fallback이 clonefile 성공을 가리지 못하게 한다.
    crate::sys::clone_file(&src, &clone).unwrap();
    assert_ne!(
        fs::metadata(&src).unwrap().ino(),
        fs::metadata(&clone).unwrap().ino()
    );
    assert_eq!(fs::read(&src).unwrap(), fs::read(&clone).unwrap());
    copy_with(&src, &fallback, |_, _| {
        Err(io::Error::from_raw_os_error(libc::EXDEV))
    })
    .unwrap();
    assert_eq!(fs::read(&src).unwrap(), fs::read(&fallback).unwrap());
    assert_eq!(fs::metadata(&fallback).unwrap().mode() & 0o777, 0o600);
    fs::write(&clone, "changed").unwrap();
    assert_eq!(fs::metadata(&src).unwrap().len(), 4 * 1024 * 1024);
    assert_eq!(fs::read(&clone).unwrap(), b"changed");
    assert!(copy_file(&src, &clone).is_err());
    assert!(
        copy_with(&src, &clone, |_, _| Err(io::Error::from_raw_os_error(
            libc::EXDEV
        )))
        .is_err()
    );
    assert_eq!(fs::read(&clone).unwrap(), b"changed");
    let source_link = dir.join("source-link");
    std::os::unix::fs::symlink(&src, &source_link).unwrap();
    let refused = dir.join("refused");
    assert!(
        copy_with(&source_link, &refused, |_, _| Err(
            io::Error::from_raw_os_error(libc::EXDEV)
        ))
        .is_err()
    );
    assert!(!refused.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn discover_resolves_repo_linked_worktree_and_non_repo_without_spawning_git() {
    let root = std::env::temp_dir().join(format!("mk-compat-discover-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let non_repo = root.join("plain/folder");
    fs::create_dir_all(&non_repo).unwrap();
    assert_eq!(discover(&non_repo), Some(Discovered::NotRepo));

    let repo = root.join("main-repo");
    let dot_git = repo.join(".git");
    fs::create_dir_all(dot_git.join("worktrees/feat")).unwrap();
    fs::write(dot_git.join("HEAD"), "ref: refs/heads/main\r\n").unwrap();
    fs::write(dot_git.join("config"), "[core]\r\n\tbare = false\r\n").unwrap();
    let nested = repo.join("crates/app");
    fs::create_dir_all(&nested).unwrap();

    let canon_repo = krill_proto::os::canonicalize(&repo).unwrap();
    assert_eq!(
        discover(&nested),
        Some(Discovered::Repo {
            root: canon_repo.clone(),
            worktree: canon_repo.clone(),
        })
    );

    let linked = root.join("worktrees/feat");
    fs::create_dir_all(linked.join("src")).unwrap();
    let wt_gitdir = canon_repo.join(".git/worktrees/feat");
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}\r\n", wt_gitdir.display()),
    )
    .unwrap();
    fs::write(wt_gitdir.join("commondir"), "../..\r\n").unwrap();

    let canon_linked = krill_proto::os::canonicalize(&linked).unwrap();
    assert_eq!(
        discover(&linked.join("src")),
        Some(Discovered::Repo {
            root: canon_repo,
            worktree: canon_linked,
        })
    );

    fs::write(
        dot_git.join("config"),
        "[core]\r\n\tworktree = ../elsewhere\r\n",
    )
    .unwrap();
    assert_eq!(discover(&nested), None);

    fs::remove_dir_all(root).unwrap();
}
