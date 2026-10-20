use super::*;
use crate::sidebar::{Selected, Sidebar, Snapshot};
use gpui::TestAppContext;
use krill_proto::SessionId;
use std::cell::Cell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Fake {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<NewWorktree>>,
    fetches: AtomicUsize,
    preparations: Mutex<Vec<PrepareWorktree>>,
    prep_supported: AtomicBool,
}

impl Source for Fake {
    fn fetch(&self) -> io::Result<Snapshot> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        Ok(Snapshot::default())
    }
    fn prepare_worktree(&self, req: PrepareWorktree) -> io::Result<Reply> {
        if !self.prep_supported.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let cancel = req.cancel;
        self.preparations.lock().unwrap().push(req);
        Ok(Reply::WorktreePreparation(if cancel {
            WorktreePreparation::Cancelled
        } else {
            WorktreePreparation::Ready
        }))
    }
    fn new_worktree(&self, req: NewWorktree) -> io::Result<Reply> {
        self.requests.lock().unwrap().push(req);
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| io::ErrorKind::Unsupported.into())
    }
}

#[test]
fn owe853_branch_suggestions_prefers_parent_and_dedupes() {
    let (default, candidates) = branch_suggestions(
        Some("feat/parent"),
        Some("main"),
        &["main".into(), "feat/x".into()],
    );
    assert_eq!(default.as_deref(), Some("feat/parent"));
    assert_eq!(candidates[0], "feat/parent");
    assert_eq!(
        candidates.iter().filter(|c| c.as_str() == "main").count(),
        1
    );
    assert!(candidates.contains(&"origin/main".to_owned()));
    assert!(candidates.contains(&"feat/x".to_owned()));

    let (default, _) = branch_suggestions(None, Some("develop"), &[]);
    assert_eq!(default.as_deref(), Some("develop"));

    let (default, candidates) = branch_suggestions(None, None, &[]);
    assert_eq!(default, None);
    assert!(candidates.contains(&"HEAD".to_owned()));
}

#[test]
fn script_controls_are_visible() {
    assert_eq!(
        visible("echo 1\n\techo\x1b[2J\r"),
        "echo 1\n\techo\u{fffd}[2J\u{fffd}"
    );
    assert_eq!(hex_string(&[0, 0xab, 0xff]), "00abff");
}

#[gpui::test]
fn hangul_name_setup_approval_and_sidebar_refresh(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::default());
    let hash = vec![7; 32];
    let created = Worktree {
        path: "/wt/repo/한글-이름".as_bytes().to_vec(),
        branch: "한글-이름".into(),
        base: "origin/main".into(),
        copied: vec![".env".into()],
        warnings: vec!["include secrets/: outside repository".into()],
        setup: Some(SessionId(9)),
    };
    fake.replies.lock().unwrap().extend([
        Reply::WorktreeFailed("name is empty after normalization".into()),
        Reply::SetupUntrusted {
            script: "npm ci\n".into(),
            hash: hash.clone(),
        },
        Reply::WorktreeCreated(created.clone()),
    ]);
    cx.update(gpui_component::init);
    let (sidebar, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    let selected = Rc::new(Cell::new(None));
    let got = selected.clone();
    let _selection =
        cx.update(|_, cx| cx.subscribe(&sidebar, move |_, e: &Selected, _| got.set(Some(e.0))));
    let source: Arc<dyn Source> = fake.clone();
    cx.update(|window, cx| {
        sidebar.update(cx, |s, cx| {
            s.connect_source(source, cx);
            s.new_worktree("/src/repo".into(), window, cx);
        });
    });
    cx.run_until_parked();
    let dialog = cx.update(|_, cx| sidebar.read(cx).creating.as_ref().unwrap().0.clone());
    let phase = |cx: &mut gpui::VisualTestContext| cx.update(|_, cx| dialog.read(cx).phase.clone());

    // 빈 이름은 보내지 않는다.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(phase(cx), Phase::Edit(Some(text::EMPTY_NAME.get().into())));
    assert!(fake.requests.lock().unwrap().is_empty());

    // 데몬 오류는 입력을 유지한 채 보인다.
    cx.simulate_input("한글 이름");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        phase(cx),
        Phase::Edit(Some("name is empty after normalization".into()))
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        phase(cx),
        Phase::Untrusted {
            script: "npm ci\n".into(),
            hash: hash.clone()
        }
    );
    let before = fake.fetches.load(Ordering::SeqCst);
    cx.update(|_, cx| dialog.update(cx, |d, cx| d.submit(Setup::Approve(hash.clone()), cx)));
    cx.run_until_parked();
    assert_eq!(phase(cx), Phase::Done(created));
    assert!(
        fake.fetches.load(Ordering::SeqCst) > before,
        "sidebar refetches"
    );
    assert_eq!(selected.get(), Some(SessionId(9)), "setup session opens");

    let requests = fake.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|r| r.name == "한글 이름" && r.repo == b"/src/repo" && r.base.is_none())
    );
    assert_eq!(requests[1].setup, Setup::Ask);
    assert_eq!(requests[2].setup, Setup::Approve(hash));

    // 기능이 없는 데몬(Unsupported)은 안내 문구로 바뀐다.
    cx.update(|_, cx| dialog.update(cx, |d, cx| d.submit(Setup::Skip, cx)));
    cx.run_until_parked();
    assert_eq!(phase(cx), Phase::Edit(Some(text::UNSUPPORTED.get().into())));
    assert_eq!(fake.requests.lock().unwrap()[3].setup, Setup::Skip);

    cx.update(|_, cx| dialog.update(cx, |_, cx| cx.emit(Event::Close)));
    cx.run_until_parked();
    assert!(cx.update(|_, cx| sidebar.read(cx).creating.is_none()));
}

