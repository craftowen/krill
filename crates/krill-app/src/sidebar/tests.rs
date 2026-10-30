use super::*;

use gpui::TestAppContext;
use std::fmt::Write as _;
use std::fs;

use std::time::Instant;

#[gpui::test]
fn iroh_group_renders_transport_status_and_revoked_guidance(cx: &mut TestAppContext) {
    use krill_hosts::{ExternalHost, HostConnectionInfo, RemoteConnector, RemoteHostSpec};
    cx.update(gpui_component::init);
    let manager = HostManager::new();
    let registration = |revoked| {
        ExternalHost::new(
            RemoteHostSpec {
                id: "iroh:studio".into(),
                label: "작업실 Mac".into(),
                address: "test-node".into(),
                port: None,
                identity_file: None,
                remote_socket: None,
            },
            PathBuf::new(),
            HostConnectionInfo {
                status: if revoked {
                    HostStatus::Disconnected
                } else {
                    HostStatus::Connected
                },
                detail: (!revoked).then(|| "직접 연결".into()),
                error: revoked.then(|| crate::i18n::link_ui::ERR_REVOKED.get().into()),
            },
            || {
                RemoteConnector::new(
                    || Err(std::io::ErrorKind::ConnectionRefused.into()),
                    |_, _| {},
                )
            },
        )
    };
    manager.sync_external_hosts(vec![registration(false)]);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    view.update(vcx, |sidebar, cx| sidebar.sync_remote_hosts(&manager, cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("host-header-local").is_none());
    // A connected device without projects needs no row in project navigation.
    assert!(vcx.debug_bounds("host-header-iroh:studio").is_none());
    view.update(vcx, |sidebar, cx| sidebar.set_group_by(GroupBy::None, cx));
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("host-header-iroh:studio").is_some());
    assert!(vcx.debug_bounds("host-route-iroh:studio").is_some());
    view.update(vcx, |sidebar, cx| sidebar.set_group_by(GroupBy::Repo, cx));
    manager.sync_external_hosts(vec![registration(true)]);
    view.update(vcx, |sidebar, cx| sidebar.sync_remote_hosts(&manager, cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        assert_eq!(
            view.read(cx).remote_info["iroh:studio"].error.as_deref(),
            Some(crate::i18n::link_ui::ERR_REVOKED.get())
        );
        window.draw(cx).clear(cx);
    });
    assert!(vcx.debug_bounds("host-header-iroh:studio").is_none());
    assert!(vcx.debug_bounds("host-error-iroh:studio").is_none());
}

#[gpui::test]
fn re_pairing_the_same_host_replaces_the_sidebar_source_without_an_empty_poll(
    cx: &mut TestAppContext,
) {
    use krill_hosts::{ExternalHost, HostConnectionInfo, RemoteConnector, RemoteHostSpec};
    use std::sync::atomic::{AtomicBool, Ordering};
    cx.update(gpui_component::init);
    let manager = HostManager::new();
    let current = Arc::new(AtomicBool::new(true));
    let registration = |key: Arc<()>, current: Arc<AtomicBool>| {
        ExternalHost::new(
            RemoteHostSpec {
                id: "iroh:studio".into(),
                label: "Studio".into(),
                address: "node".into(),
                port: None,
                identity_file: None,
                remote_socket: None,
            },
            PathBuf::from("/unused"),
            HostConnectionInfo {
                status: HostStatus::Connected,
                detail: None,
                error: None,
            },
            move || {
                let current = current.clone();
                RemoteConnector::new(|| Err(io::ErrorKind::NotConnected.into()), |_, _| {})
                    .with_lifetime(key.clone(), move || current.load(Ordering::Acquire))
            },
        )
    };
    let key = Arc::new(());
    manager.sync_external_hosts(vec![registration(key.clone(), current.clone())]);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    view.update(vcx, |s, cx| s.sync_remote_hosts(&manager, cx));
    vcx.run_until_parked();
    let old = view.read_with(vcx, |s, _| s.remote_hosts[0].source.clone().unwrap());
    // 같은 세대의 주기 갱신은 연결과 GitWatch 캐시를 유지한다.
    manager.sync_external_hosts(vec![registration(key, current.clone())]);
    view.update(vcx, |s, cx| s.sync_remote_hosts(&manager, cx));
    vcx.run_until_parked();
    assert!(view.read_with(vcx, |s, _| Arc::ptr_eq(
        &old,
        s.remote_hosts[0].source.as_ref().unwrap()
    )));
    current.store(false, Ordering::Release);
    manager.sync_external_hosts(vec![registration(
        Arc::new(()),
        Arc::new(AtomicBool::new(true)),
    )]);
    view.update(vcx, |s, cx| s.sync_remote_hosts(&manager, cx));
    vcx.run_until_parked();
    view.read_with(vcx, |s, _| {
        let new = s.remote_hosts[0].source.as_ref().unwrap();
        assert!(!Arc::ptr_eq(&old, new));
        assert!(!old.target().unwrap().is_current());
        assert!(new.target().unwrap().is_current());
    });
}

fn session(id: u64, state: AgentState, tree: Option<&str>) -> Session {
    Session {
        id: SessionId(id),
        status: AgentStatus {
            state,
            ..AgentStatus::UNKNOWN
        },
        worktree: tree.map(PathBuf::from),
        cwd: tree.map(PathBuf::from),
        account: None,
    }
}

fn fixture() -> Snapshot {
    let trees: Vec<_> = (0..250)
        .map(|i| PathBuf::from(format!("/repo/branch-{i:03}")))
        .collect();
    let sessions = trees
        .iter()
        .take(249)
        .enumerate()
        .map(|(i, path)| Session {
            id: SessionId(u64::try_from(i).unwrap()),
            status: AgentStatus::UNKNOWN,
            worktree: Some(path.clone()),
            cwd: Some(path.clone()),
            account: None,
        })
        .collect();
    Snapshot {
        sessions,
        trees: BTreeMap::from([(PathBuf::from("/repo"), trees)]),
    }
}

#[test]
fn windows_host_paths_name_and_nest_like_native_paths() {
    let root = Path::new(r"C:\Users\user\orca\sd-tax");
    assert_eq!(basename(root), "sd-tax");
    assert_eq!(basename(Path::new(r"C:\Users\user\orca\sd-tax\")), "sd-tax");
    assert!(path_within(root, root));
    assert!(path_within(
        Path::new("c:/users/user/orca/sd-tax/src"),
        root
    ));
    assert!(!path_within(
        Path::new(r"C:\Users\user\orca\sd-tax-2"),
        root
    ));
    assert!(!path_within(Path::new(r"C:\Users\user"), root));
    assert_eq!(basename(Path::new("/repo/a")), "a");
    assert!(path_within(Path::new("/repo/a/b"), Path::new("/repo/a")));
    assert!(!path_within(Path::new("/repo/ab"), Path::new("/repo/a")));
}

#[test]
fn needs_you_precedes_pins_and_collapsed_worktrees() {
    let snapshot = Snapshot {
        sessions: vec![
            session(1, AgentState::Working, Some("/repo/a")),
            session(2, AgentState::Waiting, Some("/repo/a")),
            session(3, AgentState::Blocked, None),
        ],
        trees: BTreeMap::from([(
            PathBuf::from("/repo"),
            vec!["/repo/a".into(), "/repo/b".into()],
        )]),
    };
    let prefs = BTreeMap::from([(
        PathBuf::from("/repo/b"),
        Preference {
            pinned: true,
            name: "한글 이름 🦐".into(),
        },
    )]);
    let collapsed = BTreeSet::from([PathBuf::from("/repo/a")]);
    let options = WorkspaceOptions {
        group_by: GroupBy::None,
        ..Default::default()
    };
    let rows = options::rows_with_options(
        &snapshot,
        &prefs,
        &collapsed,
        &BTreeSet::new(),
        &BTreeSet::new(),
        &options,
        &options::WorktreeContext::default(),
    );
    let project_rows = super::rows(&snapshot, &prefs, &collapsed);
    assert!(
        !project_rows.contains(&Row::NeedsYou),
        "project mode keeps attention inside projects and the toolbar bell"
    );
    assert!(matches!(
        rows[0],
        Row::GroupHeader {
            key: GroupKey::Pinned,
            ..
        }
    ));
    assert_eq!(rows[1], Row::Worktree("/repo/b".into()));
    let needs = rows.iter().position(|row| *row == Row::NeedsYou).unwrap();
    assert!(matches!(&rows[needs + 1], Row::Session(s, false) if s.id == SessionId(2)));
    assert!(matches!(&rows[needs + 2], Row::Session(s, false) if s.id == SessionId(3)));
    assert!(
        !rows
            .iter()
            .any(|r| matches!(r, Row::Session(s, true) if s.id == SessionId(1)))
    );
}

#[gpui::test]
fn virtualized_500_rows_scroll_and_idle(cx: &mut TestAppContext) {
    struct Memory(Snapshot);
    impl Source for Memory {
        fn fetch(&self) -> io::Result<Snapshot> {
            Ok(self.0.clone())
        }
    }
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let fixture = fixture();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.connect_source(Arc::new(Memory(fixture.clone())), cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).rows.len()), 500);
    assert!(cx.update(|_, cx| view.read(cx).rendered_rows) < 100);
    let before = cx.update(|_, cx| view.read(cx).renders);
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
    }
    assert_eq!(
        cx.update(|_, cx| view.read(cx).renders),
        before,
        "stable snapshots must cause zero idle renders"
    );
    let mut samples = Vec::new();
    let mut max_rows = 0;
    for i in 0..240 {
        let start = Instant::now();
        cx.update(|_, cx| {
            view.update(cx, |s, cx| {
                s.rendered_rows = 0;
                s.scroll_to_row((i * 7) % 500, cx);
                cx.notify();
            });
        });
        cx.run_until_parked();
        samples.push(start.elapsed());
        max_rows = max_rows.max(cx.update(|_, cx| view.read(cx).rendered_rows));
    }
    samples.sort_unstable();
    println!(
        "MK-046 test-platform 500 rows: frames={} p50_us={} p99_us={} max_us={} max_rows_per_frame={} idle_renders=0",
        samples.len(),
        samples[120].as_micros(),
        samples[237].as_micros(),
        samples[239].as_micros(),
        max_rows
    );
    assert!(max_rows < 100, "virtual list must not construct 500 rows");
}

#[gpui::test]
fn rename_pins_persist_and_selection_emits(cx: &mut TestAppContext) {
    use std::cell::Cell;
    use std::rc::Rc;
    let path = std::env::temp_dir().join(format!("mk-sidebar-{}-prefs.bin", std::process::id()));
    fs::remove_file(&path).ok();
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(path.clone())));
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.prefs.entry("/repo/a".into()).or_default().pinned = true;
            s.rename("/repo/a".into(), window, cx);
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("한글 🦐");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let restored = Sidebar::new_loaded(Some(path.clone()));
    assert_eq!(restored.name(Path::new("/repo/a")), "한글 🦐");
    assert!(restored.prefs[Path::new("/repo/a")].pinned);
    let selected = Rc::new(Cell::new(None));
    let got = selected.clone();
    let _subscription = cx
        .update(|_, cx| cx.subscribe(&view, move |_, event: &Selected, _| got.set(Some(event.0))));
    cx.update(|_, cx| view.update(cx, |_, cx| cx.emit(Selected(SessionId(7)))));
    cx.run_until_parked();
    assert_eq!(selected.get(), Some(SessionId(7)));
    fs::remove_file(path).ok();
}

/// MK-078: 스크린 리더가 읽는 역할·이름·누를 수 있는지(접근성 노드).
fn a11y(element: &impl gpui::Element) -> (Option<gpui::Role>, gpui::accesskit::Node) {
    let mut node = gpui::accesskit::Node::new(gpui::Role::Unknown);
    element.write_a11y_info(&mut node);
    (element.a11y_role(), node)
}

#[gpui::test]
fn rows_expose_voiceover_names_roles_and_press(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let mut waiting = session(2, AgentState::Waiting, Some("/repo/a"));
    waiting.status.agent = "claude".into();
    let snapshot = Snapshot {
        sessions: vec![waiting, session(3, AgentState::Working, None)],
        trees: BTreeMap::from([(PathBuf::from("/repo"), vec!["/repo/a".into()])]),
    };
    let labels = cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = snapshot;
            s.collapsed.insert("/repo/a".into());
            s.rebuild(cx);
            let count = s.rows.len();
            s.rows
                .clone()
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    let (role, node) = a11y(&s.render_row(i, row, cx));
                    assert_eq!(role, Some(gpui::Role::ListItem), "{row:?}");
                    assert_eq!(node.position_in_set(), Some(i + 1));
                    assert_eq!(node.size_of_set(), Some(count));
                    if let Row::Worktree(_) = row {
                        assert_eq!(node.is_expanded(), Some(false));
                    }
                    if let Row::Session(..) = row {
                        // VO-스페이스(AXPress)가 클릭과 같은 선택을 한다.
                        assert!(node.supports_action(Action::Click), "{row:?}");
                    }
                    node.label().unwrap_or_default().to_owned()
                })
                .collect::<Vec<_>>()
        })
    });
    // OWE-900: 저장소 헤더 접근성 레이블에 워크트리 수(자기 자신 제외)가 붙는다.
    assert_eq!(
        labels,
        [
            "저장소 repo 1",
            "워크트리 a, 입력 대기",
            "작업 폴더 미확인 세션",
            "세션 3, 작업 중",
        ]
    );
}

#[gpui::test]
fn session_list_is_a_named_list(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let (role, node) = cx.update(|_, cx| view.update(cx, |s, cx| a11y(&s.render_list(cx))));
    assert_eq!(role, Some(gpui::Role::List));
    assert_eq!(node.label(), Some(text::PROJECTS.get()));
}

#[gpui::test]
fn worktree_comments_display_and_expose_voiceover(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mk-sidebar-{}-comments", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    external::save_comment(&dir, Path::new("/repo/a"), "PR #42 리뷰\n대기")
        .expect("save worktree comment");
    let sidebar_file = dir.join("sidebar.bin");
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(sidebar_file)));
    let snapshot = Snapshot {
        sessions: Vec::new(),
        trees: BTreeMap::from([(PathBuf::from("/repo"), vec![PathBuf::from("/repo/a")])]),
    };
    let label = cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let comments = external::load_comments(&dir);
            s.apply(Ok((snapshot, Meta::new())), comments, cx);
            assert_eq!(s.comment(Path::new("/repo/a")), Some("PR #42 리뷰 대기"));
            assert_eq!(s.comment(Path::new("/repo")), None);
            assert_eq!(
                s.comment(Path::new("/repo/a/.claude/worktrees/agent-1")),
                None
            );
            let row = s.rows[1].clone();
            let (_, node) = a11y(&s.render_row(1, &row, cx));
            node.label().unwrap_or_default().to_owned()
        })
    });
    assert_eq!(label, "워크트리 a, PR #42 리뷰 대기");
    external::save_comment(&dir, Path::new("/repo/a"), "").expect("clear worktree comment");
    assert!(external::load_comments(&dir).is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn worktree_comments_evict_oldest_by_time() {
    let dir = std::env::temp_dir().join(format!("mk-sidebar-{}-evict", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    // 사전순 첫 키("/a0000")는 가장 새 시각, "/victim"은 가장 오래된 시각으로 1,024개를 채운다.
    let mut comments = String::from("[comments]\n");
    let mut times = String::from("[updated_at]\n");
    for i in 0..1_024u32 {
        let key = if i == 512 {
            "/victim".to_owned()
        } else {
            format!("/a{i:04}")
        };
        let stamp = if i == 512 {
            1
        } else if i == 0 {
            9_999_999
        } else {
            1_000 + u64::from(i)
        };
        writeln!(&mut comments, "\"{key}\" = \"c{i}\"").unwrap();
        writeln!(&mut times, "\"{key}\" = {stamp}").unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(external::COMMENTS_FILE),
        format!("{comments}{times}"),
    )
    .unwrap();
    external::save_comment(&dir, Path::new("/repo/new"), "새 코멘트").unwrap();
    let loaded = external::load_comments(&dir);
    assert!(loaded.contains_key(Path::new("/repo/new")));
    assert!(
        !loaded.contains_key(Path::new("/victim")),
        "oldest by time must go"
    );
    assert!(
        loaded.contains_key(Path::new("/a0000")),
        "lexicographically-first but newest must stay"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn claude_worktrees_hidden_card_and_toggle(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mk-sidebar-{}-claude", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let repo = dir.join("repo");
    let claude_wt1 = repo.join(".claude/worktrees/agent-1");
    let claude_wt2 = repo.join(".claude/worktrees/agent-2");
    std::fs::create_dir_all(&claude_wt1).expect("create claude wt1");
    std::fs::create_dir_all(&claude_wt2).expect("create claude wt2");
    let repo = std::fs::canonicalize(&repo).unwrap_or(repo);
    let claude_wt1 = std::fs::canonicalize(&claude_wt1).unwrap_or(claude_wt1);
    let claude_wt2 = std::fs::canonicalize(&claude_wt2).unwrap_or(claude_wt2);

    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let mut snapshot = Snapshot {
        sessions: Vec::new(),
        trees: BTreeMap::from([(repo.clone(), vec![repo.clone()])]),
    };
    external::include_claude_worktrees(&mut snapshot.trees);
    let (collapsed_labels, hidden_expanded) = cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.apply(Ok((snapshot, Meta::new())), BTreeMap::new(), cx);
            let labels: Vec<_> = s
                .rows
                .clone()
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    a11y(&s.render_row(i, r, cx))
                        .1
                        .label()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect();
            let expanded = a11y(&s.render_row(2, &s.rows[2].clone(), cx))
                .1
                .is_expanded();
            (labels, expanded)
        })
    });
    // OWE-900: 저장소 헤더 접근성 레이블에 워크트리 수(자기 자신 제외)가 붙는다.
    assert_eq!(
        collapsed_labels,
        [
            "저장소 repo 2",
            "워크트리 repo",
            ".claude/worktrees 숨김 (2)"
        ]
    );
    assert_eq!(hidden_expanded, Some(false));

    let (expanded_rows, hidden_expanded_after) = cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.toggle_external(&repo, cx);
            let expanded = a11y(&s.render_row(2, &s.rows[2].clone(), cx))
                .1
                .is_expanded();
            (s.rows.clone(), expanded)
        })
    });
    assert_eq!(hidden_expanded_after, Some(true));
    assert!(expanded_rows.contains(&Row::Worktree(claude_wt1)));
    assert!(expanded_rows.contains(&Row::Worktree(claude_wt2)));

    cx.update(|_, cx| view.update(cx, |s, cx| s.toggle_external(&repo, cx)));
    assert_eq!(cx.update(|_, cx| view.read(cx).rows.len()), 3);
    std::fs::remove_dir_all(dir).ok();
}

/// OWE-576: "알 수 없음" 대신 의미 있는 이름(터미널 제목 → 에이전트 → 폴더 → 세션 n).
#[test]
fn session_names_prefer_title_then_agent_then_folder() {
    let mut s = session(4, AgentState::Unknown, Some("/repo/feature"));
    assert_eq!(session_label(&s, None), ("feature".into(), String::new()));
    s.status.agent = "codex".into();
    assert_eq!(session_label(&s, None), ("codex".into(), "feature".into()));
    assert_eq!(
        session_label(&s, Some(" 리팩터링 중 ")),
        ("리팩터링 중".into(), "codex · feature".into())
    );
    let bare = session(9, AgentState::Unknown, None);
    assert_eq!(
        session_label(&bare, Some("")),
        ("세션 9".into(), String::new())
    );
}

/// 주 워크트리(.git 디렉터리)·추가 워크트리(gitdir 링크)·detached HEAD.
#[test]
fn branch_follows_git_dir_and_worktree_links() {
    let root = std::env::temp_dir().join(format!("mk-sidebar-branch-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let main = root.join("main");
    fs::create_dir_all(main.join(".git/worktrees/feat")).unwrap();
    fs::write(main.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let feat = root.join("feat");
    fs::create_dir_all(&feat).unwrap();
    fs::write(
        feat.join(".git"),
        format!("gitdir: {}\n", main.join(".git/worktrees/feat").display()),
    )
    .unwrap();
    fs::write(
        main.join(".git/worktrees/feat/HEAD"),
        "ref: refs/heads/한글/브랜치\n",
    )
    .unwrap();
    assert_eq!(ipc::branch(&main).as_deref(), Some("main"));
    assert_eq!(ipc::branch(&feat).as_deref(), Some("한글/브랜치"));
    fs::write(main.join(".git/HEAD"), "0123456789abcdef\n").unwrap();
    assert_eq!(ipc::branch(&main), None);
    assert_eq!(ipc::branch(&root.join("missing")), None);
    fs::remove_dir_all(root).ok();
}

/// 워크트리 행: 상태별 묶음(OWE-662), 바꾼 이름이 없으면 브랜치가 이름.
#[gpui::test]
fn worktree_rows_summarize_sessions_and_branch(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut a = session(1, AgentState::Working, Some("/repo/a"));
            a.status.agent = "claude".into();
            let mut b = session(2, AgentState::Blocked, Some("/repo/a"));
            b.status.agent = "claude".into();
            let mut c = session(3, AgentState::Done, Some("/repo/a"));
            c.status.agent = "codex".into();
            s.snapshot.sessions = vec![a, b, c];
            s.meta.insert(
                "/repo/a".into(),
                Tree {
                    branch: Some("feat/x".into()),
                    dirty: Some(3),
                },
            );
            s.rebuild(cx);
            let (summary, unread) = s.tree_summary(Path::new("/repo/a"));
            let groups: Vec<_> = summary
                .groups
                .iter()
                .map(|g| (g.state, g.icons.clone()))
                .collect();
            assert_eq!(
                groups,
                [
                    (AgentState::Blocked, vec!["claude"]),
                    (AgentState::Working, vec!["claude"]),
                    (AgentState::Done, vec!["codex"]),
                ]
            );
            assert!(!unread);
            assert_eq!(s.name(Path::new("/repo/a")), "feat/x");
            s.prefs.entry("/repo/a".into()).or_default().name = "내 작업".into();
            assert_eq!(s.name(Path::new("/repo/a")), "내 작업");
        });
    });
}

/// 키보드 탐색: ↓로 행을 고르고 Enter가 세션을 열며 포커스를 셸(back)로 돌린다. ←/→는 워크트리를 접고 편다.
#[gpui::test]
fn keyboard_navigation_selects_folds_and_returns_focus(cx: &mut TestAppContext) {
    use std::cell::Cell;
    use std::rc::Rc;
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let back = cx.update(|_, cx| cx.focus_handle());
    let selected = Rc::new(Cell::new(None));
    let got = selected.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &Selected, _| {
            got.set(Some(event.0));
        })
    });
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: vec![session(5, AgentState::Done, Some("/repo/a"))],
                trees: BTreeMap::from([(PathBuf::from("/repo"), vec!["/repo/a".into()])]),
            };
            s.rebuild(cx);
            s.focus(back.clone(), window, cx);
        });
    });
    cx.run_until_parked();
    // 처음 들어오면 첫 세션 행(저장소, 워크트리, 세션).
    assert_eq!(cx.update(|_, cx| view.read(cx).cursor), Some(2));
    cx.simulate_keystrokes("up left");
    assert_eq!(cx.update(|_, cx| view.read(cx).rows.len()), 2, "접힘");
    cx.simulate_keystrokes("right down enter");
    assert_eq!(selected.get(), Some(SessionId(5)));
    assert!(cx.update(|window, _| back.is_focused(window)));
}

/// OWE-576: 앱이 쓰는 gpui-component 아이콘(UI-1 타이틀바 포함)이 `main.rs`의 자산 소스에서 실제 SVG로 읽힌다.
/// 자산 소스가 없으면 gpui 기본 `()` 소스가 `Ok(None)`을 돌려 아이콘이 빈칸으로 그려진다.
#[test]
fn app_icons_resolve_in_asset_source() {
    use gpui::AssetSource as _;
    use gpui_component::{IconName, IconNamed as _};
    let used = [
        IconName::Bell,
        IconName::Bot,
        IconName::ChevronDown,
        IconName::ChevronRight,
        IconName::Ellipsis,
        IconName::ExternalLink,
        IconName::Folder,
        IconName::FolderOpen,
        IconName::Loader,
        IconName::Moon,
        IconName::PanelLeft,
        IconName::PanelRight,
        IconName::Plus,
        IconName::RefreshCw,
        IconName::Search,
        IconName::SquareTerminal,
        IconName::Star,
        IconName::StarFill,
        IconName::User,
    ];
    for icon in used {
        let path = icon.path();
        let svg = crate::ui::icons::AppAssets.load(&path).unwrap();
        assert!(svg.is_some_and(|b| b.starts_with(b"<svg")), "{path}");
    }
    // OWE-605: agy 탭 아이콘(Moon)은 기본 번들에서 실제 SVG로 읽히고 핀 별과 경로가 다르다.
    let svg = crate::ui::icons::AppAssets
        .load(&IconName::Moon.path())
        .unwrap();
    assert!(svg.is_some_and(|b| b.starts_with(b"<svg")), "moon");
    assert_ne!(
        IconName::Moon.path(),
        IconName::Star.path(),
        "agy가 핀 별과 같은 경로"
    );
    assert!(().load("icons/plus.svg").unwrap().is_none());
}

/// OWE-598(N4 + 키보드 접근): 워크트리 행에서 Enter가 새 터미널 세션을 띄워 포커스를 돌려주고,
/// `p`(핀 토글)·`o`(Finder에서 열기)·`r`(이름 변경)·`⌥⌘1`(첫 에이전트 실행)이 키보드로 동작한다.
#[gpui::test]
fn worktree_enter_and_keyboard_actions_launch_session_pin_rename_and_open_in(
    cx: &mut TestAppContext,
) {
    use krill_proto::control::Program;
    use std::cell::RefCell;
    use std::rc::Rc;

    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let back = cx.update(|_, cx| cx.focus_handle());
    let launched: Rc<RefCell<Vec<NewSession>>> = Rc::new(RefCell::new(Vec::new()));
    let got = launched.clone();
    let _sub = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &NewSession, _| {
            got.borrow_mut().push(event.clone());
        })
    });

    let workspaces = Rc::new(RefCell::new(Vec::new()));
    let got_ws = workspaces.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &WorkspaceSelected, _| {
            got_ws.borrow_mut().push(event.path.clone());
        })
        .detach();
    });

    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: vec![session(5, AgentState::Done, Some("/repo/a"))],
                trees: BTreeMap::from([(PathBuf::from("/repo"), vec!["/repo/a".into()])]),
            };
            s.rebuild(cx);
            s.focus(back.clone(), window, cx);
        });
    });
    cx.run_until_parked();

    // 위로 한 칸 올라가 워크트리 행(`/repo/a`, index 1)에 커서를 둔다.
    cx.simulate_keystrokes("up");
    assert_eq!(cx.update(|_, cx| view.read(cx).cursor), Some(1));

    // `p`로 핀 토글
    cx.simulate_keystrokes("p");
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| view.read(cx).prefs[Path::new("/repo/a")].pinned),
        "p must toggle pin on worktree row"
    );

    // `o`로 Finder에서 열기
    cx.simulate_keystrokes("o");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).last_open_in.clone()),
        Some((Target::Finder, PathBuf::from("/repo/a"))),
        "o must trigger Open in Finder for worktree row"
    );

    // `r`로 이름 변경 시작 -> 입력 중 p/o/r 글자가 단축키로 가로채지지 않고 입력되어야 함 -> enter로 확정
    cx.simulate_keystrokes("r");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).editing.is_some()));
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("pro-워크트리");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).name(Path::new("/repo/a"))),
        "pro-워크트리"
    );

    // 워크트리 행에서 Enter를 누르면 접기 대신 `Program::Login` 새 세션 이벤트를 내보내고 `back`으로 포커스를 돌려준다.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(launched.borrow().is_empty(), "Enter reuses the workspace");
    assert_eq!(workspaces.borrow().last(), Some(&PathBuf::from("/repo/a")));
    assert!(cx.update(|window, _| back.is_focused(window)));

    // 다시 사이드바에 포커스를 주고 워크트리 행에서 `alt-cmd-1`을 누르면 첫 에이전트 프로필(claude)로 새 세션을 띄운다.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| s.focus(back.clone(), window, cx));
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("alt-cmd-1");
    cx.run_until_parked();
    assert_eq!(
        launched.borrow().last(),
        Some(&NewSession {
            cwd: PathBuf::from("/repo/a"),
            program: Program::Agent {
                id: "claude".into(),
            },
        })
    );
}

/// OWE-612: 워크트리 행 호버 `+` 버튼은 우클릭 메뉴·Enter와 같은 `Program::Login` 새 세션을 띄운다.
#[gpui::test]
fn worktree_hover_plus_button_launches_login_session(cx: &mut TestAppContext) {
    use gpui::{Modifiers, MouseButton};
    use krill_proto::control::Program;
    use std::cell::RefCell;
    use std::rc::Rc;

    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let launched: Rc<RefCell<Vec<NewSession>>> = Rc::new(RefCell::new(Vec::new()));
    let got = launched.clone();
    let _sub = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &NewSession, _| {
            got.borrow_mut().push(event.clone());
        })
    });

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: Vec::new(),
                trees: BTreeMap::from([(PathBuf::from("/repo"), vec!["/repo/a".into()])]),
            };
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();

    let index = cx.update(|_, cx| {
        view.read(cx)
            .rows
            .iter()
            .position(|r| matches!(r, Row::Worktree(_)))
            .expect("worktree row")
    });
    assert_eq!(index, 1);
    // 호버 전에도 레이아웃은 있어 버튼 bounds가 기록된다(수정 전: None).
    let at = cx
        .debug_bounds("worktree-new-session-1")
        .expect("hover plus button must be rendered on worktree row")
        .center();
    // 행에 호버하면 버튼이 드러나고, 클릭하면 로그인 세션 이벤트가 나온다.
    cx.simulate_mouse_move(at, None::<MouseButton>, Modifiers::default());
    cx.run_until_parked();
    cx.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        launched.borrow().as_slice(),
        &[NewSession {
            cwd: PathBuf::from("/repo/a"),
            program: Program::Login,
        }]
    );
}

