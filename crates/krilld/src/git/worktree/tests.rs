use std::os::unix::fs::symlink;
use std::process::Command;

use super::*;

mod compat_tests;

/// 테스트마다 임시 디렉터리: `repo`(main 브랜치 커밋 하나), 워크트리 루트, 신뢰 파일.
pub(super) struct Fixture {
    pub(super) dir: PathBuf,
    pub(super) repo: PathBuf,
    pub(super) root: PathBuf,
    pub(super) trust: PathBuf,
    pub(super) queue: GitQueue,
}

impl Fixture {
    pub(super) fn new(tag: &str, config: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mkwt-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let repo = dir.join("레포");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join(CONFIG), config).unwrap();
        fs::write(repo.join(".gitignore"), ".env\nsecret/\nlink\nescape\n").unwrap();
        let f = Self {
            root: dir.join("worktrees"),
            trust: dir.join("state").join(TRUST_FILE),
            dir,
            repo,
            queue: GitQueue::new().unwrap(),
        };
        f.git(&["init", "-q", "-b", "main"]);
        f.git(&["add", "."]);
        f.git(&["commit", "-q", "-m", "init"]);
        f
    }

    pub(super) fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "core.quotePath=false",
            ])
            .args(args)
            .current_dir(&self.repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    fn create(&self, name: &str, setup: Setup) -> Result<Outcome, String> {
        let req = NewWorktree {
            repo: self.repo.as_os_str().as_bytes().to_vec(),
            name: name.to_owned(),
            base: None,
            setup,
        };
        create(&self.queue, &req, &self.root, &self.trust)
    }

    fn created(&self, name: &str, setup: Setup) -> (Worktree, Option<String>) {
        match self.create(name, setup) {
            Ok(Outcome::Created { worktree, setup }) => (worktree, setup),
            other => panic!("{other:?}"),
        }
    }

    fn remove(
        &self,
        w: &Worktree,
        force: bool,
    ) -> Result<krill_proto::control::RemovedWorktree, String> {
        remove_with_state(
            &self.queue,
            &krill_proto::control::RemoveWorktree {
                repo: self.repo.as_os_str().as_bytes().to_vec(),
                path: w.path.clone(),
                force,
            },
            self.trust.parent(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // 레포의 워크트리 등록을 지우는 것과 같다(레포째 지우므로).
        fs::remove_dir_all(&self.dir).ok();
    }
}

pub(super) fn path(w: &Worktree) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(&w.path))
}

#[test]
fn worktree_root_prefers_override_then_home_default() {
    let override_dir = |k: &str| Some(OsString::from(format!("/{k}")));
    assert_eq!(
        root_in(override_dir),
        Ok(PathBuf::from("/KRILL_WORKTREES_DIR"))
    );
    let home = |k: &str| (k == "HOME").then(|| OsString::from("/h"));
    assert_eq!(root_in(home), Ok(PathBuf::from("/h/Minikrill/worktrees")));
    assert!(root_in(|_| None).is_err());
    assert_eq!(
        isolated_root_in(Path::new("/state"), override_dir),
        PathBuf::from("/KRILL_WORKTREES_DIR")
    );
    assert_eq!(
        isolated_root_in(Path::new("/state"), home),
        PathBuf::from("/state/worktrees")
    );
}

/// 완료 조건: 한글 이름 입력, 충돌 시 -2. 경로는 `<루트>/<레포 이름>/<이름>`, base는 감지(main).
#[test]
fn korean_name_and_collision_suffix() {
    let f = Fixture::new("name", "");
    let (a, _) = f.created("새 기능", Setup::Ask);
    assert_eq!(a.branch, "새-기능");
    assert_eq!(a.base, "main");
    let real_root = fs::canonicalize(&f.root).unwrap();
    assert_eq!(path(&a), real_root.join("레포").join("새-기능"));
    assert!(path(&a).join(CONFIG).is_file(), "checkout");

    // 브랜치·디렉터리 둘 다 겹친다 → -2.
    let (b, _) = f.created("새 기능", Setup::Ask);
    assert_eq!(b.branch, "새-기능-2");
    assert_eq!(path(&b), real_root.join("레포").join("새-기능-2"));

    // 브랜치만 겹쳐도(디렉터리 없음) 피한다.
    f.git(&["branch", "다른"]);
    let (c, _) = f.created("다른", Setup::Ask);
    assert_eq!(c.branch, "다른-2");

    let branches = f.git(&["branch", "--format=%(refname:short)"]);
    for b in ["새-기능", "새-기능-2", "다른-2"] {
        assert!(branches.lines().any(|l| l == b), "{b} in {branches}");
    }
}

