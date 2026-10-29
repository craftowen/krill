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
    snapshot: Mutex<Snapshot>,
    meta: Mutex<crate::sidebar::Meta>,
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<NewWorktree>>,
    fetches: AtomicUsize,
    preparations: Mutex<Vec<PrepareWorktree>>,
    prep_supported: AtomicBool,
}

impl Source for Fake {
    fn fetch(&self) -> io::Result<Snapshot> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        Ok(self.snapshot.lock().unwrap().clone())
    }
    fn meta(&self, _: &std::collections::BTreeMap<PathBuf, Vec<PathBuf>>) -> crate::sidebar::Meta {
        self.meta.lock().unwrap().clone()
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

    // Enter는 다음 입력칸으로만 이동하며 생성하지 않는다.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(phase(cx), Phase::Edit(None));
    assert!(fake.requests.lock().unwrap().is_empty());
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.name.update(cx, |input, cx| input.focus(window, cx));
        });
    });

    // 데몬 오류는 입력을 유지한 채 보인다.
    cx.simulate_input("한글 이름");
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    assert_eq!(
        phase(cx),
        Phase::Edit(Some("name is empty after normalization".into()))
    );

    cx.simulate_keystrokes("cmd-enter");
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
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.base.update(cx, |input, cx| {
                input.set_value("feat", window, cx);
            });
        });
    });
    cx.run_until_parked();
    // 기준 ref 칸에 포커스가 없으면 후보 목록을 펼치지 않는다.
    assert!(
        cx.debug_bounds("worktree-base-option-feat/parent")
            .is_none()
    );
    // 포커스가 오면 검색어로 필터된 후보가 펼쳐진다.
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.base.update(cx, |input, cx| input.focus(window, cx));
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

#[gpui::test]
fn worktree_modal_keeps_create_visible_with_long_hook_preview(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let fake = Arc::new(Fake::default());
    let (dialog, cx) = cx.add_window_view(|window, cx| {
        Dialog::new(
            PathBuf::from("/tmp/krill-ui-viewport-test"),
            fake,
            window,
            cx,
        )
    });
    cx.run_until_parked();
    dialog.update(cx, |d, cx| {
        d.hooks.effective_setup = Some("very long setup script with many lines\n".repeat(50));
        cx.notify();
    });
    cx.simulate_resize(gpui::size(px(640.), px(480.)));
    cx.update(|window, _| window.set_rem_size(px(32.)));
    cx.run_until_parked();
    let card = cx.debug_bounds("worktree-dialog").unwrap();
    let create = cx.debug_bounds("worktree-create").unwrap();
    let body = cx.debug_bounds("worktree-dialog-body").unwrap();
    assert!(body.size.height > px(0.) && body.bottom() <= create.top());
    assert!(
        card.size.width <= px(608.) && card.size.height <= px(448.),
        "{card:?}"
    );
    assert!(
        create.left() >= card.left()
            && create.right() <= card.right()
            && create.top() >= card.top()
            && create.bottom() <= card.bottom(),
        "{create:?} outside {card:?}"
    );
    assert!(cx.debug_bounds("worktree-dialog-body").unwrap().size.height > px(0.));
}

#[test]
fn fan_out_choices_resolve_to_supported_launch_profiles() {
    let agents = krill_proto::agent::Agents::load(None).unwrap();
    for &(id, _) in FAN_OUT_AGENTS {
        assert!(
            agents.resolve(id).is_some(),
            "Fan-Out exposes unsupported agent {id}"
        );
    }
}

#[gpui::test]
fn worktree_creation_keeps_dialog_until_operation_settles(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let fake = Arc::new(Fake::default());
    let (dialog, cx) = cx.add_window_view(|window, cx| {
        Dialog::new(
            PathBuf::from("/tmp/krill-ui-creating-test"),
            fake,
            window,
            cx,
        )
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&dialog, move |_, event: &Event, _| {
            sink.lock().unwrap().push(event.clone());
        })
        .detach();
    });
    dialog.update(cx, |d, cx| {
        d.phase = Phase::Creating;
        cx.notify();
    });
    cx.simulate_keystrokes("escape");
    assert!(
        events.lock().unwrap().is_empty(),
        "closing would drop pending Fan-Out state"
    );
    dialog.update(cx, |d, cx| {
        d.phase = Phase::Edit(None);
        cx.notify();
    });
    cx.simulate_keystrokes("escape");
    assert_eq!(*events.lock().unwrap(), [Event::Close]);
}

