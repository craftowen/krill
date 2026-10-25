use super::super::tests::{Fixture, path};
use super::*;

fn request(f: &Fixture, token: u64) -> PrepareWorktree {
    PrepareWorktree {
        token,
        repo: f.repo.as_os_str().as_bytes().to_vec(),
        base: None,
        cancel: false,
    }
}

fn ready(pool: &Arc<Pool>, req: &PrepareWorktree) {
    assert_eq!(pool.request(req), Status::Preparing);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match pool.request(req) {
            Status::Ready => return,
            Status::Preparing if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            other => panic!("preparation did not become ready: {other:?}"),
        }
    }
}

fn cleaned(pool: &Pool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while lock(&pool.state).slot.is_some() {
        assert!(Instant::now() < deadline, "preparation did not clean up");
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!pool.tree().exists());
    assert!(!pool.dir.join(MARKER).exists());
}

fn pool(f: &Fixture) -> Arc<Pool> {
    Pool::new(Arc::new(GitQueue::new().unwrap()), &f.root).unwrap()
}

#[test]
fn prepared_creation_preserves_includes_trust_and_name_collisions() {
    let f = Fixture::new(
        "prep-promote",
        "[worktree]\nsetup = \"touch setup-ran\"\ninclude = [\".env\"]\n",
    );
    fs::write(f.repo.join(".env"), "old").unwrap();
    let pool = pool(&f);
    let req = request(&f, 1);
    ready(&pool, &req);
    assert!(!pool.tree().join(".env").exists());
    assert!(!pool.tree().join("setup-ran").exists());
    let create_req = NewWorktree {
        repo: req.repo.clone(),
        name: "agent".into(),
        base: None,
        setup: Setup::Ask,
    };
    assert!(matches!(
        create_with_pool(&f.queue, &create_req, &f.root, &f.trust, Some(&pool)),
        Ok(Outcome::Untrusted { .. })
    ));
    assert_eq!(pool.request(&req), Status::Ready);
    // An existing branch still gets the standard suffix; includes reflect submission time.
    f.git(&["branch", "agent"]);
    fs::write(f.repo.join(".env"), "current").unwrap();
    let Outcome::Created { worktree, .. } = create_with_pool(
        &f.queue,
        &NewWorktree {
            setup: Setup::Skip,
            ..create_req
        },
        &f.root,
        &f.trust,
        Some(&pool),
    )
    .unwrap() else {
        panic!("not created")
    };
    assert_eq!(worktree.branch, "agent-2");
    assert_eq!(
        fs::read_to_string(path(&worktree).join(".env")).unwrap(),
        "current"
    );
    assert!(path(&worktree).join(CONFIG).exists());
    assert!(
        f.git(&["worktree", "list", "--porcelain"])
            .contains("refs/heads/agent-2")
    );
    assert!(
        !f.git(&["worktree", "list", "--porcelain"])
            .contains(".krill-preparing")
    );
    cleaned(&pool);
    // Consumed worktrees survive subsequent cancellation and TTL cleanup.
    pool.request(&PrepareWorktree {
        cancel: true,
        ..req
    });
    assert!(path(&worktree).join(CONFIG).exists());
}

#[test]
fn moved_base_and_distinct_repository_do_not_consume_stale_preparation() {
    let f = Fixture::new("prep-stale", "");
    let other = Fixture::new("prep-other", "");
    let pool = pool(&f);
    let req = request(&f, 2);
    ready(&pool, &req);
    fs::write(f.repo.join("new"), "fresh").unwrap();
    f.git(&["add", "new"]);
    f.git(&["commit", "-qm", "new head"]);
    for fixture in [&f, &other] {
        let req = NewWorktree {
            repo: fixture.repo.as_os_str().as_bytes().to_vec(),
            name: "new".into(),
            base: None,
            setup: Setup::Skip,
        };
        let Outcome::Created { worktree, .. } = create_with_pool(
            &fixture.queue,
            &req,
            &fixture.root,
            &fixture.trust,
            Some(&pool),
        )
        .unwrap() else {
            panic!("not created")
        };
        assert_eq!(path(&worktree).join("new").exists(), fixture.repo == f.repo);
        assert_eq!(pool.request(&request(&f, 2)), Status::Ready);
    }
    pool.request(&PrepareWorktree {
        cancel: true,
        ..req
    });
    cleaned(&pool);
}