#[test]
fn bad_names_and_bases_are_refused() {
    let f = Fixture::new("bad", "");
    for name in ["", "  ", "-x", ".hidden", "a/b", "a..b", "x.lock"] {
        assert!(f.create(name, Setup::Ask).is_err(), "{name:?}");
    }
    let req = |base: &str| NewWorktree {
        repo: f.repo.as_os_str().as_bytes().to_vec(),
        name: "x".to_owned(),
        base: Some(base.to_owned()),
        setup: Setup::Ask,
    };
    for base in ["--detach", "없는-브랜치"] {
        assert!(create(&f.queue, &req(base), &f.root, &f.trust).is_err());
    }
    let ok = create(&f.queue, &req("main"), &f.root, &f.trust).unwrap();
    assert!(matches!(ok, Outcome::Created { worktree, .. } if worktree.base == "main"));
    // 아무것도 안 만든 실패는 디렉터리도 남기지 않는다.
    assert!(!f.root.join("레포").join("없는-브랜치").exists());
}

/// 완료 조건: include 경로 탈출·심링크 거부. gitignore된 것만 복사, 디렉터리 안 심링크는 건너뛴다.
#[test]
fn include_refuses_escape_and_symlinks() {
    let config = r#"
[worktree]
include = [".env", "secret", "../outside", "/etc/hosts", "./x", "link", "escape/x", "minikrill.toml", "none"]
"#;
    let f = Fixture::new("include", config);
    fs::write(f.repo.join(".env"), "TOKEN=1").unwrap();
    fs::create_dir(f.repo.join("secret")).unwrap();
    fs::write(f.repo.join("secret/a"), "a").unwrap();
    symlink("/etc/hosts", f.repo.join("secret/hosts")).unwrap();
    fs::write(f.dir.join("outside"), "o").unwrap();
    symlink("/etc/hosts", f.repo.join("link")).unwrap();
    symlink(&f.dir, f.repo.join("escape")).unwrap();
    fs::write(f.dir.join("x"), "x").unwrap();

    let (w, _) = f.created("inc", Setup::Ask);
    let wt = path(&w);
    assert_eq!(w.copied, [".env", "secret"]);
    assert_eq!(fs::read_to_string(wt.join(".env")).unwrap(), "TOKEN=1");
    assert_eq!(fs::read_to_string(wt.join("secret/a")).unwrap(), "a");
    assert!(fs::symlink_metadata(wt.join("secret/hosts")).is_err());
    assert!(fs::symlink_metadata(wt.join("link")).is_err());
    assert!(fs::symlink_metadata(wt.join("escape")).is_err());

    let warn = w.warnings.join("\n");
    for (entry, why) in [
        ("../outside", "leaves the repository"),
        ("/etc/hosts", "leaves the repository"),
        ("./x", "leaves the repository"),
        ("link", "symlink"),
        ("escape/x", "symlink"),
        ("secret/hosts", "symlink"),
        ("minikrill.toml", "not ignored"),
    ] {
        assert!(
            w.warnings
                .iter()
                .any(|l| l.contains(&format!("{entry:?}")) && l.contains(why)),
            "{entry}: {why} in\n{warn}"
        );
    }
    // 없는 항목은 조용히 건너뛴다.
    assert!(!warn.contains("none"), "{warn}");
}

