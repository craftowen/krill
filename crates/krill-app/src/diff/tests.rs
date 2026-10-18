use super::*;
use gpui::{Modifiers, MouseButton, ScrollStrategy};
use krill_proto::git::Numstat;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mk054-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "-q"]);
        repo
    }
    fn plain() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mk054-plain-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn git(&self, args: &[&str]) {
        assert!(
            Command::new("git")
                .current_dir(&self.0)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    fn file(path: &[u8], status: [u8; 2]) -> ChangedFile {
        ChangedFile {
            path: path.to_vec(),
            old_path: None,
            status,
            staged: None,
            unstaged: None,
        }
    }
    fn script(&self, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = self.0.join("fake-git.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn index_worktree_untracked_and_literal_paths() {
    let repo = Repo::new();
    let name = "한🙂[a].rs";
    std::fs::write(repo.0.join(name), "fn old() {}\n").unwrap();
    repo.git(&["add", "--", name]);
    let f = Repo::file(name.as_bytes(), *b"AM");
    let staged = patch(&repo.0, &f, Comparison::Index).unwrap();
    assert_eq!(staged[0].additions(), 1);
    assert_eq!(staged[0].deletions(), 0);
    std::fs::write(repo.0.join(name), "fn new() {}\n").unwrap();
    let working = patch(&repo.0, &f, Comparison::Worktree).unwrap();
    assert_eq!(working[0].additions(), 1);
    assert_eq!(working[0].deletions(), 1);
    std::fs::write(repo.0.join("new.rs"), "fn added() {}\n").unwrap();
    let f = Repo::file(b"new.rs", *b"??");
    assert_eq!(
        patch(&repo.0, &f, Comparison::Worktree).unwrap()[0].additions(),
        1
    );
    assert!(patch(&repo.0, &f, Comparison::Index).unwrap().is_empty());
}

#[test]
fn binary_invalid_encoding_path_and_patch_cap_are_explicit() {
    let repo = Repo::new();
    std::fs::write(repo.0.join("binary"), b"\0\xff").unwrap();
    let f = Repo::file(b"binary", *b"??");
    assert!(patch(&repo.0, &f, Comparison::Worktree).unwrap()[0].is_binary());
    let invalid = OsStr::from_bytes(b"bad\xff.rs");
    let f = Repo::file(invalid.as_bytes(), *b"??");
    // APFS rejects this name with EILSEQ; the query must return an error, not panic.
    assert!(patch(&repo.0, &f, Comparison::Worktree).is_err());
    assert!(
        patch(
            &repo.0,
            &Repo::file(b"../outside", *b"??"),
            Comparison::Worktree
        )
        .is_err()
    );
    std::fs::write(repo.0.join("huge"), vec![b'a'; MAX_PATCH_BYTES + 1]).unwrap();
    let error = patch(&repo.0, &Repo::file(b"huge", *b"??"), Comparison::Worktree)
        .err()
        .unwrap();
    assert!(error.to_string().contains("4 MiB"));
}

/// AS-06: EUC-KR 텍스트 파일도 통째로 거부하지 않고 손실 변환으로 보여준다.
#[test]
fn non_utf8_text_patch_is_lossy_not_rejected() {
    let repo = Repo::new();
    // "한글\n"의 EUC-KR 바이트열(UTF-8로 디코드 실패, 위치 124 근처).
    std::fs::write(repo.0.join("euc.txt"), [0xc7, 0xd1, 0xb1, 0xdb, 0x0a]).unwrap();
    let files = patch(
        &repo.0,
        &Repo::file(b"euc.txt", *b"??"),
        Comparison::Worktree,
    )
    .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].additions(), 1);
}

/// AS-05: 목록이 같아도 선택 파일은 다시 받는다(미추적 numstat 없음·같은 줄 수정·수동 새로고침).
#[test]
fn watch_action_reselects_selected_file_even_when_list_is_same() {
    use super::{WatchAction, watch_action};
    use krill_proto::git::Changes;
    let file = |path: &[u8]| ChangedFile {
        path: path.to_vec(),
        old_path: None,
        status: *b" M",
        staged: None,
        unstaged: None,
    };
    let changes = || Changes {
        files: vec![file(b"a.rs"), file(b"b.rs")],
        limited: false,
    };
    let same = changes();
    // 첫 로딩·목록 변경·선택 유지 → 다시 선택(패치 재조회).
    assert_eq!(
        watch_action(false, None, &Changes::default(), &same, Some(b"a.rs")),
        WatchAction::Reselect(0)
    );
    assert_eq!(
        watch_action(true, None, &same, &changes(), Some(b"b.rs")),
        WatchAction::Reselect(1)
    );
    // 목록이 같고 선택이 없으면 무시(불필요한 재조회 없음).
    assert_eq!(
        watch_action(true, None, &same, &changes(), None),
        WatchAction::Ignore
    );
    // 선택 파일이 목록에서 사라지면 비운다.
    let mut gone = changes();
    gone.files.remove(0);
    assert_eq!(
        watch_action(true, None, &same, &gone, Some(b"a.rs")),
        WatchAction::Reset
    );
}

/// AS-26: 패치 실패 문구는 카탈로그 한글로 보인다(원문 영어는 `patch`에 유지).
#[test]
fn patch_error_message_is_korean_catalog() {
    let too_large = io::Error::other("diff exceeds 4 MiB limit");
    assert_eq!(
        patch_error_message(&too_large),
        "diff가 4 MiB 한도를 넘었습니다"
    );
    let timeout = io::Error::new(io::ErrorKind::TimedOut, "git diff timed out");
    assert_eq!(
        patch_error_message(&timeout),
        "git diff 시간이 초과됐습니다"
    );
    let other = io::Error::other("git diff failed");
    assert!(patch_error_message(&other).contains("git diff failed"));
    assert!(patch_error_message(&other).starts_with("diff를 읽지 못했습니다"));
}

#[test]
fn status_codes_and_abbreviated_paths_match_orca_conventions() {
    assert_eq!(status_code(*b"??", Comparison::Worktree), "U");
    assert_eq!(status_code(*b".M", Comparison::Worktree), "M");
    assert_eq!(status_code(*b"A.", Comparison::Index), "A");
    assert_eq!(status_code(*b".D", Comparison::Worktree), "D");
    assert_eq!(status_code(*b"R.", Comparison::Index), "R");
    assert_eq!(status_code(*b"C.", Comparison::Index), "C");
    assert_eq!(status_code(*b"UU", Comparison::Worktree), "!");

    assert_eq!(
        abbreviate_path(b"Cargo.toml", None),
        ("Cargo.toml".to_owned(), None)
    );
    assert_eq!(
        abbreviate_path(b"src/diff.rs", None),
        ("diff.rs".to_owned(), Some("src".to_owned()))
    );
    assert_eq!(
        abbreviate_path(b"crates/krill-app/src/ui/theme.rs", None),
        ("theme.rs".to_owned(), Some("crates/…/ui".to_owned()))
    );
    assert_eq!(
        abbreviate_path(
            b"crates/krill-app/src/new.rs",
            Some(b"crates/krill-app/src/old.rs")
        ),
        (
            "new.rs".to_owned(),
            Some("crates/krill-app/src · ← old.rs".to_owned())
        )
    );
}

#[gpui::test]
fn five_thousand_lines_keep_source_positions(cx: &mut gpui::TestAppContext) {
    let mut patch =
        String::from("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -0,0 +1,5000 @@\n");
    for i in 0..5000 {
        writeln!(patch, "+let value_{i} = \"한🙂\";").unwrap();
    }
    let files = DiffFile::parse(&patch).unwrap();
    assert_eq!(files[0].additions(), 5000);
    let state = cx.new(|cx| DiffState::new(files, cx).with_inline_unit(None));
    state.update(cx, |state, cx| {
        state.set_selected_lines(
            Some(gpui_component::diff::DiffLineRange::new(
                "a.rs",
                gpui_component::diff::DiffSide::Modified,
                5000,
                5000,
            )),
            cx,
        );
        assert_eq!(state.selected_text(cx), "let value_4999 = \"한🙂\";\n");
    });
}

#[gpui::test]
fn panel_auto_refreshes_within_4s_when_visible(cx: &mut gpui::TestAppContext) {
    let repo = Repo::new();
    repo.git(&["config", "user.email", "test@example.com"]);
    repo.git(&["config", "user.name", "Test"]);
    std::fs::write(repo.0.join("tracked.txt"), "v1\n").unwrap();
    repo.git(&["add", "--", "tracked.txt"]);
    repo.git(&["commit", "-q", "-m", "init"]);

    cx.update(gpui_component::init);
    let repo_path = repo.0.clone();
    let (view, cx) = cx.add_window_view(|_, cx| Panel::new(repo_path, cx));
    // The 4s budget starts from a saved edit under an established watch. The first snapshot waits
    // for FSEvents registration, which took over 4s right after a rebuild.
    let deadline = Instant::now() + Duration::from_secs(20);
    while cx.update(|_, cx| view.read(cx).watch_gen) == 0 {
        assert!(Instant::now() < deadline, "initial snapshot should load");
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }

    std::fs::write(repo.0.join("tracked.txt"), "v2\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut found = false;
    while Instant::now() < deadline {
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        if cx.update(|_, cx| {
            view.read(cx)
                .changes
                .files
                .iter()
                .any(|f| f.path == b"tracked.txt")
        }) {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        found,
        "visible diff::Panel must auto-refresh within 4s via FSEvents watch without manual refresh()"
    );
}

#[gpui::test]
fn uds_ipc_watch_and_hidden_panel_spawns_zero_git_children(cx: &mut gpui::TestAppContext) {
    // 1. Real UDS socket server (`krilld::Registry`) serving `Request::GitWatch`.
    let repo = Repo::new();
    repo.git(&["config", "user.email", "test@example.com"]);
    repo.git(&["config", "user.name", "Test"]);
    std::fs::write(repo.0.join("ipc.txt"), "one\n").unwrap();
    repo.git(&["add", "--", "ipc.txt"]);
    repo.git(&["commit", "-q", "-m", "init"]);

    let sock_dir = Repo::plain();
    let socket = sock_dir.0.join("krilld.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let registry = Arc::new(krilld::registry::Registry::new(None, None));
    let reg = Arc::clone(&registry);
    std::thread::spawn(move || {
        let hello = ipc::ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features: ipc::FEATURES,
            build: "test".into(),
            instance_id: 1,
        };
        while let Ok((stream, _)) = listener.accept() {
            let reg = Arc::clone(&reg);
            let hello = hello.clone();
            std::thread::spawn(move || {
                reg.serve(stream, &hello).ok();
            });
        }
    });
    let paths = Paths {
        socket,
        holder: PathBuf::new(),
        krilld: PathBuf::new(),
    };

    // Edit before connecting: the first UDS snapshot must carry it. Racing the edit against watch
    // setup made this flaky in parallel runs (FSEvents registration stalled 5.6-6.5s while other
    // watcher tests churned fseventsd). The 4s save-to-refresh budget is
    // `panel_auto_refreshes_within_4s_when_visible`'s job.
    std::fs::write(repo.0.join("ipc.txt"), "one\ntwo\n").unwrap();
    cx.update(gpui_component::init);
    let repo_path = repo.0.clone();
    let (view, cx) = cx.add_window_view(|_, cx| {
        let mut panel = Panel::unconnected(repo_path, cx);
        panel.connect(paths, cx);
        panel
    });

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut updated = false;
    while Instant::now() < deadline {
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        if cx.update(|_, cx| {
            view.read(cx)
                .changes
                .files
                .iter()
                .any(|f| f.path == b"ipc.txt")
        }) {
            updated = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(updated, "UDS FEAT_GIT_WATCH must deliver file changes");

    // 2. Hiding the panel cancels and reaps in-flight git children and spawns 0 git children while hidden.
    let slow_repo = Repo::new();
    let q = Arc::new(
        krilld::git::GitQueue::with_program(slow_repo.script("echo $$ >> calls; exec sleep 30"), 1)
            .unwrap(),
    );
    let slow_source = Arc::new(TestHubSource(Mutex::new(
        krilld::git::watch::Hub::with_queue(q),
    )));
    let slow_path = slow_repo.0.clone();
    cx.update(|_, cx| {
        view.update(cx, |panel, cx| {
            panel.set_repo(slow_path, cx);
            panel.connect_source(slow_source, cx);
        });
    });
    cx.run_until_parked();
    let deadline = Instant::now() + Duration::from_secs(20);
    // Keep polling: the first query waits for FSEvents registration.
    while !slow_repo.0.join("calls").exists() {
        assert!(Instant::now() < deadline, "visible watch should start git");
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    cx.update(|_, cx| {
        view.update(cx, |panel, cx| panel.set_visible(false, cx));
    });
    cx.run_until_parked();
    let calls = std::fs::read_to_string(slow_repo.0.join("calls")).unwrap();
    let pid: i32 = calls.trim().parse().unwrap();
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err(),
        "hiding panel must cancel and reap in-flight git child"
    );
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
    }
    assert_eq!(
        std::fs::read_to_string(slow_repo.0.join("calls")).unwrap(),
        calls,
        "hidden panel must spawn 0 git children"
    );
}

#[gpui::test]
fn virtualized_500_files_and_zero_idle_renders(cx: &mut gpui::TestAppContext) {
    struct StaticSource(Changes);
    impl Source for StaticSource {
        fn watch(&self, req: &GitWatchRequest) -> io::Result<GitWatchReply> {
            if !req.visible {
                return Ok(GitWatchReply::Hidden);
            }
            if req.since == 0 || req.force {
                Ok(GitWatchReply::Changed {
                    generation: 1,
                    changes: self.0.clone(),
                })
            } else {
                Ok(GitWatchReply::Unchanged { pending: false })
            }
        }
    }

    let files: Vec<ChangedFile> = (0..500)
        .map(|i| ChangedFile {
            path: format!("crates/krill-app/src/mod_{i:03}/file_{i:03}.rs").into_bytes(),
            old_path: None,
            status: *b".M",
            staged: None,
            unstaged: Some(Numstat {
                added: Some(i + 1),
                deleted: Some(i % 7),
            }),
        })
        .collect();
    let changes = Changes {
        files,
        limited: false,
    };

    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, cx| {
        let mut p = Panel::unconnected(PathBuf::from("/repo"), cx);
        p.connect_source(Arc::new(StaticSource(changes)), cx);
        p
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).changes.files.len()), 500);
    assert!(
        cx.update(|_, cx| view.read(cx).rendered_rows) < 100,
        "uniform_list must virtualize 500 file rows"
    );

    let before = cx.update(|_, cx| view.read(cx).renders);
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }
    assert_eq!(
        cx.update(|_, cx| view.read(cx).renders),
        before,
        "unchanged watch polls must cause 0 idle renders"
    );

    let mut max_rows = 0;
    for i in 0..120 {
        cx.update(|_, cx| {
            view.update(cx, |p, cx| {
                p.rendered_rows = 0;
                p.scroll.scroll_to_item((i * 7) % 500, ScrollStrategy::Top);
                cx.notify();
            });
        });
        cx.run_until_parked();
        max_rows = max_rows.max(cx.update(|_, cx| view.read(cx).rendered_rows));
    }
    assert!(max_rows < 100, "scrolling 500 rows must stay virtualized");
}

#[gpui::test]
fn empty_states_selection_and_collapse_button_work(cx: &mut gpui::TestAppContext) {
    use std::cell::Cell;
    use std::rc::Rc;

    let not_git = Repo::plain();
    let git_repo = Repo::new();
    git_repo.git(&["config", "user.email", "test@example.com"]);
    git_repo.git(&["config", "user.name", "Test"]);

    cx.update(gpui_component::init);
    let not_git_path = not_git.0.clone();
    let (view, cx) = cx.add_window_view(|_, cx| Panel::new(not_git_path, cx));
    cx.run_until_parked();

    // 1. Non-git directory shows `diff-empty-not-git`.
    assert!(cx.debug_bounds("diff-empty-not-git").is_some());

    // 2. Switching to clean git repo shows `diff-empty-clean`.
    let git_path = git_repo.0.clone();
    cx.update(|_, cx| view.update(cx, |p, cx| p.set_repo(git_path, cx)));
    let deadline = Instant::now() + Duration::from_secs(20);
    while cx.debug_bounds("diff-empty-clean").is_none() {
        assert!(Instant::now() < deadline);
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(cx.debug_bounds("diff-empty-clean").is_some());

    // 3. Adding a file transitions to file list + `diff-empty-unselected`, and clicking the file opens `diff-viewer`.
    std::fs::write(git_repo.0.join("hello.rs"), "pub fn hi() {}\n").unwrap();
    cx.update(|_, cx| view.update(cx, Panel::refresh));
    let deadline = Instant::now() + Duration::from_secs(20);
    while cx.debug_bounds("changed-file-0").is_none() {
        assert!(Instant::now() < deadline);
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(cx.debug_bounds("diff-empty-unselected").is_some());

    let row = cx.debug_bounds("changed-file-0").unwrap();
    cx.simulate_mouse_down(row.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(row.center(), MouseButton::Left, Modifiers::default());
    let deadline = Instant::now() + Duration::from_secs(20);
    while cx.debug_bounds("diff-viewer").is_none() {
        assert!(Instant::now() < deadline);
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(cx.debug_bounds("diff-viewer").is_some());

    // 4. Clicking `diff-collapse` emits `ToggleCollapse`.
    let collapsed = Rc::new(Cell::new(false));
    let flag = collapsed.clone();
    let _sub =
        cx.update(|_, cx| cx.subscribe(&view, move |_, _: &ToggleCollapse, _| flag.set(true)));
    let btn = cx.debug_bounds("diff-collapse").unwrap();
    cx.simulate_mouse_down(btn.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(btn.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert!(collapsed.get());
}

#[test]
fn explorer_lazy_tree_filter_toggles_and_crud() {
    let repo = Repo::new();
    std::fs::write(repo.0.join(".gitignore"), "ignored.log\n").unwrap();
    std::fs::write(repo.0.join(".env"), "SECRET=1\n").unwrap();
    std::fs::write(repo.0.join("ignored.log"), "log\n").unwrap();
    std::fs::create_dir_all(repo.0.join("src/nested")).unwrap();
    std::fs::write(repo.0.join("src/lib.rs"), "pub mod nested;\n").unwrap();
    std::fs::write(repo.0.join("src/nested/deep.rs"), "pub fn ok() {}\n").unwrap();

    let mut ex = Explorer::new(repo.0.clone());
    let tmp_home = std::env::temp_dir().join(format!(
        "krill-explorer-trash-home-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::remove_dir_all(&tmp_home).ok();
    std::fs::create_dir_all(&tmp_home).unwrap();
    ex.set_home(tmp_home.clone());

    assert!(ex.dir_cache.is_empty(), "lazy before ensure_loaded");
    ex.ensure_loaded();
    assert!(ex.dir_cache.contains_key(&repo.0));
    assert!(
        !ex.dir_cache.contains_key(&repo.0.join("src")),
        "unexpanded subfolder must not be loaded eagerly"
    );

    // Folders come first, then files alphabetically.
    assert_eq!(ex.rows[0].relative_path, "src");
    assert!(ex.rows[0].is_dir);
    assert!(ex.rows.iter().any(|r| r.relative_path == ".env"));
    assert!(
        ex.rows
            .iter()
            .any(|r| r.relative_path == "ignored.log" && r.ignored)
    );

    // Toggle dotfiles and gitignored files.
    ex.toggle_dotfiles();
    assert!(!ex.rows.iter().any(|r| r.relative_path == ".env"));
    ex.toggle_dotfiles();
    assert!(ex.rows.iter().any(|r| r.relative_path == ".env"));

    ex.toggle_ignored();
    assert!(!ex.rows.iter().any(|r| r.relative_path == "ignored.log"));
    ex.toggle_ignored();
    assert!(ex.rows.iter().any(|r| r.relative_path == "ignored.log"));

    // Expanding `src` lazily loads `src` children.
    ex.toggle_dir(&repo.0.join("src"));
    assert!(ex.dir_cache.contains_key(&repo.0.join("src")));
    assert!(ex.rows.iter().any(|r| r.relative_path == "src/lib.rs"));

    // Real-time filter finds `deep.rs` and projects ancestor chain `src` -> `src/nested` -> `src/nested/deep.rs`.
    ex.set_filter_query("deep");
    let rels: Vec<&str> = ex.rows.iter().map(|r| r.relative_path.as_str()).collect();
    assert_eq!(rels, vec!["src", "src/nested", "src/nested/deep.rs"]);
    ex.set_filter_query("");

    // CRUD: create file, create folder, rename, move to trash (~/.Trash).
    let created_file = ex.create_file(&repo.0.join("src"), "created.rs").unwrap();
    std::fs::write(&created_file, "// keep me\n").unwrap();
    assert!(created_file.is_file());
    assert!(ex.rows.iter().any(|r| r.relative_path == "src/created.rs"));

    let created_dir = ex.create_folder(&repo.0.join("src"), "new_mod").unwrap();
    std::fs::write(created_dir.join("mod.rs"), "pub const X: u8 = 1;\n").unwrap();
    assert!(created_dir.is_dir());
    assert!(ex.rows.iter().any(|r| r.relative_path == "src/new_mod"));

    let renamed = ex.rename_path(&created_file, "renamed.rs").unwrap();
    assert!(!created_file.exists());
    assert!(renamed.is_file());
    assert!(ex.rows.iter().any(|r| r.relative_path == "src/renamed.rs"));

    let trashed_file = ex.delete_path(&renamed).unwrap();
    assert!(!renamed.exists());
    assert_eq!(trashed_file, tmp_home.join(".Trash").join("renamed.rs"));
    assert_eq!(
        std::fs::read_to_string(&trashed_file).unwrap(),
        "// keep me\n"
    );
    assert!(!ex.rows.iter().any(|r| r.relative_path == "src/renamed.rs"));

    // Name collision in ~/.Trash appends ` 2`.
    let again = ex.create_file(&repo.0.join("src"), "renamed.rs").unwrap();
    let trashed_again = ex.delete_path(&again).unwrap();
    assert!(!again.exists());
    assert_eq!(trashed_again, tmp_home.join(".Trash").join("renamed.rs 2"));

    let trashed_dir = ex.delete_path(&created_dir).unwrap();
    assert!(!created_dir.exists());
    assert_eq!(trashed_dir, tmp_home.join(".Trash").join("new_mod"));
    assert!(trashed_dir.join("mod.rs").is_file());

    // Security: worktree root and outside paths are rejected.
    assert_eq!(
        ex.delete_path(&repo.0).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        ex.delete_path(&tmp_home).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(ex.create_file(&repo.0, "../escape.rs").is_err());
    assert!(
        ex.rename_path(&repo.0.join("src/lib.rs"), "../escape.rs")
            .is_err()
    );

    // Security (OWE-762): symlink directories pointing outside the worktree cannot be escaped via
    // create_file, create_folder, or rename_path, and circular/external symlinks do not explode filter scan.
    let outside = tmp_home.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "secret\n").unwrap();
    let ext_link = repo.0.join("ext_link");
    std::os::unix::fs::symlink(&outside, &ext_link).unwrap();
    let loop_link = repo.0.join("loop");
    std::os::unix::fs::symlink(&repo.0, &loop_link).unwrap();

    assert_eq!(
        ex.create_file(&ext_link, "escape.txt").unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        ex.create_file(&repo.0, "ext_link/escape.txt")
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        ex.create_file(&repo.0, "ext_link/sub/escape.txt")
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(!outside.join("escape.txt").exists());
    assert!(!outside.join("sub").exists());

    assert_eq!(
        ex.create_folder(&ext_link, "escape_dir")
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        ex.create_folder(&repo.0, "ext_link/escape_dir")
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(!outside.join("escape_dir").exists());

    assert_eq!(
        ex.rename_path(&ext_link.join("secret.txt"), "pwned.txt")
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(outside.join("secret.txt").is_file());
    assert!(!outside.join("pwned.txt").exists());

    ex.refresh();
    let root_entries = ex.dir_cache.get(&repo.0).unwrap();
    assert!(
        root_entries
            .iter()
            .any(|e| e.relative_path == "ext_link" && e.is_symlink)
    );
    assert!(
        root_entries
            .iter()
            .any(|e| e.relative_path == "loop" && e.is_symlink)
    );

    ex.set_filter_query("deep");
    let filtered_rels: Vec<&str> = ex.rows.iter().map(|r| r.relative_path.as_str()).collect();
    assert_eq!(
        filtered_rels,
        vec!["src", "src/nested", "src/nested/deep.rs"]
    );
    let cached = ex.filter_files_cache.as_ref().unwrap();
    assert!(
        !cached.iter().any(|e| e.relative_path.starts_with("loop/")),
        "recursive filter scan must not descend into self-referential symlink"
    );
    assert!(
        !cached
            .iter()
            .any(|e| e.relative_path.starts_with("ext_link/")),
        "recursive filter scan must not descend into external symlink directory"
    );
    ex.set_filter_query("");

    // If moving to ~/.Trash fails, never fall back to permanent deletion and record error.
    let bad_home = tmp_home.join("bad-home");
    std::fs::create_dir_all(&bad_home).unwrap();
    std::fs::write(bad_home.join(".Trash"), "not a directory").unwrap();
    ex.set_home(bad_home);
    let keep_path = repo.0.join("src/lib.rs");
    assert!(ex.delete_path(&keep_path).is_err());
    assert!(
        keep_path.is_file(),
        "must not permanently delete on trash failure"
    );
    assert!(ex.error.is_some());

    std::fs::remove_dir_all(&tmp_home).ok();
}

#[gpui::test]
fn file_explorer_virtualized_tree_and_editor_open_work(cx: &mut gpui::TestAppContext) {
    use std::cell::RefCell;
    use std::rc::Rc;

    let repo = Repo::new();
    let tmp_home = std::env::temp_dir().join(format!(
        "krill-explorer-ui-trash-home-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::remove_dir_all(&tmp_home).ok();
    std::fs::create_dir_all(&tmp_home).unwrap();

    std::fs::create_dir_all(repo.0.join("src")).unwrap();
    std::fs::write(repo.0.join("src/alpha.rs"), "pub fn alpha() {}\n").unwrap();
    for i in 0..200 {
        std::fs::write(repo.0.join(format!("file_{i:03}.txt")), "x\n").unwrap();
    }

    cx.update(gpui_component::init);
    let repo_path = repo.0.clone();
    let home_for_panel = tmp_home.clone();
    let (view, cx) = cx.add_window_view(|_, cx| {
        let mut p = Panel::unconnected(repo_path, cx);
        p.set_explorer_home(home_for_panel);
        p
    });
    cx.run_until_parked();

    let opened: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));
    let opened_hook = opened.clone();
    let _sub = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, e: &OpenFile, _| {
            opened_hook.borrow_mut().push(e.0.clone());
        })
    });

    // Click `diff-tab-files` to switch to File Explorer tab.
    let files_tab = cx.debug_bounds("diff-tab-files").unwrap();
    cx.simulate_mouse_down(files_tab.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(files_tab.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        cx.update(|_, cx| view.read(cx).active_tab()),
        PanelTab::Files
    );
    assert!(cx.debug_bounds("explorer-pane").is_some());
    assert!(cx.debug_bounds("explorer-toolbar").is_some());
    assert_eq!(cx.update(|_, cx| view.read(cx).explorer_rows().len()), 201);
    assert!(
        cx.update(|_, cx| view.read(cx).explorer.rendered_rows) < 60,
        "uniform_list must virtualize 200+ explorer rows"
    );

    // Click row 0 (`src` directory) to expand it, then click `src/alpha.rs` to open in editor.
    let src_row = cx.debug_bounds("explorer-row-0").unwrap();
    cx.simulate_mouse_down(src_row.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(src_row.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();

    let alpha_row = cx.debug_bounds("explorer-item-src/alpha.rs").unwrap();
    cx.simulate_mouse_down(alpha_row.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(alpha_row.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();

    assert_eq!(opened.borrow().as_slice(), &[repo.0.join("src/alpha.rs")]);

    // Requesting delete opens the confirmation dialog with default focus on Cancel.
    let alpha_path = repo.0.join("src/alpha.rs");
    cx.update(|window, cx| {
        view.update(cx, |p, cx| {
            p.request_explorer_delete(&alpha_path, window, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("explorer-delete-dialog").is_some());
    let (pending_name, pending_is_dir, cancel_focused) = cx.update(|window, cx| {
        let p = view.read(cx);
        let d = p.pending_explorer_delete().unwrap();
        (d.name.clone(), d.is_dir, d.cancel_focus.is_focused(window))
    });
    assert_eq!(pending_name, "alpha.rs");
    assert!(!pending_is_dir);
    assert!(cancel_focused, "Cancel button must hold default focus");

    // Clicking Cancel closes the dialog and leaves `src/alpha.rs` untouched.
    let cancel_btn = cx.debug_bounds("explorer-delete-cancel").unwrap();
    cx.simulate_click(cancel_btn.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("explorer-delete-dialog").is_none());
    assert!(alpha_path.is_file());

    // Requesting delete again and clicking '휴지통으로 이동' moves `src/alpha.rs` to HOME/.Trash.
    cx.update(|window, cx| {
        view.update(cx, |p, cx| {
            p.request_explorer_delete(&alpha_path, window, cx);
        });
    });
    cx.run_until_parked();
    let confirm_btn = cx.debug_bounds("explorer-delete-confirm").unwrap();
    cx.simulate_click(confirm_btn.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("explorer-delete-dialog").is_none());
    assert!(!alpha_path.exists());
    assert!(tmp_home.join(".Trash").join("alpha.rs").is_file());

    // Requesting delete on folder `src` shows folder dialog (`is_dir == true`) and moves folder to HOME/.Trash.
    let src_dir = repo.0.join("src");
    cx.update(|window, cx| {
        view.update(cx, |p, cx| p.request_explorer_delete(&src_dir, window, cx));
    });
    cx.run_until_parked();
    let (dir_name, dir_is_dir, dir_cancel_focused) = cx.update(|window, cx| {
        let p = view.read(cx);
        let d = p.pending_explorer_delete().unwrap();
        (d.name.clone(), d.is_dir, d.cancel_focus.is_focused(window))
    });
    assert_eq!(dir_name, "src");
    assert!(dir_is_dir);
    assert!(dir_cancel_focused);
    let confirm_dir_btn = cx.debug_bounds("explorer-delete-confirm").unwrap();
    cx.simulate_click(confirm_dir_btn.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(!src_dir.exists());
    assert!(tmp_home.join(".Trash").join("src").is_dir());

    // `open_diff_file` switches to `PanelTab::Changes` with requested comparison and file selection.
    let r = repo.0.clone();
    cx.update(|_, cx| {
        view.update(cx, |p, cx| p.open_diff_file(r, "file_000.txt", true, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).active_tab()),
        PanelTab::Changes
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).comparison()),
        Comparison::Index
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).selected_path()),
        Some("file_000.txt".to_owned())
    );

    std::fs::remove_dir_all(&tmp_home).ok();
}

#[test]
fn workspace_search_engine_options_and_globs_work() {
    let repo = Repo::new();
    std::fs::create_dir_all(repo.0.join("src")).unwrap();
    std::fs::create_dir_all(repo.0.join("dist")).unwrap();

    std::fs::write(
        repo.0.join("src/alpha.rs"),
        "pub fn alpha_helper() {}\npub fn Alpha() {\n    // 한글 주석 alpha TargetToken 끝\n}\n",
    )
    .unwrap();
    std::fs::write(
        repo.0.join("src/alpha.test.rs"),
        "#[test]\nfn test_alpha() {\n    alpha();\n}\n",
    )
    .unwrap();
    std::fs::write(
        repo.0.join("dist/bundle.js"),
        "function alpha() { return 'alpha'; }\n",
    )
    .unwrap();
    std::fs::write(repo.0.join("src/binary.bin"), b"alpha\0binary\n").unwrap();

    for run_fn in [search_worktree, search_worktree_fallback] {
        // 1. Default case-insensitive substring search finds matches in text files and skips binary/.git.
        let base_opts = SearchOptions {
            query: "alpha".to_owned(),
            ..SearchOptions::default()
        };
        let res = run_fn(&repo.0, &base_opts).unwrap();
        assert_eq!(res.files.len(), 3);
        assert!(
            res.files
                .iter()
                .all(|f| f.relative_path != "src/binary.bin")
        );

        // 2. Case-sensitive (`Aa`): `"Alpha"` only matches line 2 of `src/alpha.rs`.
        let case_opts = SearchOptions {
            query: "Alpha".to_owned(),
            case_sensitive: true,
            ..SearchOptions::default()
        };
        let case_res = run_fn(&repo.0, &case_opts).unwrap();
        assert_eq!(case_res.files.len(), 1);
        assert_eq!(case_res.files[0].relative_path, "src/alpha.rs");
        assert_eq!(case_res.files[0].matches.len(), 1);
        assert_eq!(case_res.files[0].matches[0].line, 2);
        assert_eq!(case_res.files[0].matches[0].column, 8);

        // 3. Whole-word (`ab`): `"alpha"` + case_sensitive matches standalone `alpha`, not `alpha_helper` or `Alpha`.
        let word_opts = SearchOptions {
            query: "alpha".to_owned(),
            case_sensitive: true,
            whole_word: true,
            ..SearchOptions::default()
        };
        let word_res = run_fn(&repo.0, &word_opts).unwrap();
        let alpha_file = word_res
            .files
            .iter()
            .find(|f| f.relative_path == "src/alpha.rs")
            .unwrap();
        assert_eq!(alpha_file.matches.len(), 1);
        assert_eq!(alpha_file.matches[0].line, 3);

        // 4. Regex (`.*`): `pub fn [A-Za-z_]+` matches lines 1 and 2 of `src/alpha.rs`.
        let regex_opts = SearchOptions {
            query: r"pub fn [A-Za-z_]+".to_owned(),
            use_regex: true,
            ..SearchOptions::default()
        };
        let regex_res = run_fn(&repo.0, &regex_opts).unwrap();
        assert_eq!(regex_res.files.len(), 1);
        assert_eq!(regex_res.files[0].matches.len(), 2);

        // Invalid regex returns Err.
        let bad_regex = SearchOptions {
            query: "[unclosed".to_owned(),
            use_regex: true,
            ..SearchOptions::default()
        };
        assert!(run_fn(&repo.0, &bad_regex).is_err());

        // 5. Include & Exclude globs (`*.rs` include, `*.test.rs, dist/**` exclude).
        let glob_opts = SearchOptions {
            query: "alpha".to_owned(),
            include_pattern: "*.rs".to_owned(),
            exclude_pattern: "*.test.rs, dist/**".to_owned(),
            ..SearchOptions::default()
        };
        let glob_res = run_fn(&repo.0, &glob_opts).unwrap();
        assert_eq!(glob_res.files.len(), 1);
        assert_eq!(glob_res.files[0].relative_path, "src/alpha.rs");

        // 6. Multi-byte UTF-8 column & highlight split accuracy.
        let utf8_opts = SearchOptions {
            query: "TargetToken".to_owned(),
            case_sensitive: true,
            ..SearchOptions::default()
        };
        let utf8_res = run_fn(&repo.0, &utf8_opts).unwrap();
        assert_eq!(utf8_res.files.len(), 1);
        let hit = &utf8_res.files[0].matches[0];
        assert_eq!(hit.line, 3);
        // "    // 한글 주석 alpha " is 19 chars -> TargetToken starts at 1-based char column 20.
        assert_eq!(hit.column, 20);
        assert_eq!(hit.match_length, 11);
        let (before, matched, after) =
            split_highlighted_preview(&hit.line_content, hit.column, hit.match_length);
        assert_eq!(before, "// 한글 주석 alpha ");
        assert_eq!(matched, "TargetToken");
        assert_eq!(after, " 끝");

        // 7. Max results truncation.
        let trunc_opts = SearchOptions {
            query: "alpha".to_owned(),
            max_results: 2,
            ..SearchOptions::default()
        };
        let trunc_res = run_fn(&repo.0, &trunc_opts).unwrap();
        assert_eq!(trunc_res.total_matches, 2);
        assert!(trunc_res.truncated);
    }
}

#[gpui::test]
fn workspace_search_panel_tab_virtualized_rows_and_open_file_at_work(
    cx: &mut gpui::TestAppContext,
) {
    use gpui::Focusable as _;
    use std::cell::RefCell;
    use std::fmt::Write as _;
    use std::rc::Rc;

    let repo = Repo::new();
    std::fs::create_dir_all(repo.0.join("src")).unwrap();
    let mut big_source = String::new();
    for i in 0..150 {
        writeln!(big_source, "pub fn needle_item_{i:03}() {{}}").unwrap();
    }
    std::fs::write(repo.0.join("src/many.rs"), &big_source).unwrap();
    std::fs::write(
        repo.0.join("src/other.rs"),
        "fn main() {\n    let NEEDLE_ITEM = 1;\n}\n",
    )
    .unwrap();

    cx.update(gpui_component::init);
    let repo_path = repo.0.clone();
    let (view, cx) = cx.add_window_view(|_, cx| Panel::unconnected(repo_path, cx));
    cx.run_until_parked();

    let opened_at: Rc<RefCell<Vec<OpenFileAt>>> = Rc::new(RefCell::new(Vec::new()));
    let opened_hook = opened_at.clone();
    let _sub = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, e: &OpenFileAt, _| {
            opened_hook.borrow_mut().push(e.clone());
        })
    });

    // Click `diff-tab-search` to switch to Search tab and focus the query input.
    let search_tab = cx.debug_bounds("diff-tab-search").unwrap();
    cx.simulate_mouse_down(search_tab.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(search_tab.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        cx.update(|_, cx| view.read(cx).active_tab()),
        PanelTab::Search
    );
    assert!(
        cx.update(|window, cx| {
            view.read(cx)
                .search
                .query_input
                .as_ref()
                .unwrap()
                .focus_handle(cx)
                .is_focused(window)
        }),
        "clicking diff-tab-search must focus the search query input"
    );
    assert!(cx.debug_bounds("search-pane").is_some());
    assert!(cx.debug_bounds("search-toolbar").is_some());
    assert!(cx.debug_bounds("search-filters").is_some());
    assert!(cx.debug_bounds("search-empty-idle").is_some());

    // OWE-777: Type "needle_ite", switch to Files tab via `show_files` (⇧⌘E),
    // then click `diff-tab-search` with the mouse and immediately type "m".
    cx.simulate_keystrokes("n e e d l e _ i t e");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).search_options().query.clone()),
        "needle_ite"
    );

    cx.update(|window, cx| {
        view.update(cx, |p, cx| p.show_files(window, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).active_tab()),
        PanelTab::Files
    );

    let search_tab = cx.debug_bounds("diff-tab-search").unwrap();
    cx.simulate_mouse_down(search_tab.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(search_tab.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    cx.simulate_keystrokes("m");
    cx.run_until_parked();

    assert_eq!(
        cx.update(|_, cx| view.read(cx).search_options().query.clone()),
        "needle_item"
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).explorer.filter_query.clone()),
        ""
    );

    assert!(cx.debug_bounds("search-summary").is_some());
    assert_eq!(
        cx.update(|_, cx| view.read(cx).search_result().total_matches),
        151
    );
    assert_eq!(cx.update(|_, cx| view.read(cx).search_rows().len()), 153);
    assert!(
        cx.update(|_, cx| view.read(cx).search.rendered_rows) < 60,
        "uniform_list must virtualize 150+ search result rows"
    );

    // Click `search-toggle-case` (`Aa`) -> excludes `NEEDLE_ITEM` in `src/other.rs`.
    let case_btn = cx.debug_bounds("search-toggle-case").unwrap();
    cx.simulate_click(case_btn.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).search_result().total_matches),
        150
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).search_result().files.len()),
        1
    );

    // Click match row 1 (line 1, column 8 of `src/many.rs`) -> emits `OpenFileAt`.
    let match_row = cx.debug_bounds("search-row-1").unwrap();
    cx.simulate_mouse_down(match_row.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(match_row.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        opened_at.borrow().as_slice(),
        &[OpenFileAt {
            path: repo.0.join("src/many.rs"),
            line: 1,
            column: 8,
        }]
    );

    // Click file header row 0 -> collapses file matches.
    let header_row = cx.debug_bounds("search-row-0").unwrap();
    cx.simulate_mouse_down(header_row.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(header_row.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).search_rows().len()), 1);
}

#[gpui::test]
fn open_diff_file_renders_untracked_file_and_apply_watch_recomputes_provisional_patch(
    cx: &mut gpui::TestAppContext,
) {
    let repo = Repo::new();
    std::fs::write(
        repo.0.join("untracked_new.rs"),
        "pub fn brand_new() -> usize {\n    42\n}\n",
    )
    .unwrap();

    cx.update(gpui_component::init);
    let repo_path = repo.0.clone();
    let (view, cx) = cx.add_window_view(|_, cx| Panel::unconnected(repo_path, cx));
    cx.run_until_parked();

    // 1. Opening an untracked file before `apply_watch` has populated `changes.files`
    // probes its actual git status (`??`) and renders the added lines immediately.
    let r = repo.0.clone();
    cx.update(|_, cx| {
        view.update(cx, |p, cx| {
            p.open_diff_file(r, "untracked_new.rs", false, cx);
        });
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while cx.debug_bounds("diff-viewer").is_none() {
        assert!(
            Instant::now() < deadline,
            "open_diff_file must render untracked file diff without waiting for next watch cycle"
        );
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    cx.update(|_, cx| {
        let p = view.read(cx);
        assert_eq!(p.changes.files[0].status, *b"??");
        assert!(p.force_next);
    });

    // 2. Even if a provisional entry was inserted with `.M` (`staged: None, unstaged: None`)
    // and produced an empty patch, receiving the authoritative `??` snapshot via `apply_watch`
    // (`WatchAction::Keep`) must re-run `select` and populate the diff viewer.
    std::fs::write(
        repo.0.join("provisional.rs"),
        "pub const FIXED: bool = true;\n",
    )
    .unwrap();
    cx.update(|_, cx| {
        view.update(cx, |p, cx| {
            p.changes = Changes {
                files: vec![ChangedFile {
                    path: b"provisional.rs".to_vec(),
                    old_path: None,
                    status: *b".M",
                    staged: None,
                    unstaged: None,
                }],
                limited: false,
            };
            p.selected = Some(0);
            p.loaded = false;
            p.state.update(cx, |s, cx| s.set_files([], cx));
            p.apply_watch(
                Ok(GitWatchReply::Changed {
                    generation: 1,
                    changes: Changes {
                        files: vec![ChangedFile {
                            path: b"provisional.rs".to_vec(),
                            old_path: None,
                            status: *b"??",
                            staged: None,
                            unstaged: None,
                        }],
                        limited: false,
                    },
                }),
                cx,
            );
        });
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while cx.debug_bounds("diff-viewer").is_none() {
        assert!(
            Instant::now() < deadline,
            "apply_watch Keep on provisional entry must recompute patch for untracked file"
        );
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(cx.debug_bounds("diff-viewer").is_some());
}

#[test]
fn workspace_search_long_line_highlight_regex_step_cap_and_rg_truncation_work() {
    use std::os::unix::fs::PermissionsExt as _;

    let repo = Repo::new();
    std::fs::create_dir_all(repo.0.join("src")).unwrap();

    // 1. Long line (> 300 chars) match windowing and relative highlight splitting.
    let prefix = "x".repeat(350);
    let suffix = "y".repeat(60);
    let long_line = format!("{prefix}LongLineNeedle{suffix}\n");
    std::fs::write(repo.0.join("src/minified.js"), &long_line).unwrap();

    for run_fn in [search_worktree, search_worktree_fallback] {
        let opts = SearchOptions {
            query: "LongLineNeedle".to_owned(),
            case_sensitive: true,
            ..SearchOptions::default()
        };
        let res = run_fn(&repo.0, &opts).unwrap();
        assert_eq!(res.files.len(), 1);
        let hit = &res.files[0].matches[0];
        assert_eq!(hit.line, 1);
        assert_eq!(hit.column, 351);
        assert_eq!(hit.match_length, 14);
        assert!(hit.line_content.chars().count() <= 300);
        let (before, matched, after) =
            split_highlighted_preview(&hit.line_content, hit.column, hit.match_length);
        assert_eq!(matched, "LongLineNeedle");
        assert!(!before.is_empty());
        assert_eq!(after, suffix);
    }

    // 2. Catastrophic backtracking regex in fallback engine completes bounded by step cap.
    std::fs::write(
        repo.0.join("src/backtrack.txt"),
        format!("{}!\naaaaaab\n", "a".repeat(60)),
    )
    .unwrap();
    for pattern in ["(a+)+b", "a+a+a+a+a+a+b"] {
        let started = std::time::Instant::now();
        let res = search_worktree_fallback(
            &repo.0,
            &SearchOptions {
                query: pattern.to_owned(),
                use_regex: true,
                case_sensitive: true,
                ..SearchOptions::default()
            },
        )
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "fallback regex {pattern} must be bounded by step cap"
        );
        let bt_file = res
            .files
            .iter()
            .find(|f| f.relative_path == "src/backtrack.txt")
            .unwrap();
        assert_eq!(bt_file.matches.len(), 1);
        assert_eq!(bt_file.matches[0].line, 2);
    }

    // 3. `search_with_rg_limits` sets `truncated = true` when stdout cap or timeout is hit
    // even when parsed matches < max_results.
    let bin_dir = repo.0.join("mock_bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let mock_rg_cap = bin_dir.join("rg_cap");
    std::fs::write(
        &mock_rg_cap,
        "#!/bin/sh\nprintf '{\"type\":\"match\",\"data\":{\"path\":{\"text\":\"src/minified.js\"},\"lines\":{\"text\":\"LongLineNeedle\\\\n\"},\"line_number\":1,\"submatches\":[{\"start\":0,\"end\":14}]}}\\n'\nprintf '{\"type\":\"match\",\"data\":{\"path\":{\"text\":\"src/minified.js\"},\"lines\":{\"text\":\"truncated_line'\n",
    )
    .unwrap();
    std::fs::set_permissions(&mock_rg_cap, std::fs::Permissions::from_mode(0o755)).unwrap();

    let cap_res = super::search::search_with_rg_limits(
        &mock_rg_cap,
        &repo.0,
        &SearchOptions {
            query: "LongLineNeedle".to_owned(),
            max_results: 100,
            ..SearchOptions::default()
        },
        Duration::from_secs(2),
        180,
    )
    .unwrap();
    assert_eq!(cap_res.total_matches, 1);
    assert!(
        cap_res.truncated,
        "stdout byte cap must mark result as truncated even when total_matches < max_results"
    );

    let mock_rg_slow = bin_dir.join("rg_slow");
    std::fs::write(
        &mock_rg_slow,
        "#!/bin/sh\nprintf '{\"type\":\"match\",\"data\":{\"path\":{\"text\":\"src/minified.js\"},\"lines\":{\"text\":\"LongLineNeedle\\\\n\"},\"line_number\":1,\"submatches\":[{\"start\":0,\"end\":14}]}}\\n'\nsleep 2\n",
    )
    .unwrap();
    std::fs::set_permissions(&mock_rg_slow, std::fs::Permissions::from_mode(0o755)).unwrap();

    let slow_res = super::search::search_with_rg_limits(
        &mock_rg_slow,
        &repo.0,
        &SearchOptions {
            query: "LongLineNeedle".to_owned(),
            max_results: 100,
            ..SearchOptions::default()
        },
        Duration::from_millis(40),
        1024 * 1024,
    )
    .unwrap();
    assert!(
        slow_res.truncated,
        "rg timeout must mark result as truncated even when total_matches < max_results"
    );

    // 4. `query_git_ignored_set_with_limits` respects stdout byte cap and drops incomplete trailing entry.
    std::fs::write(repo.0.join(".gitignore"), "*.ignored\n").unwrap();
    let paths: Vec<String> = (0..20).map(|i| format!("file_{i:02}.ignored")).collect();
    let ignored = super::search::query_git_ignored_set_with_limits(
        &repo.0,
        &paths,
        Duration::from_secs(2),
        40,
    );
    assert!(ignored.contains("file_00.ignored"));
    assert!(ignored.iter().all(|entry| entry.ends_with(".ignored")));
}

#[test]
fn fit_highlighted_preview_for_cols_elides_leading_context_and_adapts_to_panel_width() {
    let repo = Repo::new();
    let long_line = format!("{}NEEDLE\n", "x".repeat(320));
    std::fs::write(repo.0.join("long.txt"), &long_line).unwrap();
    std::fs::write(
        repo.0.join("source.rs"),
        "fn main() { println!(\"CHANGED NEEDLE\"); }\n",
    )
    .unwrap();

    let res = search_worktree_fallback(
        &repo.0,
        &SearchOptions {
            query: "NEEDLE".to_owned(),
            case_sensitive: true,
            ..SearchOptions::default()
        },
    )
    .unwrap();
    let long_hit = &res
        .files
        .iter()
        .find(|f| f.relative_path == "long.txt")
        .unwrap()
        .matches[0];
    let source_hit = &res
        .files
        .iter()
        .find(|f| f.relative_path == "source.rs")
        .unwrap()
        .matches[0];

    let default_cols = max_cols_for_row_width(280.0, 6.6);
    assert_eq!(default_cols, 30);

    let (long_before_narrow, long_matched_narrow, long_after_narrow) =
        fit_highlighted_preview_for_cols(
            &long_hit.line_content,
            long_hit.column,
            long_hit.match_length,
            default_cols,
        );
    assert_eq!(long_matched_narrow, "NEEDLE");
    assert!(long_before_narrow.starts_with('…'));
    assert!(
        long_before_narrow.chars().count() + long_matched_narrow.chars().count() <= default_cols,
        "elided before + matched must fit inside default 280pt panel column budget"
    );
    assert!(long_after_narrow.is_empty());

    let (src_before_narrow, src_matched_narrow, src_after_narrow) =
        fit_highlighted_preview_for_cols(
            &source_hit.line_content,
            source_hit.column,
            source_hit.match_length,
            default_cols,
        );
    assert_eq!(src_matched_narrow, "NEEDLE");
    assert!(src_before_narrow.starts_with('…'));
    assert!(src_before_narrow.ends_with("println!(\"CHANGED "));
    assert_eq!(src_after_narrow, "\"); }");

    let wide_cols = max_cols_for_row_width(500.0, 6.6);
    let (long_before_wide, long_matched_wide, _) = fit_highlighted_preview_for_cols(
        &long_hit.line_content,
        long_hit.column,
        long_hit.match_length,
        wide_cols,
    );
    assert_eq!(long_matched_wide, "NEEDLE");
    assert!(
        long_before_wide.chars().count() > long_before_narrow.chars().count(),
        "wider panel must expose more leading context before the match"
    );

    let (src_before_wide, src_matched_wide, _) = fit_highlighted_preview_for_cols(
        &source_hit.line_content,
        source_hit.column,
        source_hit.match_length,
        wide_cols,
    );
    assert_eq!(src_matched_wide, "NEEDLE");
    assert_eq!(src_before_wide, "fn main() { println!(\"CHANGED ");
}

#[gpui::test]
fn workspace_search_default_width_panel_keeps_long_line_highlight_visible(
    cx: &mut gpui::TestAppContext,
) {
    struct NarrowHost(Entity<Panel>);
    impl Render for NarrowHost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().w(px(280.)).h(px(600.)).flex().flex_col().child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.0.clone()),
            )
        }
    }

    let repo = Repo::new();
    std::fs::write(
        repo.0.join("long.txt"),
        format!("{}NEEDLE\n", "x".repeat(320)),
    )
    .unwrap();
    std::fs::write(
        repo.0.join("source.rs"),
        "fn main() { println!(\"CHANGED NEEDLE\"); }\n",
    )
    .unwrap();

    cx.update(gpui_component::init);
    let repo_path = repo.0.clone();
    let mut panel_handle: Option<Entity<Panel>> = None;
    let (_, cx) = cx.add_window_view(|_, cx| {
        let p = cx.new(|cx| Panel::unconnected(repo_path, cx));
        panel_handle = Some(p.clone());
        NarrowHost(p)
    });
    let view = panel_handle.unwrap();
    cx.run_until_parked();

    let search_tab = cx.debug_bounds("diff-tab-search").unwrap();
    cx.simulate_mouse_down(search_tab.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(search_tab.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();

    cx.update(|_, cx| {
        view.update(cx, |p, cx| p.set_search_query("NEEDLE", cx));
    });
    cx.run_until_parked();

    let long_match = cx.debug_bounds("search-match-long.txt-1-321").unwrap();
    let long_highlight = cx.debug_bounds("search-highlight-long.txt-1-321").unwrap();
    assert!(
        long_highlight.size.width > px(0.),
        "long.txt NEEDLE highlight must have non-zero width"
    );
    assert!(
        long_highlight.origin.x >= long_match.origin.x
            && long_highlight.right() <= long_match.right(),
        "long.txt NEEDLE highlight ({long_highlight:?}) must stay inside 280pt match row ({long_match:?})"
    );

    let src_match = cx.debug_bounds("search-match-source.rs-1-31").unwrap();
    let src_highlight = cx.debug_bounds("search-highlight-source.rs-1-31").unwrap();
    assert!(
        src_highlight.size.width > px(0.),
        "source.rs NEEDLE highlight must have non-zero width"
    );
    assert!(
        src_highlight.origin.x >= src_match.origin.x && src_highlight.right() <= src_match.right(),
        "source.rs NEEDLE highlight ({src_highlight:?}) must stay inside 280pt match row ({src_match:?})"
    );
}

#[gpui::test]
fn right_panel_icon_only_tabs_switch_on_click_show_tooltips_and_fit_default_width(
    cx: &mut gpui::TestAppContext,
) {
    use crate::i18n::Language;
    use crate::pull_request::{
        Action, CheckItem, CheckState, PrInspection, ReviewComment, Runner, Service, Status,
    };

    struct MockPrRunner;
    impl Runner for MockPrRunner {
        fn run(&self, _: &Path, _: &str, _: Action) -> io::Result<Status> {
            Ok(Status::Open)
        }

        fn inspect_pr(&self, _: &Path, _: &str) -> io::Result<PrInspection> {
            Ok(PrInspection {
                status: Status::Open,
                number: Some(787),
                title: "Icon-only right panel tabs".into(),
                url: "https://github.com/acme/krill/pull/787".into(),
                checks: vec![
                    CheckItem {
                        name: "cargo nextest".into(),
                        state: CheckState::Failed,
                        description: Some("1 test failed".into()),
                        details_url: None,
                        run_id: None,
                        job_id: None,
                        log_tail: None,
                    },
                    CheckItem {
                        name: "cargo clippy".into(),
                        state: CheckState::Passed,
                        description: None,
                        details_url: None,
                        run_id: None,
                        job_id: None,
                        log_tail: None,
                    },
                ],
                review_comments: vec![ReviewComment {
                    id: "c1".into(),
                    thread_id: Some("t1".into()),
                    author: "owen".into(),
                    is_bot: false,
                    body: "Looks good".into(),
                    path: "crates/krill-app/src/diff.rs".into(),
                    line: Some(10),
                    original_line: Some(10),
                    diff_hunk: String::new(),
                    created_at: "2026-10-18T00:00:00Z".into(),
                    url: String::new(),
                    is_resolved: false,
                    is_outdated: false,
                    replies: Vec::new(),
                }],
            })
        }
    }

    struct NarrowHost(Entity<Panel>);
    impl Render for NarrowHost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().w(px(280.)).h(px(600.)).flex().flex_col().child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.0.clone()),
            )
        }
    }

    cx.update(gpui_component::init);
    cx.update(|cx| crate::i18n::set_language(Language::Ko, cx));
    assert_eq!(PanelTab::Changes.label(), "변경 사항");
    assert_eq!(PanelTab::Files.label(), "파일");
    assert_eq!(PanelTab::Search.label(), "검색");
    assert_eq!(PanelTab::Checks.label(), "PR · CI");
    assert_eq!(PanelTab::Changes.tooltip(), "변경 사항 (⇧⌘G)");
    assert_eq!(PanelTab::Files.tooltip(), "파일 탐색기 (⇧⌘E)");
    assert_eq!(PanelTab::Search.tooltip(), "워크스페이스 검색 (⇧⌘F)");
    assert_eq!(PanelTab::Checks.tooltip(), "PR · CI");

    cx.update(|cx| crate::i18n::set_language(Language::En, cx));
    assert_eq!(PanelTab::Changes.label(), "Changes");
    assert_eq!(PanelTab::Files.label(), "Files");
    assert_eq!(PanelTab::Search.label(), "Search");
    assert_eq!(PanelTab::Checks.label(), "PR · CI");
    assert_eq!(PanelTab::Changes.tooltip(), "Changes (⇧⌘G)");
    assert_eq!(PanelTab::Files.tooltip(), "File Explorer (⇧⌘E)");
    assert_eq!(PanelTab::Search.tooltip(), "Search in Workspace (⇧⌘F)");
    assert_eq!(PanelTab::Checks.tooltip(), "PR · CI");
    cx.update(|cx| crate::i18n::set_language(Language::Ko, cx));

    let repo = Repo::new();
    std::fs::write(repo.0.join("changed.rs"), "pub fn demo() {}\n").unwrap();

    let repo_path = repo.0.clone();
    let pr_service = Arc::new(Service::new(Arc::new(MockPrRunner)));
    let mut panel_handle: Option<Entity<Panel>> = None;
    let (_, cx) = cx.add_window_view(|_, cx| {
        let p = cx.new(|cx| {
            let mut panel = Panel::unconnected(repo_path, cx);
            panel.set_pr_service(pr_service);
            panel.set_checks_branch("craftowen/m10-owe-787");
            panel
        });
        panel_handle = Some(p.clone());
        NarrowHost(p)
    });
    let view = panel_handle.unwrap();
    cx.run_until_parked();

    // Default tab is Changes; only Changes active indicator bar is rendered.
    assert_eq!(
        cx.update(|_, cx| view.read(cx).active_tab()),
        PanelTab::Changes
    );
    assert!(cx.debug_bounds("diff-tab-changes-active").is_some());
    assert!(cx.debug_bounds("diff-tab-files-active").is_none());
    assert!(cx.debug_bounds("diff-tab-search-active").is_none());
    assert!(cx.debug_bounds("diff-tab-checks-active").is_none());
    assert!(cx.debug_bounds("diff-file-count").is_some());

    // Icon-only tabs stay compact (< 40pt for icon-only tabs without badges) and all 4 tabs fit before refresh/collapse buttons in 280pt panel.
    let files_bounds = cx.debug_bounds("diff-tab-files").unwrap();
    let search_bounds = cx.debug_bounds("diff-tab-search").unwrap();
    let checks_bounds = cx.debug_bounds("diff-tab-checks").unwrap();
    let refresh_bounds = cx.debug_bounds("diff-refresh").unwrap();
    assert!(
        files_bounds.size.width <= px(36.) && search_bounds.size.width <= px(36.),
        "icon-only tabs must not include inline text labels: files={files_bounds:?}, search={search_bounds:?}"
    );
    assert!(
        checks_bounds.right() <= refresh_bounds.origin.x,
        "all 4 icon tabs must fit left of refresh button inside 280pt panel: checks={checks_bounds:?}, refresh={refresh_bounds:?}"
    );

    let click = |selector: &'static str, cx: &mut gpui::VisualTestContext| {
        let bounds = cx.debug_bounds(selector).unwrap();
        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(bounds.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
    };

    // Click each tab and verify active_tab() + active bottom indicator bar transition.
    for tab in [
        PanelTab::Files,
        PanelTab::Search,
        PanelTab::Checks,
        PanelTab::Changes,
    ] {
        click(tab.selector(), cx);
        assert_eq!(cx.update(|_, cx| view.read(cx).active_tab()), tab);
        for candidate in PanelTab::ALL {
            assert_eq!(
                cx.debug_bounds(candidate.active_selector()).is_some(),
                candidate == tab,
                "active indicator mismatch for {candidate:?} when active tab is {tab:?}"
            );
        }
    }

    // After Checks inspection completes, both status dot and actionable count badge are rendered on the Checks tab icon button and still fit inside 280pt.
    assert!(cx.debug_bounds("diff-checks-dot").is_some());
    assert!(cx.debug_bounds("diff-checks-count").is_some());
    let checks_with_badge = cx.debug_bounds("diff-tab-checks").unwrap();
    let refresh_after = cx.debug_bounds("diff-refresh").unwrap();
    assert!(
        checks_with_badge.right() <= refresh_after.origin.x,
        "Checks tab with status dot and count badge must still fit left of refresh button in 280pt panel"
    );

    // Hovering each tab icon opens its specific tooltip popup.
    for tab in PanelTab::ALL {
        cx.simulate_mouse_move(gpui::point(px(275.), px(595.)), None, Modifiers::default());
        cx.run_until_parked();
        let bounds = cx.debug_bounds(tab.selector()).unwrap();
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
        cx.executor().advance_clock(Duration::from_millis(600));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds(tab.tooltip_selector()).is_some(),
            "hovering {} must display tooltip '{}' ({})",
            tab.selector(),
            tab.tooltip(),
            tab.tooltip_selector()
        );
    }
}

#[gpui::test]
fn comparison_tabs_filter_worktree_and_index_files_and_clear_mismatched_selection(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, cx| {
        let mut p = Panel::unconnected(PathBuf::from("/repo"), cx);
        p.visible = false;
        p.changes = Changes {
            files: vec![
                ChangedFile {
                    path: b"src/unstaged_only.rs".to_vec(),
                    old_path: None,
                    status: *b".M",
                    staged: None,
                    unstaged: Some(Numstat {
                        added: Some(4),
                        deleted: Some(1),
                    }),
                },
                ChangedFile {
                    path: b"src/untracked.rs".to_vec(),
                    old_path: None,
                    status: *b"??",
                    staged: None,
                    unstaged: None,
                },
                ChangedFile {
                    path: b"src/staged_only.rs".to_vec(),
                    old_path: None,
                    status: *b"M.",
                    staged: Some(Numstat {
                        added: Some(10),
                        deleted: Some(2),
                    }),
                    unstaged: None,
                },
                ChangedFile {
                    path: b"src/both.rs".to_vec(),
                    old_path: None,
                    status: *b"MM",
                    staged: Some(Numstat {
                        added: Some(3),
                        deleted: Some(0),
                    }),
                    unstaged: Some(Numstat {
                        added: Some(1),
                        deleted: Some(1),
                    }),
                },
            ],
            limited: false,
        };
        p
    });
    cx.run_until_parked();

    // 1. Default tab is `Comparison::Worktree`: shows indices 0, 1, 3 and hides staged-only index 2.
    assert_eq!(
        cx.update(|_, cx| view.read(cx).visible_file_indices(Comparison::Worktree)),
        vec![0, 1, 3]
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).visible_file_indices(Comparison::Index)),
        vec![2, 3]
    );
    assert!(cx.debug_bounds("changed-file-0").is_some());
    assert!(cx.debug_bounds("changed-file-1").is_some());
    assert!(cx.debug_bounds("changed-file-2").is_none());
    assert!(cx.debug_bounds("changed-file-3").is_some());

    // Select unstaged-only file 0, then click `comparison-1` (`Index`):
    // selection must be cleared to `None` (`diff-empty-unselected`), and only indices 2 and 3 are rendered.
    let row0 = cx.debug_bounds("changed-file-0").unwrap();
    cx.simulate_click(row0.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).selected_path()),
        Some("src/unstaged_only.rs".to_owned())
    );

    let index_tab = cx.debug_bounds("comparison-1").unwrap();
    cx.simulate_click(index_tab.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).comparison()),
        Comparison::Index
    );
    assert_eq!(cx.update(|_, cx| view.read(cx).selected_path()), None);
    assert!(cx.debug_bounds("diff-empty-unselected").is_some());
    assert!(cx.debug_bounds("changed-file-0").is_none());
    assert!(cx.debug_bounds("changed-file-1").is_none());
    assert!(cx.debug_bounds("changed-file-2").is_some());
    assert!(cx.debug_bounds("changed-file-3").is_some());

    // Select `src/both.rs` (index 3, which is in both Index and Worktree) and switch back to Worktree:
    // selection stays on `src/both.rs`.
    let row3 = cx.debug_bounds("changed-file-3").unwrap();
    cx.simulate_click(row3.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).selected_path()),
        Some("src/both.rs".to_owned())
    );

    let worktree_tab = cx.debug_bounds("comparison-0").unwrap();
    cx.simulate_click(worktree_tab.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).comparison()),
        Comparison::Worktree
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).selected_path()),
        Some("src/both.rs".to_owned())
    );
}