/// OWE-598(N19) · OWE-844: 사이드바가 접히면(`set_visible(false)`) 폴링과 git 감시를 즉시 멈추고,
/// 창이 비포커스(`set_active(false)`)여도 사이드바가 표시 중(`visible == true`)이면 완화된 주기로
/// 백그라운드 폴링·데스크톱 알림·수동 새로고침을 유지한다.
#[gpui::test]
fn sidebar_stops_polling_and_hides_git_watch_when_hidden_or_inactive(cx: &mut TestAppContext) {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct CountingSource {
        fetches: AtomicUsize,
        visible: AtomicBool,
        snapshot: Mutex<Snapshot>,
    }
    impl Source for CountingSource {
        fn fetch(&self) -> io::Result<Snapshot> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            Ok(self.snapshot.lock().unwrap().clone())
        }
        fn set_visible(&self, visible: bool) {
            self.visible.store(visible, Ordering::SeqCst);
        }
    }

    // 1. `WatchCache`가 `GitWatch`의 `Changed`/`Unchanged`를 재사용하고 `clear` 시 `visible=false`를 보내는지 확인한다.
    let mut cache = ipc::WatchCache::default();
    let trees = BTreeMap::from([(PathBuf::from("/repo"), vec![PathBuf::from("/repo/wt")])]);
    let d1 = cache.poll(&trees, |req| {
        assert_eq!(req.repo, b"/repo/wt");
        assert!(req.visible);
        assert_eq!(req.since, 0);
        Ok(krill_proto::git::GitWatchReply::Changed {
            generation: 7,
            changes: krill_proto::git::Changes {
                files: vec![krill_proto::git::ChangedFile {
                    path: b"a.rs".to_vec(),
                    old_path: None,
                    status: *b" M",
                    staged: None,
                    unstaged: None,
                }],
                limited: false,
            },
        })
    });
    assert_eq!(d1.get(Path::new("/repo/wt")), Some(&1));

    let d2 = cache.poll(&trees, |req| {
        assert_eq!(req.since, 7, "subsequent poll must pass last generation");
        Ok(krill_proto::git::GitWatchReply::Unchanged { pending: false })
    });
    assert_eq!(
        d2.get(Path::new("/repo/wt")),
        Some(&1),
        "GitWatch Unchanged must reuse cached dirty count without running git status"
    );

    let mut hidden_requests = 0usize;
    cache.clear(|req| {
        assert!(!req.visible);
        hidden_requests += 1;
        Ok(krill_proto::git::GitWatchReply::Hidden)
    });
    assert_eq!(
        hidden_requests, 1,
        "clear must send visible=false to GitWatch"
    );

    // 2. 사이드바 UI 가시성·활성 상태에 따른 폴링·알림·수동 새로고침 확인
    cx.update(gpui_component::init);
    let source = Arc::new(CountingSource {
        fetches: AtomicUsize::new(0),
        visible: AtomicBool::new(true),
        snapshot: Mutex::new(Snapshot {
            sessions: vec![session(1, AgentState::Working, Some("/repo/wt"))],
            trees: BTreeMap::from([(PathBuf::from("/repo"), vec![PathBuf::from("/repo/wt")])]),
        }),
    });
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.connect_source(source.clone(), cx);
        });
    });
    cx.run_until_parked();
    let initial = source.fetches.load(Ordering::SeqCst);
    assert!(initial >= 1);

    // 사이드바를 접으면(`set_visible(false)`) 10초가 지나도 추가 폴링이 0회이고 source.set_visible(false)가 호출된다.
    cx.update(|_, cx| view.update(cx, |s, cx| s.set_visible(false, cx)));
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| view.read(cx).is_visible()));
    assert!(!source.visible.load(Ordering::SeqCst));
    let after_hide = source.fetches.load(Ordering::SeqCst);
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
    }
    assert_eq!(
        source.fetches.load(Ordering::SeqCst),
        after_hide,
        "hidden sidebar must not poll"
    );

    // 다시 펼치면 폴링이 재개된다.
    cx.update(|_, cx| view.update(cx, |s, cx| s.set_visible(true, cx)));
    cx.run_until_parked();
    assert!(source.fetches.load(Ordering::SeqCst) > after_hide);

    // OWE-844: 창이 비포커스(`set_active(false)`)되어도 사이드바가 열려 있으면(`visible == true`)
    // 가시성·GitWatch가 유지되고, 완화된 주기(2초)로 백그라운드 상태 전환·알림 및 수동 새로고침이 동작한다.
    source.visible.store(true, Ordering::SeqCst);
    cx.update(|_, cx| {
        crate::alert::set_active(false, cx);
        view.update(cx, |s, cx| s.set_active(false, cx));
    });
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).is_visible()));
    assert!(!cx.update(|_, cx| view.read(cx).is_active()));
    assert!(
        source.visible.load(Ordering::SeqCst),
        "unfocused visible sidebar must keep GitWatch active"
    );

    // 비포커스 상태에서 에이전트가 Waiting으로 바뀌고 새 워크트리가 추가되었을 때 백그라운드 폴링과 데스크톱 알림이 반영된다.
    *source.snapshot.lock().unwrap() = Snapshot {
        sessions: vec![
            session(1, AgentState::Waiting, Some("/repo/wt")),
            session(2, AgentState::Working, Some("/repo/wt-2")),
        ],
        trees: BTreeMap::from([(
            PathBuf::from("/repo"),
            vec![PathBuf::from("/repo/wt"), PathBuf::from("/repo/wt-2")],
        )]),
    };
    let after_inactive = source.fetches.load(Ordering::SeqCst);
    for _ in 0..5 {
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
    }
    assert!(
        source.fetches.load(Ordering::SeqCst) > after_inactive,
        "unfocused visible sidebar must continue background polling"
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).session(SessionId(1)).map(|s| s.status.state)),
        Some(AgentState::Waiting),
        "agent state transition must update while window is unfocused"
    );
    assert!(
        cx.update(|_, cx| view.read(cx).session(SessionId(2)).is_some()),
        "new session/worktree must appear while window is unfocused"
    );
    assert_eq!(
        cx.update(|_, cx| crate::alert::dock_badge(cx)),
        Some("1".to_owned()),
        "desktop alert/badge must update from sidebar poll while window is unfocused"
    );

    // 비포커스 상태에서 수동 새로고침(`refresh`) 호출 시 타이머를 기다리지 않고 즉시 실행된다.
    let before_manual = source.fetches.load(Ordering::SeqCst);
    *source.snapshot.lock().unwrap() = Snapshot {
        sessions: vec![session(1, AgentState::Done, Some("/repo/wt"))],
        trees: BTreeMap::from([(PathBuf::from("/repo"), vec![PathBuf::from("/repo/wt")])]),
    };
    cx.update(|_, cx| view.update(cx, Sidebar::refresh));
    cx.run_until_parked();
    assert!(
        source.fetches.load(Ordering::SeqCst) > before_manual,
        "manual refresh must execute immediately even when window is unfocused"
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).session(SessionId(1)).map(|s| s.status.state)),
        Some(AgentState::Done),
    );
}

/// OWE-598: `sidebar.bin`(핀·이름)은 버전 바이트로 시작하며, 손상·미지원 버전 파일은 `.corrupt`로 보존하고
/// 보존하지 못한 파일은 기본값으로 덮어쓰지 않는다.
#[gpui::test]
fn corrupt_and_unreadable_sidebar_prefs_preserve_original_and_prevent_overwrite(
    cx: &mut TestAppContext,
) {
    let dir = std::env::temp_dir().join(format!("mk-sidebar-{}-corrupt", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sidebar.bin");

    // 1. 정상 저장 파일은 첫 바이트가 버전 바이트(1)여야 한다.
    let mut prefs = Preferences::new();
    prefs.insert(
        PathBuf::from("/repo/a"),
        Preference {
            pinned: true,
            name: "보존".into(),
        },
    );
    save(&path, &prefs).unwrap();
    let raw = fs::read(&path).unwrap();
    assert_eq!(
        raw.first().copied(),
        Some(FORMAT_VERSION),
        "must start with version byte 1"
    );
    assert_eq!(load(&path).unwrap(), prefs);

    // 2. 손상 파일(버전 바이트 없음/깨진 바이트)과 미래 버전 파일은 `.corrupt`로 보존한다.
    fs::write(&path, b"broken-without-version").unwrap();
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(path.clone())));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            assert!(s.error.is_some(), "corrupt sidebar.bin must surface error");
            let recovery_error = s.error.clone();
            s.apply(Ok((Snapshot::default(), Meta::new())), BTreeMap::new(), cx);
            assert_eq!(
                s.error, recovery_error,
                "poll success must retain corruption recovery error"
            );
            s.toggle_pin("/repo/a".into(), cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        fs::read(path.with_extension("corrupt")).unwrap(),
        b"broken-without-version"
    );
    assert!(
        load(&path).unwrap()[Path::new("/repo/a")].pinned,
        "after preserving .corrupt, new pin saves cleanly"
    );

    let future = [FORMAT_VERSION + 1, 42];
    fs::write(&path, future).unwrap();
    assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read(path.with_extension("1.corrupt")).unwrap(), future);
    assert_eq!(
        fs::read(path.with_extension("corrupt")).unwrap(),
        b"broken-without-version"
    );

    // 3. 보존조차 할 수 없는 경로(디렉터리 등)는 path를 비활성화해 덮어쓰기를 시도하지 않는다.
    let unreadable = dir.join("unreadable.bin");
    fs::create_dir_all(&unreadable).unwrap();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            *s = Sidebar::new_loaded(Some(unreadable.clone()));
            assert!(s.error.is_some());
            assert!(
                s.path.is_none(),
                "failed preservation must disable overwriting"
            );
            s.toggle_pin("/repo/b".into(), cx);
        });
    });
    assert!(unreadable.is_dir());
    assert!(!unreadable.with_extension("tmp").exists());

    fs::remove_dir_all(dir).ok();
}

#[test]
fn jump_items_fall_back_to_worktree_and_session_names() {
    use krill_proto::control::{AgentState, AgentStatus};
    let mut sidebar = Sidebar::new_loaded(None);
    let session = |id: u64, agent: &str, state: AgentState, worktree: Option<&str>| Session {
        id: SessionId(id),
        status: AgentStatus {
            agent: agent.into(),
            state,
            ..AgentStatus::UNKNOWN
        },
        worktree: worktree.map(PathBuf::from),
        cwd: None,
        account: None,
    };
    sidebar.snapshot.sessions = vec![
        session(1, "codex", AgentState::Working, Some("/repo/alpha")),
        session(2, "", AgentState::Unknown, Some("/repo/beta")),
        session(3, "", AgentState::Unknown, None),
    ];
    let items = sidebar.jump_items();
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[0].label,
        format!("codex · alpha · {}", state_text(AgentState::Working))
    );
    assert_eq!(
        items[1].label,
        format!("beta · {}", state_text(AgentState::Unknown))
    );
    assert_eq!(
        items[2].label,
        format!(
            "{} 3 · {}",
            text::SESSION.get(),
            state_text(AgentState::Unknown)
        )
    );
    for item in &items {
        assert!(
            !item.label.contains("  ") && !item.label.contains("·  ·"),
            "no blank slots, got {:?}",
            item.label
        );
    }
}

#[test]
fn jump_provider_uses_foreground_when_session_has_a_custom_title() {
    let mut sidebar = Sidebar::new_loaded(None);
    sidebar.snapshot.sessions = vec![Session {
        id: SessionId(1),
        status: AgentStatus {
            agent: "zsh".into(),
            foreground: Some("/opt/bin/codex".into()),
            title: Some("Renamed task".into()),
            ..AgentStatus::UNKNOWN
        },
        worktree: None,
        cwd: None,
        account: None,
    }];
    assert_eq!(sidebar.jump_items()[0].agent, "codex");
    sidebar
        .custom_titles
        .insert(SessionId(1), "Codex alias".into());
    sidebar.snapshot.sessions[0].status.foreground = None;
    sidebar.snapshot.sessions[0].status.title = Some("Claude task".into());
    let item = &sidebar.jump_items()[0];
    assert_eq!(item.title, "Codex alias");
    assert_eq!(item.agent, "claude");
}

#[gpui::test]
fn pr_badge_renders_and_missing_cli_hides_it_without_external_io(cx: &mut TestAppContext) {
    use crate::pull_request::Status;
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let path = PathBuf::from("/repo/feature");
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot.trees.insert("/repo".into(), vec![path.clone()]);
            s.prs.insert(path.clone(), Status::Open);
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("pr-badge-1").is_some());
    let renders = cx.update(|_, cx| view.read(cx).renders);
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).renders), renders);
    let header_before = cx.debug_bounds("sidebar-header").unwrap();
    let list_before = cx.debug_bounds("sidebar-viewport").unwrap();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.prs.insert(path.clone(), Status::Missing);
            s.poll_error = Some(("daemon slow".into(), false));
            s.notice = Some("sidebar notice".into());
            s.error = Some("save failed".into());
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("pr-badge-1").is_none());
    // 인라인 알림 배너는 제거하고 우하단 토스트로 위임하므로 사이드바 레이아웃 시프트가 발생하지 않는다.
    assert!(cx.debug_bounds("sidebar-pr-guidance").is_none());
    assert!(cx.debug_bounds("sidebar-preference-error").is_none());
    assert_eq!(cx.debug_bounds("sidebar-header").unwrap(), header_before);
    assert_eq!(cx.debug_bounds("sidebar-viewport").unwrap(), list_before);
    assert!(cx.update(|_, cx| view.read(cx).pr_guidance().is_some()));
    cx.update(|_, cx| view.update(cx, Sidebar::dismiss_pr_guidance));
    assert!(cx.update(|_, cx| view.read(cx).pr_guidance().is_none()));
}

/// OWE-610(4): 사이드바는 제어 연결을 재사용해 500ms마다 새 UDS 2개를 열지 않으며,
/// 왼쪽 사이드바와 오른쪽 diff 패널이 같은 저장소를 감시하다 한쪽을 접어도 다른 쪽의 `git::watch::Hub` 감시가 유지된다.
#[test]
fn sidebar_reuses_uds_connection_and_hub_refcounts_sidebar_and_diff_watches() {
    use krill_proto::screen::Msg as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::net::UnixListener;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let root = std::env::temp_dir().join(format!("mk-owe610-watch-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert!(
        Command::new("git")
            .current_dir(&repo)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    fs::write(repo.join("dirty.rs"), "pub fn x() {}\n").unwrap();

    let sock_path = root.join("krilld.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();
    let registry = Arc::new(krilld::registry::Registry::new(None, None));
    let reg = Arc::clone(&registry);
    let accepts = Arc::new(AtomicUsize::new(0));
    let accepts_bg = Arc::clone(&accepts);
    std::thread::spawn(move || {
        let hello = krill_proto::ipc::ServerHello {
            proto_min: krill_proto::ipc::PROTO_MIN,
            proto_max: krill_proto::ipc::PROTO_MAX,
            features: krill_proto::ipc::FEATURES,
            build: "test".into(),
            instance_id: 1,
        };
        while let Ok((stream, _)) = listener.accept() {
            accepts_bg.fetch_add(1, Ordering::SeqCst);
            let reg = Arc::clone(&reg);
            let hello = hello.clone();
            std::thread::spawn(move || {
                reg.serve(stream, &hello).ok();
            });
        }
    });

    let paths = Paths {
        socket: sock_path.clone(),
        holder: PathBuf::new(),
        krilld: PathBuf::new(),
    };
    let sidebar_sock = ipc::Socket::threaded(paths.clone());
    let trees = BTreeMap::from([(repo.clone(), vec![repo.clone()])]);

    // 1. 사이드바가 5회 연속 fetch() + meta()를 호출해도 UDS 연결은 딱 1개만 열려야 한다(500ms마다 2개씩 총 10개 금지).
    let mut dirty = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    for _ in 0..5 {
        let _ = sidebar_sock.fetch().unwrap();
        let meta = sidebar_sock.meta(&trees);
        if let Some(d) = meta.get(&repo).and_then(|t| t.dirty) {
            dirty = Some(d);
        }
    }
    while dirty.is_none() {
        assert!(
            Instant::now() < deadline,
            "sidebar watch should report dirty count"
        );
        std::thread::sleep(Duration::from_millis(20));
        let _ = sidebar_sock.fetch().unwrap();
        let meta = sidebar_sock.meta(&trees);
        dirty = meta.get(&repo).and_then(|t| t.dirty);
    }
    assert_eq!(dirty, Some(1));
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        1,
        "sidebar must reuse a single UDS connection across fetch() and meta()"
    );

    // 2. 오른쪽 diff 패널 연결("krill-app-diff")도 같은 저장소를 감시한다.
    let diff_hello =
        krill_proto::daemon::client_hello(krill_proto::ipc::ClientRole::App, "krill-app-diff");
    let mut diff_stream = std::os::unix::net::UnixStream::connect(&sock_path).unwrap();
    let (k, b) = krill_proto::ipc::read_frame(&mut diff_stream)
        .unwrap()
        .unwrap();
    let _ = krill_proto::ipc::ServerHello::decode(k, &b).unwrap();
    std::io::Write::write_all(&mut diff_stream, &diff_hello.encode().unwrap()).unwrap();
    let mut diff_watch = |visible: bool, since: u64| -> krill_proto::git::GitWatchReply {
        let req = krill_proto::control::Request::GitWatch(krill_proto::git::GitWatchRequest {
            repo: repo.as_os_str().as_bytes().to_vec(),
            visible,
            since,
            force: false,
        });
        std::io::Write::write_all(&mut diff_stream, &req.encode().unwrap()).unwrap();
        let (k, b) = krill_proto::ipc::read_frame(&mut diff_stream)
            .unwrap()
            .unwrap();
        let krill_proto::control::Reply::GitWatch(reply) =
            krill_proto::control::Reply::decode(k, &b).unwrap()
        else {
            panic!("expected GitWatch reply");
        };
        reply
    };

    let krill_proto::git::GitWatchReply::Changed {
        generation,
        changes,
    } = diff_watch(true, 0)
    else {
        panic!("diff should immediately get cached snapshot from active Hub watch");
    };
    assert_eq!(changes.files.len(), 1);
    let accepts_with_diff = accepts.load(Ordering::SeqCst);
    assert_eq!(accepts_with_diff, 2);

    // 3. 왼쪽 사이드바를 접어 `set_visible(false)`를 보내도 새 UDS를 열지 않고,
    // 오른쪽 diff 패널의 감시는 살아 있어 새 파일 추가가 diff에 감지되어야 한다.
    sidebar_sock.set_visible(false);
    sidebar_sock.settle();
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        2,
        "sidebar set_visible(false) must reuse existing UDS connection"
    );
    assert_eq!(
        diff_watch(true, generation),
        krill_proto::git::GitWatchReply::Unchanged { pending: false },
        "collapsing sidebar must not reset or force-recreate diff panel's active watch"
    );
    // 사이드바를 다시 열어 감시 요청으로 연결을 확실히 만든다. 빈 레지스트리의 스냅샷은 캐시에서
    // 돌아올 수 있으므로 poll_snapshot 자체가 새 연결을 열 것이라고 가정하면 실행 환경·TTL에 의존한다.
    sidebar_sock.set_visible(true);
    let reopened = sidebar_sock.meta(&trees);
    assert_eq!(reopened.get(&repo).and_then(|tree| tree.dirty), Some(1));
    assert_eq!(accepts.load(Ordering::SeqCst), 3);

    // 원격 폴링 경로(`poll_snapshot`)는 호스트 전용 스레드에서 기존 연결을 재사용하고,
    // 결과도 fetch + meta와 같다.
    let (snap, meta) = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(sidebar_sock.poll_snapshot())
        .unwrap();
    // 레지스트리는 실행 환경의 다른 저장소도 보여 주고 그 변경 수는 git 감시가 비동기로 채운다. 그래서 두 번 읽은
    // 값을 통째로 비교하지 않고 저장소 목록과 브랜치만 비교한다.
    let again = sidebar_sock.meta(&snap.trees);
    let shape = |m: &BTreeMap<PathBuf, _>| {
        m.iter()
            .map(|(k, t): (&PathBuf, &Tree)| (k.clone(), t.branch.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        snap.trees.keys().collect::<Vec<_>>(),
        sidebar_sock
            .fetch()
            .unwrap()
            .trees
            .keys()
            .collect::<Vec<_>>()
    );
    assert_eq!(shape(&meta), shape(&again));
    assert_eq!(accepts.load(Ordering::SeqCst), 3);

    fs::remove_dir_all(root).ok();
}

#[gpui::test]
fn failed_snapshot_releases_agent_sleep_assertion_and_badge(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    init_sidebar_test_settings(cx, "power", r#"keep_awake = "agent""#);
    let sidebar = cx.new(|_| Sidebar::new_loaded(None));
    sidebar.update(cx, |sidebar, cx| {
        sidebar.apply(
            Ok((
                Snapshot {
                    sessions: vec![
                        session(609, AgentState::Working, None),
                        session(610, AgentState::Waiting, None),
                    ],
                    trees: BTreeMap::new(),
                },
                Meta::new(),
            )),
            BTreeMap::new(),
            cx,
        );
        assert!(crate::alert::is_preventing_sleep(cx));
        assert_eq!(crate::alert::dock_badge(cx).as_deref(), Some("1"));
        // 감시 연결만 끊기고 폴링은 Err를 놓친 경우, 같은 스냅샷도 다시 반영한다.
        crate::alert::disconnected(cx);
        assert!(!crate::alert::is_preventing_sleep(cx));
        sidebar.apply(
            Ok((sidebar.snapshot.clone(), sidebar.meta.clone())),
            BTreeMap::new(),
            cx,
        );
        assert!(crate::alert::is_preventing_sleep(cx));
        assert_eq!(crate::alert::dock_badge(cx).as_deref(), Some("1"));
        sidebar.apply(
            Err(io::ErrorKind::ConnectionReset.into()),
            BTreeMap::new(),
            cx,
        );
        assert!(!crate::alert::is_preventing_sleep(cx));
        assert_eq!(crate::alert::dock_badge(cx), None);
    });
}

/// Orca 정합(사이드바): 헤더 높이 `h-8`(32px), 빈 상태 가운데 정렬·`py-6` 여백,
/// 미커밋 배지는 있을 때만 보이고 없어지면 사라진다.
#[gpui::test]
fn header_empty_and_dirty_badge_match_orca(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    cx.run_until_parked();

    let header = cx.debug_bounds("sidebar-header").expect("sidebar header");
    assert!(
        (f32::from(header.size.height) - 32.).abs() < 0.5,
        "header height must stay Orca h-8 (32px), got {header:?}"
    );
    let empty = cx.debug_bounds("sidebar-empty").expect("empty state");
    // py-6(24px) 상하 + 한 줄 텍스트라 48px보다 반드시 크다(수정 전 py-2면 30px 안팎).
    assert!(
        f32::from(empty.size.height) >= 48.,
        "empty state must keep Orca py-6 padding, got {empty:?}"
    );
    // 빈 블록이 헤더와 같은 전폭 행이라 `items_center`가 텍스트를 가운데로 둔다(Orca `text-center`).
    assert!(
        (f32::from(empty.center().x) - f32::from(header.center().x)).abs() < 2.,
        "empty state must span the full row like Orca, got {empty:?} vs {header:?}"
    );

    let repo = PathBuf::from("/repo");
    let tree = PathBuf::from("/repo/a");
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: Vec::new(),
                trees: BTreeMap::from([(repo.clone(), vec![tree.clone()])]),
            };
            s.meta = BTreeMap::from([(
                tree.clone(),
                Tree {
                    branch: None,
                    dirty: Some(3),
                },
            )]);
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("sidebar-empty").is_none(),
        "empty state must disappear once rows exist"
    );
    assert!(
        cx.debug_bounds("dirty-badge-1").is_some(),
        "dirty badge must show when uncommitted files exist"
    );
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.meta = Meta::new();
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("dirty-badge-1").is_none(),
        "dirty badge must disappear when the tree is clean"
    );
}

/// OWE-654: git 밖 세션은 폴더 프로젝트 > 세션으로 묶이고, 데몬이 알려 준 세션은 하나도 빠지지 않는다
/// (이전 빌드의 '탭이 있는데 세션 없음' 재현 판정 — 행이 비는 경우는 데몬 목록이 빈 때뿐이다).
#[test]
fn folder_projects_group_loose_sessions_and_every_session_has_a_row() {
    let cwd = |id: u64, state: AgentState, dir: Option<&str>| Session {
        cwd: dir.map(PathBuf::from),
        ..session(id, state, None)
    };
    let snapshot = Snapshot {
        sessions: vec![
            session(1, AgentState::Working, Some("/repo/a")),
            cwd(2, AgentState::Unknown, Some("/Users/me")),
            cwd(3, AgentState::Exited, Some("/Users/me")),
            cwd(4, AgentState::Done, Some("/tmp/x")),
            cwd(5, AgentState::Unknown, None),
        ],
        trees: BTreeMap::from([(PathBuf::from("/repo"), vec!["/repo/a".into()])]),
    };
    let all = rows(&snapshot, &Preferences::new(), &BTreeSet::new());
    for s in &snapshot.sessions {
        assert!(
            all.iter()
                .any(|r| matches!(r, Row::Session(x, _) if x.id == s.id)),
            "session {} must have a sidebar row: {all:?}",
            s.id.0
        );
    }
    let me = all
        .iter()
        .position(|r| *r == Row::Folder("/Users/me".into()))
        .expect("folder project row");
    assert!(matches!(&all[me + 1], Row::Session(s, true) if s.id == SessionId(2)));
    assert!(matches!(&all[me + 2], Row::Session(s, true) if s.id == SessionId(3)));
    assert!(all.contains(&Row::Folder("/tmp/x".into())));
    // 작업 디렉터리를 모르는 세션만 '세션' 묶음에 남는다.
    let loose = all.iter().position(|r| *r == Row::Loose).expect("loose");
    assert!(matches!(&all[loose + 1], Row::Session(s, false) if s.id == SessionId(5)));
    assert_eq!(all.len(), loose + 2);

    // 폴더 프로젝트도 워크트리처럼 접힌다.
    let folded = rows(
        &snapshot,
        &Preferences::new(),
        &BTreeSet::from([PathBuf::from("/Users/me")]),
    );
    assert!(folded.contains(&Row::Folder("/Users/me".into())));
    assert!(
        !folded
            .iter()
            .any(|r| matches!(r, Row::Session(s, _) if s.id == SessionId(2)))
    );
}

/// OWE-654: 빈 상태는 '프로젝트' 헤더 아래 안내와 '폴더 열기' 버튼을 보이고, 버튼은 셸의 폴더 열기 이벤트를 낸다.
#[gpui::test]
fn empty_state_offers_open_folder_button(cx: &mut TestAppContext) {
    use gpui::Modifiers;
    use std::cell::Cell;
    use std::rc::Rc;

    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let opened = Rc::new(Cell::new(0));
    let got = opened.clone();
    let _sub =
        cx.update(|_, cx| cx.subscribe(&view, move |_, _: &OpenFolder, _| got.set(got.get() + 1)));
    cx.run_until_parked();

    let empty = cx.debug_bounds("sidebar-empty").expect("empty state");
    let button = cx
        .debug_bounds("sidebar-open-folder")
        .expect("open folder button in empty state");
    assert!(empty.contains(&button.center()), "{button:?} in {empty:?}");
    cx.simulate_click(button.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(opened.get(), 1);

    // 세션이 생기면 버튼은 빈 상태와 함께 사라진다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: vec![Session {
                    cwd: Some("/Users/me".into()),
                    ..session(1, AgentState::Unknown, None)
                }],
                trees: BTreeMap::new(),
            };
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("sidebar-open-folder").is_none());
    assert_eq!(
        cx.update(|_, cx| view.read(cx).rows.clone()).first(),
        Some(&Row::Folder("/Users/me".into()))
    );
}

/// OWE-662: 보지 않은 세션이 완료·대기로 바뀌면 미확인, 다시 작업하거나 사라지면 해제. 보고 있는 세션은 켜지 않는다.
#[test]
fn unread_turns_on_for_unfocused_transitions() {
    use AgentState::{Blocked, Done, Waiting, Working};
    let mut unread = BTreeSet::new();
    let old = [
        session(1, Working, None),
        session(2, Working, None),
        session(3, Working, None),
    ];
    let new = [
        session(1, Done, None),
        session(2, Waiting, None),
        session(3, Blocked, None),
    ];
    track_unread(&mut unread, &old, &new, Some(SessionId(2)));
    assert_eq!(unread, BTreeSet::from([SessionId(1), SessionId(3)]));
    // 상태가 그대로면 바뀌지 않는다(처음 본 세션도 켜지 않는다).
    let more = [
        new[0].clone(),
        new[1].clone(),
        new[2].clone(),
        session(4, Done, None),
    ];
    track_unread(&mut unread, &new, &more, None);
    assert_eq!(unread, BTreeSet::from([SessionId(1), SessionId(3)]));
    // 다시 작업 → 해제, 사라진 세션 → 해제.
    track_unread(&mut unread, &more, &[session(1, Working, None)], None);
    assert!(unread.is_empty());
}

/// OWE-662: 스냅샷 반영으로 켜진 미확인은 그 세션을 활성화하면 꺼진다.
#[gpui::test]
fn unread_clears_when_session_is_focused(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let snap = |state| Snapshot {
        sessions: vec![session(1, state, Some("/repo/a"))],
        ..Snapshot::default()
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.set_focused(Some(SessionId(9)), cx);
            s.apply(
                Ok((snap(AgentState::Working), Meta::new())),
                BTreeMap::new(),
                cx,
            );
            s.apply(
                Ok((snap(AgentState::Done), Meta::new())),
                BTreeMap::new(),
                cx,
            );
            assert!(s.is_unread(SessionId(1)));
            assert!(
                s.tree_summary(Path::new("/repo/a")).1,
                "worktree shows unread"
            );
            s.set_focused(Some(SessionId(1)), cx);
            assert!(!s.is_unread(SessionId(1)));
            // 보고 있는 동안 끝나면 켜지지 않는다.
            s.apply(
                Ok((snap(AgentState::Working), Meta::new())),
                BTreeMap::new(),
                cx,
            );
            s.apply(
                Ok((snap(AgentState::Waiting), Meta::new())),
                BTreeMap::new(),
                cx,
            );
            assert!(!s.is_unread(SessionId(1)));
        });
    });
}