#[gpui::test]
fn submit_immediately_hides_modal_and_renders_pending_row_until_created(cx: &mut TestAppContext) {
    use crate::sidebar::{NewSession, Tree};

    let fake = Arc::new(Fake::default());
    fake.prep_supported.store(true, Ordering::Release);
    *fake.snapshot.lock().unwrap() = Snapshot {
        sessions: Vec::new(),
        trees: std::collections::BTreeMap::from([(
            PathBuf::from("/repo"),
            vec![PathBuf::from("/repo")],
        )]),
    };
    fake.meta.lock().unwrap().insert(
        PathBuf::from("/repo"),
        Tree {
            branch: Some("main".into()),
            dirty: None,
        },
    );
    fake.replies
        .lock()
        .unwrap()
        .push_back(Reply::WorktreeCreated(Worktree {
            path: "/repo/즉각-반응".as_bytes().to_vec(),
            branch: "즉각-반응".into(),
            base: "main".into(),
            copied: Vec::new(),
            warnings: Vec::new(),
            setup: None,
        }));
    cx.update(gpui_component::init);
    let (sidebar, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    cx.simulate_resize(gpui::size(px(900.), px(700.)));
    let opened: Rc<Cell<Option<NewSession>>> = Rc::new(Cell::new(None));
    let got = opened.clone();
    let source: Arc<dyn Source> = fake.clone();
    cx.update(|_, cx| {
        cx.subscribe(&sidebar, move |_, e: &NewSession, _| {
            got.set(Some(e.clone()));
        })
        .detach();
        sidebar.update(cx, |s, cx| {
            s.connect_source(source, cx);
        });
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        sidebar.update(cx, |s, cx| {
            s.new_worktree("/repo".into(), window, cx);
        });
        window.draw(cx).clear(cx);
    });
    assert!(
        cx.debug_bounds("worktree-dialog").is_some(),
        "modal must be visible before submit"
    );

    // 모달이 열릴 때 빈 base(None)로 잘못 선행 준비하지 않고 저장소 기본 브랜치("main")로 곧장 준비한다.
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    {
        let preps = fake.preparations.lock().unwrap();
        assert!(
            !preps.is_empty()
                && preps
                    .iter()
                    .filter(|p| !p.cancel)
                    .all(|p| p.base.as_deref() == Some("main")),
            "initial speculative preparation must target 'main' directly: {preps:?}"
        );
    }

    let dialog = cx.update(|_, cx| sidebar.read(cx).creating.as_ref().unwrap().0.clone());
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.name.update(cx, |input, cx| {
                input.set_value("즉각-반응", window, cx);
            });
            d.submit(Setup::Ask, cx);
        });
        // 백그라운드 생성 태스크가 돌기 전 같은 프레임에서 모달이 사라지고 낙관적 행이 바로 그려져야 한다.
        window.draw(cx).clear(cx);
    });
    assert!(
        fake.requests.lock().unwrap().is_empty(),
        "background new_worktree has not executed yet"
    );
    assert!(
        cx.debug_bounds("worktree-dialog").is_none(),
        "modal must disappear on the exact submit frame"
    );
    let pending_selectors = [
        "pending-worktree-0",
        "pending-worktree-1",
        "pending-worktree-2",
        "pending-worktree-3",
    ];
    assert!(
        pending_selectors
            .iter()
            .any(|sel| cx.debug_bounds(sel).is_some()),
        "optimistic pending worktree card must render immediately"
    );

    // 백그라운드 생성이 끝나면 낙관적 행이 실제 워크트리 행과 새 세션으로 매끄럽게 전환된다.
    cx.run_until_parked();
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        let s = sidebar.read(cx);
        assert!(s.creating.is_none());
        assert_eq!(s.selected_worktree(), Some(Path::new("/repo/즉각-반응")));
    });
    assert!(
        !pending_selectors
            .iter()
            .any(|sel| cx.debug_bounds(sel).is_some()),
        "pending worktree card must clear once WorktreeCreated arrives"
    );
    assert_eq!(
        opened.take().map(|s| s.cwd),
        Some(PathBuf::from("/repo/즉각-반응"))
    );
}