#[test]
fn ttl_and_cancel_cleanup_registered_locked_worktrees() {
    assert_eq!(TTL, Duration::from_secs(300));
    for cancel in [false, true] {
        let f = Fixture::new(if cancel { "prep-cancel" } else { "prep-ttl" }, "");
        let pool = Pool::with_limits(
            Arc::new(GitQueue::new().unwrap()),
            &f.root,
            if cancel {
                TTL
            } else {
                Duration::from_millis(800)
            },
            DISK_LIMIT,
        )
        .unwrap();
        let req = request(&f, 3);
        ready(&pool, &req);
        assert!(
            f.git(&["worktree", "list", "--porcelain"])
                .contains("locked krill speculative preparation")
        );
        assert_eq!(pool.request(&request(&f, 4)), Status::Busy);
        if cancel {
            assert_eq!(
                pool.request(&PrepareWorktree {
                    cancel: true,
                    ..req
                }),
                Status::Cancelled
            );
        }
        cleaned(&pool);
        assert!(
            !f.git(&["worktree", "list", "--porcelain"])
                .contains(".krill-preparing")
        );
    }
}

#[test]
fn cancellation_before_start_and_budget_limit_skip_without_checkout() {
    let f = Fixture::new("prep-budget", "");
    let pool = Pool::with_limits(Arc::new(GitQueue::new().unwrap()), &f.root, TTL, 1024).unwrap();
    let req = request(&f, 5);
    pool.request(&PrepareWorktree {
        cancel: true,
        ..req.clone()
    });
    assert_eq!(pool.request(&req), Status::Cancelled);
    let req = request(&f, 6);
    assert_eq!(pool.request(&req), Status::Preparing);
    cleaned(&pool);
    assert_eq!(pool.request(&req), Status::Skipped);
    assert!(!fits(b"100644 blob abc 999999999\tbig\0", DISK_LIMIT));
    assert!(!fits(b"160000 commit abc       -\tsubmodule\0", DISK_LIMIT));
    assert!(fits(b"100644 blob abc      10\tsmall\0", DISK_LIMIT));
}

#[test]
fn restart_recovers_marker_without_touching_created_worktrees() {
    let f = Fixture::new("prep-restart", "");
    let pool = pool(&f);
    let req = request(&f, 7);
    ready(&pool, &req);
    // Model abrupt daemon loss: detach its in-memory lease without running cleanup.
    lock(&pool.state).slot = None;
    pool.changed.notify_all();
    let recovered = Pool::new(Arc::new(GitQueue::new().unwrap()), &f.root).unwrap();
    cleaned(&recovered);
    assert!(
        !f.git(&["worktree", "list", "--porcelain"])
            .contains(".krill-preparing")
    );
}

#[test]
fn restart_removes_registration_even_when_checkout_directory_was_lost() {
    let f = Fixture::new("prep-partial", "");
    let pool = pool(&f);
    ready(&pool, &request(&f, 8));
    lock(&pool.state).slot = None;
    pool.changed.notify_all();
    fs::remove_dir_all(pool.tree()).unwrap();
    assert!(
        f.git(&["worktree", "list", "--porcelain"])
            .contains(".krill-preparing")
    );
    let recovered = Pool::new(Arc::new(GitQueue::new().unwrap()), &f.root).unwrap();
    cleaned(&recovered);
    assert!(
        !f.git(&["worktree", "list", "--porcelain"])
            .contains(".krill-preparing")
    );
}

#[test]
fn filters_are_skipped_without_executing_repository_programs() {
    let f = Fixture::new("prep-filter", "");
    f.git(&["config", "filter.test.smudge", "touch filter-ran"]);
    fs::create_dir_all(f.repo.join("sub")).unwrap();
    fs::write(f.repo.join("sub/.gitattributes"), "*.bin filter=test\n").unwrap();
    f.git(&["add", "."]);
    f.git(&["commit", "-q", "-m", "attributes"]);
    let pool = pool(&f);
    let req = request(&f, 9);
    assert_eq!(pool.request(&req), Status::Preparing);
    cleaned(&pool);
    assert_eq!(pool.request(&req), Status::Skipped);
    assert!(!f.repo.join("filter-ran").exists());
}

