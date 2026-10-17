use super::*;
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mk088-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        let r = Self(path);
        r.git(&["init", "-q"]);
        r.git(&["config", "user.name", "Test"]);
        r.git(&["config", "user.email", "test@example.invalid"]);
        r.git(&["config", "commit.gpgSign", "false"]);
        fs::create_dir(r.0.join("hooks")).unwrap();
        r.git(&[
            "config",
            "core.hooksPath",
            r.0.join("hooks").to_str().unwrap(),
        ]);
        r
    }
    fn git(&self, args: &[&str]) -> Vec<u8> {
        let out = Command::new("git")
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        out.stdout
    }
    fn edit(&self, edit: Edit) -> Result<(), String> {
        apply(&GitQueue::new().unwrap(), &self.0, edit)
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}
fn stage(path: &str) -> Edit {
    Edit::Stage {
        path: path.as_bytes().into(),
        old_path: None,
    }
}
fn unstage(path: &str) -> Edit {
    Edit::Unstage {
        path: path.as_bytes().into(),
        old_path: None,
    }
}
fn commit(message: &str) -> Edit {
    Edit::Commit {
        message: message.into(),
    }
}

#[test]
fn unborn_unstage_preserves_worktree_and_commit_only_records_index() {
    let r = Repo::new();
    let name = "한🙂[a]\t\n.txt";
    fs::write(r.0.join(name), "staged\n").unwrap();
    fs::write(r.0.join("unselected"), "must stay untracked\n").unwrap();
    r.edit(stage(name)).unwrap();
    fs::write(r.0.join(name), "working edit\n").unwrap();
    r.edit(unstage(name)).unwrap();
    assert!(r.git(&["ls-files", "-z"]).is_empty());
    assert_eq!(fs::read(r.0.join(name)).unwrap(), b"working edit\n");
    fs::write(r.0.join(name), "staged\n").unwrap();
    r.edit(stage(name)).unwrap();
    fs::write(r.0.join(name), "new working content\n").unwrap();
    r.edit(commit("한글 커밋\n\n본문")).unwrap();
    assert_eq!(r.git(&["show", &format!("HEAD:{name}")]), b"staged\n");
    assert_eq!(fs::read(r.0.join(name)).unwrap(), b"new working content\n");
    assert_eq!(r.git(&["ls-files", "-z"]), [name.as_bytes(), &[0]].concat());
    assert_eq!(
        r.git(&["log", "-1", "--format=%B"]),
        "한글 커밋\n\n본문\n\n".as_bytes()
    );
}

#[test]
fn rename_modified_after_staging_unstage_and_deletion() {
    let r = Repo::new();
    fs::write(r.0.join("old"), "one\n").unwrap();
    r.edit(stage("old")).unwrap();
    r.edit(commit("initial")).unwrap();
    r.git(&["mv", "--", "old", "-new[1]"]);
    fs::write(r.0.join("-new[1]"), "two\n").unwrap();
    r.edit(Edit::Stage {
        path: b"-new[1]".into(),
        old_path: Some(b"old".into()),
    })
    .unwrap();
    r.edit(Edit::Unstage {
        path: b"-new[1]".into(),
        old_path: Some(b"old".into()),
    })
    .unwrap();
    assert!(r.git(&["diff", "--cached"]).is_empty());
    assert_eq!(fs::read(r.0.join("-new[1]")).unwrap(), b"two\n");
    r.edit(stage("old")).unwrap();
    r.edit(commit("delete")).unwrap();
    assert!(r.git(&["ls-files"]).is_empty());
}

#[test]
fn literal_pathspec_does_not_stage_neighbors_and_subdir_resolves_root() {
    let r = Repo::new();
    fs::write(r.0.join(":(glob)*"), "literal\n").unwrap();
    fs::write(r.0.join("neighbor"), "other\n").unwrap();
    fs::create_dir(r.0.join("sub")).unwrap();
    apply(
        &GitQueue::new().unwrap(),
        &r.0.join("sub"),
        stage(":(glob)*"),
    )
    .unwrap();
    assert_eq!(r.git(&["ls-files", "-z"]), b":(glob)*\0");
    for path in ["", "../escape", "/absolute", ".", "a/../../b"] {
        assert!(r.edit(stage(path)).is_err(), "{path}");
    }
    for message in [
        String::new(),
        " \n ".into(),
        "a".repeat(MAX_MESSAGE + 1),
        "a\0b".into(),
    ] {
        assert!(r.edit(Edit::Commit { message }).is_err());
    }
}