/// 체크아웃된 심링크 디렉터리를 따라 워크트리 밖에 쓰지 않는다.
#[test]
fn include_refuses_symlinked_destination() {
    let config = "[worktree]\ninclude = [\"out/.env\"]\n";
    let f = Fixture::new("dest", config);
    // 레포에는 out이 디렉터리(무시된 파일 포함)지만, 커밋된 out은 밖을 가리키는 심링크다.
    let target = f.dir.join("target");
    fs::create_dir(&target).unwrap();
    symlink(&target, f.repo.join("out")).unwrap();
    f.git(&["add", "out"]);
    f.git(&["commit", "-q", "-m", "link"]);
    fs::remove_file(f.repo.join("out")).unwrap();
    fs::create_dir(f.repo.join("out")).unwrap();
    fs::write(f.repo.join("out/.env"), "e").unwrap();
    fs::write(f.repo.join(".git/info/exclude"), "out/.env\n").unwrap();

    let (w, _) = f.created("dest", Setup::Ask);
    assert!(w.copied.is_empty(), "{w:?}");
    assert!(!target.join(".env").exists());
    assert!(
        w.warnings
            .iter()
            .any(|l| l.contains("destination has a symlink"))
    );
}

/// 완료 조건: setup 내용 해시가 바뀌면 재확인. 신뢰 전에는 아무것도 만들지 않는다.
#[test]
fn setup_trust_follows_content_hash() {
    let f = Fixture::new("setup", "[worktree]\nsetup = \"echo one\"\n");
    let Ok(Outcome::Untrusted { script, hash }) = f.create("a", Setup::Ask) else {
        panic!("first run must ask");
    };
    assert_eq!(script, "echo one");
    assert!(!f.root.join("레포").join("a").exists(), "nothing created");

    // 다른 해시로 승인하면 다시 묻는다.
    assert!(matches!(
        f.create("a", Setup::Approve(vec![0; 32])),
        Ok(Outcome::Untrusted { .. })
    ));
    let (_, setup) = f.created("a", Setup::Approve(hash.clone()));
    assert_eq!(setup.as_deref(), Some("echo one"));
    // 같은 내용이면 묻지 않는다.
    let (_, setup) = f.created("b", Setup::Ask);
    assert_eq!(setup.as_deref(), Some("echo one"));
    // Skip이면 돌리지 않는다.
    let (_, setup) = f.created("c", Setup::Skip);
    assert_eq!(setup, None);

    // 내용이 바뀌면 다시 묻는다.
    fs::write(f.repo.join(CONFIG), "[worktree]\nsetup = \"echo two\"\n").unwrap();
    let Ok(Outcome::Untrusted { script, hash: h2 }) = f.create("d", Setup::Ask) else {
        panic!("changed script must ask again");
    };
    assert_eq!(script, "echo two");
    assert_ne!(h2, hash);
}

#[test]
fn base_priority_and_detached_fallback() {
    let f = Fixture::new("base", "[worktree]\nbase = 'configured'\n");
    f.git(&["branch", "configured"]);
    assert_eq!(f.created("config", Setup::Skip).0.base, "configured");
    let req = NewWorktree {
        repo: f.repo.as_os_str().as_bytes().to_vec(),
        name: "explicit".to_owned(),
        base: Some("main".to_owned()),
        setup: Setup::Skip,
    };
    assert!(matches!(create(&f.queue, &req, &f.root, &f.trust).unwrap(),
        Outcome::Created { worktree, .. } if worktree.base == "main"));
    fs::write(f.repo.join(CONFIG), "").unwrap();
    f.git(&["update-ref", "refs/remotes/origin/master", "HEAD"]);
    assert_eq!(
        f.created("remote-master", Setup::Skip).0.base,
        "origin/master"
    );
    f.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    assert_eq!(f.created("remote-main", Setup::Skip).0.base, "origin/main");
    f.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/master",
    ]);
    assert_eq!(f.created("remote-head", Setup::Skip).0.base, "origin/HEAD");
    for reference in [
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
    ] {
        f.git(&["update-ref", "--no-deref", "-d", reference]);
    }
    f.git(&["checkout", "--detach", "-q"]);
    f.git(&["branch", "-D", "main"]);
    assert_eq!(f.created("detached", Setup::Skip).0.base, "HEAD");
}

