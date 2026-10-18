use super::*;

use gpui::{ScrollStrategy, TestAppContext};
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
                detail: (!revoked).then(|| "직접 연결 · 응답 12ms".into()),
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
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new(None));
    view.update(vcx, |sidebar, cx| sidebar.sync_remote_hosts(&manager, cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("host-header-local").is_some());
    assert!(vcx.debug_bounds("host-header-iroh:studio").is_some());
    assert!(vcx.debug_bounds("host-route-iroh:studio").is_some());
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
    assert!(vcx.debug_bounds("host-error-iroh:studio").is_some());
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
    let rows = rows(
        &snapshot,
        &prefs,
        &BTreeSet::from([PathBuf::from("/repo/a")]),
    );
    assert_eq!(rows[0], Row::NeedsYou);
    assert!(matches!(&rows[1], Row::Session(s, false) if s.id == SessionId(2)));
    assert!(matches!(&rows[2], Row::Session(s, false) if s.id == SessionId(3)));
    assert_eq!(rows[4], Row::Worktree("/repo/b".into()));
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
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
                s.scroll.scroll_to_item((i * 7) % 500, ScrollStrategy::Top);
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(Some(path.clone())));
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
    let restored = Sidebar::new(Some(path.clone()));
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
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
    assert_eq!(
        labels,
        [
            text::NEEDS_YOU.get(),
            "claude · a, 입력 대기",
            "저장소 repo",
            "워크트리 a, 입력 대기",
            "세션",
            "세션 3, 작업 중",
        ]
    );
}

#[gpui::test]
fn session_list_is_a_named_list(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(Some(sidebar_file)));
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
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
    assert_eq!(
        collapsed_labels,
        ["저장소 repo", "워크트리 repo", ".claude/worktrees 숨김 (2)"]
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    let back = cx.update(|_, cx| cx.focus_handle());
    let selected = Rc::new(Cell::new(None));
    let got = selected.clone();
    let _subscription = cx
        .update(|_, cx| cx.subscribe(&view, move |_, event: &Selected, _| got.set(Some(event.0))));
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
        let svg = gpui_kit_assets::Assets.load(&path).unwrap();
        assert!(svg.is_some_and(|b| b.starts_with(b"<svg")), "{path}");
    }
    // OWE-605: agy 탭 아이콘(Moon)은 기본 번들에서 실제 SVG로 읽히고 핀 별과 경로가 다르다.
    let svg = gpui_kit_assets::Assets
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    let back = cx.update(|_, cx| cx.focus_handle());
    let launched: Rc<RefCell<Vec<NewSession>>> = Rc::new(RefCell::new(Vec::new()));
    let got = launched.clone();
    let _sub = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &NewSession, _| {
            got.borrow_mut().push(event.clone());
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
    assert_eq!(
        launched.borrow().as_slice(),
        &[NewSession {
            cwd: PathBuf::from("/repo/a"),
            program: Program::Login,
        }]
    );
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
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

/// OWE-598(N19): 사이드바가 숨겨지거나(`set_visible(false)`) 창이 비활성화되면(`set_active(false)`)
/// 500ms 폴링과 git 감시를 즉시 멈추고, 다시 보일 때 재개한다.
#[gpui::test]
fn sidebar_stops_polling_and_hides_git_watch_when_hidden_or_inactive(cx: &mut TestAppContext) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct CountingSource {
        fetches: AtomicUsize,
        visible: AtomicBool,
    }
    impl Source for CountingSource {
        fn fetch(&self) -> io::Result<Snapshot> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            Ok(Snapshot::default())
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

    // 2. 사이드바 UI 가시성·활성 상태에 따른 폴링 중단 확인
    cx.update(gpui_component::init);
    let source = Arc::new(CountingSource {
        fetches: AtomicUsize::new(0),
        visible: AtomicBool::new(true),
    });
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
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

    // 창이 비활성화되면(`set_active(false)`) 추가 폴링이 0회이고 source.set_visible(false)가 호출된다.
    source.visible.store(true, Ordering::SeqCst);
    cx.update(|_, cx| view.update(cx, |s, cx| s.set_active(false, cx)));
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| view.read(cx).is_visible()));
    assert!(!source.visible.load(Ordering::SeqCst));
    let after_inactive = source.fetches.load(Ordering::SeqCst);
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
    }
    assert_eq!(
        source.fetches.load(Ordering::SeqCst),
        after_inactive,
        "inactive window must not poll sidebar"
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(Some(path.clone())));
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
            *s = Sidebar::new(Some(unreadable.clone()));
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