// `git lfs install` configures filter.lfs.* for every repository (GitHub macOS runners too).
#[test]
fn configured_but_unused_filters_still_prepare() {
    let f = Fixture::new("prep-unused-filter", "");
    f.git(&["config", "filter.lfs.process", "touch filter-ran"]);
    let pool = pool(&f);
    ready(&pool, &request(&f, 10));
    assert!(!pool.tree().join("filter-ran").exists());
}

#[test]
fn speculative_checkout_does_not_run_checkout_or_fsmonitor_hooks() {
    use std::os::unix::fs::PermissionsExt as _;

    let f = Fixture::new("prep-hooks", "");
    let marker = f.dir.join("hook-ran");
    for hook in ["post-checkout", "fsmonitor-test"] {
        let path = f.repo.join(".git/hooks").join(hook);
        fs::write(&path, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    f.git(&[
        "config",
        "core.fsmonitor",
        f.repo.join(".git/hooks/fsmonitor-test").to_str().unwrap(),
    ]);
    let pool = pool(&f);
    let req = request(&f, 10);
    ready(&pool, &req);
    assert!(!marker.exists());
    pool.request(&PrepareWorktree {
        cancel: true,
        ..req
    });
    cleaned(&pool);
    assert!(!marker.exists());
}

#[test]
fn disk_guard_does_not_follow_symlinks_and_checks_allocated_bytes() {
    let f = Fixture::new("prep-disk", "");
    let target = f.dir.join("outside");
    fs::write(&target, vec![0; 100_000]).unwrap();
    let tree = f.dir.join("guard");
    fs::create_dir(&tree).unwrap();
    std::os::unix::fs::symlink(&target, tree.join("link")).unwrap();
    assert!(disk_usage(&tree, 32 * 1024).is_ok());
    fs::write(tree.join("file"), vec![0; 100_000]).unwrap();
    assert!(disk_usage(&tree, 32 * 1024).is_err());
    assert_eq!(fs::metadata(target).unwrap().len(), 100_000);
}

#[test]
fn in_flight_preparation_promotes_matching_base_and_cancels_mismatched_base() {
    let f = Fixture::new("prep-inflight", "");
    f.git(&["checkout", "-qb", "dev"]);
    fs::write(f.repo.join("dev-only"), "1").unwrap();
    f.git(&["add", "dev-only"]);
    f.git(&["commit", "-qm", "dev head"]);
    f.git(&["checkout", "-q", "main"]);

    let pool = pool(&f);
    // 1. 같은 base("dev")로 선행 체크아웃을 시작하자마자(완료 대기 없이) 생성 요청을 보내도
    //    트랜잭션 잠금 해제 전 ready 기록 덕분에 재체크아웃 없이 즉시 promote(Skipped)된다.
    let same_prep = PrepareWorktree {
        token: 41,
        repo: f.repo.as_os_str().as_bytes().to_vec(),
        base: Some("dev".into()),
        cancel: false,
    };
    assert_eq!(pool.request(&same_prep), Status::Preparing);
    let Outcome::Created { worktree, .. } = create_with_pool(
        &f.queue,
        &NewWorktree {
            repo: same_prep.repo.clone(),
            name: "fast-same".into(),
            base: Some("dev".into()),
            setup: Setup::Skip,
        },
        &f.root,
        &f.trust,
        Some(&pool),
    )
    .unwrap() else {
        panic!("not created");
    };
    assert!(path(&worktree).join("dev-only").exists());
    assert_eq!(pool.request(&same_prep), Status::Skipped);
    cleaned(&pool);

    // 2. 미완료 슬롯이 다른 base(None)를 체크아웃 중일 때 base="dev" 생성 요청이 오면 즉시 취소한다.
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut st = lock(&pool.state);
        st.slot = Some(Slot {
            token: 42,
            req_repo: f.repo.as_os_str().as_bytes().to_vec(),
            req_base: None,
            cancel: cancel_flag.clone(),
            deadline: Instant::now() + TTL,
            ready: None,
        });
    }
    pool.cancel_mismatched_in_flight(f.repo.as_os_str().as_bytes(), Some("dev"));
    assert!(cancel_flag.load(Ordering::Acquire));
    assert_eq!(pool.request(&request(&f, 42)), Status::Cancelled);
}