/// 서로 다른 링크된 워크트리에서 동시에 요청해도 이름 선택과 생성이 묶여야 한다.
#[test]
fn concurrent_requests_share_common_directory_lock() {
    let f = Fixture::new("concurrent", "");
    let other = f.created("source", Setup::Skip).0;
    let barrier = std::sync::Barrier::new(6);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..6)
            .map(|i| {
                let f = &f;
                let other = &other;
                let barrier = &barrier;
                scope.spawn(move || {
                    let req = NewWorktree {
                        repo: if i % 2 == 0 {
                            f.repo.as_os_str().as_bytes().to_vec()
                        } else {
                            other.path.clone()
                        },
                        name: "동시".to_owned(),
                        base: None,
                        setup: Setup::Skip,
                    };
                    barrier.wait();
                    create(&f.queue, &req, &f.root, &f.trust).unwrap()
                })
            })
            .collect();
        let mut branches: Vec<_> = handles
            .into_iter()
            .map(|h| match h.join().unwrap() {
                Outcome::Created { worktree, .. } => worktree.branch,
                other @ Outcome::Untrusted { .. } => panic!("{other:?}"),
            })
            .collect();
        branches.sort();
        assert_eq!(
            branches,
            ["동시", "동시-2", "동시-3", "동시-4", "동시-5", "동시-6"]
        );
    });
}

#[test]
fn include_does_not_create_directories_through_destination_symlink() {
    let f = Fixture::new("dest-parent", "[worktree]\ninclude = ['out/new/.env']\n");
    let target = f.dir.join("target");
    fs::create_dir(&target).unwrap();
    symlink(&target, f.repo.join("out")).unwrap();
    f.git(&["add", "out"]);
    f.git(&["commit", "-q", "-m", "link"]);
    fs::remove_file(f.repo.join("out")).unwrap();
    fs::create_dir_all(f.repo.join("out/new")).unwrap();
    fs::write(f.repo.join("out/new/.env"), "e").unwrap();
    fs::write(f.repo.join(".git/info/exclude"), "out/new/.env\n").unwrap();
    let (w, _) = f.created("dest", Setup::Skip);
    assert!(w.copied.is_empty());
    assert!(!target.join("new").exists(), "even mkdir must not escape");
}

#[test]
fn directory_and_dangling_symlink_names_get_suffixes() {
    let f = Fixture::new("path-collision", "");
    fs::create_dir_all(f.root.join("레포/taken")).unwrap();
    symlink("missing", f.root.join("레포/taken-2")).unwrap();
    assert_eq!(f.created("taken", Setup::Skip).0.branch, "taken-3");
}

#[test]
fn repository_path_with_newline_is_preserved() {
    let mut f = Fixture::new("bytes", "");
    let dest = f.dir.join("레포\n한글\n");
    fs::rename(&f.repo, &dest).unwrap();
    f.repo = dest;
    let (w, _) = f.created("한글", Setup::Skip);
    assert!(path(&w).join(CONFIG).is_file());
    assert_eq!(path(&w).parent().unwrap().file_name(), f.repo.file_name());
}

#[test]
fn remove_clean_worktree_and_merged_branch() {
    let f = Fixture::new("rm-clean", "");
    let w = f.created("merged", Setup::Skip).0;
    let result = f.remove(&w, false).unwrap();
    assert_eq!(result.branch.as_deref(), Some("merged"));
    assert!(result.branch_deleted);
    assert!(result.warnings.is_empty());
    assert!(!path(&w).exists());
    assert!(
        !f.git(&["worktree", "list", "--porcelain"])
            .contains("refs/heads/merged")
    );
    assert!(!f.git(&["branch", "--list", "merged"]).contains("merged"));
}

#[test]
fn removal_even_forced_preserves_unmerged_branch() {
    let f = Fixture::new("rm-unmerged", "");
    for force in [false, true] {
        let w = f
            .created(if force { "forced" } else { "unmerged" }, Setup::Skip)
            .0;
        fs::write(path(&w).join("work"), "valuable").unwrap();
        let wt = path(&w);
        f.git(&["-C", wt.to_str().unwrap(), "add", "work"]);
        f.git(&["-C", wt.to_str().unwrap(), "commit", "-q", "-m", "work"]);
        let commit = f.git(&["rev-parse", &w.branch]);
        let result = f.remove(&w, force).unwrap();
        assert!(!result.branch_deleted);
        assert!(result.warnings.iter().any(|e| e.contains("preserved")));
        assert_eq!(f.git(&["rev-parse", &w.branch]), commit);
        assert_eq!(
            f.git(&["show", &format!("{}:work", w.branch)]).trim(),
            "valuable"
        );
        assert!(!wt.exists());
    }
}