#[gpui::test]
fn preparation_only_while_dialog_open_base_change_and_explicit_cancel(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::default());
    fake.prep_supported.store(true, Ordering::Release);
    cx.update(gpui_component::init);
    let (dialog, cx) =
        cx.add_window_view(|window, cx| Dialog::new("/repo".into(), fake.clone(), window, cx));
    cx.run_until_parked();
    assert!(
        fake.preparations.lock().unwrap().is_empty(),
        "base editing is debounced"
    );
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    assert!(cx.update(|_, cx| dialog.read(cx).preparation_ready));
    let first = fake.preparations.lock().unwrap()[0].clone();
    assert!(!first.cancel);
    assert_eq!(first.base, None);
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.base.update(cx, |input, cx| {
                input.set_value("feature", window, cx);
                // Programmatic set_value is silent; emulate the event emitted by typing.
                cx.emit(InputEvent::Change);
            });
        });
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    let requests = fake.preparations.lock().unwrap().clone();
    assert!(requests.iter().any(|r| r.token == first.token && r.cancel));
    let next = requests
        .iter()
        .find(|r| !r.cancel && r.base.as_deref() == Some("feature"))
        .unwrap()
        .clone();
    assert_ne!(next.token, first.token);
    cx.update(|_, cx| dialog.update(cx, Dialog::cancel_preparation));
    cx.run_until_parked();
    assert!(
        fake.preparations
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.token == next.token && r.cancel)
    );
    assert!(cx.update(|_, cx| dialog.read(cx).preparation.is_none()));
    let count = fake.preparations.lock().unwrap().len();
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(
        fake.preparations.lock().unwrap().len(),
        count,
        "cancel must not automatically restart"
    );
}