#[test]
fn failing_hook_is_reported_and_preserves_staged_content() {
    use std::os::unix::fs::PermissionsExt as _;
    let r = Repo::new();
    fs::write(r.0.join("file"), "one\n").unwrap();
    r.edit(stage("file")).unwrap();
    let hook = r.0.join("hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\necho rejected-by-hook >&2\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        r.edit(commit("message"))
            .unwrap_err()
            .contains("rejected-by-hook")
    );
    assert_eq!(r.git(&["ls-files", "-z"]), b"file\0");
    assert_eq!(fs::read(r.0.join("file")).unwrap(), b"one\n");
}

#[test]
fn linked_worktree_uses_common_repository_and_its_own_index() {
    let r = Repo::new();
    fs::write(r.0.join("file"), "initial\n").unwrap();
    r.edit(stage("file")).unwrap();
    r.edit(commit("initial")).unwrap();
    let linked = r.0.join("linked");
    r.git(&[
        "worktree",
        "add",
        "-qb",
        "linked-test",
        linked.to_str().unwrap(),
    ]);
    let q = GitQueue::new().unwrap();
    let common = resolve(&q, &linked, "--git-common-dir").unwrap();
    assert_eq!(
        fs::canonicalize(common.parent().unwrap()).unwrap(),
        fs::canonicalize(&r.0).unwrap()
    );
    fs::write(linked.join("file"), "linked edit\n").unwrap();
    apply(&q, &linked, stage("file")).unwrap();
    assert!(r.git(&["diff", "--cached"]).is_empty());
    assert_eq!(r.git(&["-C", "linked", "show", ":file"]), b"linked edit\n");
}

#[test]
fn push_pull_sync_and_commit_and_push_roundtrip_against_local_remote() {
    let remote = Repo::new();
    let bare = remote.0.join("origin.git");
    remote.git(&["init", "--bare", "-q", bare.to_str().unwrap()]);

    let a = Repo::new();
    a.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
    fs::write(a.0.join("file.txt"), "v1\n").unwrap();
    a.edit(stage("file.txt")).unwrap();
    a.edit(Edit::CommitAndPush {
        message: "initial push".into(),
    })
    .unwrap();

    let b = Repo::new();
    b.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
    b.git(&["fetch", "-q", "origin"]);
    let branch = String::from_utf8(a.git(&["rev-parse", "--abbrev-ref", "HEAD"]))
        .unwrap()
        .trim()
        .to_owned();
    b.git(&[
        "checkout",
        "-qb",
        &branch,
        "--track",
        &format!("origin/{branch}"),
    ]);
    assert_eq!(fs::read(b.0.join("file.txt")).unwrap(), b"v1\n");

    // Push from B, Pull into A.
    fs::write(b.0.join("file.txt"), "v2\n").unwrap();
    b.edit(stage("file.txt")).unwrap();
    b.edit(commit("b update")).unwrap();
    b.edit(Edit::Push).unwrap();

    a.edit(Edit::Pull).unwrap();
    assert_eq!(fs::read(a.0.join("file.txt")).unwrap(), b"v2\n");

    // Sync from A (pulls B's non-conflicting commit and pushes A's local commit).
    fs::write(b.0.join("other.txt"), "from-b\n").unwrap();
    b.edit(stage("other.txt")).unwrap();
    b.edit(Edit::CommitAndPush {
        message: "b other".into(),
    })
    .unwrap();

    fs::write(a.0.join("local.txt"), "from-a\n").unwrap();
    a.edit(stage("local.txt")).unwrap();
    a.edit(commit("a local")).unwrap();
    a.edit(Edit::Sync).unwrap();

    b.edit(Edit::Pull).unwrap();
    assert_eq!(fs::read(b.0.join("local.txt")).unwrap(), b"from-a\n");
    assert_eq!(fs::read(a.0.join("other.txt")).unwrap(), b"from-b\n");
}