#[test]
fn branch_merged_only_to_its_upstream_is_preserved() {
    let f = Fixture::new("rm-upstream", "");
    let w = f.created("valuable", Setup::Skip).0;
    let wt = path(&w);
    fs::write(wt.join("work"), "valuable").unwrap();
    f.git(&["-C", wt.to_str().unwrap(), "add", "work"]);
    f.git(&["-C", wt.to_str().unwrap(), "commit", "-q", "-m", "work"]);
    f.git(&["branch", "upstream", "valuable"]);
    f.git(&["branch", "--set-upstream-to=upstream", "valuable"]);
    let result = f.remove(&w, false).unwrap();
    assert!(!result.branch_deleted);
    assert!(result.warnings.iter().any(|e| e.contains("not merged")));
    assert_eq!(f.git(&["show", "valuable:work"]).trim(), "valuable");
}

#[test]
fn dirty_worktrees_require_force_including_ignored_files() {
    let f = Fixture::new("rm-dirty", "");
    for kind in ["tracked", "staged", "untracked", "ignored"] {
        let w = f.created(kind, Setup::Skip).0;
        let wt = path(&w);
        let file = match kind {
            "tracked" | "staged" => CONFIG,
            "ignored" => ".env",
            _ => "untracked",
        };
        fs::write(wt.join(file), "valuable").unwrap();
        if kind == "staged" {
            f.git(&["-C", wt.to_str().unwrap(), "add", file]);
        }
        assert!(f.remove(&w, false).unwrap_err().contains("dirty"));
        assert_eq!(fs::read_to_string(wt.join(file)).unwrap(), "valuable");
        assert!(f.git(&["branch", "--list", &w.branch]).contains(&w.branch));
        assert!(f.remove(&w, true).unwrap().branch_deleted);
        assert!(!wt.exists());
    }
}

#[test]
fn locked_worktrees_are_refused_with_and_without_force() {
    let f = Fixture::new("rm-locked", "");
    for reason in [None, Some("portable\nworktree")] {
        let w = f.created("locked", Setup::Skip).0;
        let wt = path(&w);
        let mut args = vec!["worktree", "lock"];
        if let Some(reason) = reason {
            args.extend(["--reason", reason]);
        }
        args.push(wt.to_str().unwrap());
        f.git(&args);
        for force in [false, true] {
            assert!(f.remove(&w, force).unwrap_err().contains("locked"));
            assert!(wt.join(CONFIG).is_file());
        }
        f.git(&["worktree", "unlock", wt.to_str().unwrap()]);
        assert!(f.remove(&w, false).unwrap().branch_deleted);
    }
}

#[test]
fn main_and_foreign_paths_are_refused() {
    let f = Fixture::new("rm-paths", "");
    let foreign = Fixture::new("rm-foreign", "");
    for force in [false, true] {
        for target in [&f.repo, &foreign.repo, &f.dir] {
            let req = krill_proto::control::RemoveWorktree {
                repo: f.repo.as_os_str().as_bytes().to_vec(),
                path: fs::canonicalize(target)
                    .unwrap()
                    .as_os_str()
                    .as_bytes()
                    .to_vec(),
                force,
            };
            assert!(remove(&f.queue, &req).is_err());
            assert!(target.exists());
        }
    }
}

#[test]
fn deletion_from_target_with_newline_path_and_detached_head() {
    let f = Fixture::new("rm-bytes", "");
    let w = f.created("한글", Setup::Skip).0;
    let renamed = fs::canonicalize(&f.dir)
        .unwrap()
        .join(OsString::from_vec("line\n한글\n".as_bytes().to_vec()));
    text(
        git(
            &f.queue,
            &f.repo,
            [
                OsString::from("worktree"),
                OsString::from("move"),
                path(&w).into_os_string(),
                renamed.as_os_str().to_owned(),
            ],
        )
        .unwrap(),
        "move failed",
    )
    .unwrap();
    let req = krill_proto::control::RemoveWorktree {
        repo: renamed.as_os_str().as_bytes().to_vec(),
        path: renamed.as_os_str().as_bytes().to_vec(),
        force: false,
    };
    let result = remove(&f.queue, &req).unwrap();
    assert!(result.branch_deleted);
    assert!(!renamed.exists());

    let w = f.created("detached", Setup::Skip).0;
    f.git(&[
        "-C",
        path(&w).to_str().unwrap(),
        "checkout",
        "--detach",
        "-q",
    ]);
    let result = f.remove(&w, false).unwrap();
    assert_eq!(result.branch, None);
    assert!(!result.branch_deleted);
    assert!(result.warnings.is_empty());
    assert!(
        f.git(&["branch", "--list", "detached"])
            .contains("detached")
    );
}

