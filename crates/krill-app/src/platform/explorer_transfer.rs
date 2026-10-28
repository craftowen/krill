//! Explorer transfers use pinned directories and never follow a source or destination link.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix::Directory;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::Directory;

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Explorer transfer refuses links, .git and non-regular entries",
    )
}

fn normal(name: &OsStr) -> bool {
    #[cfg(windows)]
    {
        let name = name.to_string_lossy();
        // Also exclude alternate data streams and Win32 name normalization aliases.
        !name.eq_ignore_ascii_case(".git") && !name.contains(':') && !name.ends_with(['.', ' '])
    }
    #[cfg(not(windows))]
    {
        !name.eq_ignore_ascii_case(".git")
    }
}

fn relative(path: &Path) -> io::Result<Vec<OsString>> {
    path.components()
        .map(|part| match part {
            Component::Normal(name) if normal(name) => Ok(name.to_os_string()),
            _ => Err(denied()),
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    BeforeOpen,
    Opened,
    Commit,
}

/// A failed import retains its source and reports any potentially partial destination.
pub(crate) fn transfer(
    root: &Path,
    source: &Path,
    directory: &Path,
    import: bool,
    budget: usize,
) -> io::Result<PathBuf> {
    run(root, source, directory, import, budget, &mut |_, _| {})
}

fn run(
    root: &Path,
    source: &Path,
    directory: &Path,
    import: bool,
    budget: usize,
    checkpoint: &mut impl FnMut(Stage, &Path),
) -> io::Result<PathBuf> {
    let target_parts = relative(directory.strip_prefix(root).map_err(|_| denied())?)?;
    let root_path = root.canonicalize()?;
    // A repository alias itself must not anchor transfers inside Git metadata.
    if root_path
        .components()
        .any(|c| matches!(c, Component::Normal(n) if !normal(n)))
    {
        return Err(denied());
    }
    let anchor = Directory::absolute(&root_path)?;
    let destination_parent = anchor.descend(&target_parts)?;
    let name = source
        .file_name()
        .filter(|n| normal(n))
        .ok_or_else(denied)?;
    let destination = directory.join(name);
    let source_parent = if import {
        Directory::absolute(source.parent().ok_or_else(denied)?)?
    } else {
        let parts = relative(source.strip_prefix(root).map_err(|_| denied())?)?;
        if parts.is_empty() {
            return Err(denied());
        }
        anchor.descend(&parts[..parts.len() - 1])?
    };
    checkpoint(Stage::BeforeOpen, source);
    let node = source_parent.open_node(name, !import)?;
    checkpoint(Stage::Opened, source);
    let meta = node.metadata()?;
    if !meta.is_file() && !meta.is_dir() {
        return Err(denied());
    }
    if meta.is_dir() && destination_parent.contains(&node, source)? {
        return Err(denied());
    }
    if !import && source == destination {
        return Ok(destination);
    }
    checkpoint(Stage::Commit, &destination);
    if import {
        let mut budget = budget;
        copy_node(
            (&source_parent, name),
            node,
            &destination_parent,
            &mut budget,
            0,
            checkpoint,
        )
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "import failed: {error}; partial destination may remain at {}",
                    destination.display()
                ),
            )
        })?;
    } else {
        // Atomic no-replace, with no copy/delete fallback on unsupported filesystems.
        Directory::move_new(&source_parent, name, &node, &destination_parent, name)?;
    }
    Ok(destination)
}