/// 연결 안 됐고 세션도 없는 원격 호스트는 목록에 띄우지 않고, 끊긴 이유(꺼짐·인증 거부·Krill 없음)는
/// 보이는 호스트의 상태 자리에 쓴다.
#[gpui::test]
fn idle_remote_hosts_are_hidden_with_disconnect_reasons(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    let remote = |id: &str, status, sessions: Vec<Session>, error: Option<&str>| RemoteHost {
        id: id.into(),
        label: id.into(),
        status,
        snapshot: Snapshot {
            sessions,
            trees: BTreeMap::new(),
        },
        meta: Meta::new(),
        source: None,
        error: error.map(str::to_owned),
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.remote_hosts = vec![
                remote("mbp", HostStatus::Disconnected, vec![], Some("ssh remote HOME query to mbp failed: ssh: connect to host 100.65.102.14 port 22: Operation timed out")),
                remote("oci", HostStatus::Connected, vec![], None),
                remote("seol", HostStatus::Disconnected, vec![], Some("ssh: Permission denied (publickey,password).")),
                remote("kak", HostStatus::Disconnected, vec![session(3, AgentState::Done, Some("/r/wt"))], Some("unexpected EOF")),
            ];
            s.rebuild(cx);
        });
    });
    let headers = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_, cx| {
            view.read(cx)
                .rows
                .iter()
                .filter_map(|r| match r {
                    Row::HostHeader { host, .. } => Some(host.as_key().to_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
    };
    // 연결 안 된 호스트와 프로젝트 없이 연결만 된 호스트(oci, 관리는 설정)는 프로젝트 목록에 띄우지 않는다.
    assert!(headers(cx).is_empty());
    assert!(cx.update(|_, cx| view.read(cx).rows.is_empty()));

    assert_eq!(
        super::view::disconnect_reason("ssh: connect to host x port 22: Operation timed out"),
        text::HOST_REASON_OFFLINE.get()
    );
    assert_eq!(
        super::view::disconnect_reason("Permission denied (publickey)"),
        text::HOST_REASON_AUTH.get()
    );
    assert_eq!(
        super::view::disconnect_reason("krill is not installed on the remote host"),
        text::HOST_REASON_NO_KRILL.get()
    );
    assert_eq!(
        super::view::disconnect_reason("weird"),
        text::HOST_DISCONNECTED.get()
    );
}

/// 프로젝트 기준 보기에서는 Orca처럼 프로젝트가 맨 위 단계다. 다른 PC의 같은 프로젝트는 한 머리줄로
/// 합쳐지고, 연결된 PC의 상태 줄은 사라지며, 다시 연결이 필요한 프로젝트 없는 PC와
/// 작업 폴더 모르는 세션(모든 PC 것을 한 묶음)만 프로젝트 뒤에 온다.
#[gpui::test]
fn project_rows_merge_hosts_and_follow_with_idle_hosts(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let remote = |id: &str, status, snapshot: Snapshot| RemoteHost {
        id: id.into(),
        label: id.into(),
        status,
        snapshot,
        meta: Meta::new(),
        source: None,
        error: None,
    };
    view.update(cx, |s, cx| {
        s.snapshot = Snapshot {
            sessions: vec![session(1, AgentState::Working, Some("/local/a"))],
            trees: BTreeMap::from([("/local".into(), vec!["/local/a".into()])]),
        };
        s.remote_hosts = vec![
            remote("oci", HostStatus::Connected, Snapshot::default()),
            remote(
                "win",
                HostStatus::Connected,
                Snapshot {
                    sessions: vec![session(2, AgentState::Done, None)],
                    trees: BTreeMap::from([("/tax".into(), vec!["/tax".into()])]),
                },
            ),
            remote(
                "mbp",
                HostStatus::Connected,
                Snapshot {
                    sessions: Vec::new(),
                    trees: BTreeMap::from([(
                        "/Users/x/LOCAL".into(),
                        vec!["/Users/x/LOCAL".into(), "/Users/x/LOCAL-wt".into()],
                    )]),
                },
            ),
            remote(
                "seol",
                HostStatus::Connected,
                Snapshot {
                    sessions: Vec::new(),
                    trees: BTreeMap::from([("/seol".into(), vec!["/seol".into()])]),
                },
            ),
            remote(
                "off",
                HostStatus::Disconnected,
                Snapshot {
                    sessions: vec![session(3, AgentState::Done, None)],
                    trees: BTreeMap::new(),
                },
            ),
        ];
        s.rebuild(cx);
    });
    let mbp = HostId::Remote("mbp".into());
    cx.update(|_, cx| {
        let s = view.read(cx);
        let rows = s
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Repository(p) | Row::Folder(p) => Some(p.display().to_string()),
                Row::HostHeader { host, .. } => Some(format!("host:{}", host.as_key())),
                Row::Loose => Some("loose".into()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(rows, ["/local", "/tax", "/seol", "loose"]);
        // The other PC's worktree sits inside the local project, before the next project.
        let merged = s
            .rows
            .iter()
            .position(|r| matches!(r, Row::Worktree(p) if p == Path::new("/Users/x/LOCAL-wt")))
            .unwrap();
        let tax = s
            .rows
            .iter()
            .position(|r| matches!(r, Row::Repository(p) if p == Path::new("/tax")))
            .unwrap();
        assert!(merged < tax);
        assert_eq!(s.row_host(merged), &mbp);
        assert_eq!(
            s.merged_projects.get(&0).map(Vec::as_slice),
            Some(&[(mbp.clone(), PathBuf::from("/Users/x/LOCAL"))][..])
        );
        assert_eq!(
            s.host_badge_for(&mbp, Path::new("/Users/x/LOCAL-wt")),
            Some("mbp".into())
        );
        let loose = s.rows.iter().position(|r| matches!(r, Row::Loose)).unwrap();
        let loose_hosts = (loose + 1..s.rows.len())
            .map(|i| s.row_host(i).as_key().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(loose_hosts, ["win"]);
    });
    // Folding the merged header folds the other PC's part too.
    view.update(cx, |s, cx| s.fold(Path::new("/local"), Some(false), cx));
    cx.run_until_parked();
    view.read_with(cx, |s, _| {
        assert!(
            !s.rows
                .iter()
                .any(|r| matches!(r, Row::Worktree(p) if p == Path::new("/Users/x/LOCAL-wt")))
        );
    });
}

/// Projects remain the root even with remote hosts; session IDs and selection stay host-scoped.
#[gpui::test]
fn project_first_sidebar_keeps_remote_selection_and_reconnect_scoped(cx: &mut TestAppContext) {
    use gpui::Modifiers;
    use std::cell::RefCell;
    use std::rc::Rc;
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    cx.simulate_resize(gpui::size(gpui::px(360.), gpui::px(1000.)));
    let remote = HostId::Remote("studio".into());
    let selected = Rc::new(RefCell::new(Vec::new()));
    let out = selected.clone();
    let _sub = cx.update(|_, cx| {
        cx.subscribe(&view, move |sidebar, event: &Selected, cx| {
            let host = sidebar.update(cx, |s, _| s.take_action_host()).unwrap();
            out.borrow_mut().push((host, event.0));
        })
    });
    view.update(cx, |s, cx| {
        s.snapshot = Snapshot {
            sessions: vec![session(1, AgentState::Working, Some("/repo/mobile-ui"))],
            trees: BTreeMap::from([("/repo".into(), vec!["/repo/mobile-ui".into()])]),
        };
        s.focused = Some(SessionId(1));
        s.custom_titles.insert(SessionId(1), "Local title".into());
        let mut remote_snap = s.snapshot.clone();
        remote_snap
            .trees
            .insert("/remote-only".into(), vec!["/remote-only".into()]);
        s.remote_hosts = vec![RemoteHost {
            id: "studio".into(),
            label: "Studio".into(),
            status: HostStatus::Connected,
            snapshot: remote_snap,
            meta: Meta::new(),
            source: None,
            error: None,
        }];
        // Stale host folds must not hide projects after switching to project navigation.
        s.collapsed_hosts.extend([HostId::Local, remote.clone()]);
        s.rebuild(cx);
    });
    cx.run_until_parked();
    let (local_tree, remote_repo, remote_only_repo, remote_tree) = cx.update(|_, cx| {
        let s = view.read(cx);
        assert!(!s.rows.iter().any(|r| matches!(r, Row::HostHeader { .. })));
        assert!(matches!(&s.rows[0], Row::Repository(p) if p == Path::new("/repo")));
        assert_eq!(s.host_indent().to_bits(), 0_f32.to_bits());
        assert_eq!(
            s.host_badge_for(&HostId::Local, Path::new("/repo/mobile-ui")),
            None
        );
        assert_eq!(
            s.host_badge_for(&remote, Path::new("/repo/mobile-ui")),
            Some("Studio".into())
        );
        assert_eq!(
            s.host_badge_for(&remote, Path::new("/remote-only")),
            Some("Studio".into())
        );
        // The same project on another PC merges into the one project header; remote-only stays its own project.
        assert_eq!(
            s.rows
                .iter()
                .filter(|r| matches!(r, Row::Repository(_)))
                .count(),
            2
        );
        assert_eq!(
            s.merged_projects.get(&0).map(Vec::as_slice),
            Some(&[(remote.clone(), PathBuf::from("/repo"))][..])
        );
        let find = |host: &HostId, repo| {
            s.rows
                .iter()
                .enumerate()
                .position(|(i, r)| {
                    s.row_host(i) == host
                        && if repo {
                            matches!(r, Row::Repository(_))
                        } else {
                            matches!(r, Row::Worktree(_))
                        }
                })
                .unwrap()
        };
        (
            find(&HostId::Local, false),
            find(&HostId::Local, true),
            find(&remote, true),
            find(&remote, false),
        )
    });
    let sel = |prefix: &str, index| -> &'static str { format!("{prefix}-{index}").leak() };
    // A connected PC shows on its cards, not as another badge on either merged or remote-only headers.
    assert!(cx.debug_bounds(sel("project-host", remote_repo)).is_none());
    assert!(
        cx.debug_bounds(sel("project-host", remote_only_repo))
            .is_none()
    );
    let bounds = cx.debug_bounds(sel("card-session", remote_tree)).unwrap();
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        selected.borrow().as_slice(),
        &[(remote.clone(), SessionId(1))]
    );
    view.read_with(cx, |s, _| {
        assert!(!s.worktree_card_active(local_tree, Path::new("/repo/mobile-ui")));
        assert!(s.worktree_card_active(remote_tree, Path::new("/repo/mobile-ui")));
        assert!(!s.is_card_expanded(Path::new("/repo/mobile-ui")));
        let remote_session = &s.remote_hosts[0].snapshot.sessions[0];
        assert_ne!(s.session_label_on(&remote, remote_session).0, "Local title");
    });
    view.update(cx, |s, cx| {
        s.remote_hosts[0].status = HostStatus::Disconnected;
        s.rebuild(cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |s, _| {
        assert!(s.row_hosts.iter().all(HostId::is_local));
        assert!(s.merged_projects.is_empty());
    });
    assert!(cx.debug_bounds(sel("project-host", remote_repo)).is_none());
    assert!(
        cx.debug_bounds(sel("project-reconnect", remote_repo))
            .is_none()
    );
    assert!(
        cx.debug_bounds(sel("project-host", remote_only_repo))
            .is_none()
    );
    assert!(
        cx.debug_bounds(sel("project-reconnect", remote_only_repo))
            .is_none()
    );
    assert!(
        selected.borrow().len() == 1,
        "disconnect must not activate or fold the project"
    );
}

#[gpui::test]
fn project_toolbar_unread_reveals_folded_project_and_lineage(cx: &mut TestAppContext) {
    use std::cell::RefCell;
    use std::rc::Rc;
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let out = Rc::new(RefCell::new(Vec::new()));
    let events = out.clone();
    let _sub = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &Selected, _| {
            events.borrow_mut().push(event.0);
        })
    });
    view.update(cx, |s, cx| {
        s.snapshot = Snapshot {
            sessions: vec![session(42, AgentState::Done, Some("/repo/child"))],
            trees: BTreeMap::from([(
                "/repo".into(),
                vec!["/repo/parent".into(), "/repo/child".into()],
            )]),
        };
        s.parents
            .insert("/repo/child".into(), "/repo/parent".into());
        s.collapsed.insert("/repo".into());
        s.collapsed_lineage.insert("/repo/parent".into());
        s.unread.insert(SessionId(42));
        s.rebuild(cx);
    });
    cx.run_until_parked();
    let bell = cx.debug_bounds("sidebar-unread").unwrap();
    cx.simulate_click(bell.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(*out.borrow(), vec![SessionId(42)]);
    view.read_with(cx, |s, _| {
        assert!(!s.is_collapsed(Path::new("/repo")));
        assert!(!s.is_lineage_collapsed(Path::new("/repo/parent")));
        assert_eq!(s.selected_worktree(), Some(Path::new("/repo/child")));
    });
}

#[gpui::test]
fn expanded_sessions_stay_inside_worktree_card_in_both_layouts(cx: &mut TestAppContext) {
    use gpui::Modifiers;
    use std::cell::RefCell;
    use std::rc::Rc;

    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: vec![
                    session(1, AgentState::Working, Some("/repo/a")),
                    session(2, AgentState::Done, Some("/repo/a")),
                ],
                trees: BTreeMap::from([(PathBuf::from("/repo"), vec!["/repo/a".into()])]),
            };
            s.rebuild(cx);
        });
    });
    let selected = Rc::new(RefCell::new(Vec::new()));
    let out = selected.clone();
    let _sub = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &Selected, _| {
            out.borrow_mut().push(event.0);
        })
    });

    for layout in [CardLayout::Detailed, CardLayout::Compact] {
        cx.update(|_, cx| view.update(cx, |s, cx| s.set_card_layout(layout, cx)));
        cx.run_until_parked();
        let collapsed = cx.debug_bounds("worktree-card-1").unwrap();
        assert!(cx.debug_bounds("sidebar-session-2").is_none());
        cx.update(|_, cx| {
            view.update(cx, |s, cx| s.toggle_card_sessions(Path::new("/repo/a"), cx));
        });
        cx.run_until_parked();
        let card = cx.debug_bounds("worktree-card-1").unwrap();
        let summary = cx.debug_bounds("card-agent-summary-1");
        let panel = if layout == CardLayout::Detailed {
            summary.expect("expanded summary panel")
        } else {
            card
        };
        for selector in ["sidebar-session-2", "sidebar-session-3"] {
            let row = cx.debug_bounds(selector).expect("expanded session renders");
            assert!(
                row.origin.x == panel.origin.x && row.size.width == panel.size.width,
                "{layout:?}: {selector} must share the summary panel: {panel:?}, {row:?}"
            );
        }
        let first = cx.debug_bounds("sidebar-session-2").unwrap();
        let second = cx.debug_bounds("sidebar-session-3").unwrap();
        if let Some(summary) = summary {
            assert_eq!(summary.bottom_left().y, first.origin.y);
            assert!(first.origin.x > card.origin.x);
            let bottom = cx.debug_bounds("card-session-surface-3").unwrap();
            assert_eq!(bottom.origin.x, card.origin.x);
            assert_eq!(bottom.size.width, card.size.width);
            assert!(bottom.bottom_left().y > second.bottom_left().y);
        }
        assert_eq!(card.bottom_left().y, first.origin.y);
        assert_eq!(first.bottom_left().y, second.origin.y);
        assert!(second.bottom_left().y > collapsed.bottom_left().y);
        selected.borrow_mut().clear();
        cx.simulate_click(second.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            selected.borrow().as_slice(),
            &[SessionId(2)],
            "nested click must select exactly that session without activating the parent card"
        );
        assert_eq!(cx.update(|_, cx| view.read(cx).cursor), Some(3));
        selected.borrow_mut().clear();
        let press = gpui::point(second.origin.x + gpui::px(16.), second.center().y);
        cx.simulate_mouse_down(press, gpui::MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(press, gpui::MouseButton::Right, Modifiers::default());
        cx.run_until_parked();
        let first_item = gpui::point(press.x + gpui::px(30.), press.y + gpui::px(17.));
        cx.simulate_mouse_move(first_item, None, Modifiers::default());
        cx.simulate_click(first_item, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            selected.borrow().as_slice(),
            &[SessionId(2)],
            "right-click must open the nested session menu and its first action must open that session"
        );
        if let Some(summary) = summary {
            cx.simulate_click(summary.center(), Modifiers::default());
        } else {
            cx.update(|_, cx| {
                view.update(cx, |s, cx| s.toggle_card_sessions(Path::new("/repo/a"), cx));
            });
        }
        cx.run_until_parked();
        assert!(cx.debug_bounds("sidebar-session-2").is_none());
        assert!(cx.debug_bounds("sidebar-session-3").is_none());
        assert_eq!(
            cx.debug_bounds("worktree-card-1").unwrap().size.height,
            collapsed.size.height
        );
    }
}

#[gpui::test]
fn virtualized_worktree_sessions_share_summary_panel_edges(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let first = session(1, AgentState::Working, Some("/repo/a"));
            let second = session(2, AgentState::Done, Some("/repo/a"));
            s.snapshot = Snapshot {
                sessions: vec![first.clone(), second.clone()],
                trees: BTreeMap::from([(PathBuf::from("/repo"), vec!["/repo/a".into()])]),
            };
            s.rows = vec![
                Row::Worktree("/repo/a".into()),
                Row::Session(first, true),
                Row::Session(second, true),
            ];
            s.rows
                .extend((0..100).map(|i| Row::Repository(format!("/extra/{i}").into())));
            s.expanded_cards.insert("/repo/a".into());
            s.cursor = Some(0);
            s.sync_viewport();
            cx.notify();
        });
    });
    cx.run_until_parked();
    let header = cx.debug_bounds("worktree-card-0").unwrap();
    let summary = cx.debug_bounds("card-agent-summary-0").unwrap();
    let first = cx.debug_bounds("sidebar-session-1").unwrap();
    let second = cx.debug_bounds("sidebar-session-2").unwrap();
    for segment in [first, second] {
        assert_eq!(segment.origin.x, summary.origin.x);
        assert_eq!(segment.size.width, summary.size.width);
    }
    assert!(first.origin.x > header.origin.x);
    assert_eq!(
        summary.bottom_left().y,
        first.origin.y,
        "expanded summary and session list must stay aligned without a gap"
    );
    assert_eq!(
        first.bottom_left().y,
        second.origin.y,
        "expanded session rows must continue without a gap"
    );
}

#[gpui::test]
fn orca_card_expansion_agent_counts_and_reveal_focused(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut s1 = session(1, AgentState::Working, Some("/repo/a"));
            s1.status.agent = "claude".into();
            let mut s2 = session(2, AgentState::Done, Some("/repo/a"));
            s2.status.agent = "codex".into();
            s.snapshot = Snapshot {
                sessions: vec![s1, s2],
                trees: BTreeMap::from([(PathBuf::from("/repo"), vec!["/repo/a".into()])]),
            };
            s.rebuild(cx);
            assert_eq!(s.agent_counts(), (0, 1, 1));
            s.set_remote_snapshot_for_test(
                "iroh:remote",
                HostStatus::Connected,
                Snapshot {
                    sessions: vec![
                        session(3, AgentState::Waiting, Some("/repo/a")),
                        session(4, AgentState::Done, Some("/repo/a")),
                        session(5, AgentState::Unknown, Some("/repo/a")),
                    ],
                    trees: BTreeMap::new(),
                },
                cx,
            );
            assert_eq!(s.agent_counts(), (1, 1, 2));
            s.remote_hosts.clear();
            s.rebuild(cx);
            assert!(!s.expanded_cards.contains(Path::new("/repo/a")));
        });
    });
    cx.run_until_parked();
    // 이미지 디코드가 재렌더를 일으킬 수 있으므로 누적 렌더 호출 수 대신
    // 현재 목록의 항목 수와 실제 세션 행의 표시 여부를 검사한다.
    // 기본 상태는 저장소 + 워크트리 카드 2개다.
    assert_eq!(
        cx.update(|_, cx| view.read(cx).card_list.state.item_count()),
        2,
        "collapsed card should hide nested session rows by default"
    );
    for selector in ["sidebar-session-2", "sidebar-session-3"] {
        assert!(
            cx.debug_bounds(selector).is_none(),
            "{selector}: collapsed card should hide nested session rows by default"
        );
    }

    // 카드 요약 바 토글 시 하위 세션 행 2개가 펼쳐진다(렌더 행 4개).
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.toggle_card_sessions(Path::new("/repo/a"), cx);
            assert!(s.expanded_cards.contains(Path::new("/repo/a")));
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).card_list.state.item_count()),
        4,
        "expanded card should render nested session rows"
    );
    for selector in ["sidebar-session-2", "sidebar-session-3"] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector}: expanded card should render nested session rows"
        );
    }

    // 하위 세션에 커서가 있을 때 다시 토글하면 커서를 워크트리 행으로 되돌리고 하위 세션 행을 실제로 접는다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.cursor = Some(2);
            s.toggle_card_sessions(Path::new("/repo/a"), cx);
            assert!(!s.expanded_cards.contains(Path::new("/repo/a")));
            assert_eq!(s.cursor, Some(1));
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).card_list.state.item_count()),
        2,
        "re-collapsing card while cursor was on nested session must hide session rows"
    );
    for selector in ["sidebar-session-2", "sidebar-session-3"] {
        assert!(
            cx.debug_bounds(selector).is_none(),
            "{selector}: re-collapsing card while cursor was on nested session must hide session rows"
        );
    }

    // 숨겨진 `.claude/worktrees` 안의 세션에 포커스가 있을 때 reveal_focused가 숨김을 풀고 워크트리를 가리킨다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let claude_wt = PathBuf::from("/repo/.claude/worktrees/wt-1");
            let mut s3 = session(3, AgentState::Working, claude_wt.to_str());
            s3.status.agent = "claude".into();
            s.snapshot = Snapshot {
                sessions: vec![s3],
                trees: BTreeMap::from([(
                    PathBuf::from("/repo"),
                    vec![PathBuf::from("/repo/a"), claude_wt.clone()],
                )]),
            };
            s.meta.insert(
                claude_wt.clone(),
                Tree {
                    branch: Some("feat/claude-wt".into()),
                    dirty: Some(12),
                },
            );
            s.prs
                .insert(claude_wt.clone(), crate::pull_request::Status::Open);
            s.comments
                .insert(claude_wt.clone(), "중첩 코멘트 테스트".into());
            s.collapsed.insert(PathBuf::from("/repo"));
            s.shown_external.clear();
            s.rebuild(cx);
            s.set_focused(Some(SessionId(3)), cx);
            s.reveal_focused(cx);
            assert!(!s.collapsed.contains(Path::new("/repo")));
            assert!(
                s.shown_external.contains(Path::new("/repo")),
                "reveal_focused must unhide .claude/worktrees"
            );
            assert_eq!(s.cursor_worktree(), Some(claude_wt));
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("dirty-badge-3").is_some());
    assert!(cx.debug_bounds("pr-badge-3").is_some());
}

/// AS-09: UI 반영은 백그라운드가 완성한 스냅샷만 소비해야 한다.
#[gpui::test]
fn apply_never_discovers_worktrees_on_ui_thread(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mk-cross-{}-apply", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let repo = dir.clone();
    fs::create_dir_all(repo.join(".claude/worktrees/extra")).unwrap();
    let snapshot = Snapshot {
        trees: BTreeMap::from([(repo.clone(), vec![repo])]),
        ..Snapshot::default()
    };
    let sidebar = cx.new(|_| Sidebar::new_loaded(None));
    sidebar.update(cx, |s, cx| {
        s.apply(Ok((snapshot.clone(), Meta::new())), BTreeMap::new(), cx);
        assert_eq!(
            s.snapshot, snapshot,
            "apply must not scan or enrich the snapshot"
        );
    });
    fs::remove_dir_all(dir).ok();
}

/// AS-25: 정상 폴링과 연결 실패 모두 저장 오류를 덮어쓰면 안 된다.
#[gpui::test]
fn snapshot_poll_preserves_preference_failure(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mk-cross-{}-save", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let sidebar = cx.new(|_| Sidebar::new_loaded(None));
    sidebar.update(cx, |s, cx| {
        // 파일 자리에 디렉터리를 두면 root 권한에서도 저장이 실패한다.
        s.path = Some(dir.clone());
        s.toggle_pin("/repo/a".into(), cx);
    });
    cx.run_until_parked();
    sidebar.update(cx, |s, cx| {
        let saved_error = s.error.clone();
        assert!(saved_error.is_some());
        s.apply(Ok((Snapshot::default(), Meta::new())), BTreeMap::new(), cx);
        assert_eq!(
            s.error, saved_error,
            "success poll must retain save failure"
        );
        s.apply(
            Err(io::ErrorKind::ConnectionReset.into()),
            BTreeMap::new(),
            cx,
        );
        assert_eq!(
            s.error, saved_error,
            "connection failure must retain save failure"
        );
        s.apply(Ok((Snapshot::default(), Meta::new())), BTreeMap::new(), cx);
        assert_eq!(s.error, saved_error);
    });
    fs::remove_dir_all(dir).ok();
}

/// OWE-737: 첫 프롬프트 자동 제목(`session-titles.json`)이 사이드바 세션 행과 워크트리 카드에 표시되고,
/// 사용자 지정 이름은 덮어쓰지 않는다.
#[gpui::test]
fn owe737_auto_titles_render_on_session_and_worktree_rows(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mk-sidebar-{}-owe737", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    krill_proto::git::record_session_auto_title(
        &dir,
        SessionId(11),
        "Fix auth token refresh on mobile",
    )
    .unwrap();
    let sidebar_file = dir.join("sidebar.bin");

    init_sidebar_test_settings(cx, "auto-titles", "");
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(sidebar_file)));

    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut claude = session(11, AgentState::Done, Some("/repo/wt-1"));
            claude.status.agent = "claude".into();
            let wt = PathBuf::from("/repo/wt-1");
            s.snapshot = Snapshot {
                sessions: vec![claude.clone()],
                trees: BTreeMap::from([(PathBuf::from("/repo"), vec![wt.clone()])]),
            };
            s.meta.insert(
                wt.clone(),
                Tree {
                    branch: Some("worktree-1".into()),
                    dirty: None,
                },
            );
            s.expanded_cards.insert(wt.clone());
            s.rebuild(cx);

            // 1. 기본 브랜치(`worktree-1`)와 세션 라벨은 첫 프롬프트 기반 자동 제목을 사용한다.
            assert_eq!(s.name(&wt), "Fix auth token refresh on mobile");
            assert_eq!(
                s.session_label(&claude).0,
                "Fix auth token refresh on mobile"
            );

            // 2. 사용자가 직접 지정한 워크트리 이름은 자동 제목보다 우선한다.
            s.prefs.entry(wt.clone()).or_default().name = "직접 정한 이름".into();
            assert_eq!(s.name(&wt), "직접 정한 이름");
        });
    });
    fs::remove_dir_all(dir).ok();
}

/// 제안 B: 사이드바 세션 행 호버 시 구조화된 멀티라인 호버 카드(`Turn N`, 세션 제목,
/// 단답형 응답 병합된 마지막 프롬프트 상자, 이전 턴 요약)가 표시된다.
#[gpui::test]
fn session_row_hover_card_shows_last_prompt_turn_badge_and_previous_turn(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mk-sidebar-{}-prompt-hover", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let sid = SessionId(11);
    let t1 = 10_000_000_000_000_u64;
    krill_proto::git::record_session_prompt(&dir, sid, "로그인 세션 만료 버그 수정해줘", t1)
        .unwrap();
    krill_proto::git::record_session_prompt(
        &dir,
        sid,
        "쿠키 만료 회귀 테스트 추가해줘",
        t1 + 10_000_000_000,
    )
    .unwrap();
    krill_proto::git::record_session_prompt(&dir, sid, "ㅇㅇ 진행해", t1 + 20_000_000_000).unwrap();
    let sidebar_file = dir.join("sidebar.bin");

    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(sidebar_file)));
    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut claude = session(11, AgentState::Working, Some("/repo/wt-1"));
            claude.status.agent = "claude".into();
            claude.status.since_unix_ns = t1 + 20_000_000_000;
            let wt = PathBuf::from("/repo/wt-1");
            s.snapshot = Snapshot {
                sessions: vec![claude],
                trees: BTreeMap::from([(PathBuf::from("/repo"), vec![wt.clone()])]),
            };
            s.expanded_cards.insert(wt);
            s.set_now_unix_ns_for_test(Some(t1 + 80_000_000_000), cx);
            s.rebuild(cx);
        });
    });
    vcx.run_until_parked();

    let row_bounds = vcx
        .debug_bounds("session-title-2")
        .expect("session row title bounds");
    vcx.simulate_mouse_move(row_bounds.center(), None, gpui::Modifiers::default());
    vcx.executor().advance_clock(Duration::from_millis(600));
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("tooltip:sidebar-session-hover-11")
            .is_some(),
        "hovering sidebar session row must render structured hover card"
    );
    assert!(
        vcx.debug_bounds("tooltip-turn:sidebar-session-hover-11:3")
            .is_some(),
        "hover card must display Turn 3 badge"
    );
    assert!(
        vcx.debug_bounds("tooltip-last-prompt:sidebar-session-hover-11")
            .is_some(),
        "hover card must render last prompt surface box"
    );
    assert!(
        vcx.debug_bounds(
            "tooltip-prompt-text:sidebar-session-hover-11:쿠키 만료 회귀 테스트 추가해줘 (→ ㅇㅇ 진행해)"
        )
        .is_some(),
        "hover card must merge short ack with substantive prompt"
    );
    assert!(
        vcx.debug_bounds("tooltip-prev-prompt:sidebar-session-hover-11")
            .is_some(),
        "hover card must render previous turn summary line"
    );

    fs::remove_dir_all(dir).ok();
}

/// OWE-731: 그룹화(`Repo` / `WorkspaceStatus` / `PrStatus` / `None`)와 정렬(`Smart` / `Recent` / `Name`)이
/// 워크트리 목록과 그룹 헤더(`Row::GroupHeader`)를 결정론적으로 재구성하며, 핀 고정 워크트리가 그룹 내 최상단에 온다.
#[gpui::test]
fn workspace_grouping_and_sorting_modes_order_rows_and_fold_groups(cx: &mut TestAppContext) {
    use crate::pull_request::Status as PrStatus;
    use krill_proto::orchestration::WorkspaceStatus;

    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));

    let wt_wait = PathBuf::from("/repo/wt-wait");
    let wt_unread = PathBuf::from("/repo/wt-unread");
    let wt_work = PathBuf::from("/repo/wt-work");
    let wt_done = PathBuf::from("/repo/wt-done");
    let wt_idle = PathBuf::from("/repo/wt-idle");

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: vec![
                    session(1, AgentState::Done, wt_done.to_str()),
                    session(2, AgentState::Working, wt_work.to_str()),
                    session(3, AgentState::Done, wt_unread.to_str()),
                    session(4, AgentState::Waiting, wt_wait.to_str()),
                ],
                trees: BTreeMap::from([(
                    PathBuf::from("/repo"),
                    vec![
                        wt_idle.clone(),
                        wt_done.clone(),
                        wt_work.clone(),
                        wt_unread.clone(),
                        wt_wait.clone(),
                    ],
                )]),
            };
            s.unread.insert(SessionId(3));
            s.worktree_updated_at.insert(wt_idle.clone(), 500);
            s.worktree_updated_at.insert(wt_done.clone(), 100);
            s.worktree_updated_at.insert(wt_work.clone(), 300);
            s.worktree_updated_at.insert(wt_unread.clone(), 200);
            s.worktree_updated_at.insert(wt_wait.clone(), 400);
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();

    let worktree_order = |rows: &[Row]| -> Vec<PathBuf> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Worktree(p) => Some(p.clone()),
                _ => None,
            })
            .collect()
    };

    cx.update(|_, cx| view.update(cx, |s, cx| s.set_sort_by(SortBy::Smart, cx)));
    // 1. 기본값(GroupBy::Repo + SortBy::Smart): Waiting -> Unread Done -> Working -> Done -> Idle 순
    let smart_order = cx.update(|_, cx| worktree_order(&view.read(cx).rows));
    assert_eq!(
        smart_order,
        vec![
            wt_wait.clone(),
            wt_unread.clone(),
            wt_work.clone(),
            wt_done.clone(),
            wt_idle.clone(),
        ]
    );

    // 2. SortBy::Recent: 최근 활동 시각 내림차순 (500 -> 400 -> 300 -> 200 -> 100), 핀 고정 시 최상단
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.set_sort_by(SortBy::Recent, cx);
        });
    });
    let recent_order = cx.update(|_, cx| worktree_order(&view.read(cx).rows));
    assert_eq!(
        recent_order,
        vec![
            wt_idle.clone(),
            wt_wait.clone(),
            wt_work.clone(),
            wt_unread.clone(),
            wt_done.clone(),
        ]
    );

    // 3. SortBy::Name: 이름 오름차순 (wt-done -> wt-idle -> wt-unread -> wt-wait -> wt-work)
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.set_sort_by(SortBy::Name, cx);
        });
    });
    let name_order = cx.update(|_, cx| worktree_order(&view.read(cx).rows));
    assert_eq!(
        name_order,
        vec![
            wt_done.clone(),
            wt_idle.clone(),
            wt_unread.clone(),
            wt_wait.clone(),
            wt_work.clone(),
        ]
    );

    // 4. GroupBy::WorkspaceStatus: Todo -> InProgress -> InReview -> Completed 그룹 헤더 및 접기/펼치기
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.set_worktree_status(wt_idle.clone(), WorkspaceStatus::Todo, cx);
            s.set_worktree_status(wt_work.clone(), WorkspaceStatus::InProgress, cx);
            s.set_worktree_status(wt_wait.clone(), WorkspaceStatus::InReview, cx);
            s.set_worktree_status(wt_done.clone(), WorkspaceStatus::Completed, cx);
            s.set_worktree_status(wt_unread.clone(), WorkspaceStatus::InProgress, cx);
            s.set_group_by(GroupBy::WorkspaceStatus, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("group-header-status-todo").is_some());
    assert!(cx.debug_bounds("group-header-status-in-progress").is_some());
    assert!(cx.debug_bounds("group-header-status-in-review").is_some());
    assert!(cx.debug_bounds("group-header-status-completed").is_some());

    // InProgress 그룹을 접으면 하위 워크트리 2개가 숨겨진다.
    let before_fold = cx.update(|_, cx| worktree_order(&view.read(cx).rows).len());
    assert_eq!(before_fold, 5);
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.fold_group(
                GroupKey::WorkspaceStatus(WorkspaceStatus::InProgress),
                Some(false),
                cx,
            );
        });
    });
    let after_fold = cx.update(|_, cx| worktree_order(&view.read(cx).rows));
    assert_eq!(
        after_fold,
        vec![wt_idle.clone(), wt_wait.clone(), wt_done.clone()]
    );

    // 5. GroupBy::PrStatus: Merged -> Open -> DraftOrNoPr -> Closed
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.prs.insert(wt_done.clone(), PrStatus::Merged);
            s.prs.insert(wt_wait.clone(), PrStatus::Open);
            s.prs.insert(wt_work.clone(), PrStatus::Draft);
            s.prs.insert(wt_idle.clone(), PrStatus::Closed);
            s.set_group_by(GroupBy::PrStatus, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("group-header-pr-merged").is_some());
    assert!(cx.debug_bounds("group-header-pr-open").is_some());
    assert!(cx.debug_bounds("group-header-pr-draft").is_some());
    assert!(cx.debug_bounds("group-header-pr-closed").is_some());

    // 6. GroupBy::None: 저장소·그룹 헤더 없이 워크트리만 평면 나열
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.set_group_by(GroupBy::None, cx);
        });
    });
    let flat_rows = cx.update(|_, cx| view.read(cx).rows.clone());
    assert!(
        !flat_rows
            .iter()
            .any(|r| matches!(r, Row::Repository(_) | Row::GroupHeader { .. }))
    );
    assert_eq!(worktree_order(&flat_rows).len(), 5);
}