#[test]
fn prune_removes_old_missing_registration_and_keeps_locked_registration() {
    let f = Fixture::new("rm-prune", "");
    for (name, locked) in [("stale", false), ("stale-locked", true)] {
        let w = f.created(name, Setup::Skip).0;
        if locked {
            f.git(&["worktree", "lock", path(&w).to_str().unwrap()]);
        }
        fs::remove_dir_all(path(&w)).unwrap();
        let gitdir = f.repo.join(".git/worktrees").join(name).join("gitdir");
        fs::File::options()
            .write(true)
            .open(gitdir)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
    }
    let w = f.created("remove", Setup::Skip).0;
    f.remove(&w, false).unwrap();
    let list = f.git(&["worktree", "list", "--porcelain"]);
    assert!(!list.contains("refs/heads/stale\n"), "{list}");
    assert!(list.contains("refs/heads/stale-locked\n"), "{list}");
    assert!(f.git(&["branch", "--list", "stale"]).contains("stale"));
}

/// OWE-605: setup 세션은 요청 크기로 띄우고, 없으면 80×24 기본값을 쓴다.
#[test]
fn setup_session_uses_requested_size_or_default() {
    use krill_proto::holder::WinSize;
    let outcome = || Outcome::Created {
        worktree: Worktree {
            path: b"/w".to_vec(),
            branch: "b".to_owned(),
            base: "main".to_owned(),
            copied: Vec::new(),
            warnings: Vec::new(),
            setup: None,
        },
        setup: Some("echo hi".to_owned()),
    };
    for (size, want) in [
        (
            Some(WinSize {
                cols: 200,
                rows: 50,
            }),
            (200_u16, 50_u16),
        ),
        (None, (80_u16, 24_u16)),
    ] {
        let mut got = None;
        let reply = finish(Ok(outcome()), size, |c| {
            got = Some((c.cols, c.rows));
            Ok(Reply::Created {
                session: krill_proto::SessionId(1),
                pid: 1,
            })
        });
        assert!(
            matches!(reply, Reply::WorktreeCreated(ref w) if w.setup == Some(krill_proto::SessionId(1))),
            "{reply:?}"
        );
        assert_eq!(got, Some(want), "size {size:?}");
    }
}

#[test]
fn setup_finds_executable_in_login_shell_rc_path() {
    use std::os::unix::fs::PermissionsExt as _;

    let f = Fixture::new(
        "setup-rc-path",
        "[worktree]\nsetup = \"rc-setup-tool > setup-ran\"\n",
    );
    let home = f.dir.join("home");
    let rc_bin = home.join("rc-bin");
    fs::create_dir_all(&rc_bin).unwrap();
    let tool = rc_bin.join("rc-setup-tool");
    fs::write(&tool, "#!/bin/sh\necho setup-from-rc\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(home.join(".zshrc"), "export PATH=\"$HOME/rc-bin:$PATH\"\n").unwrap();

    let Ok(Outcome::Untrusted { hash, .. }) = f.create("rc-wt", Setup::Ask) else {
        panic!("first run must ask");
    };
    let user = crate::sys::User {
        name: b"owen".to_vec(),
        home: home.as_os_str().as_bytes().to_vec(),
        shell: b"/bin/zsh".to_vec(),
    };
    let reply = finish(f.create("rc-wt", Setup::Approve(hash)), None, |c| {
        let Program::Custom { program, args } = c.program else {
            panic!("expected Program::Custom");
        };
        let launch =
            crate::shell::Shell::Custom { program, args }.launch_with(Some(&user), Vec::new());
        let (prog, args) = launch.argv.split_first().unwrap();
        let out = Command::new(OsStr::from_bytes(prog))
            .args(args.iter().map(|a| OsStr::from_bytes(a)))
            .current_dir(OsStr::from_bytes(&c.cwd))
            .env_clear()
            .envs(
                launch
                    .env
                    .iter()
                    .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
            )
            .output()
            .unwrap();
        assert!(out.status.success(), "setup failed: {out:?}");
        Ok(Reply::Created {
            session: krill_proto::SessionId(1),
            pid: 1,
        })
    });
    let Reply::WorktreeCreated(wt) = reply else {
        panic!("{reply:?}");
    };
    assert_eq!(wt.setup, Some(krill_proto::SessionId(1)));
    assert_eq!(
        fs::read_to_string(path(&wt).join("setup-ran")).unwrap(),
        "setup-from-rc\n"
    );
}