#[gpui::test]
fn pr_badge_renders_and_missing_cli_hides_it_without_external_io(cx: &mut TestAppContext) {
    use crate::pull_request::Status;
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
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
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.prs.insert(path.clone(), Status::Missing);
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("pr-badge-1").is_none());
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
    let sidebar_sock = ipc::Socket::new(paths.clone());
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

    fs::remove_dir_all(root).ok();
}

#[gpui::test]
fn failed_snapshot_releases_agent_sleep_assertion_and_badge(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let sidebar = cx.new(|_| Sidebar::new(None));
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
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

/// MK-131(OWE-667): 원격 호스트가 설정되면 최상단 `'내 Mac'` 섹션과 원격 호스트 섹션으로 묶이고,
/// 각 호스트 헤더에 연결 상태 점(`연결됨` / `연결 중` / `끊김`)과 세션 수 배지·접기/펼치기가 동작하며,
/// 끊긴 호스트는 마지막으로 알려진 세션 목록을 흐리게(`is_row_dimmed`) 유지하고 `'다시 연결'` 버튼을 제공한다.
#[gpui::test]
fn multi_host_sidebar_groups_local_and_remote_with_status_collapse_dimming_and_reconnect(
    cx: &mut TestAppContext,
) {
    use gpui::Modifiers;
    use std::cell::RefCell;
    use std::rc::Rc;

    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    let activated: Rc<RefCell<Vec<(HostId, SessionId)>>> = Rc::new(RefCell::new(Vec::new()));
    let reconnected: Rc<RefCell<Vec<HostId>>> = Rc::new(RefCell::new(Vec::new()));

    let act_out = activated.clone();
    let _sub_act = cx.update(|_, cx| {
        cx.subscribe(&view, move |s, event: &Selected, cx| {
            let host = s
                .update(cx, |s, _| s.take_action_host())
                .unwrap_or(HostId::Local);
            act_out.borrow_mut().push((host, event.0));
        })
    });
    let recon_out = reconnected.clone();
    let _sub_recon = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &ReconnectHost, _| {
            recon_out.borrow_mut().push(event.0.clone());
        })
    });

    let studio_id = HostId::Remote("studio".into());
    let mini_id = HostId::Remote("mini".into());

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.snapshot = Snapshot {
                sessions: vec![session(1, AgentState::Working, Some("/local/repo/main"))],
                trees: BTreeMap::from([(
                    PathBuf::from("/local/repo"),
                    vec![PathBuf::from("/local/repo/main")],
                )]),
            };
            s.remote_hosts = vec![
                RemoteHost {
                    id: "studio".into(),
                    label: "Mac Studio".into(),
                    status: HostStatus::Connected,
                    snapshot: Snapshot {
                        sessions: vec![
                            session(1, AgentState::Done, Some("/remote/studio/wt")),
                            session(2, AgentState::Working, Some("/remote/studio/wt")),
                        ],
                        trees: BTreeMap::from([(
                            PathBuf::from("/remote/studio"),
                            vec![PathBuf::from("/remote/studio/wt")],
                        )]),
                    },
                    meta: Meta::new(),
                    source: None,
                },
                RemoteHost {
                    id: "mini".into(),
                    label: "M4 Mini".into(),
                    status: HostStatus::Disconnected,
                    snapshot: Snapshot {
                        sessions: vec![session(7, AgentState::Waiting, Some("/remote/mini/wt"))],
                        trees: BTreeMap::from([(
                            PathBuf::from("/remote/mini"),
                            vec![PathBuf::from("/remote/mini/wt")],
                        )]),
                    },
                    meta: Meta::new(),
                    source: None,
                },
            ];
            s.rebuild(cx);
        });
    });
    cx.run_until_parked();

    // 1. 최상단에 '내 Mac' 헤더, 그 아래 'studio', 'mini' 호스트 헤더가 순서대로 배치된다.
    let rows_now = cx.update(|_, cx| view.read(cx).rows.clone());
    let local_hdr = rows_now
        .iter()
        .position(|r| {
            matches!(
                r,
                Row::HostHeader {
                    host: HostId::Local,
                    ..
                }
            )
        })
        .expect("local host header");
    let studio_hdr = rows_now
        .iter()
        .position(|r| matches!(r, Row::HostHeader { host, .. } if *host == studio_id))
        .expect("studio host header");
    let mini_hdr = rows_now
        .iter()
        .position(|r| matches!(r, Row::HostHeader { host, .. } if *host == mini_id))
        .expect("mini host header");
    assert_eq!(local_hdr, 0);
    assert!(local_hdr < studio_hdr && studio_hdr < mini_hdr);

    // 2. 각 호스트 헤더에 상태 점과 세션 수 배지가 렌더링되고, 끊긴 호스트('mini')에만 '다시 연결' 버튼이 보인다.
    for selector in [
        "host-header-local",
        "host-status-local",
        "host-count-local",
        "host-header-studio",
        "host-status-studio",
        "host-count-studio",
        "host-header-mini",
        "host-status-mini",
        "host-count-mini",
        "host-reconnect-mini",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "missing selector {selector}"
        );
    }
    assert!(cx.debug_bounds("host-reconnect-local").is_none());
    assert!(cx.debug_bounds("host-reconnect-studio").is_none());

    // 3. 끊긴 호스트('mini') 아래의 행들은 흐리게(dimmed) 표시되고, 연결된 호스트('local', 'studio') 행들은 흐리지 않다.
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert!(!s.is_row_dimmed(local_hdr + 1));
        assert!(!s.is_row_dimmed(studio_hdr + 1));
        assert!(
            s.is_row_dimmed(mini_hdr + 1),
            "disconnected remote host rows must remain dimmed"
        );
    });

    // 4. 같은 SessionId(1)이 로컬과 원격('studio') 양쪽에 있어도 원격 세션 행을 클릭하면 원격 HostId가 전달된다.
    let studio_session_idx = rows_now
        .iter()
        .enumerate()
        .position(|(i, r)| {
            i > studio_hdr
                && i < mini_hdr
                && matches!(r, Row::Session(s, _) if s.id == SessionId(1))
        })
        .expect("studio session 1 row");
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.select_session(studio_session_idx, SessionId(1), cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        activated.borrow().as_slice(),
        &[(studio_id.clone(), SessionId(1))]
    );

    // 5. 호스트 헤더 접기/펼치기: 'studio' 헤더를 접으면 'studio' 하위 행만 숨고 '내 Mac'과 'mini'는 유지된다.
    let studio_header_bounds = cx.debug_bounds("host-header-studio").unwrap();
    cx.simulate_click(studio_header_bounds.center(), Modifiers::default());
    cx.run_until_parked();
    let folded_rows = cx.update(|_, cx| view.read(cx).rows.clone());
    assert!(folded_rows.len() < rows_now.len());
    let studio_hdr_after = folded_rows
        .iter()
        .position(|r| matches!(r, Row::HostHeader { host, .. } if *host == studio_id))
        .unwrap();
    let mini_hdr_after = folded_rows
        .iter()
        .position(|r| matches!(r, Row::HostHeader { host, .. } if *host == mini_id))
        .unwrap();
    assert_eq!(
        studio_hdr_after + 1,
        mini_hdr_after,
        "collapsing studio header must hide all rows under studio"
    );

    // 다시 클릭하면 펼쳐진다.
    let studio_header_bounds = cx.debug_bounds("host-header-studio").unwrap();
    cx.simulate_click(studio_header_bounds.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).rows.len()), rows_now.len());

    // 6. 끊긴 호스트의 '다시 연결' 버튼 클릭 시 ReconnectHost 이벤트가 나가고 상태가 Connecting으로 바뀐다.
    let reconnect_btn = cx.debug_bounds("host-reconnect-mini").unwrap();
    cx.simulate_click(reconnect_btn.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        reconnected.borrow().as_slice(),
        std::slice::from_ref(&mini_id)
    );
    cx.update(|_, cx| {
        let s = view.read(cx);
        let mini = s.remote_hosts.iter().find(|h| h.id == "mini").unwrap();
        assert_eq!(mini.status, HostStatus::Connecting);
    });
}