/// OWE-731: 가시성 필터(`hide_sleeping` + `always_show_default_branch`, `hide_automation`, `hide_cli`, `repos`, `hosts`)와
/// 카드 레이아웃(`Detailed` / `Compact`)·표시 속성(`issue`, `pr`, `comment`, `ports`, `branch`, `host`) 토글 및 `sidebar.bin` 영속화.
#[gpui::test]
fn workspace_filters_card_properties_compact_layout_and_persistence(cx: &mut TestAppContext) {
    use crate::pull_request::Status as PrStatus;

    let dir = std::env::temp_dir().join(format!("mk-owe731-persist-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let sidebar_bin = dir.join("sidebar.bin");

    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(sidebar_bin.clone())));

    let repo_a = PathBuf::from("/repo-a");
    let repo_b = PathBuf::from("/repo-b");
    let wt_main = PathBuf::from("/repo-a/main");
    let wt_sleep = PathBuf::from("/repo-a/feature-sleep");
    let wt_active = PathBuf::from("/repo-a/feature-active");
    let wt_auto = PathBuf::from("/repo-a/.orca/automations/nightly");
    let wt_cli = PathBuf::from("/repo-a/.claude/worktrees/agent-1");
    let wt_b = PathBuf::from("/repo-b/main");

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut active_sess = session(1, AgentState::Working, wt_active.to_str());
            active_sess.status.agent = "claude".into();
            s.snapshot = Snapshot {
                sessions: vec![active_sess],
                trees: BTreeMap::from([
                    (
                        repo_a.clone(),
                        vec![
                            wt_main.clone(),
                            wt_sleep.clone(),
                            wt_active.clone(),
                            wt_auto.clone(),
                            wt_cli.clone(),
                        ],
                    ),
                    (repo_b.clone(), vec![wt_b.clone()]),
                ]),
            };
            s.meta.insert(
                wt_main.clone(),
                Tree {
                    branch: Some("main".into()),
                    dirty: Some(1),
                },
            );
            s.meta.insert(
                wt_sleep.clone(),
                Tree {
                    branch: Some("feat/sleep".into()),
                    dirty: None,
                },
            );
            s.meta.insert(
                wt_active.clone(),
                Tree {
                    branch: Some("feat/active".into()),
                    dirty: Some(2),
                },
            );
            s.shown_external.insert(repo_a.clone());
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();

    let has_wt = |cx: &mut gpui::VisualTestContext, target: &Path| -> bool {
        cx.update(|_, cx| {
            view.read(cx)
                .rows
                .iter()
                .any(|r| matches!(r, Row::Worktree(p) if p == target))
        })
    };

    // 1. 헤더 옵션 버튼(`sidebar-options`)은 기본 표시되고, 필터 활성 전에는 배지(`sidebar-options-badge`)가 없다.
    assert!(cx.debug_bounds("sidebar-options").is_some());
    assert!(cx.debug_bounds("sidebar-options-badge").is_none());

    // 2. 유휴 숨기기(`hide_sleeping`) + 기본 브랜치 예외(`always_show_default_branch = true`)
    cx.update(|_, cx| view.update(cx, Sidebar::toggle_hide_sleeping));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("sidebar-options-badge").is_some(),
        "active filter badge must appear when hide_sleeping is enabled"
    );
    assert!(
        has_wt(cx, &wt_main),
        "default branch main must stay visible when always_show_default_branch is true"
    );
    assert!(
        has_wt(cx, &wt_active),
        "active worktree with Working session must stay visible"
    );
    assert!(
        !has_wt(cx, &wt_sleep),
        "sleeping non-default worktree must be hidden"
    );

    // 기본 브랜치 예외를 끄면 유휴 상태인 main도 숨겨진다.
    cx.update(|_, cx| view.update(cx, Sidebar::toggle_always_show_default_branch));
    assert!(!has_wt(cx, &wt_main));
    cx.update(|_, cx| view.update(cx, Sidebar::toggle_always_show_default_branch));
    assert!(has_wt(cx, &wt_main));
    cx.update(|_, cx| view.update(cx, Sidebar::toggle_hide_sleeping));

    // 3. 자동화 숨기기(`hide_automation`) & CLI 숨기기(`hide_cli`) & 저장소 필터(`repos`)
    assert!(has_wt(cx, &wt_auto));
    assert!(has_wt(cx, &wt_cli));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.toggle_hide_automation(cx);
            s.toggle_hide_cli(cx);
            s.toggle_repo_filter(repo_a.clone(), cx);
        });
    });
    assert!(!has_wt(cx, &wt_auto), "automation worktree must be hidden");
    assert!(!has_wt(cx, &wt_cli), "cli worktree must be hidden");
    assert!(
        !has_wt(cx, &wt_b),
        "repo_b must be filtered out when repo_a filter is active"
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).options().active_filter_count()),
        3
    );

    // 필터 초기화(`reset_filters`)
    cx.update(|_, cx| view.update(cx, Sidebar::reset_filters));
    assert_eq!(
        cx.update(|_, cx| view.read(cx).options().active_filter_count()),
        0
    );
    assert!(has_wt(cx, &wt_b));

    // 4. 카드 표시 속성(`issue`, `pr`, `comment`, `ports`, `branch`, `host`) 개별 토글과 `Compact` / `Detailed` 레이아웃
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: Vec::new(),
                trees: BTreeMap::from([(repo_a.clone(), vec![wt_active.clone()])]),
            };
            s.prs.insert(wt_active.clone(), PrStatus::Open);
            s.comments
                .insert(wt_active.clone(), "카드 속성 검증 코멘트".into());
            s.set_worktree_issue(wt_active.clone(), "OWE-731", cx);
            s.set_worktree_ports(wt_active.clone(), vec![3000, 8080], cx);
            s.set_worktree_host_label(wt_active.clone(), "Mac Studio", cx);
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();

    for selector in [
        "worktree-card-1",
        "worktree-branch-1",
        "worktree-comment-1",
        "issue-badge-1",
        "pr-badge-1",
        "ports-badge-1",
        "host-badge-1",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "card property {selector} must render when enabled"
        );
    }
    let detailed_h = f32::from(cx.debug_bounds("worktree-card-1").unwrap().size.height);
    assert!(detailed_h >= 40., "detailed card height={detailed_h}");

    // 각 속성을 끄면 해당 요소가 카드에서 사라진다.
    for (prop, selector) in [
        (CardProperty::Issue, "issue-badge-1"),
        (CardProperty::Pr, "pr-badge-1"),
        (CardProperty::Comment, "worktree-comment-1"),
        (CardProperty::Ports, "ports-badge-1"),
        (CardProperty::Branch, "worktree-branch-1"),
        (CardProperty::Host, "host-badge-1"),
    ] {
        cx.update(|_, cx| view.update(cx, |s, cx| s.set_card_property(prop, false, cx)));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds(selector).is_none(),
            "{selector} must hide when {prop:?} is disabled"
        );
        cx.update(|_, cx| view.update(cx, |s, cx| s.set_card_property(prop, true, cx)));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} must reappear when {prop:?} is re-enabled"
        );
    }

    // Compact 레이아웃으로 전환하면 1줄 높이(28px)로 줄어들고 `sidebar.bin`에 영속화된다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.set_card_layout(CardLayout::Compact, cx);
            s.set_group_by(GroupBy::PrStatus, cx);
            s.set_sort_by(SortBy::Recent, cx);
            s.set_card_property(CardProperty::Ports, false, cx);
            s.toggle_hide_automation(cx);
        });
    });
    cx.run_until_parked();
    let compact_h = f32::from(cx.debug_bounds("worktree-card-1").unwrap().size.height);
    assert!(
        compact_h <= 30. && compact_h < detailed_h,
        "compact card must be shorter than detailed card ({compact_h} vs {detailed_h})"
    );

    let restored = Sidebar::new_loaded(Some(sidebar_bin));
    assert_eq!(restored.options().group_by, GroupBy::PrStatus);
    assert_eq!(restored.options().sort_by, SortBy::Recent);
    assert_eq!(restored.options().card_layout, CardLayout::Compact);
    assert!(!restored.options().card_properties.ports);
    assert!(restored.options().card_properties.issue);
    assert!(restored.options().filters.hide_automation);

    fs::remove_dir_all(dir).ok();
}

/// 재부팅 복원(2026-10-28): 데몬이 아직 새 세션을 목록에 올리지 않아 살아 있는 세션이 없어도, 자동 재개한 칸의
/// 워크트리는 수면 숨김(`hide_sleeping`)에서 보인다. 정상 종료 세션만 있어 재개되지 않은 워크트리와 사용자가 재운
/// 워크트리는 계속 숨는다.
#[gpui::test]
fn restored_worktree_stays_visible_while_hiding_sleeping(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let repo = PathBuf::from("/reboot-repo");
    let restored = repo.join("restored");
    let closed_only = repo.join("closed-only");
    let slept = repo.join("slept");
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: Vec::new(),
                trees: BTreeMap::from([(
                    repo.clone(),
                    vec![
                        repo.clone(),
                        restored.clone(),
                        closed_only.clone(),
                        slept.clone(),
                    ],
                )]),
            };
            s.shown_external.insert(repo.clone());
            s.toggle_hide_sleeping(cx);
            s.sleep_worktree(slept.clone(), cx);
            s.reveal_restored_worktree(restored.clone(), cx);
            s.reveal_restored_worktree(slept.clone(), cx);
        });
    });
    let rows = cx.update(|_, cx| view.read(cx).rows.clone());
    let has = |target: &Path| {
        rows.iter()
            .any(|r| matches!(r, Row::Worktree(p) if p == target))
    };
    assert!(has(&restored), "restored worktree must stay visible");
    assert!(!has(&closed_only), "closed-only worktree must stay hidden");
    assert!(!has(&slept), "user-slept worktree must stay hidden");
}

#[gpui::test]
fn setup_script_prompt_card_enables_and_dismisses_in_sidebar(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mk-owe728-sidebar-prompt-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let repo = dir.join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::write(repo.join("orca.yaml"), "scripts:\n  setup: pnpm install\n").unwrap();
    let sidebar_bin = dir.join("sidebar.bin");

    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(sidebar_bin)));
    vcx.simulate_resize(gpui::size(gpui::px(360.), gpui::px(900.)));
    vcx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: Vec::new(),
                trees: BTreeMap::from([(repo.clone(), vec![repo.clone()])]),
            };
            s.rebuild(cx);
        });
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("setup-script-prompt-card").is_some());
    let enable = vcx.debug_bounds("setup-prompt-enable").unwrap();
    vcx.simulate_click(enable.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("setup-script-prompt-card").is_none());
    let summary = krill_proto::orchestration::inspect_repo_hooks(&repo, Some(&dir));
    assert_eq!(summary.store_setup.as_deref(), Some("pnpm install"));

    fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn setup_prompt_render_uses_last_completed_scan(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("krill-prompt-render-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let repo = dir.join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    let config = repo.join("orca.yaml");
    fs::write(&config, "scripts:\n  setup: pnpm install\n").unwrap();
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(dir.join("sidebar.bin"))));
    vcx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.snapshot.trees.insert(repo.clone(), vec![repo.clone()]);
            s.rebuild(cx);
        });
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("setup-script-prompt-card").is_some());

    // 디스크 내용이 바뀌어도 draw는 IO 없이 마지막 스캔 결과만 사용한다.
    fs::write(config, "scripts: {}\n").unwrap();
    vcx.update(|window, cx| {
        view.update(cx, |_, cx| cx.notify());
        window.draw(cx).clear(cx);
    });
    assert!(vcx.debug_bounds("setup-script-prompt-card").is_some());
    vcx.executor().advance_clock(Duration::from_secs(2));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("setup-script-prompt-card").is_none());
    fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn worktree_lineage_hierarchy_fold_sleep_delete_and_persistence(cx: &mut TestAppContext) {
    struct DummySource;
    impl Source for DummySource {
        fn fetch(&self) -> io::Result<Snapshot> {
            Ok(Snapshot::default())
        }
        fn remove_worktree(&self, _req: krill_proto::control::RemoveWorktree) -> io::Result<Reply> {
            Ok(Reply::WorktreeRemoved(
                krill_proto::control::RemovedWorktree {
                    branch: None,
                    branch_deleted: false,
                    warnings: Vec::new(),
                },
            ))
        }
    }

    cx.update(gpui_component::init);
    let dir = std::env::temp_dir().join(format!("krill-lineage-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let sidebar_bin = dir.join("sidebar.bin");

    let repo = PathBuf::from("/repo");
    let parent = PathBuf::from("/repo/parent");
    let child_a = PathBuf::from("/repo/child-a");
    let grandchild = PathBuf::from("/repo/grandchild");
    let child_b = PathBuf::from("/repo/child-b");
    let solo = PathBuf::from("/repo/solo");

    let snap = Snapshot {
        sessions: vec![
            session(1, AgentState::Working, Some("/repo/parent")),
            session(2, AgentState::Working, Some("/repo/child-a")),
            session(3, AgentState::Working, Some("/repo/grandchild")),
        ],
        trees: BTreeMap::from([(
            repo.clone(),
            vec![
                solo.clone(),
                grandchild.clone(),
                child_b.clone(),
                parent.clone(),
                child_a.clone(),
            ],
        )]),
    };

    let sb_path = sidebar_bin.clone();
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(sb_path)));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.set_group_by(GroupBy::None, cx);
            s.set_sort_by(SortBy::Name, cx);
            s.snapshot = snap;
            // 모든 세션을 접어서 워크트리 행 인덱스를 0..5로 고정한다.
            for wt in [&parent, &child_a, &grandchild, &child_b, &solo] {
                s.collapsed.insert(wt.clone());
            }
            s.set_worktree_parent(child_a.clone(), parent.clone(), cx);
            s.set_worktree_parent(grandchild.clone(), child_a.clone(), cx);
            s.set_worktree_parent(child_b.clone(), parent.clone(), cx);
        });
    });
    cx.run_until_parked();

    // 렌더가 쓰는 계보 캐시는 rebuild 뒤 원래 계산과 같아야 한다.
    let lineage = [
        parent.clone(),
        child_a.clone(),
        grandchild.clone(),
        child_b.clone(),
        solo.clone(),
    ];
    let cache_matches = |s: &Sidebar| {
        for wt in &lineage {
            assert_eq!(s.cached_lineage_depth(wt), s.lineage_depth(wt), "{wt:?}");
            assert_eq!(
                s.cached_lineage_child_count(wt),
                s.lineage_children(wt).len(),
                "{wt:?}"
            );
        }
    };

    // 순환 참조 방지 검증: 조상을 자손의 자식으로 지정할 수 없다.
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert!(!s.can_set_worktree_parent(&parent, &parent));
        assert!(!s.can_set_worktree_parent(&parent, &child_a));
        assert!(!s.can_set_worktree_parent(&parent, &grandchild));
        assert!(s.can_set_worktree_parent(&solo, &grandchild));

        let wt_rows: Vec<_> = s
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Worktree(p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            wt_rows,
            vec![
                parent.clone(),
                child_a.clone(),
                grandchild.clone(),
                child_b.clone(),
                solo.clone(),
            ]
        );
        assert_eq!(s.lineage_depth(&parent), 0);
        assert_eq!(s.lineage_children(&parent).len(), 2);
        assert_eq!(s.lineage_depth(&child_a), 1);
        assert_eq!(s.lineage_children(&child_a).len(), 1);
        assert_eq!(s.lineage_depth(&grandchild), 2);
        assert_eq!(s.lineage_children(&grandchild).len(), 0);
        assert_eq!(s.lineage_depth(&child_b), 1);
        assert_eq!(s.lineage_children(&child_b).len(), 0);
        assert_eq!(s.lineage_depth(&solo), 0);
        cache_matches(s);
    });

    // 자식 워크트리 카드는 부모 카드보다 오른쪽으로 들여쓰기되고 접기 토글 배지가 렌더링된다.
    let parent_bounds = cx.debug_bounds("worktree-card-0").unwrap();
    let child_bounds = cx.debug_bounds("worktree-card-1").unwrap();
    let grandchild_bounds = cx.debug_bounds("worktree-card-2").unwrap();
    assert!(
        child_bounds.origin.x > parent_bounds.origin.x,
        "child card must be indented relative to parent ({:?} vs {:?})",
        child_bounds.origin.x,
        parent_bounds.origin.x
    );
    assert!(
        grandchild_bounds.origin.x > child_bounds.origin.x,
        "grandchild card must be indented relative to child ({:?} vs {:?})",
        grandchild_bounds.origin.x,
        child_bounds.origin.x
    );
    assert!(cx.debug_bounds("worktree-lineage-toggle-0").is_some());
    assert!(cx.debug_bounds("worktree-lineage-toggle-1").is_some());

    // 부모 해제·그룹 변경 뒤에도 렌더용 캐시가 다시 계산된다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.clear_worktree_parent(&grandchild, cx);
            s.set_group_by(GroupBy::None, cx);
            cache_matches(s);
            assert_eq!(s.cached_lineage_depth(&grandchild), 0);
            assert_eq!(s.cached_lineage_child_count(&child_a), 0);
            s.set_group_by(GroupBy::Repo, cx);
            s.set_worktree_parent(grandchild.clone(), child_a.clone(), cx);
            cache_matches(s);
        });
    });
    cx.run_until_parked();

    // 부모를 접으면 자손(child-a, grandchild, child-b)이 숨겨지고 다시 펼치면 복원된다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| s.toggle_lineage(&parent, cx));
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = view.read(cx);
        let wt_rows: Vec<_> = s
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Worktree(p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(wt_rows, vec![parent.clone(), solo.clone()]);
        assert!(s.is_lineage_collapsed(&parent));
    });

    cx.update(|_, cx| {
        view.update(cx, |s, cx| s.toggle_lineage(&parent, cx));
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let wt_count = view
            .read(cx)
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Worktree(_)))
            .count();
        assert_eq!(wt_count, 5);
    });

    // 자식 생성 모달을 열면 부모 경로가 지정된 상태로 열린다.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.source = Some(Arc::new(DummySource));
            s.new_child_worktree(parent.clone(), window, cx);
        });
        let dlg_parent = view
            .read(cx)
            .creating
            .as_ref()
            .and_then(|(dlg, _)| dlg.read(cx).parent().map(Path::to_path_buf));
        assert_eq!(dlg_parent.as_deref(), Some(parent.as_path()));
        view.update(cx, |s, cx| {
            s.creating = None;
            cx.notify();
        });
    });

    // 자손 포함 수면(Sleep with Descendants) 시 부모와 모든 자손이 수면 상태가 된다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| s.sleep_with_descendants(parent.clone(), cx));
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert!(s.is_worktree_sleeping(&parent));
        assert!(s.is_worktree_sleeping(&child_a));
        assert!(s.is_worktree_sleeping(&grandchild));
        assert!(s.is_worktree_sleeping(&child_b));
        assert!(!s.is_worktree_sleeping(&solo));
    });

    // `worktree-comments.toml` 및 `sidebar.bin`에 계보·수면 상태가 영속화되는지 검증한다.
    let restored = Sidebar::new_loaded(Some(sidebar_bin));
    assert_eq!(
        restored.worktree_parent(&child_a).as_deref(),
        Some(parent.as_path())
    );
    assert_eq!(
        restored.worktree_parent(&grandchild).as_deref(),
        Some(child_a.as_path())
    );
    assert!(restored.is_worktree_sleeping(&parent));
    assert!(restored.is_worktree_sleeping(&grandchild));

    // 깨우기 후 자손 포함 삭제(Delete with Descendants) 시 데몬 확인 뒤 부모와 자손이 모두 제거된다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.wake_worktree(&parent, cx);
            assert!(!s.is_worktree_sleeping(&parent));
            s.delete_with_descendants(parent.clone(), false, cx);
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let wt_rows: Vec<_> = view
            .read(cx)
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Worktree(p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(wt_rows, vec![solo]);
    });

    fs::remove_dir_all(dir).ok();
}

/// OWE-759: 원격 호스트(`HostId::Remote`) 워크트리 카드를 Detailed 및 Compact 레이아웃에서 클릭할 때
/// 로컬 스냅샷(`s.snapshot`)이 아니라 해당 원격 호스트 스냅샷(`s.snapshot_for(&row_host)`)에서 세션을 찾고,
/// 세션이 있으면 `action_host`를 원격 호스트로 설정해 `Selected`를 내보내며,
/// 세션이 없으면(로컬에 동명 경로 세션이 있더라도) 원격 호스트에 `NewSession`을 생성한다.
#[gpui::test]
fn remote_worktree_card_click_selects_or_spawns_on_remote_host_in_both_layouts(
    cx: &mut TestAppContext,
) {
    use gpui::Modifiers;
    use std::cell::RefCell;
    use std::rc::Rc;

    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    cx.simulate_resize(gpui::size(gpui::px(360.), gpui::px(900.)));

    let selected: Rc<RefCell<Vec<(HostId, PathBuf)>>> = Rc::new(RefCell::new(Vec::new()));
    let launched: Rc<RefCell<Vec<OpenWorkspace>>> = Rc::new(RefCell::new(Vec::new()));

    let sel_out = selected.clone();
    let _sub_sel = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &WorkspaceSelected, _| {
            sel_out
                .borrow_mut()
                .push((event.host.clone(), event.path.clone()));
        })
    });
    let new_out = launched.clone();
    let _sub_new = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &OpenWorkspace, _| {
            new_out.borrow_mut().push(event.clone());
        })
    });

    let studio_id = HostId::Remote("studio".into());
    let wt_with_remote_sess = PathBuf::from("/shared/repo/wt-active");
    let wt_without_remote_sess = PathBuf::from("/shared/repo/wt-empty");

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            // 로컬에는 두 경로 모두에 로컬 세션(1, 2)이 존재함(경로 충돌 유도).
            s.snapshot = Snapshot {
                sessions: vec![
                    session(1, AgentState::Working, wt_with_remote_sess.to_str()),
                    session(2, AgentState::Working, wt_without_remote_sess.to_str()),
                ],
                trees: BTreeMap::from([(
                    PathBuf::from("/shared/repo"),
                    vec![wt_with_remote_sess.clone(), wt_without_remote_sess.clone()],
                )]),
            };
            // 원격 호스트 'studio'에는 wt_with_remote_sess에만 원격 세션(21)이 있고 wt_without_remote_sess에는 세션이 없음.
            s.remote_hosts = vec![RemoteHost {
                id: "studio".into(),
                label: "Mac Studio".into(),
                status: HostStatus::Connected,
                snapshot: Snapshot {
                    sessions: vec![session(21, AgentState::Done, wt_with_remote_sess.to_str())],
                    trees: BTreeMap::from([(
                        PathBuf::from("/shared/repo"),
                        vec![wt_with_remote_sess.clone(), wt_without_remote_sess.clone()],
                    )]),
                },
                meta: Meta::new(),
                source: None,
                error: None,
            }];
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();

    let find_remote_wt_index = |cx: &mut gpui::VisualTestContext, target: &Path| -> usize {
        cx.update(|_, cx| {
            let s = view.read(cx);
            s.rows
                .iter()
                .enumerate()
                .position(|(i, r)| {
                    s.row_host(i) == &studio_id && matches!(r, Row::Worktree(p) if p == target)
                })
                .expect("remote worktree row")
        })
    };

    let card_selector = |idx: usize| -> &'static str {
        match idx {
            0 => "worktree-card-0",
            1 => "worktree-card-1",
            2 => "worktree-card-2",
            3 => "worktree-card-3",
            4 => "worktree-card-4",
            5 => "worktree-card-5",
            6 => "worktree-card-6",
            7 => "worktree-card-7",
            8 => "worktree-card-8",
            9 => "worktree-card-9",
            10 => "worktree-card-10",
            _ => panic!("unexpected worktree card row index {idx}"),
        }
    };

    for layout in [CardLayout::Detailed, CardLayout::Compact] {
        selected.borrow_mut().clear();
        launched.borrow_mut().clear();
        cx.update(|_, cx| {
            view.update(cx, |s, cx| s.set_card_layout(layout, cx));
        });
        cx.run_until_parked();

        let idx_active = find_remote_wt_index(cx, &wt_with_remote_sess);
        let idx_empty = find_remote_wt_index(cx, &wt_without_remote_sess);

        let bounds_active = cx
            .debug_bounds(card_selector(idx_active))
            .expect("remote active worktree card bounds");
        cx.simulate_click(
            gpui::point(
                bounds_active.origin.x + gpui::px(8.),
                bounds_active.center().y,
            ),
            Modifiers::default(),
        );
        cx.run_until_parked();

        assert_eq!(
            selected.borrow().as_slice(),
            &[(studio_id.clone(), wt_with_remote_sess.clone())],
            "clicking remote worktree card with session in {layout:?} must select remote session on studio host"
        );
        assert!(
            launched.borrow().is_empty(),
            "clicking remote worktree card with session in {layout:?} must not spawn new session"
        );

        let bounds_empty = cx
            .debug_bounds(card_selector(idx_empty))
            .expect("remote empty worktree card bounds");
        cx.simulate_click(
            gpui::point(
                bounds_empty.origin.x + gpui::px(8.),
                bounds_empty.center().y,
            ),
            Modifiers::default(),
        );
        cx.run_until_parked();

        assert!(launched.borrow().is_empty());
        assert_eq!(
            selected.borrow().last(),
            Some(&(studio_id.clone(), wt_without_remote_sess.clone())),
            "clicking remote worktree card without remote session in {layout:?} must select studio host even if local has same path"
        );
    }
}

#[gpui::test]
fn delete_worktree_failure_preserves_lineage_and_ssot_respects_cli_changes(
    cx: &mut TestAppContext,
) {
    struct SelectiveRemoveSource {
        fail_paths: std::sync::Mutex<BTreeSet<PathBuf>>,
        attempted: std::sync::Mutex<Vec<PathBuf>>,
    }
    impl Source for SelectiveRemoveSource {
        fn fetch(&self) -> io::Result<Snapshot> {
            Ok(Snapshot::default())
        }
        fn remove_worktree(&self, req: krill_proto::control::RemoveWorktree) -> io::Result<Reply> {
            use std::os::unix::ffi::OsStrExt as _;
            let target = PathBuf::from(std::ffi::OsStr::from_bytes(&req.path));
            self.attempted.lock().unwrap().push(target.clone());
            if self.fail_paths.lock().unwrap().contains(&target) {
                Ok(Reply::WorktreeFailed(format!(
                    "worktree is dirty: {}",
                    target.display()
                )))
            } else {
                Ok(Reply::WorktreeRemoved(
                    krill_proto::control::RemovedWorktree {
                        branch: None,
                        branch_deleted: false,
                        warnings: Vec::new(),
                    },
                ))
            }
        }
    }

    cx.update(gpui_component::init);
    let dir = std::env::temp_dir().join(format!("krill-owe-756-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let sidebar_bin = dir.join("sidebar.bin");

    let repo = PathBuf::from("/repo");
    let parent = PathBuf::from("/repo/parent");
    let child_a = PathBuf::from("/repo/child-a");
    let grandchild = PathBuf::from("/repo/grandchild");
    let child_b = PathBuf::from("/repo/child-b");
    let solo = PathBuf::from("/repo/solo");

    let source = Arc::new(SelectiveRemoveSource {
        fail_paths: std::sync::Mutex::new(BTreeSet::from([child_a.clone(), grandchild.clone()])),
        attempted: std::sync::Mutex::new(Vec::new()),
    });
    let dyn_source: Arc<dyn Source> = source.clone();

    let snap = Snapshot {
        sessions: Vec::new(),
        trees: BTreeMap::from([(
            repo.clone(),
            vec![
                parent.clone(),
                child_a.clone(),
                grandchild.clone(),
                child_b.clone(),
                solo.clone(),
            ],
        )]),
    };

    let sb_path = sidebar_bin.clone();
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(sb_path)));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.set_group_by(GroupBy::None, cx);
            s.set_sort_by(SortBy::Name, cx);
            s.snapshot = snap;
            s.source = Some(dyn_source);
            s.set_worktree_parent(child_a.clone(), parent.clone(), cx);
            s.set_worktree_parent(grandchild.clone(), child_a.clone(), cx);
            s.set_worktree_parent(child_b.clone(), parent.clone(), cx);
            s.sleep_worktree(child_a.clone(), cx);
            s.toggle_lineage(&child_a, cx);
        });
    });
    cx.run_until_parked();

    // 1. 단일 워크트리 삭제가 거부(Reply::WorktreeFailed)되면 스냅샷·계보·접기·수면 상태가 모두 보존된다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.delete_worktree(child_a.clone(), false, cx);
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert!(s.contains_worktree(&child_a));
        assert_eq!(
            s.worktree_parent(&child_a).as_deref(),
            Some(parent.as_path())
        );
        assert_eq!(
            s.worktree_parent(&grandchild).as_deref(),
            Some(child_a.as_path())
        );
        assert!(s.is_worktree_sleeping(&child_a));
        assert!(s.is_lineage_collapsed(&child_a));
        assert!(
            s.notice
                .as_deref()
                .is_some_and(|msg| msg.contains("worktree is dirty"))
        );
        let store = krill_proto::orchestration::WorktreeStore::load(&dir).unwrap();
        assert_eq!(
            store.checkpoint("/repo/child-a").parent.as_deref(),
            Some("/repo/parent")
        );
        assert_eq!(
            store.checkpoint("/repo/grandchild").parent.as_deref(),
            Some("/repo/child-a")
        );
    });

    // 2. delete_with_descendants에서 자손(grandchild) 삭제가 실패하면 조상(child_a, parent)은 건너뛰고
    //    독립 자손(child_b)만 삭제된다.
    source.attempted.lock().unwrap().clear();
    source.fail_paths.lock().unwrap().remove(&child_a);
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.toggle_lineage(&child_a, cx);
            s.delete_with_descendants(parent.clone(), false, cx);
        });
    });
    cx.run_until_parked();
    {
        let attempted = source.attempted.lock().unwrap().clone();
        assert!(attempted.contains(&grandchild));
        assert!(attempted.contains(&child_b));
        assert!(
            !attempted.contains(&child_a),
            "ancestor child_a must be skipped when grandchild fails: {attempted:?}"
        );
        assert!(
            !attempted.contains(&parent),
            "ancestor parent must be skipped when grandchild fails: {attempted:?}"
        );
    }
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert!(!s.contains_worktree(&child_b));
        assert!(s.contains_worktree(&grandchild));
        assert!(s.contains_worktree(&child_a));
        assert!(s.contains_worktree(&parent));
        assert_eq!(
            s.worktree_parent(&child_a).as_deref(),
            Some(parent.as_path())
        );
        assert_eq!(
            s.worktree_parent(&grandchild).as_deref(),
            Some(child_a.as_path())
        );
        assert_eq!(s.worktree_parent(&child_b), None);
    });

    // 3. 중간 노드(child_a) 단독 삭제 성공 시 자손(grandchild)이 조부모(parent)로 승계(splice)된다.
    source.fail_paths.lock().unwrap().clear();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.delete_worktree(child_a.clone(), false, cx);
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert!(!s.contains_worktree(&child_a));
        assert!(s.contains_worktree(&grandchild));
        assert_eq!(
            s.worktree_parent(&grandchild).as_deref(),
            Some(parent.as_path())
        );
        let store = krill_proto::orchestration::WorktreeStore::load(&dir).unwrap();
        assert_eq!(
            store.checkpoint("/repo/grandchild").parent.as_deref(),
            Some("/repo/parent")
        );
    });

    // 4. CLI(WorktreeStore)에서 부모를 변경하거나 해제하면 sidebar.bin의 과거 상태가 덮어쓰지 않는다.
    krill_proto::orchestration::WorktreeStore::mutate(&dir, |store| {
        store.set_parent("/repo/grandchild", Some("/repo/solo"))
    })
    .unwrap();
    cx.update(|_, cx| {
        view.update(cx, Sidebar::reload_external);
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(
            s.worktree_parent(&grandchild).as_deref(),
            Some(solo.as_path())
        );
    });

    krill_proto::orchestration::WorktreeStore::mutate(&dir, |store| {
        store.set_parent("/repo/grandchild", None)
    })
    .unwrap();
    cx.update(|_, cx| {
        view.update(cx, Sidebar::reload_external);
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(s.worktree_parent(&grandchild), None);
    });

    let reloaded = Sidebar::new_loaded(Some(sidebar_bin));
    assert_eq!(reloaded.worktree_parent(&grandchild), None);

    fs::remove_dir_all(dir).ok();
}

