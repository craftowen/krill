use super::*;
use std::fs;
use std::os::unix::fs::symlink;

struct Fixture(PathBuf);
impl Fixture {
    #[expect(
        clippy::unwrap_used,
        reason = "test fixture setup must fail immediately"
    )]
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "krill-transfer-position-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("repo/dst")).unwrap();
        fs::create_dir_all(path.join("repo/.git")).unwrap();
        fs::create_dir_all(path.join("external")).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn root(&self) -> PathBuf {
        self.0.join("repo")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}
#[expect(
    clippy::unwrap_used,
    reason = "test helper requires an explicit conflict error"
)]
fn conflict(result: io::Result<PathBuf>) {
    let error = result.unwrap_err().to_string();
    assert!(error.contains("namespace conflict"), "{error}");
}
fn content(path: &Path, directory: bool) -> PathBuf {
    if directory {
        path.join("file")
    } else {
        path.to_path_buf()
    }
}
#[expect(
    clippy::unwrap_used,
    reason = "test fixture writes must fail immediately"
)]
fn write_entry(path: &Path, directory: bool, text: &str) {
    if directory {
        fs::create_dir(path).unwrap();
    }
    fs::write(content(path, directory), text).unwrap();
}

#[test]
fn opened_and_commit_source_swaps_reject_links_and_regular_replacements() {
    for stage in [Stage::Opened, Stage::Commit] {
        for import in [false, true] {
            for directory in [false, true] {
                for link in [false, true] {
                    let fixture = Fixture::new();
                    let root = fixture.root();
                    let parent = if import {
                        fixture.0.join("external")
                    } else {
                        root.clone()
                    };
                    let source = parent.join("source");
                    let saved = parent.join("saved");
                    let secret = fixture.0.join("secret");
                    write_entry(&source, directory, "original");
                    write_entry(&secret, directory, "secret");
                    conflict(run(
                        &root,
                        &source,
                        &root.join("dst"),
                        import,
                        100,
                        &mut |at, _| {
                            if at == stage {
                                fs::rename(&source, &saved).unwrap();
                                if link {
                                    symlink(&secret, &source).unwrap();
                                } else {
                                    write_entry(&source, directory, "concurrent");
                                }
                            }
                        },
                    ));
                    assert_eq!(
                        fs::read_to_string(content(&saved, directory)).unwrap(),
                        "original"
                    );
                    assert_eq!(
                        fs::read_to_string(content(&secret, directory)).unwrap(),
                        "secret"
                    );
                    if link {
                        assert_eq!(fs::read_link(&source).unwrap(), secret);
                    } else {
                        assert_eq!(
                            fs::read_to_string(content(&source, directory)).unwrap(),
                            "concurrent"
                        );
                    }
                    assert!(!root.join("dst/source").exists());
                }
            }
        }
    }
}

#[test]
fn displaced_destination_into_git_rejects_before_any_copy_or_move_write() {
    for stage in [Stage::BeforeOpen, Stage::Opened, Stage::Commit] {
        for import in [false, true] {
            for directory in [false, true] {
                let fixture = Fixture::new();
                let root = fixture.root();
                let source = if import {
                    fixture.0.join("external/source")
                } else {
                    root.join("source")
                };
                write_entry(&source, directory, "original");
                conflict(run(
                    &root,
                    &source,
                    &root.join("dst"),
                    import,
                    100,
                    &mut |at, _| {
                        if at == stage {
                            fs::rename(root.join("dst"), root.join(".git/pinned")).unwrap();
                            fs::create_dir(root.join("dst")).unwrap();
                        }
                    },
                ));
                assert_eq!(
                    fs::read_to_string(content(&source, directory)).unwrap(),
                    "original"
                );
                assert_eq!(fs::read_dir(root.join(".git/pinned")).unwrap().count(), 0);
                assert_eq!(fs::read_dir(root.join("dst")).unwrap().count(), 0);
            }
        }
    }
}

