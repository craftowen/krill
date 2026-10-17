use super::super::changes::tests::Dir;
use super::*;
use std::thread;

#[test]
fn pacing_debounces_without_starvation_and_has_safety_poll() {
    let start = Instant::now();
    let mut pace = Pace::default();
    pace.event(start);
    assert!(!pace.due(start + DEBOUNCE.checked_sub(Duration::from_millis(1)).unwrap()));
    assert!(pace.due(start + DEBOUNCE));
    pace.start(start + DEBOUNCE);
    for n in 1..35 {
        pace.event(start + Duration::from_millis(n * 100));
    }
    assert!(!pace.due(start + Duration::from_secs(3)));
    assert!(pace.due(start + DEBOUNCE + MIN_INTERVAL));
    pace.start(start + DEBOUNCE + MIN_INTERVAL);
    assert!(
        !pace.due(
            start
                + DEBOUNCE
                + MIN_INTERVAL
                + SAFETY_INTERVAL
                    .checked_sub(Duration::from_millis(1))
                    .unwrap()
        )
    );
    assert!(pace.due(start + DEBOUNCE + MIN_INTERVAL + SAFETY_INTERVAL));
}

#[test]
fn ignored_build_events_and_git_metadata_are_distinguished() {
    let repo = Path::new("/repo");
    let metadata = vec![repo.join(".git"), PathBuf::from("/common/.git")];
    for path in [
        "/repo/node_modules/a",
        "/repo/target/a",
        "/repo/dist/a",
        "/repo/.git/objects/a",
    ] {
        assert!(!relevant(Path::new(path), repo, &metadata));
    }
    for path in [
        "/repo/src/a",
        "/repo/.git/index",
        "/repo/.git/HEAD",
        "/common/.git/packed-refs",
        "/common/.git/worktrees/topic/HEAD",
    ] {
        assert!(relevant(Path::new(path), repo, &metadata));
    }
}

#[test]
fn hidden_watch_never_calls_git_and_hide_cancels_pending_query() {
    let dir = Dir::new("hidden");
    dir.git(&["init", "-q"]);
    let q =
        Arc::new(GitQueue::with_program(dir.script("echo $$ >> calls; exec sleep 30"), 1).unwrap());
    let mut watch = Watch::new(q, dir.0.clone());
    for _ in 0..20 {
        assert!(!watch.poll());
    }
    assert!(!dir.0.join("calls").exists());
    watch.set_visibility(Visibility::Visible).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    // The first query waits for FSEvents registration, which may finish after set_visibility.
    while !dir.0.join("calls").exists() {
        assert!(Instant::now() < deadline);
        watch.poll();
        thread::sleep(Duration::from_millis(5));
    }
    watch.set_visibility(Visibility::Hidden).unwrap();
    let calls = fs::read_to_string(dir.0.join("calls")).unwrap();
    let pid: i32 = calls.trim().parse().unwrap();
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
    );
    for _ in 0..20 {
        assert!(!watch.poll());
    }
    assert_eq!(fs::read_to_string(dir.0.join("calls")).unwrap(), calls);
}

/// K-16: 끝나지 않는 조회가 있어도 숨김 처리는 `Hub` 잠금을 오래 쥐지 않는다.
#[test]
fn hide_does_not_block_on_stuck_query() {
    let dir = Dir::new("hide-stuck");
    dir.git(&["init", "-q"]);
    let mut watch = Watch::new(Arc::new(GitQueue::new().unwrap()), dir.0.clone());
    // 절대 끝나지 않는 대기 중 조회를 흉내낸다(송신 쪽은 살려 둬 recv가 막히게 한다).
    let (tx, rx) = mpsc::channel::<Result<Changes, String>>();
    watch.pending = Some(rx);
    let start = Instant::now();
    watch.set_visibility(Visibility::Hidden).unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "hide blocked the caller on a stuck query"
    );
    drop(tx);
}

#[test]
fn fsevents_updates_saved_file_and_reveal_refreshes_hidden_edits() {
    let dir = Dir::new("events");
    dir.git(&["init", "-q"]);
    fs::write(dir.0.join("file"), "one\n").unwrap();
    dir.git(&["add", "file"]);
    dir.git(&["commit", "-qm", "initial"]);
    let mut watch = Watch::new(Arc::new(GitQueue::new().unwrap()), dir.0.clone());
    watch.set_visibility(Visibility::Visible).unwrap();
    // Reveals include FSEvents registration (over 4s right after a rebuild); saves get 4s.
    wait(&mut watch, SETUP, |c| c.files.is_empty());
    fs::write(dir.0.join("file"), "one\ntwo\n").unwrap();
    wait(&mut watch, SAVE, |c| {
        c.files.iter().any(|f| {
            f.path == b"file"
                && f.unstaged
                    == Some(krill_proto::git::Numstat {
                        added: Some(1),
                        deleted: Some(0),
                    })
        })
    });
    watch.set_visibility(Visibility::Hidden).unwrap();
    fs::write(dir.0.join("file"), "one\ntwo\nthree\n").unwrap();
    watch.set_visibility(Visibility::Visible).unwrap();
    wait(&mut watch, SETUP, |c| {
        c.files
            .iter()
            .any(|f| f.unstaged.is_some_and(|s| s.added == Some(2)))
    });
}