/// OWE-791: 사이드바 '프로젝트' 헤더의 워크스페이스 옵션·필터 메뉴가 Orca 구성(그룹화 4세그먼트 토글 바,
/// 호스트/프로젝트/정렬/카드 레이아웃/속성 표시 접힌 서브메뉴, 스위치 토글 필터 행, 활성 필터 배지·초기화 버튼)으로 동작하고 영속화된다.
#[gpui::test]
fn orca_workspace_options_menu_segments_submenus_switches_badge_and_persistence(
    cx: &mut TestAppContext,
) {
    use super::options::{
        format_filter_badge_count, host_visibility_summary, project_visibility_summary,
        properties_summary,
    };

    use gpui::{ParentElement as _, Styled as _};
    struct MenuHost(gpui::Entity<Sidebar>);
    impl gpui::Render for MenuHost {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
                .size_full()
                .child(gpui::div().w(gpui::px(420.)).h_full().child(self.0.clone()))
        }
    }

    assert_eq!(format_filter_badge_count(1), "1");
    assert_eq!(format_filter_badge_count(9), "9");
    assert_eq!(format_filter_badge_count(10), "9+");

    let dir = std::env::temp_dir().join(format!("mk-owe791-menu-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let sidebar_bin = dir.join("sidebar.bin");

    cx.update(gpui_component::init);
    let (host, vcx) = cx.add_window_view(|_, cx| {
        MenuHost(cx.new(|_| Sidebar::new_loaded(Some(sidebar_bin.clone()))))
    });
    let view = vcx.update(|_, cx| host.read(cx).0.clone());
    vcx.simulate_resize(gpui::size(gpui::px(1100.), gpui::px(900.)));

    let repo_a = PathBuf::from("/repo-alpha");
    let repo_b = PathBuf::from("/repo-beta");
    let wt_a = PathBuf::from("/repo-alpha/main");
    let wt_b = PathBuf::from("/repo-beta/main");

    vcx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: Vec::new(),
                trees: BTreeMap::from([
                    (repo_a.clone(), vec![wt_a.clone()]),
                    (repo_b.clone(), vec![wt_b.clone()]),
                ]),
            };
            s.remote_hosts = vec![RemoteHost {
                id: "studio".into(),
                label: "Mac Studio".into(),
                status: crate::hosts::HostStatus::Connected,
                snapshot: Snapshot::default(),
                meta: Meta::new(),
                source: None,
                error: None,
            }];
            s.rebuild(cx);
        });
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();

    // 1. 옵션 버튼 클릭 전에는 메뉴와 배지가 닫혀/숨겨져 있다.
    assert!(vcx.debug_bounds("sidebar-options").is_some());
    assert!(vcx.debug_bounds("sidebar-options-badge").is_none());
    assert!(vcx.debug_bounds("sidebar-options-menu").is_none());

    // 2. 슬라이더 아이콘 클릭 시 Orca 구조의 컴팩트 메뉴가 열리고, 하위 섹션 패널들은 기본 접혀 있다.
    let btn_bounds = vcx.debug_bounds("sidebar-options").unwrap();
    vcx.simulate_click(btn_bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("sidebar-options-menu").is_some());
    assert!(vcx.debug_bounds("sidebar-options-header").is_some());
    assert!(vcx.debug_bounds("sidebar-submenu-hosts").is_some());
    assert!(vcx.debug_bounds("sidebar-submenu-projects").is_some());
    assert!(vcx.debug_bounds("sidebar-hosts-panel").is_none());
    assert!(vcx.debug_bounds("sidebar-projects-panel").is_none());
    assert!(vcx.debug_bounds("sidebar-sort-panel").is_none());
    assert!(vcx.debug_bounds("sidebar-card-layout-panel").is_none());
    assert!(vcx.debug_bounds("sidebar-card-properties-panel").is_none());

    // 2b. 표시(호스트·프로젝트) 서브메뉴 열기/선택/초기화 검증
    let hosts_trigger = vcx.debug_bounds("sidebar-submenu-hosts").unwrap();
    vcx.simulate_click(hosts_trigger.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let hosts_panel = vcx.debug_bounds("sidebar-hosts-panel").unwrap();
    let hosts_row = vcx.debug_bounds("sidebar-submenu-hosts").unwrap();
    assert!(
        hosts_panel.origin.x >= hosts_row.right(),
        "host choices must open to the right"
    );
    assert!((hosts_panel.origin.y - hosts_row.origin.y).abs() < gpui::px(12.));
    let host_studio = vcx.debug_bounds("sidebar-host-item-studio").unwrap();
    vcx.simulate_click(host_studio.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.update(|_, cx| { view.read(cx).options().filters.hosts.contains("studio") }));
    let host_all = vcx.debug_bounds("sidebar-host-all").unwrap();
    vcx.simulate_click(host_all.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.update(|_, cx| view.read(cx).options().filters.hosts.is_empty()));

    let projects_trigger = vcx.debug_bounds("sidebar-submenu-projects").unwrap();
    vcx.simulate_click(projects_trigger.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("sidebar-hosts-panel").is_none());
    let projects_panel = vcx.debug_bounds("sidebar-projects-panel").unwrap();
    let projects_row = vcx.debug_bounds("sidebar-submenu-projects").unwrap();
    assert!(
        projects_panel.origin.x >= projects_row.right(),
        "project choices must open to the right"
    );
    assert_eq!(
        projects_trigger, projects_row,
        "opening a panel must not move its parent row"
    );
    let proj_alpha = vcx.debug_bounds("sidebar-project-item-repo-alpha").unwrap();
    vcx.simulate_click(proj_alpha.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.update(|_, cx| { view.read(cx).options().filters.repos.contains(&repo_a) }));
    let proj_clear = vcx.debug_bounds("sidebar-projects-clear").unwrap();
    vcx.simulate_click(proj_clear.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.update(|_, cx| view.read(cx).options().filters.repos.is_empty()));
    let projects_trigger = vcx.debug_bounds("sidebar-submenu-projects").unwrap();
    vcx.simulate_click(projects_trigger.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    // 3. 그룹화 4세그먼트(`없음`·`상태`·`PR`·`프로젝트`)가 한 줄에 나란히 배치되고 클릭 즉시 반영된다.
    let seg_none = vcx.debug_bounds("sidebar-group-by-none").unwrap();
    let seg_status = vcx.debug_bounds("sidebar-group-by-status").unwrap();
    let seg_pr = vcx.debug_bounds("sidebar-group-by-pr").unwrap();
    let seg_repo = vcx.debug_bounds("sidebar-group-by-repo").unwrap();
    assert!(
        (f32::from(seg_none.origin.y) - f32::from(seg_status.origin.y)).abs() < 1.0
            && (f32::from(seg_status.origin.y) - f32::from(seg_pr.origin.y)).abs() < 1.0
            && (f32::from(seg_pr.origin.y) - f32::from(seg_repo.origin.y)).abs() < 1.0,
        "all 4 GroupBy segments must render on a single horizontal row"
    );

    vcx.simulate_click(seg_status.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, cx| view.read(cx).options().group_by),
        GroupBy::WorkspaceStatus
    );
    assert!(
        vcx.debug_bounds("sidebar-options-menu").is_some(),
        "options menu must remain open after segment click"
    );

    // 4. 정렬·카드 레이아웃·속성 표시 서브메뉴 토글 및 선택 반영
    let sort_trigger = vcx.debug_bounds("sidebar-submenu-sort").unwrap();
    vcx.simulate_click(sort_trigger.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("sidebar-sort-panel").is_some());

    let sort_recent = vcx.debug_bounds("sidebar-sort-recent").unwrap();
    vcx.simulate_click(sort_recent.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, cx| view.read(cx).options().sort_by),
        SortBy::Recent
    );

    let layout_trigger = vcx.debug_bounds("sidebar-submenu-card-layout").unwrap();
    vcx.simulate_click(layout_trigger.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("sidebar-sort-panel").is_none(),
        "opening card layout submenu must collapse sort submenu"
    );
    assert!(vcx.debug_bounds("sidebar-card-layout-panel").is_some());

    let layout_compact = vcx.debug_bounds("sidebar-card-layout-compact").unwrap();
    vcx.simulate_click(layout_compact.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, cx| view.read(cx).options().card_layout),
        CardLayout::Compact
    );

    let props_trigger = vcx.debug_bounds("sidebar-submenu-card-properties").unwrap();
    vcx.simulate_click(props_trigger.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("sidebar-card-properties-panel").is_some());
    assert!(
        vcx.debug_bounds("sidebar-card-properties-hover-hint")
            .is_some(),
        "compact mode must display hover hint in card properties panel"
    );

    let prop_ports = vcx.debug_bounds("sidebar-prop-ports").unwrap();
    vcx.simulate_click(prop_ports.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(!vcx.update(|_, cx| view.read(cx).options().card_properties.ports));

    // 부모 메뉴의 그룹화로 돌아오면 서브패널만 닫힌다.
    let group = vcx.debug_bounds("sidebar-group-by-status").unwrap();
    vcx.simulate_click(group.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    // 5. 가시성 필터 스위치 토글 행(`FilterToggleRow`)과 조건부 들여쓰기(`always_show_default_branch`), 배지·초기화 버튼 검증
    assert!(
        vcx.debug_bounds("sidebar-filter-keep-default-branch")
            .is_none()
    );
    assert!(vcx.debug_bounds("sidebar-filter-reset").is_none());

    let hide_sleeping = vcx.debug_bounds("sidebar-filter-hide-sleeping").unwrap();
    vcx.simulate_click(hide_sleeping.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.update(|_, cx| view.read(cx).options().filters.hide_sleeping));
    assert!(
        vcx.debug_bounds("sidebar-filter-keep-default-branch")
            .is_some(),
        "enabling hide_sleeping must immediately reveal indented default-branch row"
    );
    assert!(vcx.debug_bounds("sidebar-filter-reset").is_some());
    assert!(vcx.debug_bounds("sidebar-options-badge").is_some());

    let hide_auto = vcx.debug_bounds("sidebar-filter-hide-automation").unwrap();
    vcx.simulate_click(hide_auto.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, cx| view.read(cx).options().active_filter_count()),
        2
    );

    // 요약 헬퍼 검증
    vcx.update(|_, cx| {
        let opts = view.read(cx).options().clone();
        assert_eq!(
            properties_summary(&opts),
            super::text::SHOW_PROPERTIES_HOVER.get()
        );
        let hosts = vec![
            (HostId::Local, "내 Mac".to_owned()),
            (HostId::Remote("studio".into()), "Mac Studio".to_owned()),
        ];
        assert_eq!(
            host_visibility_summary(&opts.filters, &hosts),
            super::text::ALL_HOSTS.get()
        );
        assert_eq!(
            project_visibility_summary(&opts.filters, &[repo_a.clone(), repo_b.clone()]),
            super::text::ALL_PROJECTS.get()
        );
    });

    // `sidebar.bin` 영속화 확인 후 필터 초기화 클릭
    let persisted = Sidebar::new_loaded(Some(sidebar_bin.clone()));
    assert_eq!(persisted.options().group_by, GroupBy::WorkspaceStatus);
    assert_eq!(persisted.options().sort_by, SortBy::Recent);
    assert_eq!(persisted.options().card_layout, CardLayout::Compact);
    assert!(!persisted.options().card_properties.ports);
    assert!(persisted.options().filters.hide_sleeping);
    assert!(persisted.options().filters.hide_automation);

    let reset_btn = vcx.debug_bounds("sidebar-filter-reset").unwrap();
    vcx.simulate_click(reset_btn.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, cx| view.read(cx).options().active_filter_count()),
        0
    );
    assert!(vcx.debug_bounds("sidebar-options-badge").is_none());
    assert!(vcx.debug_bounds("sidebar-filter-reset").is_none());

    // 오른쪽 패널을 연 채 Esc로 전체 메뉴를 닫고, 재진입 후 바깥 클릭으로도 닫는다.
    let projects = vcx.debug_bounds("sidebar-submenu-projects").unwrap();
    vcx.simulate_click(projects.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("sidebar-projects-panel").is_some());
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("sidebar-options-menu").is_none());
    assert!(vcx.debug_bounds("sidebar-projects-panel").is_none());
    let button = vcx.debug_bounds("sidebar-options").unwrap();
    vcx.simulate_click(button.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let projects = vcx.debug_bounds("sidebar-submenu-projects").unwrap();
    vcx.simulate_click(projects.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.simulate_click(
        gpui::point(gpui::px(1050.), gpui::px(850.)),
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("sidebar-options-menu").is_none());
    assert!(vcx.debug_bounds("sidebar-projects-panel").is_none());
    // 좁은 창에서 부모와 겹쳐 보이더라도 서브패널의 선택을 받아야 한다.
    vcx.simulate_resize(gpui::size(gpui::px(420.), gpui::px(900.)));
    let button = vcx.debug_bounds("sidebar-options").unwrap();
    vcx.simulate_click(button.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let hosts = vcx.debug_bounds("sidebar-submenu-hosts").unwrap();
    vcx.simulate_click(hosts.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let studio = vcx.debug_bounds("sidebar-host-item-studio").unwrap();
    assert!(studio.origin.x >= gpui::px(0.) && studio.right() <= gpui::px(420.));
    vcx.simulate_click(studio.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.update(|_, cx| view.read(cx).options().filters.hosts.contains("studio")));
    fs::remove_dir_all(dir).ok();
}

/// OWE-900: Orca 이관 직후(16개 프로젝트·122개 이상 워크트리·세션 없음)에도 워크트리 행이
/// 평문 1줄이 아니라 카드(상태·브랜치·들여쓰기)로 렌더되고, 16개 프로젝트가 모두 보인다.
///
/// 이관한 대량 워크트리도 상세 카드를 가상화해 아이콘·브랜치·들여쓰기를 유지한다.
/// 행 수에 따라 평문 행이나 다른 카드 레이아웃으로 전환하지 않는다.
#[gpui::test]
fn owe900_imported_idle_worktrees_render_as_cards_and_all_projects_visible(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    cx.simulate_resize(gpui::size(gpui::px(360.), gpui::px(900.)));

    // 실측 이관 결과(프로젝트 16·워크트리 122)를 본뜬 세션 없는 스냅샷.
    let mut trees: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    let mut meta = Meta::new();
    for i in 0..16 {
        let repo = PathBuf::from(format!("/repo-{i:02}"));
        let mut list = Vec::new();
        for j in 0..8 {
            let wt = PathBuf::from(format!("/repo-{i:02}/wt-{j:02}"));
            meta.insert(
                wt.clone(),
                Tree {
                    branch: Some(format!("m10-owe-{i:02}{j}")),
                    dirty: None,
                },
            );
            list.push(wt);
        }
        trees.insert(repo, list);
    }
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: Vec::new(),
                trees,
            };
            s.meta = meta;
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();

    let (repo_count, wt_count, row_count) = cx.update(|_, cx| {
        view.read(cx)
            .rows
            .iter()
            .fold((0, 0, 0), |(repos, wts, total), row| match row {
                Row::Repository(_) => (repos + 1, wts, total + 1),
                Row::Worktree(_) => (repos, wts + 1, total + 1),
                _ => (repos, wts, total + 1),
            })
    });
    assert_eq!(repo_count, 16, "16 imported projects must all be visible");
    assert_eq!(wt_count, 128, "all imported idle worktrees must be visible");
    assert!(
        row_count >= 100,
        "fixture must hit the virtualized path (rows={row_count})"
    );
    // 기본 필터(빈 repos/hosts)는 모든 프로젝트·호스트를 보인다.
    cx.update(|_, cx| {
        let opts = view.read(cx).options();
        assert_eq!(opts.active_filter_count(), 0);
        assert!(opts.filters.contains_repo(Path::new("/repo-15")));
        assert!(opts.filters.contains_host(&HostId::Local));
    });

    // 첫 워크트리 행(rows[1])이 카드로 그려진다. 평문 폴백 행에는
    // `debug_selector`가 없어 이 assert가 회귀를 잡는다.
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    for selector in ["worktree-card-1", "worktree-branch-1"] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} must render for idle imported worktree in virtualized list"
        );
    }
    // 저장소 헤더와 워크트리 수 배지가 보인다(rows[0] = 첫 프로젝트, 워크트리 8개).
    assert!(cx.debug_bounds("repo-header-0").is_some());
    assert!(cx.debug_bounds("repo-count-0").is_some());
}

/// OWE-900: 세션 없는 워크트리가 10개를 넘는 프로젝트는 접힌 상태로 시작한다.
/// 핀·세션이 있는 프로젝트는 펼친 채로 두고, 사용자가 펼치면 재시작 후에도 유지된다.
#[gpui::test]
fn owe900_idle_heavy_repo_auto_collapses_and_expand_persists(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let dir = std::env::temp_dir().join(format!("owe900-auto-collapse-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sidebar.bin");
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(path.clone())));

    let big: Vec<PathBuf> = (0..12)
        .map(|i| PathBuf::from(format!("/repo-big/wt-{i:02}")))
        .collect();
    let small: Vec<PathBuf> = (0..3)
        .map(|i| PathBuf::from(format!("/repo-small/wt-{i:02}")))
        .collect();
    let active: Vec<PathBuf> = (0..12)
        .map(|i| PathBuf::from(format!("/repo-active/wt-{i:02}")))
        .collect();
    let pinned: Vec<PathBuf> = (0..12)
        .map(|i| PathBuf::from(format!("/repo-pinned/wt-{i:02}")))
        .collect();
    let mut active_sess = session(1, AgentState::Working, Some("/repo-active/wt-00"));
    active_sess.status.agent = "claude".into();

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: vec![active_sess],
                trees: BTreeMap::from([
                    (PathBuf::from("/repo-active"), active.clone()),
                    (PathBuf::from("/repo-big"), big.clone()),
                    (PathBuf::from("/repo-pinned"), pinned.clone()),
                    (PathBuf::from("/repo-small"), small.clone()),
                ]),
            };
            s.prefs.insert(
                PathBuf::from("/repo-pinned/wt-00"),
                Preference {
                    pinned: true,
                    name: String::new(),
                },
            );
            let (_, auto_changed) = s.sync_imported_projects_from_disk();
            assert!(auto_changed, "idle-heavy repo must trigger auto-collapse");
            s.rebuild(cx);
            assert!(
                s.collapsed.contains(Path::new("/repo-big")),
                "repo with 12 idle worktrees must start collapsed"
            );
            for repo in ["/repo-active", "/repo-pinned", "/repo-small"] {
                assert!(
                    !s.collapsed.contains(Path::new(repo)),
                    "{repo} must stay expanded"
                );
            }
        });
    });
    cx.run_until_parked();

    // 접힌 프로젝트는 헤더+수 배지만 보이고 하위 워크트리 행은 없다.
    let big_idx = cx.update(|_, cx| {
        view.read(cx)
            .rows
            .iter()
            .position(|r| matches!(r, Row::Repository(p) if p == Path::new("/repo-big")))
            .unwrap()
    });
    assert!(
        !cx.update(|_, cx| view
            .read(cx)
            .rows
            .iter()
            .any(|r| matches!(r, Row::Worktree(p) if p.starts_with("/repo-big")))),
        "collapsed repo must hide its worktree rows"
    );
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    // `debug_bounds`는 `&'static str` 선택자를 받으므로 동적 인덱스를 누수시킨다(테스트 1회성).
    let header_sel: &'static str = Box::leak(format!("repo-header-{big_idx}").into_boxed_str());
    let count_sel: &'static str = Box::leak(format!("repo-count-{big_idx}").into_boxed_str());
    assert!(cx.debug_bounds(header_sel).is_some());
    assert!(cx.debug_bounds(count_sel).is_some());

    // 사용자가 펼치면 파일에 저장되고, 재시작 후에도 펼쳐진 채로 유지된다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.fold(&PathBuf::from("/repo-big"), Some(true), cx);
        });
    });
    cx.run_until_parked();
    let mut restored = Sidebar::new_loaded(Some(path.clone()));
    assert!(
        !restored.collapsed.contains(Path::new("/repo-big")),
        "user-expanded repo must stay expanded after restart"
    );
    assert!(
        restored.prefs.contains_key(Path::new("/repo-big")),
        "auto-collapse marker must persist so restart does not re-collapse"
    );
    restored.snapshot = Snapshot {
        sessions: Vec::new(),
        trees: BTreeMap::from([(PathBuf::from("/repo-big"), big.clone())]),
    };
    let (_, auto_changed) = restored.sync_imported_projects_from_disk();
    assert!(
        !auto_changed,
        "seen repo must not auto-collapse again after restart"
    );
    assert!(!restored.collapsed.contains(Path::new("/repo-big")));

    fs::remove_dir_all(dir).ok();
}

/// OWE-846:
/// 1) 핀 고정 시 우측 `★` 버튼이 제목·`[기본]` 배지·우측 배지와 겹치지 않고, 이름 변경 버튼(`worktree-rename-*`)이 동작한다.
/// 2) `card_title == branch_subtitle`(`main`)이면 중복 브랜치 줄을 생략하고, 배지가 많은 카드에서는
///    브랜치 줄과 배지 줄을 분리해 브랜치명이 `...`로 찌그러지거나 체크포인트 배지가 `88px`에서 잘리지 않는다.
/// 3) 100행 이상 가상화 모드에서도 워크트리 행 마우스 클릭이 `Selected` / `NewSession`을 발생시킨다.
#[gpui::test]
fn owe_846_worktree_card_actions_branch_dedupe_badges_and_virtualized_click(
    cx: &mut TestAppContext,
) {
    use crate::pull_request::Status as PrStatus;
    use gpui::{Modifiers, px, size};
    use krill_proto::orchestration::WorktreeStore;
    use std::cell::RefCell;
    use std::rc::Rc;

    let dir = std::env::temp_dir().join(format!("mk-owe846-card-ux-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let sidebar_bin = dir.join("sidebar.bin");

    let repo = PathBuf::from("/repo");
    let wt_feature = PathBuf::from("/repo/feat-ux");
    let store = WorktreeStore {
        statuses: BTreeMap::from([(
            wt_feature.to_string_lossy().into_owned(),
            "in-review".into(),
        )]),
        unread: BTreeMap::from([(wt_feature.to_string_lossy().into_owned(), true)]),
        prs: BTreeMap::from([(wt_feature.to_string_lossy().into_owned(), 128)]),
        issues: BTreeMap::from([(wt_feature.to_string_lossy().into_owned(), "OWE-626".into())]),
        references: BTreeMap::from([(
            wt_feature.to_string_lossy().into_owned(),
            vec![
                "https://example.com/1".into(),
                "https://example.com/2".into(),
            ],
        )]),
        ..WorktreeStore::default()
    };
    store.save(&dir).unwrap();

    assert_eq!(
        external::checkpoint_badge_for(Some(&sidebar_bin), &wt_feature).as_deref(),
        Some("REVIEW #128 OWE-626 links 2")
    );
    assert_eq!(
        external::checkpoint_badge_for_card(Some(&sidebar_bin), &wt_feature, true).as_deref(),
        Some("REVIEW #128 links 2"),
        "card checkpoint badge must omit issue key when issue_badge is separately shown"
    );

    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(sidebar_bin.clone())));
    vcx.simulate_resize(size(px(240.), px(720.)));

    let selected_events: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_got = selected_events.clone();
    let _sub_sel = vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, ev: &WorkspaceSelected, _| {
            selected_got.borrow_mut().push(ev.path.clone());
        })
    });
    let launched_events: Rc<RefCell<Vec<OpenWorkspace>>> = Rc::new(RefCell::new(Vec::new()));
    let launched_got = launched_events.clone();
    let _sub_new = vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, ev: &OpenWorkspace, _| {
            launched_got.borrow_mut().push(ev.clone());
        })
    });

    vcx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: Vec::new(),
                trees: BTreeMap::from([(repo.clone(), vec![repo.clone(), wt_feature.clone()])]),
            };
            s.meta.insert(
                repo.clone(),
                Tree {
                    branch: Some("main".into()),
                    dirty: None,
                },
            );
            s.meta.insert(
                wt_feature.clone(),
                Tree {
                    branch: Some("feat/owe-846-worktree-card-ux".into()),
                    dirty: Some(3),
                },
            );
            s.prs.insert(wt_feature.clone(), PrStatus::Open);
            s.set_worktree_issue(wt_feature.clone(), "OWE-626", cx);
            // 기본 워크트리(repo)를 핀 고정해 `[기본]` 배지와 `★` 핀 버튼이 겹치지 않는지 확인한다.
            s.toggle_pin(repo.clone(), cx);
        });
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();

    // row 1: 핀 고정된 기본 워크트리(`repo`, `main`), row 2: `wt_feature`
    assert!(
        vcx.debug_bounds("worktree-branch-1").is_none(),
        "default branch card where card_title == branch_subtitle ('main') must not duplicate branch on row 2"
    );
    let default_badge = vcx
        .debug_bounds("worktree-default-badge-1")
        .expect("default worktree badge must render");
    let pin_btn = vcx
        .debug_bounds("worktree-pin-1")
        .expect("pin button must render");
    let rename_btn = vcx
        .debug_bounds("worktree-rename-1")
        .expect("rename button must render");
    let new_btn = vcx
        .debug_bounds("worktree-new-session-1")
        .expect("new session button must render");
    assert!(
        f32::from(pin_btn.origin.x) > f32::from(rename_btn.origin.x)
            && f32::from(rename_btn.origin.x) > f32::from(new_btn.origin.x),
        "pin button must be placed at the right end of the action bar so idle pinned star sits at the right edge"
    );
    assert!(
        f32::from(default_badge.origin.x + default_badge.size.width) <= f32::from(pin_btn.origin.x),
        "default badge right edge ({default_badge:?}) must not overlap pinned star button ({pin_btn:?})"
    );

    // 배지가 많은 `wt_feature`(row 2): 브랜치 줄이 전체 폭을 쓰고 배지 줄이 아래에 배치되어야 한다.
    let branch_bounds = vcx
        .debug_bounds("worktree-branch-3")
        .expect("distinct branch subtitle must render on feature worktree");
    let checkpoint_bounds = vcx
        .debug_bounds("checkpoint-badge-3")
        .expect("checkpoint badge must render");
    assert!(
        f32::from(branch_bounds.size.width) >= 140.0,
        "branch subtitle width ({branch_bounds:?}) must not be crushed to '...' by badges"
    );
    assert!(
        f32::from(checkpoint_bounds.origin.y) >= f32::from(branch_bounds.origin.y) + 10.0,
        "dense badges must wrap onto a separate row below the branch subtitle ({checkpoint_bounds:?} vs {branch_bounds:?})"
    );

    // Compact 모드에서도 핀 고정된 카드의 우측 배지가 `★` 버튼과 겹치지 않고 `main` 중복 브랜치가 없어야 한다.
    // 두 카드 모두 핀 고정돼도 [기본] 워크트리(repo)는 항상 맨 위(행 1)다.
    vcx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.toggle_pin(wt_feature.clone(), cx);
            s.set_card_layout(CardLayout::Compact, cx);
        });
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();
    let mut row_index = |path: &Path| {
        vcx.update(|_, cx| {
            view.read(cx)
                .rows
                .iter()
                .position(|row| matches!(row, Row::Worktree(p) if p == path))
                .unwrap()
        })
    };
    let repo_idx = row_index(&repo);
    let feature_idx = row_index(&wt_feature);
    let selector = |prefix: &str, index: usize| -> &'static str {
        Box::leak(format!("{prefix}-{index}").into_boxed_str())
    };
    let repo_branch = selector("worktree-branch", repo_idx);
    let feature_branch = selector("worktree-branch", feature_idx);
    let feature_pr = selector("pr-badge", feature_idx);
    let feature_pin = selector("worktree-pin", feature_idx);
    let repo_rename = selector("worktree-rename", repo_idx);
    assert!(
        vcx.debug_bounds(repo_branch).is_none(),
        "compact layout must also suppress duplicate branch when card_title == branch_subtitle"
    );
    assert!(
        vcx.debug_bounds(feature_branch).is_some(),
        "compact layout must keep distinct branch subtitle on feature worktree"
    );
    let compact_pr = vcx
        .debug_bounds(feature_pr)
        .expect("compact pr badge must render");
    let compact_pin = vcx
        .debug_bounds(feature_pin)
        .expect("compact pin button must render");
    assert!(
        f32::from(compact_pr.origin.x + compact_pr.size.width) <= f32::from(compact_pin.origin.x),
        "compact right badges ({compact_pr:?}) must not overlap pinned star ({compact_pin:?})"
    );

    // 이름 변경(연필) 버튼 클릭 시 인라인 이름 편집 입력창이 열려야 한다.
    // 호버 전용 버튼이라 먼저 마우스를 올려 가시화한다(OWE-612 패턴).
    let rename_at = vcx.debug_bounds(repo_rename).unwrap().center();
    vcx.simulate_mouse_move(rename_at, None::<gpui::MouseButton>, Modifiers::default());
    vcx.run_until_parked();
    vcx.simulate_click(rename_at, Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, cx| view.read(cx).editing.is_some()),
        "clicking rename pencil button must start inline rename"
    );
    vcx.update(|_, cx| view.update(cx, |s, cx| s.finish_rename(None, cx)));

    // 3) 100행 이상 가상화 모드에서 워크트리 행 클릭 시 `activate_worktree_card`가 호출되어야 한다.
    vcx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.set_card_layout(CardLayout::Detailed, cx);
            let trees: Vec<PathBuf> = (0..120)
                .map(|i| PathBuf::from(format!("/repo/wt-{i:03}")))
                .collect();
            let sess = session(77, AgentState::Working, Some("/repo/wt-000"));
            s.snapshot = Snapshot {
                sessions: vec![sess],
                trees: BTreeMap::from([(repo.clone(), trees)]),
            };
            s.rebuild(cx);
        });
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();
    assert!(vcx.update(|_, cx| view.read(cx).rows.len()) >= 100);

    // 세션이 있는 첫 워크트리(index 1: `/repo/wt-000`) 클릭 → `Selected(SessionId(77))`
    let wt0_center = vcx
        .debug_bounds("worktree-card-1")
        .expect("virtualized worktree row 1 must have debug_bounds")
        .center();
    vcx.simulate_click(wt0_center, Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        selected_events.borrow().as_slice(),
        &[PathBuf::from("/repo/wt-000")]
    );

    // 세션이 없는 두 번째 워크트리(`/repo/wt-001`) 클릭 → `NewSession`.
    // `wt-000`에 세션이 있어 행 구조가 [Repo, wt-000, Session77, wt-001, ...]이므로
    // 두 번째 워크트리 카드는 index 3이다.
    let wt1_center = vcx
        .debug_bounds("worktree-card-3")
        .expect("virtualized worktree row 3 must have debug_bounds")
        .center();
    vcx.simulate_click(wt1_center, Modifiers::default());
    vcx.run_until_parked();
    assert!(launched_events.borrow().is_empty());
    assert_eq!(
        selected_events.borrow().last(),
        Some(&PathBuf::from("/repo/wt-001"))
    );
    fs::remove_dir_all(dir).ok();
}

