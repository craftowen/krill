use super::*;

#[test]
fn orca_setup_native_precedence_and_manifest_security() {
    let f = Fixture::new("orca", "");
    fs::write(f.repo.join("orca.yaml"), "scripts:\n  setup: |\n    printf 'one'\n    printf 'two'\n  archive: ignored\ndefaultTabs: []\n").unwrap();
    fs::write(
        f.repo.join(".worktreeinclude"),
        "# local files\n.env\n.env\n*.key\n!secret\nsecret\n../escape\nlink\nminikrill.toml\n",
    )
    .unwrap();
    fs::write(f.repo.join(".env"), "secret").unwrap();
    fs::create_dir(f.repo.join("secret")).unwrap();
    fs::write(f.repo.join("secret/local"), "local").unwrap();
    symlink(f.dir.join("outside"), f.repo.join("link")).unwrap();
    let Outcome::Untrusted { script, hash } = f.create("new", Setup::Ask).unwrap() else {
        panic!("must ask")
    };
    assert_eq!(script, "printf 'one'\nprintf 'two'\n");
    assert!(!f.root.exists());
    let (w, setup) = f.created("new", Setup::Approve(hash));
    assert_eq!(setup.as_deref(), Some(script.as_str()));
    assert_eq!(w.copied, [".env", "secret"]);
    assert_eq!(fs::read(path(&w).join(".env")).unwrap(), b"secret");
    assert_eq!(fs::read(path(&w).join("secret/local")).unwrap(), b"local");
    assert!(
        w.warnings
            .iter()
            .any(|w| w.contains("leaves the repository"))
    );
    assert!(!path(&w).join("link").exists());
    assert!(f.created("automatic", Setup::Ask).1.is_some());
    assert!(f.created("skip", Setup::Skip).1.is_none());
    fs::write(f.repo.join(CONFIG), "[worktree]\nsetup = 'native'\n").unwrap();
    let Outcome::Untrusted { script, .. } = f.create("native", Setup::Ask).unwrap() else {
        panic!("must ask")
    };
    assert_eq!(script, "native");
    fs::write(f.repo.join(CONFIG), "[worktree]\nsetup = ''\n").unwrap();
    assert!(f.created("disabled", Setup::Ask).1.is_none());
}

#[test]
fn malformed_and_oversized_configuration_does_not_create_worktrees() {
    let f = Fixture::new("orca-bad", "");
    for bad in ["scripts: [", "scripts:\n  setup: [a, b]\n"] {
        fs::write(f.repo.join("orca.yaml"), bad).unwrap();
        assert!(
            f.create("bad", Setup::Ask)
                .unwrap_err()
                .contains("orca.yaml")
        );
    }
    fs::remove_file(f.repo.join("orca.yaml")).unwrap();
    fs::write(f.repo.join(".worktreeinclude"), vec![b'x'; 256 * 1024 + 1]).unwrap();
    assert!(f.create("bad", Setup::Ask).unwrap_err().contains("256 KiB"));
    assert!(!f.root.exists());
}

#[test]
fn environment_resolves_main_root_and_nested_worktree_without_losing_bytes() {
    let mut f = Fixture::new("orca-env", "");
    let renamed = f.dir.join(OsString::from("레포-'\n한글"));
    fs::rename(&f.repo, &renamed).unwrap();
    f.repo = renamed;
    let (w, _) = f.created("한글", Setup::Skip);
    let top = path(&w);
    fs::create_dir(top.join("nested")).unwrap();
    let env = compat::environment_in(&f.queue, &top.join("nested")).unwrap();
    let value = |key: &[u8]| env.iter().find(|(k, _)| k == key).unwrap().1.clone();
    let root = fs::canonicalize(&f.repo).unwrap();
    for key in [
        b"KRILL_ROOT_PATH".as_slice(),
        b"ORCA_ROOT_PATH",
        b"CONDUCTOR_ROOT_PATH",
        b"GHOSTX_ROOT_PATH",
    ] {
        assert_eq!(value(key), root.as_os_str().as_bytes());
    }
    for key in [b"KRILL_WORKTREE_PATH".as_slice(), b"ORCA_WORKTREE_PATH"] {
        assert_eq!(value(key), top.as_os_str().as_bytes());
    }
    assert_eq!(value(b"ORCA_WORKSPACE_NAME"), "한글".as_bytes());
    assert_eq!(value(b"KRILL_WORKSPACE_NAME"), "한글".as_bytes());
    assert!(compat::environment_in(&f.queue, &f.dir).is_err());
}