#[gpui::test]
fn orca_card_expansion_agent_counts_and_reveal_focused(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
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
            assert_eq!(s.agent_counts(), (1, 1));
            assert!(!s.expanded_cards.contains(Path::new("/repo/a")));
        });
    });
    cx.run_until_parked();
    // 기본 상태에서는 카드 요약 바만 보이고 하위 세션 행은 접혀 있다(렌더 행 2개: 저장소 + 워크트리 카드).
    assert_eq!(
        cx.update(|_, cx| view.read(cx).rendered_rows),
        2,
        "collapsed card should hide nested session rows by default"
    );

    // 카드 요약 바 토글 시 하위 세션 행 2개가 펼쳐진다(렌더 행 4개).
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.rendered_rows = 0;
            s.toggle_card_sessions(Path::new("/repo/a"), cx);
            assert!(s.expanded_cards.contains(Path::new("/repo/a")));
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).rendered_rows),
        4,
        "expanded card should render nested session rows"
    );

    // 하위 세션에 커서가 있을 때 다시 토글하면 커서를 워크트리 행으로 되돌리고 하위 세션 행을 실제로 접는다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.cursor = Some(2);
            s.rendered_rows = 0;
            s.toggle_card_sessions(Path::new("/repo/a"), cx);
            assert!(!s.expanded_cards.contains(Path::new("/repo/a")));
            assert_eq!(s.cursor, Some(1));
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).rendered_rows),
        2,
        "re-collapsing card while cursor was on nested session must hide session rows"
    );

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
    let sidebar = cx.new(|_| Sidebar::new(None));
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
    let sidebar = cx.new(|_| Sidebar::new(None));
    sidebar.update(cx, |s, cx| {
        // 파일 자리에 디렉터리를 두면 root 권한에서도 저장이 실패한다.
        s.path = Some(dir.clone());
        s.toggle_pin("/repo/a".into(), cx);
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

/// OWE-737: Claude 유휴/대기 세션의 프롬프트 캐시 카운트다운(`CacheTimer`)과 첫 프롬프트 자동 제목(`session-titles.json`)이
/// 사이드바 세션 행과 워크트리 카드에 표시되고, 만료 시 자동으로 숨겨지며 사용자 지정 이름은 덮어쓰지 않는다.
#[gpui::test]
fn owe737_cache_timer_and_auto_titles_render_on_session_and_worktree_rows(cx: &mut TestAppContext) {
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

    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new(Some(sidebar_file)));
    let base_ns = 10_000_000_000_000_u64;
    let now_ns = base_ns + 120 * 1_000_000_000; // 120초 경과 → 5분(300초) 중 3:00 남음

    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut claude = session(11, AgentState::Done, Some("/repo/wt-1"));
            claude.status.agent = "claude".into();
            claude.status.since_unix_ns = base_ns;
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
            s.set_now_unix_ns_for_test(Some(now_ns), cx);
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
    vcx.run_until_parked();

    // 행 인덱스: 0 = Repository("/repo"), 1 = Worktree("/repo/wt-1"), 2 = Session(11)
    assert!(
        vcx.debug_bounds("worktree-cache-timer-1").is_some(),
        "worktree row must render most urgent Claude prompt cache timer"
    );
    assert!(
        vcx.debug_bounds("session-cache-timer-2").is_some(),
        "session row must render Claude prompt cache timer"
    );

    // 3. TTL(300초) 경과 후에는 타이머 배지가 사라진다.
    vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.set_now_unix_ns_for_test(Some(base_ns + 305 * 1_000_000_000), cx);
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("worktree-cache-timer-1").is_none());
    assert!(vcx.debug_bounds("session-cache-timer-2").is_none());

    fs::remove_dir_all(dir).ok();
}