/// OWE-842: 사이드바 프로젝트·워크트리·그룹·원격 호스트 접힘/펼침, 다중 세션 카드 펼침, 선택된 워크트리가
/// `sidebar.bin`에 저장·복원되며, `UiExpansionState` 꼬리가 없는 기존 v3 `sidebar.bin`도 그대로 읽힌다.
#[gpui::test]
fn owe_842_sidebar_persists_expansion_state_and_selected_worktree(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let dir = std::env::temp_dir().join(format!("mk-sidebar-owe-842-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let sidebar_bin = dir.join("sidebar.bin");

    let repo = PathBuf::from("/repo");
    let wt_a = PathBuf::from("/repo/a");
    let wt_b = PathBuf::from("/repo/b");
    let group = options::GroupKey::PrStatus(options::PrGroup::Open);
    let remote = crate::hosts::HostId::Remote("remote-dev".into());

    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(sidebar_bin.clone())));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: vec![
                    session(1, AgentState::Working, Some("/repo/a")),
                    session(2, AgentState::Done, Some("/repo/b")),
                ],
                trees: BTreeMap::from([(repo.clone(), vec![wt_a.clone(), wt_b.clone()])]),
            };
            s.rebuild(cx);
            s.fold(&wt_a, Some(false), cx);
            s.fold_group(group, Some(false), cx);
            s.fold_host(&remote, Some(false), cx);
            s.toggle_card_sessions(&wt_b, cx);
            s.select_worktree(Some(wt_b.clone()), cx);
        });
    });
    cx.run_until_parked();

    let reloaded = Sidebar::new_loaded(Some(sidebar_bin.clone()));
    assert!(reloaded.is_collapsed(&wt_a));
    assert!(!reloaded.is_collapsed(&wt_b));
    assert!(reloaded.is_group_collapsed(group));
    assert!(reloaded.is_host_collapsed(&remote));
    assert!(reloaded.is_card_expanded(&wt_b));
    assert_eq!(
        reloaded.selected_worktree().map(Path::to_path_buf),
        Some(wt_b.clone())
    );

    // 새 창에서 다시 그려도(rebuild) 선택된 워크트리(`wt_b`)로 커서가 복원된다.
    let (view2, cx) = cx.add_window_view(|_, _| reloaded);
    cx.update(|_, cx| {
        view2.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: vec![
                    session(1, AgentState::Working, Some("/repo/a")),
                    session(2, AgentState::Done, Some("/repo/b")),
                ],
                trees: BTreeMap::from([(repo, vec![wt_a, wt_b.clone()])]),
            };
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = view2.read(cx);
        let cur = s.cursor.expect("restored cursor");
        assert_eq!(s.rows.get(cur), Some(&Row::Worktree(wt_b)));
    });

    fs::remove_dir_all(dir).ok();
}

/// OWE-845: 저장소 헤더 접기/펼치기가 하위 워크트리를 숨기고 재시작 후에도 유지된다.
#[gpui::test]
fn repo_header_fold_hides_children_and_persists(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mk-sidebar-{}-owe845", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sidebar.bin");
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(path.clone())));
    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot.trees.insert(
                PathBuf::from("/repo"),
                vec![PathBuf::from("/repo"), PathBuf::from("/repo/a")],
            );
            s.rebuild(cx);
            assert!(
                s.rows
                    .iter()
                    .any(|r| matches!(r, Row::Worktree(w) if w == Path::new("/repo/a"))),
                "펼친 저장소는 하위 워크트리를 보여야 한다"
            );
            s.fold(&PathBuf::from("/repo"), None, cx);
            assert!(
                s.rows.iter().all(|r| !matches!(r, Row::Worktree(_))),
                "접은 저장소는 워크트리 행을 숨겨야 한다"
            );
            assert!(
                s.rows
                    .iter()
                    .any(|r| matches!(r, Row::Repository(rp) if rp == Path::new("/repo"))),
                "접어도 저장소 헤더 행은 남아야 한다"
            );
        });
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    assert!(vcx.debug_bounds("repo-header-0").is_some());
    let restored = Sidebar::new_loaded(Some(path.clone()));
    assert!(
        restored.collapsed.contains(Path::new("/repo")),
        "저장소 접힘은 재시작 후에도 유지되어야 한다"
    );
    fs::remove_dir_all(dir).ok();
}

/// OWE-845: 접힘·펼침·숨김·제거 UI 상태가 `sidebar.bin`에 영속화된다.
#[gpui::test]
fn ui_fold_state_round_trips_through_sidebar_bin(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mk-sidebar-{}-owe845u", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sidebar.bin");
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(path.clone())));
    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.collapsed.insert(PathBuf::from("/repo/a"));
            s.collapsed_groups.insert(GroupKey::PrStatus(PrGroup::Open));
            s.expanded_cards.insert(PathBuf::from("/repo/b"));
            s.shown_external.insert(PathBuf::from("/repo"));
            s.collapsed_hosts.insert(HostId::Local);
            s.removed_projects.insert(PathBuf::from("/old"));
            s.persist(cx);
        });
    });
    vcx.run_until_parked();
    let restored = Sidebar::new_loaded(Some(path.clone()));
    assert!(restored.collapsed.contains(Path::new("/repo/a")));
    assert!(
        restored
            .collapsed_groups
            .contains(&GroupKey::PrStatus(PrGroup::Open))
    );
    assert!(restored.expanded_cards.contains(Path::new("/repo/b")));
    assert!(restored.shown_external.contains(Path::new("/repo")));
    assert!(restored.collapsed_hosts.contains(&HostId::Local));
    assert!(restored.removed_projects.contains(Path::new("/old")));
    fs::remove_dir_all(dir).ok();
}

/// OWE-845: 목록에서 제거한 저장소는 행에서 사라지고 재시작 후에도 숨겨진다.
#[gpui::test]
fn removed_project_hides_repo_and_survives_restart(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mk-sidebar-{}-owe845r", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sidebar.bin");
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(Some(path.clone())));
    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot.trees.insert(
                PathBuf::from("/repo"),
                vec![PathBuf::from("/repo"), PathBuf::from("/repo/a")],
            );
            s.rebuild(cx);
            assert!(
                s.rows
                    .iter()
                    .any(|r| matches!(r, Row::Repository(rp) if rp == Path::new("/repo")))
            );
            s.remove_project(PathBuf::from("/repo"), cx);
            assert!(
                s.rows.iter().all(|r| !matches!(r, Row::Repository(_))),
                "제거한 저장소 헤더는 목록에서 사라져야 한다"
            );
        });
    });
    vcx.run_until_parked();
    let restored = Sidebar::new_loaded(Some(path.clone()));
    assert!(restored.removed_projects.contains(Path::new("/repo")));
    fs::remove_dir_all(dir).ok();
}

/// 재연결 확인: 재연결 중에서 풀린 뒤 처음 도착한 목록만 세션 수와 함께 한 번 알린다.
#[gpui::test]
fn fresh_list_after_reconnect_emits_relisted_once(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    let got = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = got.clone();
    let _sub = cx
        .update(|_, cx| cx.subscribe(&view, move |_, e: &Relisted, _| sink.borrow_mut().push(e.0)));
    let apply = |cx: &mut gpui::VisualTestContext, ok: bool| {
        cx.update(|_, cx| {
            view.update(cx, |s, cx| {
                let result = if ok {
                    Ok((
                        Snapshot {
                            sessions: vec![
                                session(1, AgentState::Working, None),
                                session(2, AgentState::Done, None),
                            ],
                            trees: BTreeMap::new(),
                        },
                        Meta::new(),
                    ))
                } else {
                    Err(io::Error::other("daemon down"))
                };
                s.apply(result, BTreeMap::new(), cx);
            });
        });
    };
    let reconnecting = |cx: &mut gpui::VisualTestContext, on: bool| {
        cx.update(|_, cx| view.update(cx, |s, cx| s.set_local_reconnecting(on, cx)));
    };
    apply(cx, true);
    reconnecting(cx, false);
    apply(cx, true);
    assert!(got.borrow().is_empty(), "재연결이 없었으면 알리지 않는다");
    reconnecting(cx, true);
    apply(cx, false);
    apply(cx, true);
    assert!(
        got.borrow().is_empty(),
        "셸이 다시 붙기 전 목록은 알리지 않는다"
    );
    reconnecting(cx, false);
    apply(cx, false);
    assert!(got.borrow().is_empty(), "실패한 폴링은 목록 도착이 아니다");
    apply(cx, true);
    apply(cx, true);
    assert_eq!(*got.borrow(), vec![2]);
}

/// 로컬 데몬 재연결 중의 '내 Mac' 끊김만 보존 톤이고, 원격·실패는 destructive 그대로다.
#[test]
fn local_reconnecting_host_is_preserve() {
    let p = &crate::ui::DARK;
    let (c, label) = view::host_status_tone(HostStatus::Disconnected, true, None, p);
    assert_eq!((c, label), (p.preserve, text::HOST_RECONNECTING.get()));
    let (c, _) = view::host_status_tone(HostStatus::Disconnected, false, None, p);
    assert_eq!(c, p.destructive);
    let (c, _) = view::host_status_tone(HostStatus::Connected, true, None, p);
    assert_eq!(c, p.success);
}

/// OWE-847: 이름 변경은 카드 제목 자리에서 인라인으로 편집하고 Enter/저장 버튼(✓)으로 저장·Esc/취소 버튼(✕)으로 취소한다.
#[gpui::test]
fn inline_rename_saves_on_enter_and_cancels_on_escape(cx: &mut TestAppContext) {
    use gpui::Modifiers;
    use std::cell::RefCell;
    use std::rc::Rc;
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let launched: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
    let launched_out = launched.clone();
    let _sub = vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, _: &NewSession, _| {
            *launched_out.borrow_mut() += 1;
        })
    });
    vcx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.snapshot.trees.insert(
                PathBuf::from("/repo"),
                vec![PathBuf::from("/repo"), PathBuf::from("/repo/a")],
            );
            s.rebuild(cx);
            s.rename(PathBuf::from("/repo/a"), window, cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    assert!(
        vcx.debug_bounds("rename-inline-1").is_some(),
        "이름 변경 입력창은 워크트리 카드 제목 자리(행 2, [기본] /repo 다음)에 있어야 한다"
    );
    assert!(
        vcx.debug_bounds("rename-inline-save-1").is_some()
            && vcx.debug_bounds("rename-inline-cancel-1").is_some(),
        "인라인 이름 변경 행에는 저장(✓)·취소(✕) 버튼이 표시되어야 한다"
    );
    // 입력창 영역을 클릭해도 워크트리 카드 활성화(세션 실행/포커스 탈취)가 일어나면 안 된다.
    let inline_center = vcx.debug_bounds("rename-inline-1").unwrap().center();
    vcx.simulate_click(inline_center, Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        *launched.borrow(),
        0,
        "인라인 입력창 클릭이 카드 활성화로 전파되면 안 된다"
    );
    // Esc: 저장하지 않고 닫는다.
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |s, _| {
            assert!(s.editing.is_none(), "Esc는 편집을 취소해야 한다");
            assert!(
                !s.prefs.contains_key(Path::new("/repo/a")),
                "취소 시 이름이 저장되면 안 된다"
            );
        });
    });
    assert!(vcx.debug_bounds("rename-inline-1").is_none());
    // Enter: 저장한다.
    vcx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.rename(PathBuf::from("/repo/a"), window, cx);
        });
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("cmd-a");
    vcx.simulate_input("새 이름");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |s, _| {
            assert!(s.editing.is_none(), "Enter는 편집을 마쳐야 한다");
            assert_eq!(
                s.prefs[Path::new("/repo/a")].name,
                "새 이름",
                "Enter는 입력한 이름을 저장해야 한다"
            );
        });
    });
    // 저장(✓)·취소(✕) 버튼 클릭으로도 각각 저장·취소할 수 있어야 한다.
    vcx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.rename(PathBuf::from("/repo/a"), window, cx);
        });
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("cmd-a");
    vcx.simulate_input("버튼 저장");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    // Saving the first name may reorder the cards; follow the edited path.
    let index = vcx.update(|_, cx| {
        view.read(cx)
            .rows
            .iter()
            .position(|row| matches!(row, Row::Worktree(path) if path == Path::new("/repo/a")))
            .unwrap()
    });
    let save_at = vcx
        .debug_bounds(format!("rename-inline-save-{index}").leak())
        .unwrap()
        .center();
    vcx.simulate_click(save_at, Modifiers::default());
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |s, _| {
            assert!(s.editing.is_none(), "저장 버튼 클릭은 편집을 마쳐야 한다");
            assert_eq!(s.prefs[Path::new("/repo/a")].name, "버튼 저장");
        });
    });
}

/// OWE-847: 워크트리 삭제는 확인 모달을 거쳐 확정해야 실행되고 강제 플래그가 전달된다.
#[gpui::test]
fn delete_confirm_modal_requires_explicit_confirm_with_force(cx: &mut TestAppContext) {
    struct RecordingSource {
        removed: std::sync::Mutex<Vec<(PathBuf, bool)>>,
        snapshot: Snapshot,
    }
    impl Source for RecordingSource {
        fn fetch(&self) -> io::Result<Snapshot> {
            Ok(self.snapshot.clone())
        }
        fn meta(&self, _trees: &BTreeMap<PathBuf, Vec<PathBuf>>) -> Meta {
            BTreeMap::from([(
                PathBuf::from("/repo/a"),
                Tree {
                    branch: Some("feat".into()),
                    dirty: Some(2),
                },
            )])
        }
        fn remove_worktree(&self, req: krill_proto::control::RemoveWorktree) -> io::Result<Reply> {
            use std::os::unix::ffi::OsStrExt as _;
            let path = PathBuf::from(std::ffi::OsStr::from_bytes(&req.path));
            self.removed
                .lock()
                .expect("test lock")
                .push((path.clone(), req.force));
            Ok(Reply::WorktreeRemoved(
                krill_proto::control::RemovedWorktree {
                    branch: Some("feat".into()),
                    branch_deleted: true,
                    warnings: Vec::new(),
                },
            ))
        }
    }
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let events = Arc::new(RecordingSource {
        removed: std::sync::Mutex::new(Vec::new()),
        snapshot: Snapshot {
            sessions: Vec::new(),
            trees: BTreeMap::from([(
                PathBuf::from("/repo"),
                vec![PathBuf::from("/repo"), PathBuf::from("/repo/a")],
            )]),
        },
    });
    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.meta.insert(
                PathBuf::from("/repo/a"),
                Tree {
                    branch: Some("feat".into()),
                    dirty: Some(2),
                },
            );
            s.connect_source(events.clone(), cx);
        });
    });
    vcx.run_until_parked();
    // 확인 모달을 열기만 해서는 삭제되지 않는다.
    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.request_delete_worktree(PathBuf::from("/repo/a"), false, cx);
            assert!(s.delete_confirm.is_some());
        });
    });
    vcx.run_until_parked();
    assert!(
        events.removed.lock().expect("test lock").is_empty(),
        "모달 확정 전에는 삭제 IPC를 보내면 안 된다"
    );
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    assert!(vcx.debug_bounds("delete-worktree-dialog").is_some());
    assert!(vcx.debug_bounds("delete-worktree-force").is_some());
    // 강제 토글 후 확정하면 force=true로 삭제된다.
    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.toggle_delete_force(cx);
            s.confirm_delete(cx);
            assert!(s.delete_confirm.is_none());
        });
    });
    vcx.run_until_parked();
    assert_eq!(
        events.removed.lock().expect("test lock").as_slice(),
        [(PathBuf::from("/repo/a"), true)],
        "확정 시 강제 플래그와 함께 삭제해야 한다"
    );
    // 취소는 모달만 닫는다.
    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.request_delete_worktree(PathBuf::from("/repo/a"), false, cx);
            s.cancel_delete(cx);
            assert!(s.delete_confirm.is_none());
        });
    });
    vcx.run_until_parked();
    assert_eq!(
        events.removed.lock().expect("test lock").len(),
        1,
        "취소 시 추가 삭제가 없어야 한다"
    );
}

/// OWE-854: 세션 행 메뉴 액션(열기·이어하기·종료)은 세션 이벤트를 내보내고 호스트를 남긴다.
#[gpui::test]
fn session_row_actions_emit_session_events(cx: &mut TestAppContext) {
    use std::cell::RefCell;
    use std::rc::Rc;
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut s1 = session(1, AgentState::Working, Some("/repo/a"));
            s1.status.agent = "claude".into();
            s.snapshot = Snapshot {
                sessions: vec![s1],
                trees: BTreeMap::from([(PathBuf::from("/repo"), vec!["/repo/a".into()])]),
            };
            s.toggle_card_sessions(Path::new("/repo/a"), cx);
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();
    let selected: Rc<RefCell<Vec<SessionId>>> = Rc::new(RefCell::new(Vec::new()));
    let closed: Rc<RefCell<Vec<SessionId>>> = Rc::new(RefCell::new(Vec::new()));
    let continued: Rc<RefCell<Vec<SessionId>>> = Rc::new(RefCell::new(Vec::new()));
    let renamed: Rc<RefCell<Vec<SessionId>>> = Rc::new(RefCell::new(Vec::new()));
    let (s_out, c_out, t_out) = (selected.clone(), closed.clone(), continued.clone());
    let r_out = renamed.clone();
    let _sub_rename = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &RenameSession, _| {
            r_out.borrow_mut().push(event.0);
        })
    });
    let _sub_selected = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &Selected, _| {
            s_out.borrow_mut().push(event.0);
        })
    });
    let _sub_close = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &CloseSession, _| {
            c_out.borrow_mut().push(event.0);
        })
    });
    let _sub_continue = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &ContinueSession, _| {
            t_out.borrow_mut().push(event.0);
        })
    });
    let idx = cx.update(|_, cx| {
        view.read(cx)
            .rows
            .iter()
            .position(|r| matches!(r, Row::Session(s, _) if s.id == SessionId(1)))
            .expect("nested session row renders when card is expanded")
    });
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.select_session(idx, SessionId(1), cx);
            s.request_close_session(idx, SessionId(1), cx);
            s.request_continue_session(idx, SessionId(1), cx);
            s.request_rename_session(idx, SessionId(1), cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(renamed.borrow().as_slice(), &[SessionId(1)]);
    assert_eq!(selected.borrow().as_slice(), &[SessionId(1)]);
    assert_eq!(closed.borrow().as_slice(), &[SessionId(1)]);
    assert_eq!(continued.borrow().as_slice(), &[SessionId(1)]);
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(s.cursor, Some(idx));
        assert_eq!(s.action_host, Some(HostId::Local));
    });
}

/// OWE-854: 1개 세션 카드의 세션 줄 클릭은 펼치기 토글이 아니라 해당 세션 선택이다.
#[gpui::test]
fn single_session_card_line_selects_session(cx: &mut TestAppContext) {
    use gpui::Modifiers;
    use std::cell::RefCell;
    use std::rc::Rc;
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut s1 = session(1, AgentState::Working, Some("/repo/a"));
            s1.status.agent = "claude".into();
            s.snapshot = Snapshot {
                sessions: vec![s1],
                trees: BTreeMap::from([(PathBuf::from("/repo"), vec!["/repo/a".into()])]),
            };
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();
    let emitted: Rc<RefCell<Vec<SessionId>>> = Rc::new(RefCell::new(Vec::new()));
    let emitted_out = emitted.clone();
    let _sub = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &Selected, _| {
            emitted_out.borrow_mut().push(event.0);
        })
    });
    let card_idx = cx.update(|_, cx| {
        view.read(cx)
            .rows
            .iter()
            .position(|r| matches!(r, Row::Worktree(p) if p == Path::new("/repo/a")))
            .expect("worktree card row")
    });
    let sel: &'static str = Box::leak(format!("card-session-{card_idx}").into_boxed_str());
    let line = cx
        .debug_bounds(sel)
        .expect("single-session line renders in detailed card");
    cx.simulate_click(line.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(emitted.borrow().as_slice(), &[SessionId(1)]);
    assert!(
        !cx.update(|_, cx| view.read(cx).expanded_cards.contains(Path::new("/repo/a"))),
        "single-session line click must select, not expand"
    );
}
/// OWE-898: 스냅샷에서 사라진 워크트리의 파생 상태(상태·갱신시각·이슈·포트·호스트 라벨·자동화/CLI 표시)는
/// 다음 폴링 적용 시 정리된다. 디스크 저장소가 삭제된 항목을 계속 내놓아도 누적되지 않게 한다.
/// 사용자 접힘·핀·부모 설정은 건드리지 않는다.
#[gpui::test]
fn stale_worktree_state_pruned_when_snapshot_shrinks(cx: &mut TestAppContext) {
    use krill_proto::orchestration::WorkspaceStatus;
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let wt_live = PathBuf::from("/repo/wt");
    let wt_gone = PathBuf::from("/repo/gone");
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.set_worktree_status(wt_live.clone(), WorkspaceStatus::InProgress, cx);
            s.set_worktree_status(wt_gone.clone(), WorkspaceStatus::Todo, cx);
            s.set_worktree_updated_at(wt_live.clone(), 7, cx);
            s.set_worktree_updated_at(wt_gone.clone(), 8, cx);
            s.set_worktree_issue(wt_live.clone(), "live-issue", cx);
            s.set_worktree_issue(wt_gone.clone(), "gone-issue", cx);
            s.set_worktree_ports(wt_live.clone(), vec![3000], cx);
            s.set_worktree_ports(wt_gone.clone(), vec![8080], cx);
            s.set_worktree_host_label(wt_live.clone(), "live-host", cx);
            s.set_worktree_host_label(wt_gone.clone(), "gone-host", cx);
            s.mark_automation_worktree(wt_gone.clone(), cx);
            s.mark_cli_worktree(wt_gone.clone(), cx);
            // 사용자 접힘은 유지돼야 한다.
            s.collapsed.insert(wt_gone.clone());
        });
    });
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).worktree_statuses.contains_key(&wt_gone)));

    // 라이브 집합에 `/repo/wt`만 남긴 스냅샷을 적용한다.
    let snapshot = Snapshot {
        sessions: vec![session(1, AgentState::Working, Some("/repo/wt"))],
        trees: BTreeMap::from([(PathBuf::from("/repo"), vec![wt_live.clone()])]),
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.apply(Ok((snapshot, Meta::new())), BTreeMap::new(), cx);
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert!(s.worktree_statuses.contains_key(&wt_live));
        assert!(!s.worktree_statuses.contains_key(&wt_gone));
        assert!(!s.worktree_updated_at.contains_key(&wt_gone));
        assert!(!s.issues.contains_key(&wt_gone));
        assert!(!s.ports.contains_key(&wt_gone));
        assert!(!s.worktree_hosts.contains_key(&wt_gone));
        assert!(!s.automation_worktrees.contains(&wt_gone));
        assert!(!s.cli_worktrees.contains(&wt_gone));
        // 라이브 항목과 사용자 접힘은 유지.
        assert!(s.issues.contains_key(&wt_live));
        assert!(s.ports.contains_key(&wt_live));
        assert!(s.collapsed.contains(&wt_gone));
    });
}

#[gpui::test]
fn owe731_grouped_sorted_500_rows_60fps_within_budget(cx: &mut TestAppContext) {
    struct Memory(Snapshot);
    impl Source for Memory {
        fn fetch(&self) -> io::Result<Snapshot> {
            Ok(self.0.clone())
        }
    }
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    let fixture = fixture();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.connect_source(Arc::new(Memory(fixture.clone())), cx);
        });
    });
    cx.run_until_parked();

    let mut samples = Vec::new();
    for (group_by, sort_by) in [
        (GroupBy::Repo, SortBy::Smart),
        (GroupBy::WorkspaceStatus, SortBy::Recent),
        (GroupBy::PrStatus, SortBy::Name),
        (GroupBy::None, SortBy::Smart),
    ] {
        let group_start = Instant::now();
        cx.update(|_, cx| {
            view.update(cx, |s, cx| {
                s.set_group_by(group_by, cx);
            });
        });
        cx.run_until_parked();
        samples.push(group_start.elapsed());

        let sort_start = Instant::now();
        cx.update(|_, cx| {
            view.update(cx, |s, cx| {
                s.set_sort_by(sort_by, cx);
            });
        });
        cx.run_until_parked();
        samples.push(sort_start.elapsed());

        let len = cx.update(|_, cx| view.read(cx).rows.len()).max(1);
        for i in 0..30 {
            let start = Instant::now();
            cx.update(|_, cx| {
                view.update(cx, |s, cx| {
                    s.rendered_rows = 0;
                    s.scroll_to_row((i * 17) % len, cx);
                    cx.notify();
                });
            });
            cx.run_until_parked();
            samples.push(start.elapsed());
        }
    }
    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p99 = samples[(samples.len() * 99) / 100];
    let max = *samples.last().unwrap();
    println!(
        "OWE-731 500-row grouped/sorted benchmark: frames={} p50_us={} p99_us={} max_us={}",
        samples.len(),
        p50.as_micros(),
        p99.as_micros(),
        max.as_micros(),
    );
    assert!(
        p99 < Duration::from_micros(16_670),
        "p99 frame time {p99:?} exceeded 16.67ms (60fps) budget"
    );
}

/// Orca처럼 프로젝트 머리줄을 끌어다 놓으면 순서가 바뀌고, `sidebar.bin`에 남아 다시 열어도 유지된다.
#[gpui::test]
fn dragging_a_project_reorders_and_persists(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let dir = std::env::temp_dir().join(format!("mk-project-order-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sidebar.bin");
    let snapshot = Snapshot {
        sessions: vec![],
        trees: BTreeMap::from([
            (PathBuf::from("/p/a"), vec![PathBuf::from("/p/a")]),
            (PathBuf::from("/p/b"), vec![PathBuf::from("/p/b")]),
            (PathBuf::from("/p/c"), vec![PathBuf::from("/p/c")]),
        ]),
    };
    let repos = |s: &Sidebar| {
        s.rows
            .iter()
            .filter_map(|r| match r {
                Row::Repository(p) => Some(p.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let path = file.clone();
    let (view, cx) = cx.add_window_view(move |_, _| Sidebar::new_loaded(Some(path)));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = snapshot.clone();
            s.rebuild(cx);
            assert_eq!(repos(s), ["/p/a", "/p/b", "/p/c"]);
            s.move_project(Path::new("/p/c"), Path::new("/p/a"), cx);
            assert_eq!(repos(s), ["/p/c", "/p/a", "/p/b"]);
        });
    });
    cx.run_until_parked();
    let (_, _, _, _, order, _) = load_state(&file).unwrap();
    assert_eq!(
        order,
        [
            PathBuf::from("/p/c"),
            PathBuf::from("/p/a"),
            PathBuf::from("/p/b")
        ]
    );
    fs::remove_dir_all(&dir).ok();
}

/// [기본] 워크트리(저장소 본 체크아웃)는 정렬 기준·활동·이름·핀과 무관하게 프로젝트 맨 위에 오고,
/// 계보 자식은 여전히 부모 바로 아래에 붙는다.
#[test]
fn primary_worktree_is_always_first_in_project() {
    let repo = PathBuf::from("/r/krill");
    let brand = PathBuf::from("/r/brand-identity");
    let ci = PathBuf::from("/r/harness-ci");
    let child = PathBuf::from("/r/harness-ci-child");
    let snapshot = Snapshot {
        sessions: vec![session(1, AgentState::Waiting, brand.to_str())],
        trees: BTreeMap::from([(
            repo.clone(),
            vec![repo.clone(), brand.clone(), ci.clone(), child.clone()],
        )]),
    };
    let mut prefs = Preferences::new();
    prefs.insert(
        ci.clone(),
        Preference {
            pinned: true,
            name: String::new(),
        },
    );
    let updated_at = BTreeMap::from([(brand.clone(), 900), (ci.clone(), 800), (repo.clone(), 1)]);
    let parents = BTreeMap::from([(child.clone(), ci.clone())]);
    let ctx = options::WorktreeContext {
        updated_at: Some(&updated_at),
        parents: Some(&parents),
        ..options::WorktreeContext::default()
    };
    for sort_by in [SortBy::Smart, SortBy::Recent, SortBy::Name, SortBy::Created] {
        let opts = WorkspaceOptions {
            sort_by,
            ..WorkspaceOptions::default()
        };
        let order: Vec<PathBuf> = options::rows_with_options(
            &snapshot,
            &prefs,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            &opts,
            &ctx,
        )
        .into_iter()
        .filter_map(|r| match r {
            Row::Worktree(p) => Some(p),
            _ => None,
        })
        .collect();
        // 기본 → 핀(harness-ci) → 그 자식 → 나머지(대기 중·최근·이름이 앞서는 brand-identity).
        assert_eq!(
            order,
            if matches!(sort_by, SortBy::Smart | SortBy::Recent) {
                vec![ci.clone(), brand.clone(), repo.clone(), child.clone()]
            } else {
                vec![ci.clone(), brand.clone(), child.clone(), repo.clone()]
            },
            "{sort_by:?}"
        );
    }
}

/// 상세 세션은 요약 박스 안에, 컴팩트 세션은 한 단계 안에 두고 계보 깊이는 한 번만 반영한다.
#[gpui::test]
fn nested_session_rows_indent_under_their_card_title(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let dir = std::env::temp_dir().join(format!("krill-nested-indent-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let parent = PathBuf::from("/repo/parent");
    let child = PathBuf::from("/repo/child");
    let mut loose = session(3, AgentState::Working, None);
    loose.cwd = Some("/tmp/x".into());
    let sb_path = dir.join("sidebar.bin");
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(Some(sb_path)));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.set_sort_by(SortBy::Name, cx);
            s.snapshot = Snapshot {
                sessions: vec![
                    session(1, AgentState::Working, Some("/repo/parent")),
                    session(2, AgentState::Working, Some("/repo/child")),
                    loose,
                ],
                trees: BTreeMap::from([(
                    PathBuf::from("/repo"),
                    vec![parent.clone(), child.clone()],
                )]),
            };
            s.expanded_cards.insert(parent.clone());
            s.expanded_cards.insert(child.clone());
            s.set_worktree_parent(child.clone(), parent.clone(), cx);
            s.rebuild(cx);
            assert_eq!(s.lineage_depth(&child), 1);
        });
    });
    cx.run_until_parked();
    let rows = cx.update(|_, cx| view.read(cx).rows.clone());
    let at = |pred: &dyn Fn(&Row) -> bool| rows.iter().position(pred).expect("row");
    let parent_row = at(&|r| matches!(r, Row::Worktree(p) if p == Path::new("/repo/parent")));
    let child_row = at(&|r| matches!(r, Row::Worktree(p) if p == Path::new("/repo/child")));
    let s1 = at(&|r| matches!(r, Row::Session(s, true) if s.id == SessionId(1)));
    let s2 = at(&|r| matches!(r, Row::Session(s, true) if s.id == SessionId(2)));
    let folder = at(&|r| matches!(r, Row::Folder(_)));
    let s3 = at(&|r| matches!(r, Row::Session(s, true) if s.id == SessionId(3)));
    let x = |cx: &mut gpui::VisualTestContext, sel: String| {
        cx.debug_bounds(sel.clone().leak())
            .unwrap_or_else(|| panic!("missing {sel}"))
            .origin
            .x
    };
    for layout in [CardLayout::Detailed, CardLayout::Compact] {
        cx.update(|_, cx| view.update(cx, |s, cx| s.set_card_layout(layout, cx)));
        cx.run_until_parked();
        for (tree, sess) in [(parent_row, s1), (child_row, s2)] {
            let card = x(cx, format!("worktree-card-{tree}"));
            let title = x(cx, format!("worktree-title-{tree}"));
            let glyph = x(cx, format!("session-state-{sess}"));
            let parent_state = x(cx, format!("worktree-state-{tree}"));
            let session_title = x(cx, format!("session-title-{sess}"));
            if layout == CardLayout::Detailed {
                let panel = cx
                    .debug_bounds(format!("card-agent-summary-{tree}").leak())
                    .unwrap();
                assert_eq!(glyph, panel.origin.x + gpui::px(7.));
                assert!(session_title > glyph && session_title < panel.top_right().x);
            } else {
                assert!(glyph >= parent_state + gpui::px(CHILD_INDENT));
                assert!(glyph <= parent_state + gpui::px(CHILD_INDENT + 4.));
                assert!(
                    glyph >= card + gpui::px(8.) && glyph < title,
                    "{layout:?}: session glyph={glyph:?} is nested one step before title={title:?} (card={card:?})"
                );
                assert!(
                    session_title >= title && session_title <= title + gpui::px(36.),
                    "{layout:?}: session name {session_title:?} must stay near card title {title:?}"
                );
            }
        }
        assert!(
            x(cx, format!("session-state-{s2}")) > x(cx, format!("session-state-{s1}")),
            "{layout:?}: lineage child sessions follow their deeper card"
        );
        let folder_title = x(cx, format!("folder-title-{folder}"));
        let glyph = x(cx, format!("session-state-{s3}"));
        assert!(
            glyph < folder_title,
            "{layout:?}: folder session glyph={glyph:?} must not repeat the title indent={folder_title:?}"
        );
    }
}