#[gpui::test]
fn remote_host_and_repo_selection_creates_worktree_on_selected_host(cx: &mut TestAppContext) {
    use crate::hosts::{HostId, HostStatus};
    use crate::sidebar::NewSession;
    use std::collections::BTreeMap;

    let local_fake = Arc::new(Fake::default());
    local_fake.snapshot.lock().unwrap().trees = BTreeMap::from([(
        PathBuf::from("/local/repo"),
        vec![PathBuf::from("/local/repo")],
    )]);

    let remote_fake = Arc::new(Fake::default());
    remote_fake.snapshot.lock().unwrap().trees = BTreeMap::from([
        (
            PathBuf::from("/remote/repo-a"),
            vec![PathBuf::from("/remote/repo-a")],
        ),
        (
            PathBuf::from("/remote/repo-b"),
            vec![PathBuf::from("/remote/repo-b")],
        ),
    ]);
    remote_fake
        .replies
        .lock()
        .unwrap()
        .push_back(Reply::WorktreeCreated(Worktree {
            path: b"/remote/repo-b/wt-remote".to_vec(),
            branch: "wt-remote".into(),
            base: "origin/main".into(),
            copied: Vec::new(),
            warnings: Vec::new(),
            setup: None,
        }));

    cx.update(gpui_component::init);
    let (sidebar, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    cx.simulate_resize(gpui::size(px(900.), px(800.)));

    let spawned = Arc::new(Mutex::new(Vec::<(Option<HostId>, NewSession)>::new()));
    let spawned_sink = spawned.clone();
    let sidebar_for_sub = sidebar.clone();
    cx.update(|_, cx| {
        cx.subscribe(&sidebar_for_sub, move |s, e: &NewSession, cx| {
            let host = s.update(cx, |s, _| s.take_action_host());
            spawned_sink.lock().unwrap().push((host, e.clone()));
        })
        .detach();
    });

    let local_source: Arc<dyn Source> = local_fake.clone();
    let remote_source: Arc<dyn Source> = remote_fake.clone();
    cx.update(|window, cx| {
        sidebar.update(cx, |s, cx| {
            s.connect_source(local_source, cx);
            s.set_remote_host_source("mini", "Mac mini", HostStatus::Connected, remote_source, cx);
        });
        window.draw(cx).clear(cx);
    });
    cx.run_until_parked();

    cx.update(|window, cx| {
        sidebar.update(cx, |s, cx| {
            s.new_worktree("/local/repo".into(), window, cx);
        });
        window.draw(cx).clear(cx);
    });
    cx.run_until_parked();

    // 칩 나열 대신 프로젝트·실행 위치 콤보박스 두 개만 보인다.
    assert!(cx.debug_bounds("worktree-repo-selector").is_some());
    assert!(cx.debug_bounds("worktree-host-selector").is_some());
    assert!(cx.debug_bounds("worktree-repo-0").is_none());

    let dialog = cx.update(|_, cx| sidebar.read(cx).creating.as_ref().unwrap().0.clone());
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).host().clone()),
        HostId::Local
    );

    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            // 원격에만 있는 프로젝트를 고르면 그 호스트로 옮겨 간다.
            d.select_project("repo-b", window, cx);
            d.name.update(cx, |input, cx| {
                input.set_value("wt-remote", window, cx);
            });
            d.submit(Setup::Ask, cx);
        });
    });
    cx.run_until_parked();

    assert!(
        local_fake.requests.lock().unwrap().is_empty(),
        "local daemon must not receive remote worktree request"
    );
    let remote_reqs = remote_fake.requests.lock().unwrap().clone();
    assert_eq!(remote_reqs.len(), 1);
    assert_eq!(remote_reqs[0].repo, b"/remote/repo-b");
    assert_eq!(remote_reqs[0].name, "wt-remote");

    let events = spawned.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, Some(HostId::Remote("mini".into())));
    assert_eq!(events[0].1.cwd, PathBuf::from("/remote/repo-b/wt-remote"));
}