/// OWE-731: 그룹화(`Repo` / `WorkspaceStatus` / `PrStatus` / `None`)와 정렬(`Smart` / `Recent` / `Name`)이
/// 워크트리 목록과 그룹 헤더(`Row::GroupHeader`)를 결정론적으로 재구성하며, 핀 고정 워크트리가 그룹 내 최상단에 온다.
#[gpui::test]
fn workspace_grouping_and_sorting_modes_order_rows_and_fold_groups(cx: &mut TestAppContext) {
    use crate::pull_request::Status as PrStatus;
    use krill_proto::orchestration::WorkspaceStatus;

    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));

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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(Some(sidebar_bin.clone())));

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

    let restored = Sidebar::new(Some(sidebar_bin));
    assert_eq!(restored.options().group_by, GroupBy::PrStatus);
    assert_eq!(restored.options().sort_by, SortBy::Recent);
    assert_eq!(restored.options().card_layout, CardLayout::Compact);
    assert!(!restored.options().card_properties.ports);
    assert!(restored.options().card_properties.issue);
    assert!(restored.options().filters.hide_automation);

    fs::remove_dir_all(dir).ok();
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
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new(Some(sidebar_bin)));
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
    assert!(vcx.debug_bounds("setup-script-prompt-card").is_some());
    let enable = vcx.debug_bounds("setup-prompt-enable").unwrap();
    vcx.simulate_click(enable.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("setup-script-prompt-card").is_none());
    let summary = krill_proto::orchestration::inspect_repo_hooks(&repo, Some(&dir));
    assert_eq!(summary.store_setup.as_deref(), Some("pnpm install"));

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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(Some(sb_path)));
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
    let restored = Sidebar::new(Some(sidebar_bin));
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
    use krill_proto::control::Program;
    use std::cell::RefCell;
    use std::rc::Rc;

    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(None));
    cx.simulate_resize(gpui::size(gpui::px(360.), gpui::px(900.)));

    let selected: Rc<RefCell<Vec<(HostId, SessionId)>>> = Rc::new(RefCell::new(Vec::new()));
    let launched: Rc<RefCell<Vec<(HostId, NewSession)>>> = Rc::new(RefCell::new(Vec::new()));

    let sel_out = selected.clone();
    let _sub_sel = cx.update(|_, cx| {
        cx.subscribe(&view, move |s, event: &Selected, cx| {
            let host = s
                .update(cx, |s, _| s.take_action_host())
                .unwrap_or(HostId::Local);
            sel_out.borrow_mut().push((host, event.0));
        })
    });
    let new_out = launched.clone();
    let _sub_new = cx.update(|_, cx| {
        cx.subscribe(&view, move |s, event: &NewSession, cx| {
            let host = s
                .update(cx, |s, _| s.take_action_host())
                .unwrap_or(HostId::Local);
            new_out.borrow_mut().push((host, event.clone()));
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
            &[(studio_id.clone(), SessionId(21))],
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

        assert_eq!(
            launched.borrow().as_slice(),
            &[(
                studio_id.clone(),
                NewSession {
                    cwd: wt_without_remote_sess.clone(),
                    program: Program::Login,
                }
            )],
            "clicking remote worktree card without remote session in {layout:?} must spawn on studio host even if local has same path"
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
    let (view, cx) = cx.add_window_view(|_, _| Sidebar::new(Some(sb_path)));
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
        let s = view.read(cx);
        assert_eq!(s.worktree_parent(&grandchild), None);
    });

    let reloaded = Sidebar::new(Some(sidebar_bin));
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

    assert_eq!(format_filter_badge_count(1), "1");
    assert_eq!(format_filter_badge_count(9), "9");
    assert_eq!(format_filter_badge_count(10), "9+");

    let dir = std::env::temp_dir().join(format!("mk-owe791-menu-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let sidebar_bin = dir.join("sidebar.bin");

    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new(Some(sidebar_bin.clone())));
    vcx.simulate_resize(gpui::size(gpui::px(420.), gpui::px(900.)));

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
    assert!(vcx.debug_bounds("sidebar-hosts-panel").is_some());
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
    assert!(vcx.debug_bounds("sidebar-projects-panel").is_some());
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

    // 서브메뉴를 다시 닫아 필터 행들이 뷰포트 안에 오도록 한다.
    let props_trigger = vcx.debug_bounds("sidebar-submenu-card-properties").unwrap();
    vcx.simulate_click(props_trigger.center(), gpui::Modifiers::default());
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
    let persisted = Sidebar::new(Some(sidebar_bin.clone()));
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
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new(Some(path.clone())));
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
    let restored = Sidebar::new(Some(path.clone()));
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
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new(Some(path.clone())));
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
    let restored = Sidebar::new(Some(path.clone()));
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
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new(Some(path.clone())));
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
    let restored = Sidebar::new(Some(path.clone()));
    assert!(restored.removed_projects.contains(Path::new("/repo")));
    fs::remove_dir_all(dir).ok();
}