#[test]
fn postopen_source_ancestor_replacement_is_an_explicit_conflict() {
    for stage in [Stage::BeforeOpen, Stage::Opened, Stage::Commit] {
        for import in [false, true] {
            let fixture = Fixture::new();
            let root = fixture.root();
            let base = if import {
                fixture.0.join("external")
            } else {
                root.clone()
            };
            let parent = base.join("parent");
            let saved = base.join("saved");
            fs::create_dir(&parent).unwrap();
            let source = parent.join("file");
            fs::write(&source, "original").unwrap();
            conflict(run(
                &root,
                &source,
                &root.join("dst"),
                import,
                100,
                &mut |at, _| {
                    if at == stage {
                        fs::rename(&parent, &saved).unwrap();
                        fs::create_dir(&parent).unwrap();
                        fs::write(&source, "concurrent").unwrap();
                    }
                },
            ));
            assert_eq!(fs::read_to_string(saved.join("file")).unwrap(), "original");
            assert_eq!(fs::read_to_string(&source).unwrap(), "concurrent");
            assert!(!root.join("dst/file").exists());
        }
    }
}

#[test]
fn displaced_repository_root_is_rejected_even_if_selected_directories_return() {
    // Moving dst back to its expected pathname must not hide replacement of root.
    let fixture = Fixture::new();
    let root = fixture.root();
    let saved = fixture.0.join("saved");
    let source = fixture.0.join("external/file");
    fs::write(&source, "original").unwrap();
    conflict(run(
        &root,
        &source,
        &root.join("dst"),
        true,
        100,
        &mut |at, _| {
            if at == Stage::Commit {
                fs::rename(&root, &saved).unwrap();
                fs::create_dir(&root).unwrap();
                fs::rename(saved.join("dst"), root.join("dst")).unwrap();
            }
        },
    ));
    assert_eq!(fs::read_to_string(&source).unwrap(), "original");
    assert_eq!(fs::read_dir(root.join("dst")).unwrap().count(), 0);
}

#[test]
fn recursive_positions_are_checked_after_open_before_child_content_writes() {
    for source_swap in [false, true] {
        for stage in [Stage::BeforeOpen, Stage::Opened] {
            let fixture = Fixture::new();
            let root = fixture.root();
            let source = fixture.0.join("external/tree");
            fs::create_dir_all(source.join("child")).unwrap();
            fs::write(source.join("child/file"), "original").unwrap();
            let result = run(
                &root,
                &source,
                &root.join("dst"),
                true,
                100,
                &mut |at, path| {
                    if at == stage && path == source.join("child") {
                        if source_swap {
                            fs::rename(&source, fixture.0.join("external/saved")).unwrap();
                            fs::create_dir_all(source.join("child")).unwrap();
                            fs::write(source.join("child/file"), "concurrent").unwrap();
                        } else {
                            fs::rename(root.join("dst/tree"), root.join(".git/pinned")).unwrap();
                            fs::create_dir(root.join("dst/tree")).unwrap();
                        }
                    }
                },
            );
            let error = result.unwrap_err().to_string();
            assert!(error.contains("namespace conflict"), "{error}");
            assert!(error.contains("partial destination may remain"), "{error}");
            if source_swap {
                assert_eq!(
                    fs::read_to_string(fixture.0.join("external/saved/child/file")).unwrap(),
                    "original"
                );
                assert_eq!(
                    fs::read_to_string(source.join("child/file")).unwrap(),
                    "concurrent"
                );
            } else {
                assert_eq!(
                    fs::read_to_string(source.join("child/file")).unwrap(),
                    "original"
                );
                assert_eq!(fs::read_dir(root.join(".git/pinned")).unwrap().count(), 0);
            }
            assert!(!root.join("dst/tree/child/file").exists());
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn verified_tmp_and_var_aliases_still_import_and_move() {
    for alias in [Path::new("/tmp"), Path::new("/var/tmp")] {
        let fixture = Fixture::new();
        let path = alias.join(fixture.0.file_name().unwrap());
        fs::create_dir(&path).unwrap();
        let result = (|| {
            fs::write(path.join("file"), "original")?;
            transfer(
                &fixture.root(),
                &path.join("file"),
                &fixture.root().join("dst"),
                true,
                100,
            )?;
            transfer(
                &fixture.root(),
                &fixture.root().join("dst/file"),
                &fixture.root(),
                false,
                100,
            )?;
            assert_eq!(fs::read_to_string(fixture.root().join("file"))?, "original");
            Ok::<_, io::Error>(())
        })();
        fs::remove_dir_all(&path).unwrap();
        result.unwrap();
    }
}