#[test]
fn worktree_symlinks_create_traverse_and_remove_cleanly() {
    let f = Fixture::new(
        "symlinks",
        "[worktree]\nsymlinks = ['.env', 'secret/nested.txt', '../escape', '.git/config', 'link']\n",
    );
    fs::write(
        f.repo.join("orca.yaml"),
        "worktree:\n  sharedDirectories:\n    - shared-dir/\n",
    )
    .unwrap();
    let state_dir = f.trust.parent().unwrap();
    let repo_key = fs::canonicalize(&f.repo)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    krill_proto::orchestration::WorktreeStore::mutate(state_dir, |store| {
        store.add_symlink(&repo_key, ".env.local")?;
        Ok(())
    })
    .unwrap();

    fs::write(f.repo.join(".env"), "ROOT_ENV=1").unwrap();
    fs::write(f.repo.join(".env.local"), "LOCAL_ENV=2").unwrap();
    fs::create_dir_all(f.repo.join("secret")).unwrap();
    fs::write(f.repo.join("secret/nested.txt"), "nested-secret").unwrap();
    fs::create_dir_all(f.repo.join("shared-dir")).unwrap();
    fs::write(f.repo.join("shared-dir/cache.txt"), "shared-cache").unwrap();
    symlink(f.dir.join("outside"), f.repo.join("link")).unwrap();

    let (w, _) = f.created("linked", Setup::Skip);
    let wt_path = path(&w);
    assert_eq!(
        w.copied,
        [".env", ".env.local", "secret/nested.txt", "shared-dir"]
    );
    for rel in [".env", ".env.local", "secret/nested.txt", "shared-dir"] {
        let meta = fs::symlink_metadata(wt_path.join(rel)).unwrap();
        assert!(meta.file_type().is_symlink(), "{rel} should be a symlink");
    }
    assert_eq!(
        fs::read_to_string(wt_path.join(".env")).unwrap(),
        "ROOT_ENV=1"
    );
    assert_eq!(
        fs::read_to_string(wt_path.join(".env.local")).unwrap(),
        "LOCAL_ENV=2"
    );
    assert_eq!(
        fs::read_to_string(wt_path.join("secret/nested.txt")).unwrap(),
        "nested-secret"
    );
    assert_eq!(
        fs::read_to_string(wt_path.join("shared-dir/cache.txt")).unwrap(),
        "shared-cache"
    );
    // Traversal, .git, and symlink-in-source entries must be refused with warnings.
    assert!(
        w.warnings
            .iter()
            .any(|msg| msg.contains("../escape") && msg.contains("leaves the repository"))
    );
    assert!(
        w.warnings
            .iter()
            .any(|msg| msg.contains(".git/config") && msg.contains("leaves the repository"))
    );
    assert!(
        w.warnings
            .iter()
            .any(|msg| msg.contains("\"link\"") && msg.contains("has a symlink"))
    );
    assert!(!wt_path.join("link").exists());

    // An extra untracked regular file still makes the worktree dirty on removal without --force,
    // and keeps the symlinks intact when removal is rejected.
    fs::write(wt_path.join("untracked.txt"), "dirty").unwrap();
    assert!(f.remove(&w, false).unwrap_err().contains("dirty"));
    assert!(
        fs::symlink_metadata(wt_path.join(".env.local"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    // Removing the untracked regular file allows clean worktree removal without --force
    // even though .env.local and shared-dir are untracked symlinks, and preserves root files.
    fs::remove_file(wt_path.join("untracked.txt")).unwrap();
    let removed = f.remove(&w, false).unwrap();
    assert!(removed.branch_deleted);
    assert!(!wt_path.exists());
    assert_eq!(
        fs::read_to_string(f.repo.join(".env")).unwrap(),
        "ROOT_ENV=1"
    );
    assert_eq!(
        fs::read_to_string(f.repo.join("shared-dir/cache.txt")).unwrap(),
        "shared-cache"
    );
}

#[test]
fn archive_hook_gate_blocks_on_failure_and_allows_on_success() {
    let f = Fixture::new("archive-hook", "[worktree]\nsymlinks = ['.env']\n");
    fs::write(f.repo.join(".env"), "ENV_VAL=ok").unwrap();
    let marker = f.dir.join("archive-ran.txt");
    fs::write(
        f.repo.join("orca.yaml"),
        format!(
            "scripts:\n  archive: |\n    printf '%s' \"$ORCA_WORKSPACE_NAME\" > {}\n    exit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    f.git(&["add", "orca.yaml"]);
    f.git(&["commit", "-q", "-m", "add orca.yaml"]);

    let (w, _) = f.created("gate-test", Setup::Skip);
    let wt_path = path(&w);

    // Failing archive hook must block removal and leave both the worktree and its symlinks intact.
    let err = f.remove(&w, false).unwrap_err();
    assert!(
        err.contains("archive hook failed"),
        "unexpected error: {err}"
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), "gate-test");
    assert!(wt_path.exists());
    assert!(
        fs::symlink_metadata(wt_path.join(".env"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    // Store override with a succeeding archive hook runs in the worktree and allows removal.
    let state_dir = f.trust.parent().unwrap();
    let repo_key = fs::canonicalize(&f.repo)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let ok_marker = f.dir.join("archive-ok.txt");
    krill_proto::orchestration::WorktreeStore::mutate(state_dir, |store| {
        store.set_archive_script(
            &repo_key,
            &format!(
                "printf '%s:%s' \"$KRILL_WORKSPACE_NAME\" \"$(cat .env)\" > {}",
                ok_marker.display()
            ),
        )?;
        Ok(())
    })
    .unwrap();

    let removed = f.remove(&w, false).unwrap();
    assert!(removed.branch_deleted);
    assert!(!wt_path.exists());
    assert_eq!(
        fs::read_to_string(&ok_marker).unwrap(),
        "gate-test:ENV_VAL=ok"
    );
}