#[gpui::test]
fn closing_dialog_cancels_its_preparation(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::default());
    fake.prep_supported.store(true, Ordering::Release);
    cx.update(gpui_component::init);
    let (sidebar, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    let source: Arc<dyn Source> = fake.clone();
    cx.update(|window, cx| {
        sidebar.update(cx, |s, cx| {
            s.connect_source(source, cx);
            s.new_worktree("/repo".into(), window, cx);
        });
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    let req = fake.preparations.lock().unwrap()[0].clone();
    cx.update(|_, cx| {
        let dialog = sidebar.read(cx).creating.as_ref().unwrap().0.clone();
        dialog.update(cx, |_, cx| cx.emit(Event::Close));
    });
    cx.run_until_parked();
    assert!(
        fake.preparations
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.token == req.token && r.cancel)
    );
}

#[gpui::test]
fn escape_closes_modal(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::default());
    cx.update(gpui_component::init);
    let (sidebar, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    let source: Arc<dyn Source> = fake.clone();
    cx.update(|window, cx| {
        sidebar.update(cx, |s, cx| {
            s.connect_source(source, cx);
            s.new_worktree("/repo".into(), window, cx);
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|_, cx| assert!(sidebar.read(cx).creating.is_none()));
}

#[gpui::test]
fn dialog_renders_symlinks_setup_and_archive_preview(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::default());
    let temp = std::env::temp_dir().join(format!(
        "krill-wt-dialog-preview-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(
        temp.join("orca.yaml"),
        "scripts:\n  setup: pnpm install\n  archive: ./scripts/cleanup.sh\nworktree:\n  sharedDirectories:\n    - .env\n    - node_modules\n",
    )
    .unwrap();
    cx.update(gpui_component::init);
    let repo_path = temp.clone();
    let (dialog, vcx) =
        cx.add_window_view(|window, cx| Dialog::new(repo_path.clone(), fake.clone(), window, cx));
    vcx.simulate_resize(gpui::size(px(800.), px(900.)));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        let summary = krill_proto::orchestration::inspect_repo_hooks(&dialog.read(cx).repo, None);
        assert_eq!(summary.effective_symlinks, vec![".env", "node_modules"]);
        assert_eq!(summary.effective_setup.as_deref(), Some("pnpm install"));
        assert_eq!(
            summary.effective_archive.as_deref(),
            Some("./scripts/cleanup.sh")
        );
    });
    assert!(vcx.debug_bounds("worktree-symlinks-preview").is_some());
    assert!(vcx.debug_bounds("worktree-setup-preview").is_some());
    assert!(vcx.debug_bounds("worktree-archive-preview").is_some());
    std::fs::remove_dir_all(temp).ok();
}

#[gpui::test]
fn owe853_base_default_and_searchable_dropdown(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::default());
    cx.update(gpui_component::init);
    let (dialog, cx) =
        cx.add_window_view(|window, cx| Dialog::new("/repo".into(), fake.clone(), window, cx));
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.set_branch_candidates(
                vec![
                    "main".to_owned(),
                    "feat/parent".to_owned(),
                    "origin/main".to_owned(),
                ],
                cx,
            );
            d.set_base_default(Some("feat/parent".to_owned()), window, cx);
        });
    });
    cx.run_until_parked();
    // 현재 브랜치가 기본값으로 들어간다.
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).base_value(cx)),
        "feat/parent"
    );
    // 입력済み면 덮지 않는다.
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.base.update(cx, |input, cx| {
                input.set_value("custom", window, cx);
            });
            d.set_base_default(Some("other".to_owned()), window, cx);
        });
    });
    assert_eq!(cx.update(|_, cx| dialog.read(cx).base_value(cx)), "custom");
    // 검색어로 필터된다.
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.base.update(cx, |input, cx| {
                input.set_value("feat", window, cx);
            });
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("worktree-base-option-feat/parent")
            .is_some()
    );
    assert!(cx.debug_bounds("worktree-base-option-main").is_none());
}

#[gpui::test]
fn owe853_created_closes_dialog_and_opens_new_worktree(cx: &mut TestAppContext) {
    use crate::sidebar::NewSession;
    let fake = Arc::new(Fake::default());
    let created = Worktree {
        path: "/repo/wt-new".as_bytes().to_vec(),
        branch: "wt-new".into(),
        base: "main".into(),
        copied: Vec::new(),
        warnings: Vec::new(),
        setup: None,
    };
    fake.replies
        .lock()
        .unwrap()
        .push_back(Reply::WorktreeCreated(created.clone()));
    cx.update(gpui_component::init);
    let (sidebar, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    let opened: Rc<Cell<Option<NewSession>>> = Rc::new(Cell::new(None));
    let got = opened.clone();
    cx.update(|_, cx| {
        cx.subscribe(&sidebar, move |_, e: &NewSession, _| {
            got.set(Some(e.clone()));
        })
        .detach();
    });
    let source: Arc<dyn Source> = fake.clone();
    cx.update(|window, cx| {
        sidebar.update(cx, |s, cx| {
            s.connect_source(source, cx);
            s.new_worktree("/repo".into(), window, cx);
        });
    });
    cx.run_until_parked();
    let dialog = cx.update(|_, cx| sidebar.read(cx).creating.as_ref().unwrap().0.clone());
    // 이름 입력 후 생성 제출 → Done 없이 바로 닫히고 NewSession이 열린다.
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.name.update(cx, |input, cx| {
                input.set_value("wt-new", window, cx);
            });
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| dialog.update(cx, |d, cx| d.submit(Setup::Ask, cx)));
    cx.run_until_parked();
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| sidebar.read(cx).creating.is_none()),
        "dialog must auto-close on Created"
    );
    let session = opened.take();
    assert_eq!(
        session.map(|s| s.cwd),
        Some(std::path::PathBuf::from("/repo/wt-new"))
    );
}