/// OWE-737: 첫 프롬프트 기반 브랜치 자동 개명(기본 이름·충돌 `-2`/`-3`·보호 브랜치·수동 이름·upstream 가드).
#[test]
fn rename_branch_from_prompt_renames_default_and_handles_collisions_and_guards() {
    let f = Fixture::new("auto-rename", "");
    // 1. 주 저장소(main 브랜치)는 보호 대상이라 개명하지 않는다.
    assert_eq!(
        rename_branch_from_prompt(&f.queue, &f.repo, "Fix login timeout bug").unwrap(),
        None
    );

    // 2. 이미 사용자가 의미 있는 이름(custom-branch)으로 만든 워크트리는 건드리지 않는다.
    let custom = f.created("custom-branch", Setup::Skip).0;
    assert_eq!(
        rename_branch_from_prompt(&f.queue, &path(&custom), "Fix login timeout bug").unwrap(),
        None
    );

    // 3. 기본 이름(worktree-1) 워크트리는 첫 프롬프트 슬러그로 개명된다.
    let wt1 = f.created("worktree-1", Setup::Skip).0;
    let wt1_path = path(&wt1);
    let renamed1 =
        rename_branch_from_prompt(&f.queue, &wt1_path, "Please fix login timeout bug!").unwrap();
    assert_eq!(renamed1.as_deref(), Some("fix-login-timeout-bug"));
    assert_eq!(
        f.git(&["-C", wt1_path.to_str().unwrap(), "branch", "--show-current"])
            .trim(),
        "fix-login-timeout-bug"
    );
    // 한 번 개명된 뒤에는 더 이상 기본 이름이 아니므로 두 번째 프롬프트가 와도 덮어쓰지 않는다.
    assert_eq!(
        rename_branch_from_prompt(&f.queue, &wt1_path, "Another prompt should not rename").unwrap(),
        None
    );

    // 4. 같은 슬러그가 이미 있으면 `-2`, `-3` 충돌 접미사를 붙인다.
    let wt2 = f.created("claude-1", Setup::Skip).0;
    let wt2_path = path(&wt2);
    let renamed2 = rename_branch_from_prompt(&f.queue, &wt2_path, "Fix login timeout bug").unwrap();
    assert_eq!(renamed2.as_deref(), Some("fix-login-timeout-bug-2"));

    let wt3 = f.created("orca", Setup::Skip).0;
    let wt3_path = path(&wt3);
    let renamed3 = rename_branch_from_prompt(&f.queue, &wt3_path, "Fix login timeout bug").unwrap();
    assert_eq!(renamed3.as_deref(), Some("fix-login-timeout-bug-3"));

    // 5. 한글 프롬프트도 유효한 브랜치 슬러그로 개명된다.
    let wt_ko = f.created("worktree-2", Setup::Skip).0;
    let wt_ko_path = path(&wt_ko);
    let renamed_ko =
        rename_branch_from_prompt(&f.queue, &wt_ko_path, "로그인 세션 만료 버그 수정").unwrap();
    assert_eq!(renamed_ko.as_deref(), Some("로그인-세션-만료-버그-수정"));

    // 6. upstream 추적 브랜치가 설정된 기본 브랜치는 건드리지 않는다.
    let wt_up = f.created("worktree-3", Setup::Skip).0;
    let wt_up_path = path(&wt_up);
    f.git(&["branch", "upstream-ref", "worktree-3"]);
    f.git(&["branch", "--set-upstream-to=upstream-ref", "worktree-3"]);
    assert_eq!(
        rename_branch_from_prompt(&f.queue, &wt_up_path, "Should skip tracked branch").unwrap(),
        None
    );
}