#[test]
fn orchestration_session_labels_preserve_provenance_across_title_changes() {
    use krill_proto::orchestration::{DispatchSessionOrigin, DispatchStatus, OrchestrationStore};

    let mut sidebar = Sidebar::new_loaded(None);
    let mut worker = session(21, AgentState::Working, Some("/repo"));
    worker.status.agent = "codex".into();
    worker.status.title = Some("조사 신고리스트".into());
    assert_eq!(sidebar.session_label(&worker).0, "조사 신고리스트");

    let store = &mut sidebar.disk.orchestration;
    let run = store.create_run("Title provenance", None).unwrap();
    store
        .start_worker(&run.id, None, "worker", "codex", Some(21), "/repo", "p")
        .unwrap();
    // Persisted dispatches also identify workers created before an app restart.
    sidebar.disk.orchestration =
        serde_json::from_slice(&serde_json::to_vec(&sidebar.disk.orchestration).unwrap()).unwrap();
    assert_eq!(
        sidebar.session_label(&worker).0,
        "[orchestration] 조사 신고리스트"
    );
    assert_eq!(
        sidebar
            .session_label_on_with_title(&HostId::Local, &worker, Some("⠂ 새 터미널 제목"))
            .0,
        "[orchestration] 새 터미널 제목"
    );
    sidebar
        .custom_titles
        .insert(worker.id, "직접 정한 제목".into());
    assert_eq!(
        sidebar.session_label(&worker).0,
        "[orchestration] 직접 정한 제목"
    );
    sidebar
        .custom_titles
        .insert(worker.id, "[orchestration] 기존 접두사".into());
    assert_eq!(
        sidebar.session_label(&worker).0,
        "[orchestration] 기존 접두사"
    );

    sidebar.disk.orchestration.dispatches[0].status = DispatchStatus::Succeeded;
    sidebar.custom_titles.clear();
    worker.status.title = None;
    sidebar.auto_titles.insert(worker.id, "첫 프롬프트".into());
    assert_eq!(
        sidebar.session_label(&worker).0,
        "[orchestration] 첫 프롬프트"
    );
    sidebar.auto_titles.clear();
    assert_eq!(sidebar.session_label(&worker).0, "[orchestration] codex");

    let remote = HostId::Remote("other-host".into());
    assert_eq!(sidebar.session_label_on(&remote, &worker).0, "codex");
    sidebar.disk.orchestration.dispatches[0].session_origin = DispatchSessionOrigin::Borrowed;
    assert_eq!(sidebar.session_label(&worker).0, "codex");
    sidebar.disk.orchestration = OrchestrationStore::default();
    assert_eq!(sidebar.session_label(&worker).0, "codex");
}

/// 칸이 열리지 않은 세션도 krilld가 보낸 터미널 제목으로 이름을 보이고, 칸 제목의 상태 글리프는 떼며,
/// 에이전트 기본 제목(`Claude Code`)은 첫 프롬프트 자동 제목에 양보한다.
#[gpui::test]
fn session_label_uses_daemon_title_and_strips_status_glyphs(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut bg = session(21, AgentState::Working, Some("/repo"));
            bg.status.title = Some("krill 프로젝트 진행 상황".into());
            assert_eq!(s.session_label(&bg).0, "krill 프로젝트 진행 상황");

            let open = session(22, AgentState::Working, Some("/repo"));
            s.set_title(open.id, "⠂ PWA QR 코드 스캔 UX", cx);
            assert_eq!(s.session_label(&open).0, "PWA QR 코드 스캔 UX");

            let mut generic = session(23, AgentState::Done, Some("/repo"));
            generic.status.title = Some("Claude Code".into());
            s.set_auto_title(generic.id, "fix-login-timeout", cx);
            assert_eq!(s.session_label(&generic).0, "fix-login-timeout");
            generic.status.title = Some("Windows PowerShell".into());
            assert_eq!(
                s.session_label(&generic).0,
                "Windows PowerShell",
                "a shell without an identified agent retains its title"
            );
            generic.status.agent = "claude".into();
            assert_eq!(s.session_label(&generic).0, "fix-login-timeout");
            s.auto_titles.remove(&generic.id);
            assert_eq!(
                s.session_label(&generic).0,
                "Windows PowerShell",
                "an agent without a known prompt must not invent a title"
            );
            s.set_auto_title(generic.id, "fix-login-timeout", cx);

            // The pane's cached generic OSC label must not hide a newer title
            // reported by the remote daemon, or borrow a local title by ID.
            let remote = HostId::Remote("windows".into());
            let mut remote_session = generic.clone();
            remote_session.status.title = Some("원격 대화 제목".into());
            assert_eq!(
                s.session_label_on_with_title(&remote, &remote_session, Some("✳ Claude Code"))
                    .0,
                "원격 대화 제목"
            );
            assert_eq!(
                s.session_label_on_with_title(&remote, &remote_session, Some("⠂ 원격 터미널 제목"))
                    .0,
                "원격 터미널 제목"
            );
            remote_session.status.title = None;
            assert_eq!(
                s.session_label_on_with_title(&remote, &remote_session, Some("✳ Claude Code"))
                    .0,
                "Claude Code"
            );
            assert_eq!(
                s.session_label_on_with_title(&remote, &remote_session, Some("Windows PowerShell"))
                    .0,
                "Windows PowerShell",
                "a remote ID never borrows the local prompt title"
            );
        });
    });
}

#[gpui::test]
fn project_filter_and_icons_include_remote_daemon_projects(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let selected = PathBuf::from("/Users/local/projects/selected");
    let hidden_local = PathBuf::from("/Users/local/projects/other");
    let remote_repo = PathBuf::from("/Users/remote/projects/gwiinsaju");
    let remote_home = PathBuf::from("/Users/remote");
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: vec![],
                trees: BTreeMap::from([
                    (selected.clone(), vec![selected.clone()]),
                    (hidden_local.clone(), vec![hidden_local.clone()]),
                ]),
            };
            s.project_visuals.insert(
                PathBuf::from("/Users/local/projects/gwiinsaju"),
                external::ProjectVisualMeta {
                    display_name: Some("귀인사주".into()),
                    badge_color: Some("#6366f1".into()),
                    icon: Some(crate::ui::icons::ProjectIcon::Emoji("🔮".into())),
                },
            );
            let mut loose = session(5, AgentState::Unknown, None);
            loose.cwd = Some(remote_home.clone());
            s.remote_hosts = vec![RemoteHost {
                id: "paired-mac".into(),
                label: "Mac-mini".into(),
                status: HostStatus::Connected,
                snapshot: Snapshot {
                    sessions: vec![loose],
                    trees: BTreeMap::from([
                        (remote_repo.clone(), vec![remote_repo.clone()]),
                        (hidden_local.clone(), vec![hidden_local.clone()]),
                    ]),
                },
                meta: Meta::new(),
                source: None,
                error: None,
            }];
            // 원격 호스트 저장소도 필터 가능한 프로젝트 목록에 포함되며, 동명 프로젝트는 하나로 합쳐진다.
            let filterable = s.filterable_project_repos();
            assert_eq!(
                filterable,
                vec![hidden_local.clone(), selected.clone(), remote_repo.clone(),]
            );
            // 원격 저장소 경로라도 프로젝트 키(basename) 매칭으로 아이콘·배지 색상·표시 이름을 찾는다.
            assert_eq!(
                s.project_icon(&remote_repo),
                Some(&crate::ui::icons::ProjectIcon::Emoji("🔮".into()))
            );
            assert_eq!(s.project_badge_color(&remote_repo), Some("#6366f1"));
            assert_eq!(s.project_header_name(&remote_repo), "귀인사주");

            // 프로젝트 필터 선택 시 로컬과 원격 모두 선택된 프로젝트만 표시한다.
            s.options.filters.repos.insert(remote_repo.clone());
            s.rebuild(cx);
            let local = s.local_rows();
            assert!(!local.contains(&Row::Repository(selected.clone())));
            assert!(!local.contains(&Row::Repository(hidden_local.clone())));
            let remote = s.remote_rows(&s.remote_hosts[0], &s.collapsed);
            assert!(remote.contains(&Row::Repository(remote_repo.clone())));
            assert!(remote.contains(&Row::Worktree(remote_repo.clone())));
            assert!(!remote.contains(&Row::Repository(hidden_local.clone())));
            assert!(remote.contains(&Row::Folder(remote_home.clone())));
            assert_eq!(s.remote_hosts[0].snapshot.sessions.len(), 1);
            // Host-level visibility remains an explicit independent control.
            s.options.filters.repos.clear();
            s.options
                .filters
                .hosts
                .insert(HostId::Local.as_key().to_owned());
            s.rebuild(cx);
            assert!(s.row_hosts.iter().all(HostId::is_local));
        });
    });
}

#[test]
fn ports_badge_text_folds_overflow_into_count() {
    use super::view::ports_badge_text;
    assert_eq!(ports_badge_text(&[3000, 8080], 3), ":3000 :8080");
    assert_eq!(ports_badge_text(&[1, 2, 3], 3), ":1 :2 :3");
    assert_eq!(ports_badge_text(&[1, 2, 3, 4, 5], 3), ":1 :2 :3 +2");
    assert_eq!(ports_badge_text(&[1, 2, 3, 4], usize::MAX), ":1 :2 :3 :4");
}

/// Orca `activeWorktreeId` 패리티: `selected_workspace`가 Cmd+T·새 에이전트용 (호스트, 경로)를 푼다.
/// 커서 행이 우선(동명 경로 구분)이고, 커서 밖 선택은 스냅샷 소유자로 찾는다.
/// 끊긴 원격·양쪽 동명·어디에도 없음은 None(실행 위치 메뉴 폴백)이다.
#[gpui::test]
fn selected_workspace_resolves_host_and_path(cx: &mut TestAppContext) {
    use std::collections::BTreeMap;

    cx.update(|cx| {
        gpui_component::init(cx);
    });
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let shared = PathBuf::from("/shared/repo/wt");
    let remote_only = PathBuf::from("/remote/repo/wt-1");
    let local_only = PathBuf::from("/local/repo/wt-2");
    let nowhere = PathBuf::from("/어디에도/없음");
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: Vec::new(),
                trees: BTreeMap::from([
                    (PathBuf::from("/shared"), vec![shared.clone()]),
                    (PathBuf::from("/local/repo"), vec![local_only.clone()]),
                ]),
            };
            s.set_remote_snapshot_for_test(
                "studio",
                HostStatus::Connected,
                Snapshot {
                    sessions: Vec::new(),
                    trees: BTreeMap::from([
                        (PathBuf::from("/shared"), vec![shared.clone()]),
                        (PathBuf::from("/remote/repo"), vec![remote_only.clone()]),
                    ]),
                },
                cx,
            );
        });
    });
    cx.run_until_parked();

    // 1. 커서 없는 복원 상태: 원격 전용 경로는 원격 호스트다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.cursor = None;
            s.select_worktree(Some(remote_only.clone()), cx);
            assert_eq!(
                s.selected_workspace(),
                Some((HostId::Remote("studio".into()), remote_only.clone()))
            );
        });
    });
    // 2. 로컬 전용 경로는 로컬이다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.select_worktree(Some(local_only.clone()), cx);
            assert_eq!(
                s.selected_workspace(),
                Some((HostId::Local, local_only.clone()))
            );
        });
    });
    // 3. 양쪽 동명 경로는 커서 없이 판단하지 않는다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.select_worktree(Some(shared.clone()), cx);
            assert_eq!(s.selected_workspace(), None);
        });
    });
    // 4. 커서가 원격 행을 가리키면 동명 경로도 원격이다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let studio = HostId::Remote("studio".into());
            let idx = s
                .rows
                .iter()
                .position(|r| matches!(r, Row::Worktree(p) if p == &shared))
                .and_then(|_| {
                    s.rows.iter().enumerate().position(|(i, r)| {
                        s.row_hosts.get(i) == Some(&studio)
                            && matches!(r, Row::Worktree(p) if p == &shared)
                    })
                })
                .expect("remote worktree row");
            s.cursor = Some(idx);
            s.select_worktree(Some(shared.clone()), cx);
            assert_eq!(s.selected_workspace(), Some((studio, shared.clone())));
        });
    });
    // 5. 원격이 끊기면 커서가 가리켜도 None이다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.set_remote_snapshot_for_test(
                "studio",
                HostStatus::Disconnected,
                Snapshot {
                    sessions: Vec::new(),
                    trees: BTreeMap::from([(
                        PathBuf::from("/remote/repo"),
                        vec![remote_only.clone()],
                    )]),
                },
                cx,
            );
            s.cursor = None;
            s.select_worktree(Some(remote_only.clone()), cx);
            assert_eq!(s.selected_workspace(), None);
        });
    });
    // 6. 어디에도 없는 경로는 None이다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.select_worktree(Some(nowhere.clone()), cx);
            assert_eq!(s.selected_workspace(), None);
        });
    });
}

#[gpui::test]
fn card_session_lists_running_subagents_below_it(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut lead = session(1, AgentState::Working, Some("/repo/a"));
            lead.status.subagents = vec![krill_proto::control::Subagent {
                kind: "general-purpose".into(),
                description: "각도1".into(),
                since_unix_ns: 1,
            }];
            s.snapshot = Snapshot {
                sessions: vec![lead.clone()],
                trees: BTreeMap::from([(PathBuf::from("/repo"), vec!["/repo/a".into()])]),
            };
            s.rows = vec![Row::Worktree("/repo/a".into()), Row::Session(lead, true)];
            s.expanded_cards.insert("/repo/a".into());
            s.sync_viewport();
            cx.notify();
        });
    });
    cx.run_until_parked();
    let row = cx.debug_bounds("sidebar-session-1").unwrap();
    let sub = cx.debug_bounds("session-subagent-1-0").unwrap();
    assert!(sub.origin.y >= row.bottom_left().y);
    assert!(sub.size.height < row.size.height);
}

#[gpui::test]
fn virtual_worktree_toolbar_appears_on_hover_without_layout_shift(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    view.update(cx, |s, cx| {
        s.snapshot = fixture();
        s.rebuild(cx);
    });
    cx.run_until_parked();
    let index = view.read_with(cx, |s, _| {
        s.rows
            .iter()
            .position(|r| matches!(r, Row::Worktree(_)))
            .unwrap()
    });
    let placeholder: &'static str =
        Box::leak(format!("worktree-new-session-{index}").into_boxed_str());
    let control: &'static str =
        Box::leak(format!("worktree-new-session-control-{index}").into_boxed_str());
    let before = cx.debug_bounds(placeholder).unwrap();
    assert!(cx.debug_bounds(control).is_none());
    cx.simulate_mouse_move(
        before.center(),
        None::<gpui::MouseButton>,
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    assert!(cx.debug_bounds(control).is_some());
    assert_eq!(cx.debug_bounds(placeholder).unwrap(), before);
    // 상태 폴링으로 행이 재생성되어도 같은 워크스페이스의 툴바는 유지한다.
    view.update(cx, Sidebar::rebuild);
    cx.run_until_parked();
    assert!(cx.debug_bounds(control).is_some());
}

fn init_sidebar_test_settings(cx: &mut TestAppContext, name: &str, settings: &str) {
    let dir = std::env::temp_dir().join(format!("krill-sidebar-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("settings.toml"), settings).unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::settings::init_at(Some(dir), cx);
    });
    cx.run_until_parked();
}

#[gpui::test]
fn terminal_focus_moves_the_single_workspace_selection(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    view.update(cx, |s, cx| {
        s.snapshot = Snapshot {
            sessions: vec![
                session(1, AgentState::Working, Some("/repo/a")),
                session(2, AgentState::Working, Some("/repo/b")),
            ],
            trees: BTreeMap::from([("/repo".into(), vec!["/repo/a".into(), "/repo/b".into()])]),
        };
        s.rebuild(cx);
        s.set_focused(Some(SessionId(1)), cx);
        s.set_focused(Some(SessionId(2)), cx);
        let active: Vec<_> = s
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r {
                Row::Worktree(p) if s.worktree_card_active(i, p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(active, vec![PathBuf::from("/repo/b")]);
    });
}

#[gpui::test]
fn status_summary_reuses_snapshot_until_sidebar_changes(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let sidebar = cx.new(|_| Sidebar::new(None));
    sidebar.update(cx, |sidebar, cx| {
        sidebar.apply(
            Ok((
                Snapshot {
                    sessions: vec![
                        session(1, AgentState::Waiting, Some("/repo")),
                        session(2, AgentState::Working, Some("/repo")),
                    ],
                    trees: BTreeMap::new(),
                },
                Meta::new(),
            )),
            BTreeMap::new(),
            cx,
        );
    });
    let first = sidebar.read_with(cx, |sidebar, _| sidebar.status_summary());
    let second = sidebar.read_with(cx, |sidebar, _| sidebar.status_summary());
    assert_eq!(first.0, 1);
    assert_eq!(*first.1, vec![AgentState::Waiting, AgentState::Working]);
    assert!(Arc::ptr_eq(&first.1, &second.1));
}

/// 기본 워크트리에 프로젝트 표시 이름·폴더 이름이 `prefs`(`projects.toml` 동기화분 포함)로 들어 있어도
/// 프로젝트 머리줄과 중복 표기하지 않고 브랜치 이름을 제목으로 쓰며, 사용자가 다른 이름으로 직접 바꾼 경우만 유지한다.
#[gpui::test]
fn default_worktree_ignores_redundant_project_name_in_prefs(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let repo = PathBuf::from("/Users/test/gwiinsaju");
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: Vec::new(),
                trees: BTreeMap::from([(repo.clone(), vec![repo.clone()])]),
            };
            s.meta.insert(
                repo.clone(),
                Tree {
                    branch: Some("main".into()),
                    dirty: None,
                },
            );
            s.prefs.insert(
                repo.clone(),
                Preference {
                    pinned: false,
                    name: "gwiinsaju".into(),
                },
            );
            s.rebuild(cx);
            assert_eq!(s.name(&repo), "main");
        });
        window.draw(cx).clear(cx);
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("worktree-branch-1").is_none(),
        "default worktree card title must resolve to 'main' (matching branch_subtitle) instead of repeating 'gwiinsaju'"
    );

    cx.update(|_, cx| {
        view.update(cx, |s, _| {
            s.prefs.entry(repo.clone()).or_default().name = "직접 정한 기본 워크트리".into();
            assert_eq!(s.name(&repo), "직접 정한 기본 워크트리");
        });
    });

    // `projects.toml`로 등록된 프로젝트(세션 없음)도 `.git/HEAD`에서 브랜치를 읽어 폴더명 대신 브랜치를 표기한다.
    let dir = std::env::temp_dir().join(format!("krill-imported-branch-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let imported_repo = dir.join("oh-my-vibecode");
    fs::create_dir_all(imported_repo.join(".git")).unwrap();
    fs::write(imported_repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let canon_repo = fs::canonicalize(&imported_repo).unwrap();
    fs::write(
        dir.join(external::PROJECTS_FILE),
        format!(
            "[[projects]]\nid = \"omv\"\npath = \"{}\"\ndisplay_name = \"oh-my-vibecode\"\nkind = \"git\"\n\n\
             [worktrees.\"{}\"]\nrepo_id = \"omv\"\n",
            canon_repo.display(),
            canon_repo.display()
        ),
    )
    .unwrap();
    let loaded = Sidebar::new_loaded(Some(dir.join("sidebar.bin")));
    assert_eq!(loaded.name(&canon_repo), "main");
    fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
fn dashboard_read_order_persists_and_resets_on_recompletion(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mk-dashboard-read-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let sidebar_bin = dir.join("sidebar.bin");

    let mut s1 = session(1, AgentState::Done, Some("/repo/a"));
    s1.status.since_unix_ns = 100;
    let mut s2 = session(2, AgentState::Done, Some("/repo/b"));
    s2.status.since_unix_ns = 200;

    let sidebar = cx.new(|_| Sidebar::new_loaded(Some(sidebar_bin.clone())));
    sidebar.update(cx, |s, cx| {
        s.apply(
            Ok((
                Snapshot {
                    sessions: vec![s1.clone(), s2.clone()],
                    trees: BTreeMap::new(),
                },
                Meta::new(),
            )),
            BTreeMap::new(),
            cx,
        );
        assert!(s.is_dashboard_unread(&HostId::Local, &s1));
        assert!(s.is_dashboard_unread(&HostId::Local, &s2));

        assert!(s.mark_session_read(&HostId::Local, SessionId(2), cx));
        assert!(s.mark_session_read(&HostId::Local, SessionId(1), cx));
        assert_eq!(s.dashboard_read_order(&HostId::Local, &s2), Some(1));
        assert_eq!(s.dashboard_read_order(&HostId::Local, &s1), Some(2));
    });
    cx.run_until_parked();

    // 재시작 후에도 dashboard-read.json에서 읽음 순서가 복원된다.
    let reloaded = cx.new(|_| Sidebar::new_loaded(Some(sidebar_bin.clone())));
    reloaded.update(cx, |s, cx| {
        s.apply(
            Ok((
                Snapshot {
                    sessions: vec![s1.clone(), s2.clone()],
                    trees: BTreeMap::new(),
                },
                Meta::new(),
            )),
            BTreeMap::new(),
            cx,
        );
        assert_eq!(s.dashboard_read_order(&HostId::Local, &s2), Some(1));
        assert_eq!(s.dashboard_read_order(&HostId::Local, &s1), Some(2));

        // s2가 다시 Working 후 새 시각으로 Done이 되면 읽음 기록이 해제되어 다시 미확인(None)이 된다.
        let mut s2_working = s2.clone();
        s2_working.status.state = AgentState::Working;
        s2_working.status.since_unix_ns = 300;
        s.apply(
            Ok((
                Snapshot {
                    sessions: vec![s1.clone(), s2_working],
                    trees: BTreeMap::new(),
                },
                Meta::new(),
            )),
            BTreeMap::new(),
            cx,
        );
        let mut s2_done_again = s2.clone();
        s2_done_again.status.since_unix_ns = 400;
        s.apply(
            Ok((
                Snapshot {
                    sessions: vec![s1.clone(), s2_done_again.clone()],
                    trees: BTreeMap::new(),
                },
                Meta::new(),
            )),
            BTreeMap::new(),
            cx,
        );
        assert_eq!(s.dashboard_read_order(&HostId::Local, &s2_done_again), None);
        assert!(s.is_dashboard_unread(&HostId::Local, &s2_done_again));
        assert_eq!(s.dashboard_read_order(&HostId::Local, &s1), Some(2));
    });
    fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
fn worktree_delete_enters_cancel_focus_and_restores_background_on_escape(cx: &mut TestAppContext) {
    struct DeleteProbe(std::sync::atomic::AtomicUsize);
    impl Source for DeleteProbe {
        fn fetch(&self) -> io::Result<Snapshot> {
            Ok(Snapshot::default())
        }
        fn remove_worktree(&self, _: krill_proto::control::RemoveWorktree) -> io::Result<Reply> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(io::ErrorKind::Unsupported.into())
        }
    }
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let source = Arc::new(DeleteProbe(std::sync::atomic::AtomicUsize::new(0)));
    let selected = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = selected.clone();
    let previous = cx.update(|window, cx| {
        cx.subscribe(&view, move |_, _: &Selected, _| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
        .detach();
        view.update(cx, |s, cx| {
            s.connect_source(source.clone(), cx);
            let focus = cx.focus_handle();
            s.focus = Some(focus.clone());
            focus.focus(window, cx);
            s.request_delete_worktree(PathBuf::from("/repo/ui-delete-probe"), false, cx);
            focus
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        assert!(
            view.read(cx)
                .delete_cancel_focus
                .as_ref()
                .unwrap()
                .is_focused(window)
        );
    });
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.update(|window, cx| {
        assert!(view.read(cx).delete_confirm.is_none());
        assert!(previous.is_focused(window));
        view.update(cx, |s, cx| {
            s.request_delete_worktree(PathBuf::from("/repo/ui-delete-probe"), false, cx);
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("tab escape");
    cx.run_until_parked();
    cx.update(|window, cx| {
        assert!(view.read(cx).delete_confirm.is_none());
        assert!(previous.is_focused(window));
    });
    assert_eq!(source.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(selected.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[gpui::test]
fn worktree_creation_and_deferred_session_focus_keep_sidebar_selection_synced(
    cx: &mut TestAppContext,
) {
    struct CreateProbe;
    impl Source for CreateProbe {
        fn fetch(&self) -> io::Result<Snapshot> {
            Ok(Snapshot {
                sessions: vec![session(1, AgentState::Working, Some("/repo/ui-개선"))],
                trees: BTreeMap::from([(
                    PathBuf::from("/repo"),
                    vec![
                        PathBuf::from("/repo"),
                        PathBuf::from("/repo/ui-개선"),
                        PathBuf::from("/repo/orca-비교"),
                    ],
                )]),
            })
        }
        fn new_worktree(
            &self,
            _: krill_proto::control::NewWorktree,
        ) -> io::Result<krill_proto::control::Reply> {
            Ok(krill_proto::control::Reply::WorktreeCreated(
                krill_proto::control::Worktree {
                    path: "/repo/orca-비교".as_bytes().to_vec(),
                    branch: "orca-비교".into(),
                    base: "main".into(),
                    copied: Vec::new(),
                    warnings: Vec::new(),
                    setup: None,
                },
            ))
        }
    }

    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let source: Arc<dyn Source> = Arc::new(CreateProbe);
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.connect_source(source, cx);
            s.options.filters.hide_sleeping = true;
            s.options.filters.always_show_default_branch = true;
            s.apply(
                Ok((
                    Snapshot {
                        sessions: vec![session(1, AgentState::Working, Some("/repo/ui-개선"))],
                        trees: BTreeMap::from([(
                            PathBuf::from("/repo"),
                            vec![PathBuf::from("/repo"), PathBuf::from("/repo/ui-개선")],
                        )]),
                    },
                    BTreeMap::from([
                        (
                            PathBuf::from("/repo"),
                            Tree {
                                branch: Some("main".into()),
                                dirty: None,
                            },
                        ),
                        (
                            PathBuf::from("/repo/ui-개선"),
                            Tree {
                                branch: Some("ui-개선".into()),
                                dirty: None,
                            },
                        ),
                    ]),
                )),
                BTreeMap::new(),
                cx,
            );
            s.set_focused(Some(SessionId(1)), cx);
            s.new_worktree("/repo".into(), window, cx);
        });
    });
    cx.run_until_parked();

    // 새 워크트리 생성 제출 및 완료(아직 새 PTY 세션 SessionId(2)가 스냅샷에 들어오기 전)
    cx.simulate_input("orca-비교");
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    cx.run_until_parked();

    // 1) 스냅샷이 새 세션을 알기 전에 셸이 먼저 set_focused(Some(SessionId(2)))를 호출해도
    //    이전 워크트리(ui-개선)가 아니라 새로 만든 워크트리(orca-비교)가 숨김 필터를 뚫고 유일한 활성 카드가 된다.
    view.update(cx, |s, cx| {
        s.set_focused(Some(SessionId(2)), cx);
        let active: Vec<PathBuf> = s
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r {
                Row::Worktree(p) if s.worktree_card_active(i, p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(active, vec![PathBuf::from("/repo/orca-비교")]);
        assert_eq!(s.focused_synced, None);
    });

    // 2) 뒤늦게 폴링 스냅샷에 로그인 셸 세션(SessionId(2), AgentState::Unknown)이 도착하면
    //    지연된 포커스가 즉시 동기화되어 커서와 세션 하이라이트가 orca-비교로 맞춰진다.
    let meta = BTreeMap::from([
        (
            PathBuf::from("/repo"),
            Tree {
                branch: Some("main".into()),
                dirty: None,
            },
        ),
        (
            PathBuf::from("/repo/ui-개선"),
            Tree {
                branch: Some("ui-개선".into()),
                dirty: None,
            },
        ),
        (
            PathBuf::from("/repo/orca-비교"),
            Tree {
                branch: Some("orca-비교".into()),
                dirty: None,
            },
        ),
    ]);
    view.update(cx, |s, cx| {
        s.apply(
            Ok((
                Snapshot {
                    sessions: vec![
                        session(1, AgentState::Working, Some("/repo/ui-개선")),
                        session(2, AgentState::Unknown, Some("/repo/orca-비교")),
                    ],
                    trees: BTreeMap::from([(
                        PathBuf::from("/repo"),
                        vec![
                            PathBuf::from("/repo"),
                            PathBuf::from("/repo/ui-개선"),
                            PathBuf::from("/repo/orca-비교"),
                        ],
                    )]),
                },
                meta.clone(),
            )),
            BTreeMap::new(),
            cx,
        );
        assert_eq!(s.focused_synced, Some(SessionId(2)));
        let wt_idx = s
            .rows
            .iter()
            .position(|r| matches!(r, Row::Worktree(p) if p == Path::new("/repo/orca-비교")))
            .expect("new worktree row must stay visible under hide_sleeping");
        assert_eq!(s.cursor, Some(wt_idx));
        assert!(s.is_session_focused(wt_idx, SessionId(2)));
    });

    // 3) 같은 워크트리에 두 번째 세션(SessionId(3))이 추가되고 포커스가 넘어가면
    //    2+ 세션 카드가 자동으로 펼쳐지며 활성 세션 행(Row::Session)으로 커서와 강조가 이동한다.
    view.update(cx, |s, cx| {
        s.apply(
            Ok((
                Snapshot {
                    sessions: vec![
                        session(1, AgentState::Working, Some("/repo/ui-개선")),
                        session(2, AgentState::Unknown, Some("/repo/orca-비교")),
                        session(3, AgentState::Unknown, Some("/repo/orca-비교")),
                    ],
                    trees: BTreeMap::from([(
                        PathBuf::from("/repo"),
                        vec![
                            PathBuf::from("/repo"),
                            PathBuf::from("/repo/ui-개선"),
                            PathBuf::from("/repo/orca-비교"),
                        ],
                    )]),
                },
                meta,
            )),
            BTreeMap::new(),
            cx,
        );
        s.set_focused(Some(SessionId(3)), cx);
        assert!(s.expanded_cards.contains(Path::new("/repo/orca-비교")));
        let s3_idx = s
            .rows
            .iter()
            .position(|r| matches!(r, Row::Session(sess, true) if sess.id == SessionId(3)))
            .expect("expanded card must expose SessionId(3) row");
        let s2_idx = s
            .rows
            .iter()
            .position(|r| matches!(r, Row::Session(sess, true) if sess.id == SessionId(2)))
            .expect("expanded card must expose SessionId(2) row");
        assert_eq!(s.cursor, Some(s3_idx));
        assert!(s.is_session_focused(s3_idx, SessionId(3)));
        assert!(!s.is_session_focused(s2_idx, SessionId(2)));

        // 같은 워크트리 내 다른 칸(SessionId(2))으로 포커스를 바꾸면 세션 행 선택도 즉시 따라간다.
        s.set_focused(Some(SessionId(2)), cx);
        assert_eq!(s.cursor, Some(s2_idx));
        assert!(s.is_session_focused(s2_idx, SessionId(2)));
        assert!(!s.is_session_focused(s3_idx, SessionId(3)));
    });
}

#[gpui::test]
fn remote_host_worktree_creation_and_child_lineage(cx: &mut TestAppContext) {
    use krill_proto::control::{RemoveWorktree, RemovedWorktree, Worktree};
    use std::sync::Mutex;

    struct RemoteSource {
        created: Arc<Mutex<Vec<NewWorktree>>>,
        removed: Arc<Mutex<Vec<RemoveWorktree>>>,
    }
    impl Source for RemoteSource {
        fn fetch(&self) -> io::Result<Snapshot> {
            let mut list = vec![
                PathBuf::from("/remote/repo"),
                PathBuf::from("/remote/repo/feat-parent"),
            ];
            for req in self.created.lock().expect("lock").iter() {
                list.push(PathBuf::from(format!("/remote/repo/{}", req.name)));
            }
            let removed: Vec<PathBuf> = self
                .removed
                .lock()
                .expect("lock")
                .iter()
                .map(|r| PathBuf::from(String::from_utf8_lossy(&r.path).into_owned()))
                .collect();
            list.retain(|p| !removed.contains(p));
            Ok(Snapshot {
                sessions: vec![],
                trees: BTreeMap::from([(PathBuf::from("/remote/repo"), list)]),
            })
        }
        fn new_worktree(&self, req: NewWorktree) -> io::Result<Reply> {
            let repo_str = String::from_utf8_lossy(&req.repo);
            let path = format!("{repo_str}/{}", req.name).into_bytes();
            let branch = req.name.clone();
            let base = req.base.clone().unwrap_or_else(|| "main".into());
            self.created.lock().expect("lock").push(req);
            Ok(Reply::WorktreeCreated(Worktree {
                path,
                branch,
                base,
                copied: Vec::new(),
                warnings: Vec::new(),
                setup: None,
            }))
        }
        fn remove_worktree(&self, req: RemoveWorktree) -> io::Result<Reply> {
            self.removed.lock().expect("lock").push(req);
            Ok(Reply::WorktreeRemoved(RemovedWorktree {
                branch: Some("child-wt".into()),
                branch_deleted: true,
                warnings: Vec::new(),
            }))
        }
    }

    cx.update(gpui_component::init);
    let created = Arc::new(Mutex::new(Vec::new()));
    let removed = Arc::new(Mutex::new(Vec::new()));
    let remote_src: Arc<dyn Source> = Arc::new(RemoteSource {
        created: Arc::clone(&created),
        removed: Arc::clone(&removed),
    });
    let remote_host = HostId::Remote("mac-mini".into());
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    vcx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.remote_hosts = vec![RemoteHost {
                id: "mac-mini".into(),
                label: "Mac mini".into(),
                status: HostStatus::Connected,
                snapshot: Snapshot {
                    sessions: vec![],
                    trees: BTreeMap::from([(
                        PathBuf::from("/remote/repo"),
                        vec![
                            PathBuf::from("/remote/repo"),
                            PathBuf::from("/remote/repo/feat-parent"),
                        ],
                    )]),
                },
                meta: BTreeMap::from([
                    (
                        PathBuf::from("/remote/repo"),
                        Tree {
                            branch: Some("main".into()),
                            dirty: Some(0),
                        },
                    ),
                    (
                        PathBuf::from("/remote/repo/feat-parent"),
                        Tree {
                            branch: Some("feat-parent".into()),
                            dirty: Some(0),
                        },
                    ),
                ]),
                source: Some(remote_src),
                error: None,
            }];
            s.rebuild(cx);
        });
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();

    // 1. 연결된 원격 저장소 헤더에 마우스를 올리면 새 워크트리(+) 버튼이 노출되고,
    //    사이드바 상단에도 새 워크트리 드롭다운 버튼이 노출된다.
    let repo_idx = vcx.update(|_, cx| {
        view.read(cx)
            .rows
            .iter()
            .position(|r| *r == Row::Repository(PathBuf::from("/remote/repo")))
            .expect("remote repository row")
    });
    let repo_hdr = vcx
        .debug_bounds(format!("repo-header-{repo_idx}").leak())
        .expect("remote repository header bounds");
    vcx.simulate_mouse_move(repo_hdr.center(), None, gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        vcx.debug_bounds(format!("repo-new-worktree-{repo_idx}").leak())
            .is_some(),
        "connected remote repository header should expose the new-worktree button"
    );
    assert!(
        vcx.debug_bounds("sidebar-new-worktree").is_some(),
        "sidebar header should expose the new-worktree dropdown when a remote repo is connected"
    );

    // 2. 원격 워크트리에서 자식 워크트리 만들기를 실행하면 원격 소스로 생성되고 부모 계보가 연결된다.
    let remote_host_for_child = remote_host.clone();
    vcx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.new_child_worktree_on(
                &remote_host_for_child,
                PathBuf::from("/remote/repo/feat-parent"),
                window,
                cx,
            );
        });
    });
    vcx.run_until_parked();

    vcx.simulate_input("child-wt");
    vcx.simulate_keystrokes("cmd-enter");
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(10));
        vcx.run_until_parked();
        let done = vcx.update(|_, cx| view.read(cx).creating.is_none());
        if done {
            break;
        }
    }

    vcx.update(|_, cx| {
        let s = view.read(cx);
        let remote = s
            .remote_hosts
            .iter()
            .find(|h| h.id == "mac-mini")
            .expect("remote entry");
        let list = remote
            .snapshot
            .trees
            .get(Path::new("/remote/repo"))
            .expect("remote repo trees");
        assert!(
            list.contains(&PathBuf::from("/remote/repo/child-wt")),
            "created remote worktree should be optimistically added to remote snapshot"
        );
        assert_eq!(
            s.worktree_parent(Path::new("/remote/repo/child-wt")),
            Some(PathBuf::from("/remote/repo/feat-parent")),
            "remote child worktree should record its parent lineage"
        );
        assert_eq!(
            s.eligible_parent_worktrees_on(&remote_host, Path::new("/remote/repo/feat-parent")),
            vec![PathBuf::from("/remote/repo")],
            "remote parent candidates should exclude self and descendants within the remote repo"
        );
    });

    let sent = created.lock().expect("lock").clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].repo, b"/remote/repo".to_vec());
    assert_eq!(sent[0].name, "child-wt");
    assert_eq!(sent[0].base.as_deref(), Some("feat-parent"));

    // 3. 원격 워크트리 삭제 확인 및 실행 시 원격 소스로 RemoveWorktree가 전달되고 스냅샷에서 제거된다.
    let remote_host_for_del = remote_host.clone();
    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.request_delete_worktree_on(
                remote_host_for_del.clone(),
                PathBuf::from("/remote/repo/child-wt"),
                false,
                cx,
            );
            assert_eq!(
                s.delete_confirm.as_ref().map(|c| &c.host),
                Some(&remote_host_for_del)
            );
            s.confirm_delete(cx);
        });
    });
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(10));
        vcx.run_until_parked();
        if !removed.lock().expect("lock").is_empty() {
            break;
        }
    }
    vcx.run_until_parked();
    let del_sent = removed.lock().expect("lock").clone();
    assert_eq!(del_sent.len(), 1);
    assert_eq!(del_sent[0].repo, b"/remote/repo".to_vec());
    assert_eq!(del_sent[0].path, b"/remote/repo/child-wt".to_vec());
}