#[gpui::test]
fn owe924_multi_agent_fanout_and_initial_prompt(cx: &mut TestAppContext) {
    use crate::sidebar::NewSession;
    use krill_proto::control::Program;
    let fake = Arc::new(Fake::default());
    fake.replies.lock().unwrap().extend([
        Reply::WorktreeCreated(Worktree {
            path: "/repo/wt-fan-claude".as_bytes().to_vec(),
            branch: "wt-fan-claude".into(),
            base: "main".into(),
            copied: Vec::new(),
            warnings: Vec::new(),
            setup: None,
        }),
        Reply::WorktreeCreated(Worktree {
            path: "/repo/wt-fan-codex".as_bytes().to_vec(),
            branch: "wt-fan-codex".into(),
            base: "main".into(),
            copied: Vec::new(),
            warnings: Vec::new(),
            setup: None,
        }),
    ]);
    cx.update(gpui_component::init);
    let (sidebar, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    let opened: Rc<std::cell::RefCell<Vec<NewSession>>> =
        Rc::new(std::cell::RefCell::new(Vec::new()));
    let got = opened.clone();
    cx.update(|_, cx| {
        cx.subscribe(&sidebar, move |_, e: &NewSession, _| {
            got.borrow_mut().push(e.clone());
        })
        .detach();
    });
    let source: Arc<dyn Source> = fake.clone();
    cx.update(|window, cx| {
        sidebar.update(cx, |s, cx| {
            s.connect_source(source, cx);
            s.new_worktree("/repo".into(), window, cx);
        });
    });
    cx.run_until_parked();
    let dialog = cx.update(|_, cx| sidebar.read(cx).creating.as_ref().unwrap().0.clone());
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.name.update(cx, |input, cx| {
                input.set_value("wt-fan", window, cx);
            });
            d.toggle_agent("claude", cx);
            d.toggle_agent("codex", cx);
            d.set_initial_prompt("Fix the flaky test", window, cx);
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| dialog.update(cx, |d, cx| d.submit(Setup::Ask, cx)));
    cx.run_until_parked();
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| sidebar.read(cx).creating.is_none()),
        "dialog must auto-close after all fan-out worktrees finish"
    );
    let requests = fake.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].name, "wt-fan-claude");
    assert_eq!(requests[1].name, "wt-fan-codex");
    let sessions = opened.borrow().clone();
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions[0],
        NewSession {
            cwd: std::path::PathBuf::from("/repo/wt-fan-claude"),
            program: Program::Agent {
                id: "claude".into()
            },
        }
    );
    assert_eq!(
        sessions[1],
        NewSession {
            cwd: std::path::PathBuf::from("/repo/wt-fan-codex"),
            program: Program::Agent { id: "codex".into() },
        }
    );
    let (p1, p2) = cx.update(|_, cx| {
        sidebar.update(cx, |s, _| {
            (
                s.take_initial_prompt(std::path::Path::new("/repo/wt-fan-claude")),
                s.take_initial_prompt(std::path::Path::new("/repo/wt-fan-codex")),
            )
        })
    });
    assert_eq!(p1.as_deref(), Some("Fix the flaky test"));
    assert_eq!(p2.as_deref(), Some("Fix the flaky test"));
}