const SAVE: Duration = Duration::from_secs(4);
const SETUP: Duration = Duration::from_secs(20);

fn wait(watch: &mut Watch, within: Duration, matches: impl Fn(&Changes) -> bool) {
    let deadline = Instant::now() + within;
    loop {
        watch.poll();
        if let Some(result) = watch.snapshot()
            && matches(result.as_ref().unwrap())
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no update within {within:?}: {:?}",
            watch.snapshot()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn hub_resolves_subdirectory_omits_unchanged_generation_and_reports_non_git() {
    use krill_proto::git::{GitWatchReply, GitWatchRequest};
    use std::os::unix::ffi::OsStrExt;

    let mut hub = Hub::default();
    let non_git = Dir::new("hub_non_git");
    let deadline = Instant::now() + Duration::from_secs(20);
    let non_git_err = loop {
        let reply = hub.handle(&GitWatchRequest {
            repo: non_git.0.as_os_str().as_bytes().to_vec(),
            visible: true,
            since: 0,
            force: false,
        });
        if let GitWatchReply::Failed { error, .. } = reply {
            break error;
        }
        assert!(
            Instant::now() < deadline,
            "Hub did not report non-git error"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(!non_git_err.is_empty());

    let dir = Dir::new("hub_subdir");
    dir.git(&["init", "-q"]);
    let subdir = dir.0.join("nested/sub");
    fs::create_dir_all(&subdir).unwrap();
    fs::write(subdir.join("hello.rs"), "fn main() {}\n").unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    let (generation, changes) = loop {
        let reply = hub.handle(&GitWatchRequest {
            repo: subdir.as_os_str().as_bytes().to_vec(),
            visible: true,
            since: 0,
            force: false,
        });
        if let GitWatchReply::Changed {
            generation,
            changes,
        } = reply
        {
            break (generation, changes);
        }
        assert!(
            Instant::now() < deadline,
            "Hub did not produce initial snapshot"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(generation > 0);
    assert_eq!(changes.files.len(), 1);

    let unchanged = hub.handle(&GitWatchRequest {
        repo: subdir.as_os_str().as_bytes().to_vec(),
        visible: true,
        since: generation,
        force: false,
    });
    assert!(matches!(unchanged, GitWatchReply::Unchanged { .. }));

    let hidden = hub.handle(&GitWatchRequest {
        repo: subdir.as_os_str().as_bytes().to_vec(),
        visible: false,
        since: generation,
        force: false,
    });
    assert_eq!(hidden, GitWatchReply::Hidden);
}

#[test]
fn hub_keeps_watch_visible_until_all_subscribers_hide() {
    use krill_proto::git::{GitWatchReply, GitWatchRequest};
    use std::os::unix::ffi::OsStrExt;

    let mut hub = Hub::default();
    let dir = Dir::new("hub_refcount");
    dir.git(&["init", "-q"]);
    fs::write(dir.0.join("tracked.rs"), "pub fn one() {}\n").unwrap();
    let repo_bytes = dir.0.as_os_str().as_bytes().to_vec();

    let deadline = Instant::now() + Duration::from_secs(20);
    let gen1 = loop {
        let reply = hub.handle_for(
            "krill-app-sidebar",
            &GitWatchRequest {
                repo: repo_bytes.clone(),
                visible: true,
                since: 0,
                force: false,
            },
        );
        if let GitWatchReply::Changed { generation, .. } = reply {
            break generation;
        }
        assert!(Instant::now() < deadline, "sidebar initial watch timed out");
        thread::sleep(Duration::from_millis(20));
    };

    // Diff subscribes to the same repo and immediately receives the cached snapshot.
    let diff_reply = hub.handle_for(
        "krill-app-diff",
        &GitWatchRequest {
            repo: repo_bytes.clone(),
            visible: true,
            since: 0,
            force: false,
        },
    );
    assert!(matches!(diff_reply, GitWatchReply::Changed { generation, .. } if generation == gen1));

    // Hiding diff must NOT hide the underlying watch while sidebar is still subscribed.
    let hidden = hub.handle_for(
        "krill-app-diff",
        &GitWatchRequest {
            repo: repo_bytes.clone(),
            visible: false,
            since: gen1,
            force: false,
        },
    );
    assert_eq!(hidden, GitWatchReply::Hidden);
    assert!(
        hub.entries
            .values()
            .all(|e| e.watch.visible && e.subscribers.contains_key("krill-app-sidebar"))
    );

    // Hiding sidebar (last subscriber) hides the underlying watch.
    let hidden_last = hub.handle_for(
        "krill-app-sidebar",
        &GitWatchRequest {
            repo: repo_bytes,
            visible: false,
            since: gen1,
            force: false,
        },
    );
    assert_eq!(hidden_last, GitWatchReply::Hidden);
    assert!(hub.entries.values().all(|e| !e.watch.visible));
}
