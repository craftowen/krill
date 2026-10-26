//! Real Git regression checks shared by macOS and Windows (including verbatim paths).
use super::*;
use krill_proto::control::RemoveWorktree;
use krill_proto::os::ffi::OsStringExt as _;

struct Repository(PathBuf, GitQueue);

impl Repository {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("krill-git 한글-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let repo = Self(dir, GitQueue::new().unwrap());
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--allow-empty",
            "-qm",
            "init",
        ]);
        repo
    }

    fn git(&self, args: &[&str]) {
        let output = self
            .1
            .submit(&self.0, args.iter().copied())
            .recv()
            .unwrap()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
    }

    fn remove(&self, path: &Path) -> Result<krill_proto::control::RemovedWorktree, String> {
        worktree::remove_with_state(
            &self.1,
            &RemoveWorktree {
                repo: self.0.as_os_str().as_bytes().to_vec(),
                path: fs::canonicalize(path).unwrap().into_os_string().into_vec(),
                force: false,
            },
            None,
        )
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn git_queue_streams_changes_and_keeps_worktree_deletion_guards() {
    let repo = Repository::new("guards");
    let tree = repo.0.join("작업 공간");
    repo.git(&["worktree", "add", "-qb", "topic", tree.to_str().unwrap()]);
    fs::write(tree.join("untracked.txt"), "preserve me").unwrap();
    let changes = changes::query(&repo.1, tree.clone(), Arc::new(AtomicBool::new(false)))
        .recv()
        .unwrap()
        .unwrap();
    assert_eq!(changes.files.len(), 1);
    assert!(repo.remove(&tree).unwrap_err().contains("dirty"));
    assert_eq!(
        fs::read_to_string(tree.join("untracked.txt")).unwrap(),
        "preserve me"
    );
    fs::remove_file(tree.join("untracked.txt")).unwrap();
    repo.git(&["worktree", "lock", tree.to_str().unwrap()]);
    assert!(repo.remove(&tree).unwrap_err().contains("locked"));
    assert!(tree.is_dir());
    repo.git(&["worktree", "unlock", tree.to_str().unwrap()]);
    let removed = repo.remove(&tree).unwrap();
    assert_eq!(removed.branch.as_deref(), Some("topic"));
    assert!(removed.branch_deleted, "{removed:?}");
    assert!(!tree.exists());
    assert!(repo.remove(&repo.0).unwrap_err().contains("main worktree"));
}

#[test]
fn queued_git_handles_both_output_streams_and_nonzero_exit() {
    let repo = Repository::new("streams");
    let output = repo
        .1
        .submit(&repo.0, ["config", "--get", "core.quotePath"])
        .recv()
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"false\n");
    let output = repo
        .1
        .submit(&repo.0, ["krill-missing-subcommand"])
        .recv()
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
}

#[test]
fn git_timeout_releases_worker_and_cancel_guard_stays_responsive() {
    let repo = Repository::new("deadline");
    let before = Instant::now();
    let result = repo
        .1
        .submit(&repo.0, ["-c", "alias.krill-hang=!sleep 30", "krill-hang"])
        .recv()
        .unwrap()
        .unwrap_err();
    assert_eq!(result.kind(), io::ErrorKind::TimedOut);
    assert!(before.elapsed() < Duration::from_secs(10));
    repo.git(&["status", "--porcelain"]);
    let args = ["-c", "alias.krill-hang=!sleep 30", "krill-hang"].map(OsString::from);
    let mut calls = 0;
    let result = run_guarded(Path::new("git"), &repo.0, &args, || {
        calls += 1;
        if calls >= 3 {
            Err(io::ErrorKind::Interrupted.into())
        } else {
            Ok(())
        }
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
}

#[test]
fn trusted_archive_uses_git_shell_on_windows_and_keeps_trust_gate() {
    let repo = Repository::new("archive");
    let script = "printf archive > \"$KRILL_ROOT_PATH/archive-marker\"";
    fs::write(
        repo.0.join("krill.toml"),
        format!("[worktree]\narchive = '{script}'\n"),
    )
    .unwrap();
    repo.git(&["add", "krill.toml"]);
    repo.git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "archive",
    ]);
    let tree = repo.0.join("archive tree");
    repo.git(&["worktree", "add", "-qb", "archived", tree.to_str().unwrap()]);
    let state = repo.0.join("state");
    let req = RemoveWorktree {
        repo: repo.0.as_os_str().as_bytes().to_vec(),
        path: tree.as_os_str().as_bytes().to_vec(),
        force: false,
    };
    assert!(
        worktree::remove_with_state(&repo.1, &req, Some(&state))
            .unwrap_err()
            .contains("not trusted")
    );
    assert!(!repo.0.join("archive-marker").exists());
    worktree::remember(
        &state.join("setup-trust"),
        &worktree::digest(&fs::canonicalize(&repo.0).unwrap(), script),
    )
    .unwrap();
    worktree::remove_with_state(&repo.1, &req, Some(&state)).unwrap();
    assert_eq!(
        fs::read_to_string(repo.0.join("archive-marker")).unwrap(),
        "archive"
    );
    assert!(!tree.exists());
}