fn copy_node(
    source: (&Directory, &OsStr),
    mut node: File,
    target_parent: &Directory,
    budget: &mut usize,
    depth: usize,
    checkpoint: &mut impl FnMut(Stage, &Path),
) -> io::Result<()> {
    let (source_parent, name) = source;
    if depth > 64 {
        return Err(io::Error::other("Explorer import depth limit exceeded"));
    }
    *budget = budget
        .checked_sub(1)
        .ok_or_else(|| io::Error::other("Explorer import entry limit exceeded"))?;
    let meta = node.metadata()?;
    if meta.is_file() {
        let mut output = target_parent.create_file(name)?;
        io::copy(&mut node, &mut output)?;
    } else if meta.is_dir() {
        let source = source_parent.child(name, node);
        let target = target_parent.create_dir(name)?;
        for name in source.names()? {
            let name = name?;
            if !normal(&name) {
                return Err(denied());
            }
            let path = source.path.join(&name);
            checkpoint(Stage::BeforeOpen, &path);
            let node = source.open_node(&name, false)?;
            checkpoint(Stage::Opened, &path);
            copy_node(
                (&source, &name),
                node,
                &target,
                budget,
                depth + 1,
                checkpoint,
            )?;
        }
    } else {
        return Err(denied());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "krill-safe-transfer-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }
    #[cfg(unix)]
    fn link(target: &Path, name: &Path, _: bool) {
        std::os::unix::fs::symlink(target, name).unwrap();
    }
    #[cfg(windows)]
    fn link(target: &Path, name: &Path, dir: bool) {
        if dir {
            let status = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(name)
                .arg(target)
                .status()
                .unwrap();
            assert!(status.success());
        } else {
            std::os::windows::fs::symlink_file(target, name).unwrap();
        }
    }
    fn fixture() -> (Temp, PathBuf, PathBuf) {
        let temp = Temp::new();
        let root = temp.0.join("repo");
        let external = temp.0.join("external");
        fs::create_dir_all(root.join("dst")).unwrap();
        fs::create_dir_all(&external).unwrap();
        (temp, root, external)
    }
    #[test]
    fn atomic_move_rejects_concurrent_file_and_directory_collisions() {
        for directory in [false, true] {
            let (_temp, root, _) = fixture();
            let source = root.join("source");
            let dst = root.join("dst");
            if directory {
                fs::create_dir(&source).unwrap();
            } else {
                fs::write(&source, "source").unwrap();
            }
            let result = run(&root, &source, &dst, false, 100, &mut |stage, path| {
                if stage == Stage::Commit {
                    if directory {
                        fs::create_dir(path).unwrap();
                    } else {
                        fs::write(path, "concurrent").unwrap();
                    }
                }
            });
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
            assert!(source.exists());
            if directory {
                assert!(dst.join("source").is_dir());
            } else {
                assert_eq!(
                    fs::read_to_string(dst.join("source")).unwrap(),
                    "concurrent"
                );
                assert_eq!(fs::read_to_string(&source).unwrap(), "source");
            }
        }
    }
    #[test]
    fn file_and_directory_import_move_and_partial_errors_preserve_sources() {
        let (_temp, root, external) = fixture();
        let dst = root.join("dst");
        fs::write(external.join("file"), "file").unwrap();
        transfer(&root, &external.join("file"), &dst, true, 100).unwrap();
        assert_eq!(fs::read_to_string(dst.join("file")).unwrap(), "file");
        assert!(external.join("file").exists());
        transfer(&root, &dst.join("file"), &root, false, 100).unwrap();
        assert_eq!(fs::read_to_string(root.join("file")).unwrap(), "file");
        assert!(!dst.join("file").exists());
        fs::create_dir_all(external.join("tree/sub")).unwrap();
        fs::write(external.join("tree/sub/file"), "tree").unwrap();
        transfer(&root, &external.join("tree"), &dst, true, 100).unwrap();
        assert_eq!(
            fs::read_to_string(dst.join("tree/sub/file")).unwrap(),
            "tree"
        );
        transfer(&root, &dst.join("tree"), &root, false, 100).unwrap();
        assert!(root.join("tree/sub/file").exists());
        assert!(!dst.join("tree").exists());
        for import in [false, true] {
            assert!(
                transfer(
                    &root,
                    &root.join("tree"),
                    &root.join("tree/sub"),
                    import,
                    100
                )
                .is_err()
            );
        }
        let error = transfer(&root, &external.join("tree"), &root, true, 100).unwrap_err();
        assert!(error.to_string().contains("partial destination may remain"));
        assert!(external.join("tree/sub/file").exists());
        fs::create_dir(external.join("tree/.git")).unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        let error = transfer(
            &root,
            &external.join("tree"),
            &root.join("empty"),
            true,
            100,
        )
        .unwrap_err();
        assert!(error.to_string().contains("partial destination may remain"));
        assert!(external.join("tree/sub/file").exists());
    }
    #[test]
    fn git_alias_and_intermediate_source_links_are_rejected() {
        let (_temp, root, external) = fixture();
        fs::create_dir(root.join(".git")).unwrap();
        link(&root.join(".git"), &root.join("alias"), true);
        fs::write(root.join("source"), "source").unwrap();
        assert!(transfer(&root, &root.join("source"), &root.join("alias"), false, 100).is_err());
        assert!(root.join("source").exists());
        assert!(!root.join(".git/source").exists());
        fs::create_dir(external.join("real")).unwrap();
        fs::write(external.join("real/file"), "secret").unwrap();
        link(&external.join("real"), &external.join("alias"), true);
        assert!(
            transfer(
                &root,
                &external.join("alias/file"),
                &root.join("dst"),
                true,
                100
            )
            .is_err()
        );
        assert!(!root.join("dst/file").exists());
    }
    #[test]
    fn source_file_and_directory_swap_before_open_never_follow_links() {
        for directory in [false, true] {
            let (_temp, root, external) = fixture();
            let source = external.join("source");
            let secret = external.join("secret");
            if directory {
                fs::create_dir(&source).unwrap();
                fs::create_dir(&secret).unwrap();
                fs::write(secret.join("secret"), "secret").unwrap();
            } else {
                fs::write(&source, "original").unwrap();
                fs::write(&secret, "secret").unwrap();
            }
            let result = run(
                &root,
                &source,
                &root.join("dst"),
                true,
                100,
                &mut |stage, path| {
                    if stage == Stage::BeforeOpen && path == source {
                        fs::rename(&source, external.join("original")).unwrap();
                        link(&secret, &source, directory);
                    }
                },
            );
            assert!(result.is_err());
            assert!(external.join("original").exists());
            assert!(!root.join("dst/source").exists());
        }
    }
    #[test]
    fn recursive_source_swap_reports_partial_destination_and_never_reads_secret() {
        let (_temp, root, external) = fixture();
        let source = external.join("source");
        fs::create_dir_all(source.join("child")).unwrap();
        fs::write(source.join("child/file"), "original").unwrap();
        fs::create_dir(external.join("secret")).unwrap();
        fs::write(external.join("secret/file"), "secret").unwrap();
        let result = run(
            &root,
            &source,
            &root.join("dst"),
            true,
            100,
            &mut |stage, path| {
                if stage == Stage::BeforeOpen && path == source.join("child") {
                    fs::rename(path, source.join("original")).unwrap();
                    link(&external.join("secret"), path, true);
                }
            },
        );
        let error = result.unwrap_err();
        assert!(error.to_string().contains("partial destination may remain"));
        assert!(source.join("original/file").exists());
        assert!(!root.join("dst/source/child/file").exists());
    }
    #[test]
    fn pinned_source_and_destination_resist_swaps_after_open() {
        for directory in [false, true] {
            let (_temp, root, external) = fixture();
            let source = external.join("source");
            let secret = external.join("secret");
            if directory {
                fs::create_dir(&source).unwrap();
                fs::create_dir(&secret).unwrap();
                fs::write(source.join("file"), "original").unwrap();
                fs::write(secret.join("file"), "secret").unwrap();
            } else {
                fs::write(&source, "original").unwrap();
                fs::write(&secret, "secret").unwrap();
            }
            run(
                &root,
                &source,
                &root.join("dst"),
                true,
                100,
                &mut |stage, path| {
                    if stage == Stage::Opened && path == source {
                        let swapped = fs::rename(&source, external.join("original"));
                        #[cfg(windows)]
                        assert!(
                            swapped.is_err(),
                            "open source must deny rename/delete sharing"
                        );
                        #[cfg(unix)]
                        {
                            swapped.unwrap();
                            link(&secret, &source, directory);
                        }
                    }
                    if stage == Stage::Commit {
                        let swapped = fs::rename(root.join("dst"), root.join("pinned-dst"));
                        #[cfg(windows)]
                        assert!(
                            swapped.is_err(),
                            "open destination must deny rename/delete sharing"
                        );
                        #[cfg(unix)]
                        {
                            swapped.unwrap();
                            link(&external, &root.join("dst"), true);
                        }
                    }
                },
            )
            .unwrap();
            #[cfg(windows)]
            let imported = root.join("dst/source");
            #[cfg(unix)]
            let imported = root.join("pinned-dst/source");
            let imported = if directory {
                imported.join("file")
            } else {
                imported
            };
            assert_eq!(fs::read_to_string(imported).unwrap(), "original");
            assert!(!external.join("source/file-from-dst").exists());
            assert!(!external.join("source/source").exists());
        }
    }
    #[cfg(windows)]
    #[test]
    fn in_place_destination_junction_retarget_never_writes_into_target() {
        let (_temp, root, external) = fixture();
        fs::write(external.join("source"), "original").unwrap();
        fs::create_dir(external.join("secret")).unwrap();
        fs::write(external.join("secret/source"), "secret").unwrap();
        let result = run(
            &root,
            &external.join("source"),
            &root.join("dst"),
            true,
            100,
            &mut |stage, _| {
                if stage == Stage::Commit {
                    windows::attach_junction(&root.join("dst"), &external.join("secret")).unwrap();
                }
            },
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("partial destination may remain")
        );
        assert_eq!(
            fs::read_to_string(external.join("secret/source")).unwrap(),
            "secret"
        );
        assert_eq!(
            fs::read_to_string(external.join("source")).unwrap(),
            "original"
        );
    }
}