#[gpui::test]
fn missing_daemon_sessions_leave_sidebar_and_local_caches(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let sidebar = cx.new(|_| Sidebar::new_loaded(None));
    sidebar.update(cx, |sidebar, cx| {
        let mut done = session(203, AgentState::Done, None);
        done.cwd = Some("/tmp/probe".into());
        done.status.agent = "claude".into();
        sidebar.apply(
            Ok((
                Snapshot {
                    sessions: vec![done.clone()],
                    ..Snapshot::default()
                },
                Meta::new(),
            )),
            BTreeMap::new(),
            cx,
        );
        assert!(
            sidebar
                .rows
                .iter()
                .any(|row| matches!(row, Row::Session(s, _) if s.id == done.id))
        );
        sidebar.titles.insert(done.id, "screen title".into());
        sidebar.auto_titles.insert(done.id, "prompt title".into());
        sidebar.unread.insert(done.id);
        sidebar.set_remote_snapshot_for_test(
            "remote",
            HostStatus::Connected,
            Snapshot {
                sessions: vec![done.clone()],
                ..Snapshot::default()
            },
            cx,
        );
        sidebar.apply(Ok((Snapshot::default(), Meta::new())), BTreeMap::new(), cx);
        assert!(sidebar.snapshot.sessions.is_empty());
        assert!(!sidebar.titles.contains_key(&done.id));
        assert!(!sidebar.auto_titles.contains_key(&done.id));
        assert!(!sidebar.unread.contains(&done.id));
        assert_eq!(
            sidebar.dashboard_sessions().len(),
            1,
            "the other host matching ID must survive"
        );
        assert!(
            sidebar
                .rows
                .iter()
                .any(|row| matches!(row, Row::Session(s, _) if s.id == done.id))
        );
    });
}

#[gpui::test]
fn exited_processes_leave_sidebar_dashboard_counts_and_unread(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    view.update(cx, |sidebar, cx| {
        let mut ended = session(1, AgentState::Exited, Some("/repo/a"));
        ended.status.agent = "codex".into();
        ended.status.source = Some(krill_proto::control::Source::Process);
        let mut hook_ended = session(2, AgentState::Exited, Some("/repo/a"));
        hook_ended.status.agent = "claude".into();
        hook_ended.status.source = Some(krill_proto::control::Source::Hook);
        let mut loose = ended.clone();
        loose.id = SessionId(3);
        loose.worktree = None;
        loose.cwd = None;
        let mut folder = ended.clone();
        folder.id = SessionId(4);
        folder.worktree = None;
        folder.cwd = Some("/folder".into());
        let done = session(5, AgentState::Done, Some("/repo/a"));
        sidebar.unread = BTreeSet::from([SessionId(1), SessionId(2)]);
        sidebar.apply(
            Ok((
                Snapshot {
                    sessions: vec![ended, hook_ended, loose, folder, done],
                    trees: BTreeMap::from([("/repo".into(), vec!["/repo/a".into()])]),
                },
                Meta::new(),
            )),
            BTreeMap::new(),
            cx,
        );
        let mut row_ids: Vec<_> = sidebar
            .rows
            .iter()
            .filter_map(|row| {
                if let Row::Session(session, _) = row {
                    Some(session.id)
                } else {
                    None
                }
            })
            .collect();
        row_ids.sort();
        assert_eq!(row_ids, vec![SessionId(2), SessionId(5)]);
        assert_eq!(sidebar.agent_counts(), (0, 0, 1));
        assert!(sidebar.unread.is_empty());
        assert_eq!(
            sidebar
                .dashboard_sessions()
                .iter()
                .map(|(_, session)| session.id)
                .collect::<Vec<_>>(),
            vec![SessionId(5)]
        );
        // Matching IDs on other hosts must be treated independently.
        let mut remote_done = session(1, AgentState::Done, None);
        remote_done.status.agent = "codex".into();
        let mut remote_waiting = session(2, AgentState::Waiting, None);
        remote_waiting.status.agent = "claude".into();
        sidebar.set_remote_snapshot_for_test(
            "remote",
            HostStatus::Connected,
            Snapshot {
                sessions: vec![remote_done, remote_waiting],
                ..Snapshot::default()
            },
            cx,
        );
        assert_eq!(sidebar.dashboard_sessions().len(), 3);
        assert_eq!(sidebar.agent_counts(), (1, 0, 2));
    });
}

#[gpui::test]
fn windows_identity_merges_imported_repositories_sessions_and_preferences(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let sidebar = cx.new(|_| Sidebar::new(None));
    sidebar.update(cx, |s, cx| {
        let repo = PathBuf::from(r"C:\Users\Owen\프로젝트");
        let tree = PathBuf::from(r"C:\Workspaces\간단작업");
        s.snapshot = Snapshot {
            trees: BTreeMap::from([
                (repo.clone(), vec![repo.clone(), tree.clone()]),
                (
                    PathBuf::from(r"\\?\c:\users\owen\프로젝트"),
                    vec![PathBuf::from("c:/workspaces/간단작업/")],
                ),
            ]),
            sessions: vec![
                Session {
                    cwd: Some(PathBuf::from(r"\\?\C:\WORKSPACES\간단작업")),
                    worktree: None,
                    ..session(1, AgentState::Working, None)
                },
                Session {
                    cwd: Some(PathBuf::from("c:/workspaces/간단작업/sub")),
                    worktree: None,
                    ..session(2, AgentState::Unknown, None)
                },
            ],
        };
        s.prefs.insert(
            tree.clone(),
            Preference {
                pinned: true,
                name: String::new(),
            },
        );
        let alias = PathBuf::from(r"\\?\C:\WORKSPACES\간단작업");
        s.comments.insert(tree.clone(), String::new());
        s.comments.insert(alias.clone(), "Review me".into());
        s.meta.insert(
            tree.clone(),
            Tree {
                branch: None,
                dirty: Some(0),
            },
        );
        s.meta.insert(
            alias.clone(),
            Tree {
                branch: Some("topic".into()),
                dirty: Some(3),
            },
        );
        s.worktree_updated_at.insert(tree.clone(), 10);
        s.worktree_updated_at.insert(alias.clone(), 20);
        s.prefs.insert(
            PathBuf::from(r"\\?\C:\WORKSPACES\간단작업"),
            Preference {
                name: "간단작업".into(),
                pinned: false,
            },
        );
        s.selected_worktree = Some(PathBuf::from("c:/workspaces/간단작업"));
        s.rebuild(cx);
        assert_eq!(s.snapshot.trees.len(), 1);
        assert_eq!(s.snapshot.trees[&repo], [repo.clone(), tree.clone()]);
        assert!(
            s.snapshot
                .sessions
                .iter()
                .all(|session| session.worktree.as_ref() == Some(&tree))
        );
        assert!(!s.rows.iter().any(|row| matches!(row, Row::Folder(_))));
        assert_eq!(s.selected_worktree, Some(tree.clone()));
        assert_eq!(s.prefs[&tree].name, "간단작업");
        assert!(s.prefs[&tree].pinned);
        assert_eq!(s.comments[&tree], "Review me");
        assert_eq!(s.meta[&tree].branch.as_deref(), Some("topic"));
        assert_eq!(s.meta[&tree].dirty, Some(3));
        assert_eq!(s.worktree_updated_at[&tree], 20);
        assert_eq!(
            s.worktree_for_path(Path::new(r"\\?\c:\workspaces\간단작업\sub")),
            Some(tree.clone())
        );
        assert_eq!(
            s.repo_for_worktree(Path::new("C:/WORKSPACES/간단작업")),
            Some(repo)
        );
        assert_eq!(
            s.session_for_workspace(&HostId::Local, Path::new("C:/WORKSPACES/간단작업")),
            Some(SessionId(1))
        );
        let before = s.snapshot.clone();
        s.rebuild(cx);
        assert_eq!(before, s.snapshot);
        s.prefs = BTreeMap::from([
            (
                tree.clone(),
                Preference {
                    pinned: false,
                    name: "간단작업".into(),
                },
            ),
            (
                alias,
                Preference {
                    pinned: true,
                    name: String::new(),
                },
            ),
        ]);
        s.rebuild(cx);
        assert!(s.prefs[&tree].pinned);
        assert_eq!(s.prefs[&tree].name, "간단작업");
    });
}

#[gpui::test]
fn windows_slash_unc_snapshot_merges_aliases_without_drive_paths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let sidebar = cx.new(|_| Sidebar::new(None));
    sidebar.update(cx, |s, cx| {
        s.snapshot = Snapshot {
            trees: BTreeMap::from([
                (
                    "//SERVER/Share".into(),
                    vec!["//SERVER/Share/간단작업".into()],
                ),
                (
                    "//server/share".into(),
                    vec!["//server/share/간단작업".into()],
                ),
            ]),
            sessions: vec![Session {
                cwd: Some("//server/share/간단작업/sub".into()),
                worktree: None,
                ..session(1, AgentState::Working, None)
            }],
        };
        s.rebuild(cx);
        assert_eq!(s.snapshot.trees.len(), 1);
        assert_eq!(s.snapshot.trees.values().next().unwrap().len(), 1);
        assert!(krill_proto::path::equivalent(
            s.snapshot.sessions[0].worktree.as_deref().unwrap(),
            Path::new("//server/share/간단작업"),
        ));
    });
}

#[gpui::test]
fn dragging_worktree_cards_changes_manual_order_and_persists_without_reordering_projects(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let dir = std::env::temp_dir().join(format!("krill-card-drag-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sidebar.bin");
    let (view, cx) = cx.add_window_view({
        let file = file.clone();
        move |_, _| Sidebar::new_loaded(Some(file))
    });
    view.update(cx, |s, cx| {
        s.snapshot = Snapshot {
            sessions: vec![],
            trees: BTreeMap::from([
                (
                    "/repo".into(),
                    vec!["/repo".into(), "/a".into(), "/b".into()],
                ),
                ("/other".into(), vec!["/other".into()]),
            ]),
        };
        s.options.sort_by = SortBy::Smart;
        s.project_order = vec!["/repo".into(), "/other".into()];
        s.rebuild(cx);
    });
    cx.run_until_parked();
    let bounds = |path: &str, cx: &mut gpui::VisualTestContext| {
        let index = view.read_with(cx, |s, _| {
            s.rows
                .iter()
                .position(|r| matches!(r, Row::Worktree(p) if p == Path::new(path)))
                .unwrap()
        });
        let selector: &'static str = Box::leak(format!("worktree-card-{index}").into_boxed_str());
        cx.debug_bounds(selector).unwrap()
    };
    // Exercise actual detailed and compact card drag handlers.
    for layout in [CardLayout::Detailed, CardLayout::Compact] {
        view.update(cx, |s, cx| {
            s.options.card_layout = layout;
            s.worktree_order.clear();
            s.set_sort_by(SortBy::Smart, cx);
        });
        cx.run_until_parked();
        let from = bounds("/b", cx).center();
        let to = bounds("/a", cx).center();
        cx.simulate_mouse_down(from, gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(
            to,
            Some(gpui::MouseButton::Left),
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        cx.simulate_mouse_up(to, gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        view.update(cx, |s, cx| {
            let cards = |s: &Sidebar| {
                s.rows
                    .iter()
                    .filter_map(|r| match r {
                        Row::Worktree(p) => Some(p.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(s.options.sort_by, SortBy::Created);
            assert_eq!(cards(s), ["/b", "/a", "/repo", "/other"].map(PathBuf::from));
            assert_eq!(s.project_order, ["/repo", "/other"].map(PathBuf::from));
            s.move_worktree(Path::new("/b"), Path::new("/repo"), cx);
            assert_eq!(cards(s), ["/a", "/repo", "/b", "/other"].map(PathBuf::from));
            // A drop into a different repository must not move or regroup cards.
            let order = s.worktree_order.clone();
            s.move_worktree(Path::new("/b"), Path::new("/other"), cx);
            assert_eq!(s.worktree_order, order);
            s.set_sort_by(SortBy::Name, cx);
            assert_eq!(cards(s), ["/a", "/b", "/repo", "/other"].map(PathBuf::from));
        });
    }
    cx.run_until_parked();
    let restored = Sidebar::new_loaded(Some(file.clone()));
    assert_eq!(
        restored.worktree_order,
        ["/a", "/repo", "/b"].map(PathBuf::from)
    );
    assert_eq!(restored.options.sort_by, SortBy::Name);
    // Existing five-segment files still load with an empty manual card order.
    let mut legacy = vec![FORMAT_VERSION];
    for segment in [
        postcard::to_stdvec(&restored.prefs).unwrap(),
        postcard::to_stdvec(&restored.options).unwrap(),
        postcard::to_stdvec(&lineage::LineageState::default()).unwrap(),
        postcard::to_stdvec(&UiExpansionState::default()).unwrap(),
        postcard::to_stdvec(&restored.project_order).unwrap(),
    ] {
        legacy.extend(segment);
    }
    assert!(decode_state(&legacy).unwrap().5.is_empty());
    fs::remove_dir_all(&dir).ok();
}

/// 백그라운드 폴링 재시도는 조용히 복구하고 권한·프로토콜 오류는 조치를 요청한다.
#[gpui::test]
fn transient_poll_errors_keep_diagnostics_without_popup(cx: &mut TestAppContext) {
    let sidebar = cx.new(|_| Sidebar::new_loaded(None));
    sidebar.update(cx, |s, cx| {
        for kind in [
            io::ErrorKind::TimedOut,
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::ConnectionReset,
        ] {
            s.apply(Err(kind.into()), BTreeMap::new(), cx);
            assert!(s.poll_error().is_some());
            assert_eq!(s.poll_error_notice(), None);
            assert_eq!(s.local_status, HostStatus::Disconnected);
        }
        s.apply(
            Err(io::ErrorKind::PermissionDenied.into()),
            BTreeMap::new(),
            cx,
        );
        assert!(s.poll_error_notice().is_some());
        s.apply(Ok((Snapshot::default(), Meta::new())), BTreeMap::new(), cx);
        assert_eq!(s.poll_error(), None);
        assert_eq!(s.poll_error_notice(), None);
        assert_eq!(s.local_status, HostStatus::Connected);
    });
}

#[test]
fn jump_connected_snapshots_keep_host_identity_labels_paths_and_local_preferences_separate() {
    let mut sidebar = Sidebar::new_loaded(None);
    let mut local = session(7, AgentState::Working, Some("/srv/repo/feature"));
    local.cwd = Some("/srv/repo/feature/local-subdir".into());
    sidebar.snapshot = Snapshot {
        sessions: vec![local],
        trees: [("/srv/repo".into(), vec!["/srv/repo/feature".into()])].into(),
    };
    sidebar
        .custom_titles
        .insert(SessionId(7), "Local alias".into());
    sidebar.focused = Some(SessionId(7));
    sidebar.unread.insert(SessionId(7));
    let mut remote_session = session(7, AgentState::Waiting, Some("/srv/repo/feature"));
    remote_session.cwd = Some("/srv/repo/feature/zzremotezz".into());
    remote_session.status.title = Some("Remote task".into());
    for (id, status) in [
        ("studio", HostStatus::Connected),
        ("offline", HostStatus::Disconnected),
    ] {
        sidebar.remote_hosts.push(RemoteHost {
            id: id.into(),
            label: format!("{id} PC"),
            status,
            snapshot: Snapshot {
                sessions: vec![remote_session.clone()],
                trees: [("/srv/repo".into(), vec!["/srv/repo/feature".into()])].into(),
            },
            meta: Meta::new(),
            source: None,
            error: None,
        });
    }
    // 접힘·사이드바 필터로 보이지 않는 세션도 Jump의 수신 스냅샷에는 남아 있다.
    sidebar
        .collapsed_hosts
        .insert(HostId::Remote("studio".into()));
    let items = sidebar.jump_items();
    assert_eq!(items.len(), 2);
    let local = items.iter().find(|i| i.host.is_local()).unwrap();
    let remote = items.iter().find(|i| i.host.is_remote()).unwrap();
    assert_eq!(local.title, "Local alias");
    assert!(local.is_current_tab && local.unread);
    assert_eq!(remote.host, HostId::Remote("studio".into()));
    assert_eq!(remote.host_label, "studio PC");
    assert_eq!(remote.title, "Remote task");
    assert!(!remote.is_current_tab && !remote.is_current_worktree && !remote.unread);
    assert_eq!(remote.repo_name, "repo");
    assert_eq!(remote.cwd, "/srv/repo/feature/zzremotezz");
    assert_eq!(crate::jump::search(&items, "zzremotezz").len(), 1);
    let trees = sidebar.jump_worktrees();
    assert_eq!(trees.len(), 2, "same path on two hosts remains two rows");
    let remote_tree = trees.iter().find(|t| t.host.is_remote()).unwrap();
    assert_eq!(remote_tree.host, remote.host);
    assert_eq!(remote_tree.session_id, Some(SessionId(7)));
    assert!(!remote_tree.is_current && !remote_tree.unread);
}

/// A local snapshot refresh must not steal focus from a colliding remote ID.
#[gpui::test]
fn local_poll_keeps_remote_focus_and_local_unread(cx: &mut TestAppContext) {
    let sidebar = cx.new(|_| Sidebar::new_loaded(None));
    sidebar.update(cx, |s, cx| {
        let local = session(7, AgentState::Done, Some("/srv/local"));
        let remote = session(7, AgentState::Waiting, Some("/srv/remote"));
        s.snapshot.sessions = vec![local];
        s.snapshot
            .trees
            .insert("/srv/local".into(), vec!["/srv/local".into()]);
        s.remote_hosts.push(RemoteHost {
            id: "studio".into(),
            label: "Studio".into(),
            status: HostStatus::Connected,
            snapshot: Snapshot {
                sessions: vec![remote],
                trees: [("/srv/remote".into(), vec!["/srv/remote".into()])].into(),
            },
            meta: Meta::new(),
            source: None,
            error: None,
        });
        s.unread.insert(SessionId(7));
        s.set_focused_on(HostId::Remote("studio".into()), Some(SessionId(7)), cx);
        let mut next = s.snapshot.clone();
        next.sessions[0].status.title = Some("Local update".into());
        s.apply(Ok((next, Meta::new())), BTreeMap::new(), cx);
        assert_eq!(s.focused_host, HostId::Remote("studio".into()));
        assert!(s.unread.contains(&SessionId(7)));
        assert_eq!(s.focused_synced, None);
        assert!(
            s.cursor
                .is_some_and(|i| s.row_host(i) == &HostId::Remote("studio".into()))
        );
    });
}

#[gpui::test]
fn focusing_session_expands_collapsed_project_and_selects_session_row_across_hosts(
    cx: &mut TestAppContext,
) {
    let view = cx.new(|cx| {
        let mut s = Sidebar::new(None);
        let mut local_a = session(7, AgentState::Working, Some("/repo-a/feat-a"));
        local_a.cwd = Some("/repo-a/feat-a".into());
        let mut local_b = session(8, AgentState::Waiting, Some("/repo-b/feat-b"));
        local_b.cwd = Some("/repo-b/feat-b".into());
        s.snapshot = Snapshot {
            sessions: vec![local_a, local_b],
            trees: [
                (
                    "/repo-a".into(),
                    vec!["/repo-a".into(), "/repo-a/feat-a".into()],
                ),
                (
                    "/repo-b".into(),
                    vec!["/repo-b".into(), "/repo-b/feat-b".into()],
                ),
            ]
            .into(),
        };
        let mut remote_s = session(7, AgentState::Working, Some("/srv/repo-r/feat-r"));
        remote_s.cwd = Some("/srv/repo-r/feat-r".into());
        s.remote_hosts.push(RemoteHost {
            id: "studio".into(),
            label: "studio PC".into(),
            status: HostStatus::Connected,
            snapshot: Snapshot {
                sessions: vec![remote_s],
                trees: [(
                    "/srv/repo-r".into(),
                    vec!["/srv/repo-r".into(), "/srv/repo-r/feat-r".into()],
                )]
                .into(),
            },
            meta: Meta::new(),
            source: None,
            error: None,
        });
        s.collapsed.insert("/repo-b".into());
        s.collapsed.insert("/repo-b/feat-b".into());
        s.collapsed.insert("/srv/repo-r".into());
        s.collapsed.insert("/srv/repo-r/feat-r".into());
        s.collapsed_hosts.insert(HostId::Remote("studio".into()));
        s.rebuild(cx);
        s
    });

    // 1. 접혀 있는 로컬 저장소·워크트리의 세션을 포커싱하면 펼치고 세션 행을 선택·노출한다.
    view.update(cx, |s, cx| {
        s.set_focused_on(HostId::Local, Some(SessionId(8)), cx);
        assert!(!s.is_collapsed(Path::new("/repo-b")));
        assert!(!s.is_collapsed(Path::new("/repo-b/feat-b")));
        assert_eq!(s.selected_worktree(), Some(Path::new("/repo-b/feat-b")));
        let idx = s.cursor.expect("local session row must be selected");
        assert_eq!(s.row_host(idx), &HostId::Local);
        assert!(matches!(
            s.rows.get(idx),
            Some(Row::Session(sess, true)) if sess.id == SessionId(8)
        ));
        assert!(
            s.card_list.contains_row(idx),
            "focused local session row must be visible in card_list viewport"
        );
        assert_eq!(s.card_list.pending_reveal.get(), Some(idx));
    });

    // 2. 동일한 SessionId(7)를 가진 원격 호스트 세션을 포커싱하면 원격 호스트·저장소·워크트리를 펼치고
    //    로컬이 아닌 원격 호스트의 세션 행을 정확히 선택·노출한다.
    let remote_host = HostId::Remote("studio".into());
    view.update(cx, |s, cx| {
        s.set_focused_on(remote_host.clone(), Some(SessionId(7)), cx);
        assert!(!s.is_host_collapsed(&remote_host));
        assert!(!s.is_collapsed(Path::new("/srv/repo-r")));
        assert!(!s.is_collapsed(Path::new("/srv/repo-r/feat-r")));
        assert_eq!(
            s.selected_workspace(),
            Some((remote_host.clone(), PathBuf::from("/srv/repo-r/feat-r")))
        );
        let idx = s.cursor.expect("remote session row must be selected");
        assert_eq!(s.row_host(idx), &remote_host);
        assert!(matches!(
            s.rows.get(idx),
            Some(Row::Session(sess, true)) if sess.id == SessionId(7)
        ));
        assert!(
            s.card_list.contains_row(idx),
            "focused remote session row must be visible in card_list viewport"
        );
        assert_eq!(s.card_list.pending_reveal.get(), Some(idx));
    });
}

#[gpui::test]
fn daemon_title_change_replaces_stale_screen_title(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    view.update(vcx, |sidebar, cx| {
        let mut task = session(51, AgentState::Working, Some("/repo"));
        task.status.title = Some("First task".into());
        let snapshot = |task| Snapshot {
            sessions: vec![task],
            ..Snapshot::default()
        };
        sidebar.apply(
            Ok((snapshot(task.clone()), Meta::new())),
            BTreeMap::new(),
            cx,
        );
        sidebar.set_title(task.id, "First screen title", cx);
        assert_eq!(sidebar.session_label(&task).0, "First screen title");
        task.status.title = Some("Second task".into());
        sidebar.apply(
            Ok((snapshot(task.clone()), Meta::new())),
            BTreeMap::new(),
            cx,
        );
        assert_eq!(sidebar.session_label(&task).0, "Second task");
        sidebar.set_custom_titles(BTreeMap::from([(task.id, "My tab".into())]), cx);
        task.status.title = Some("Third task".into());
        sidebar.apply(
            Ok((snapshot(task.clone()), Meta::new())),
            BTreeMap::new(),
            cx,
        );
        assert_eq!(sidebar.session_label(&task).0, "My tab");
    });
}

#[gpui::test]
fn transient_poll_errors_keep_diagnostics_without_warning_card(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    view.update(vcx, |sidebar, cx| {
        for kind in [
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionRefused,
            io::ErrorKind::TimedOut,
            io::ErrorKind::WouldBlock,
        ] {
            sidebar.apply(Err(kind.into()), BTreeMap::new(), cx);
            assert!(
                sidebar.poll_error.is_some(),
                "retain diagnostic for {kind:?}"
            );
            assert!(
                sidebar.poll_error_notice().is_none(),
                "retry silently for {kind:?}"
            );
        }
        sidebar.apply(
            Err(io::ErrorKind::PermissionDenied.into()),
            BTreeMap::new(),
            cx,
        );
        assert!(sidebar.poll_error_notice().is_some());
        sidebar.apply(Ok((Snapshot::default(), Meta::new())), BTreeMap::new(), cx);
        assert!(sidebar.poll_error.is_none());
    });
}
