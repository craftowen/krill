use super::*;
use gpui::TestAppContext;
use krill_proto::agent_history::Agent;
use std::sync::Mutex;

pub(super) fn sample() -> Entry {
    Entry {
        agent: Agent::Codex,
        id: "11111111-2222-4333-8444-555555555555".into(),
        cwd: "/tmp/한글".into(),
        modified: 0,
    }
}

#[gpui::test]
fn on_demand_scan_refresh_and_resume_emit_new_session(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let call: Call = Arc::new(move |r| {
        seen.lock().unwrap().push(r.clone());
        Ok(Reply::AgentHistory(match r {
            Request::AgentHistory(Action::Scan) => Response::Scanned(Snapshot {
                entries: vec![sample()],
                truncated: false,
                skipped: false,
            }),
            _ => Response::Resumed {
                session: SessionId(91),
            },
        }))
    });
    let (panel, cx) = cx.add_window_view(|_, _| Panel::new(call));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let events = opened.clone();
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, &Open(id), _| {
            events.lock().unwrap().push(id);
        })
        .detach();
    });
    cx.run_until_parked();
    assert!(log.lock().unwrap().is_empty(), "기동 시 스캔 없음");
    panel.update(cx, Panel::toggle);
    cx.run_until_parked();
    assert_eq!(log.lock().unwrap().len(), 1);
    // 시간이 흘러도 목록이 열린 채로 폴링하지 않는다.
    cx.executor().advance_clock(Duration::from_secs(60));
    cx.run_until_parked();
    assert_eq!(log.lock().unwrap().len(), 1);
    panel.update(cx, |p, cx| p.run(Action::Scan, cx));
    cx.run_until_parked();
    assert_eq!(log.lock().unwrap().len(), 2);
    panel.update(cx, |p, cx| p.resume(sample(), cx));
    cx.run_until_parked();
    assert_eq!(
        log.lock().unwrap().last(),
        Some(&Request::AgentHistory(Action::Resume(sample())))
    );
    assert_eq!(*opened.lock().unwrap(), [SessionId(91)]);
    panel.read_with(cx, |p, _| assert!(!p.open && !p.busy && p.data.is_none()));
}

#[test]
fn older_daemon_receives_no_new_variant() {
    let mut wire = io::Cursor::new(Vec::new());
    assert_eq!(
        exchange(
            FEAT_CONTROL,
            &mut wire,
            &Request::AgentHistory(Action::Scan)
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::Unsupported
    );
    assert!(wire.get_ref().is_empty());
    assert_eq!(
        exchange(
            FEAT_CONTROL | FEAT_AGENT_HISTORY,
            &mut wire,
            &Request::AgentHistory(Action::Search(SearchQuery::default()))
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::Unsupported
    );
    assert!(wire.get_ref().is_empty());
}

#[gpui::test]
fn failed_resume_preserves_list_and_emits_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (panel, cx) = cx.add_window_view(|_, _| {
        Panel::new(Arc::new(|_| {
            Ok(Reply::Failed(krill_proto::control::Failure::NoHolder))
        }))
    });
    panel.update(cx, |p, cx| {
        p.open = true;
        p.data = Some(Snapshot {
            entries: vec![sample()],
            truncated: false,
            skipped: false,
        });
        p.resume(sample(), cx);
        p.resume(sample(), cx); // 처리 중 중복 클릭은 무시한다.
    });
    cx.run_until_parked();
    panel.read_with(cx, |p, _| {
        assert!(p.open && p.data.is_some() && p.error.is_some() && !p.busy);
    });
}

/// 세션 기록은 패널 본문으로 그리며 바깥 클릭으로 닫히지 않는다.
#[gpui::test]
fn panel_fills_available_space_and_stays_open_on_outside_click(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let call: Call = Arc::new(|_| {
        Ok(Reply::AgentHistory(Response::Scanned(Snapshot {
            entries: vec![sample()],
            truncated: false,
            skipped: false,
        })))
    });
    let (panel, cx) = cx.add_window_view(|_, _| Panel::new(call));
    panel.update(cx, Panel::toggle);
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds("agent-history-panel")
        .expect("history panel");
    let size = cx.update(|window, _| window.viewport_size());
    assert_eq!(bounds.size, size);
    let list = cx.debug_bounds("agent-history-list").expect("history list");
    assert!(list.size.height > size.height / 2.);
    cx.simulate_click(
        gpui::point(gpui::px(10.), size.height - gpui::px(10.)),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    panel.read_with(cx, |p, _| assert!(p.open && p.data.is_some()));
    panel.update(cx, Panel::close);
    cx.run_until_parked();
    panel.read_with(cx, |p, _| assert!(!p.open && p.data.is_none()));
}

#[test]
fn highlight_spans_splits_matching_query_tokens_and_ignores_operators() {
    let spans = highlight_spans(
        "Updated OAuth2 refresh handler in auth_service.rs",
        "agent:claude oauth2 auth_service",
    );
    let highlighted: Vec<String> = spans
        .into_iter()
        .filter(|(_, hit)| *hit)
        .map(|(s, _)| s)
        .collect();
    assert_eq!(highlighted, vec!["OAuth2", "auth_service"]);
}

#[gpui::test]
fn vault_panel_filters_by_scope_and_agent_and_one_click_resumes(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let sample_hits = vec![
        VaultHit {
            agent: VaultAgent::Claude,
            id: "11111111-2222-4333-8444-555555555555".into(),
            cwd: "/tmp/proj-a/wt-1".into(),
            modified: 300,
            title: "OAuth2 Token Fix".into(),
            summary: "Fixed refresh race".into(),
            snippet: "Updated OAuth2 refresh handler".into(),
            role: "assistant".into(),
            score: 140,
        },
        VaultHit {
            agent: VaultAgent::Agy,
            id: "33333333-2222-4333-8444-555555555555".into(),
            cwd: "/tmp/proj-a/wt-2".into(),
            modified: 200,
            title: "GPUI Vault Panel".into(),
            summary: "Added scope filters".into(),
            snippet: "Implemented VaultScope pills".into(),
            role: "assistant".into(),
            score: 120,
        },
        VaultHit {
            agent: VaultAgent::OpenCode,
            id: "ses_opencode_99".into(),
            cwd: "/tmp/proj-b".into(),
            modified: 100,
            title: "SQLite Migration".into(),
            summary: "Added index".into(),
            snippet: "CREATE INDEX payments_idx".into(),
            role: "user".into(),
            score: 100,
        },
    ];
    let hits_for_call = sample_hits.clone();
    let call: Call = Arc::new(move |r| {
        seen.lock().unwrap().push(r.clone());
        Ok(Reply::AgentHistory(match r {
            Request::AgentHistory(Action::Scan) => Response::Scanned(Snapshot {
                entries: vec![sample()],
                truncated: false,
                skipped: false,
            }),
            Request::AgentHistory(Action::Search(_)) => Response::Searched(SearchSnapshot {
                hits: hits_for_call.clone(),
                truncated: false,
                skipped: false,
            }),
            Request::AgentHistory(Action::Resume(_) | Action::ResumeVault(_)) => {
                Response::Resumed {
                    session: SessionId(77),
                }
            }
            _ => unreachable!(),
        }))
    });
    let (panel, cx) = cx.add_window_view(|_, _| Panel::new(call));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let events = opened.clone();
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, &Open(id), _| {
            events.lock().unwrap().push(id);
        })
        .detach();
    });

    panel.update(cx, |p, cx| {
        p.set_scope_paths(
            Some("/tmp/proj-a/wt-1".into()),
            vec!["/tmp/proj-a/wt-1".into(), "/tmp/proj-a/wt-2".into()],
        );
        p.toggle(cx);
    });
    cx.run_until_parked();

    // Trigger Vault search with query
    panel.update(cx, |p, cx| p.set_query("VaultScope OAuth2 CREATE", cx));
    cx.run_until_parked();
    panel.update(cx, |p, cx| p.set_query("", cx));
    cx.run_until_parked();

    // Scope = All -> 3 hits
    panel.read_with(cx, |p, _| assert_eq!(p.visible_hits().len(), 3));

    // Scope = Project -> 2 hits in /tmp/proj-a/*
    panel.update(cx, |p, cx| p.set_scope(VaultScope::Project, cx));
    cx.run_until_parked();
    panel.read_with(cx, |p, _| assert_eq!(p.visible_hits().len(), 2));

    // Scope = Workspace -> 1 hit in /tmp/proj-a/wt-1
    panel.update(cx, |p, cx| p.set_scope(VaultScope::Workspace, cx));
    cx.run_until_parked();
    panel.read_with(cx, |p, _| {
        let hits = p.visible_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].agent, VaultAgent::Claude);
    });

    // Reset scope to All, filter by Agent = Agy -> 1 hit
    panel.update(cx, |p, cx| {
        p.set_scope(VaultScope::All, cx);
        p.set_agent_filter(Some(VaultAgent::Agy), cx);
    });
    cx.run_until_parked();
    panel.read_with(cx, |p, _| {
        let hits = p.visible_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].agent, VaultAgent::Agy);
    });

    // One-click Resume on the filtered Agy hit
    let resume_btn = cx
        .debug_bounds("agent-history-resume-0")
        .expect("resume button for filtered hit");
    cx.simulate_click(resume_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        log.lock().unwrap().last(),
        Some(&Request::AgentHistory(Action::ResumeVault(VaultTarget {
            agent: VaultAgent::Agy,
            id: "33333333-2222-4333-8444-555555555555".into(),
            cwd: "/tmp/proj-a/wt-2".into(),
            modified: 200,
        })))
    );
    assert_eq!(*opened.lock().unwrap(), [SessionId(77)]);
}

/// OWE-768: 초기 스캔(`Action::Scan`)이나 직전 검색(`Action::Search`) 진행 중(`busy`)에
/// 검색어·범위·에이전트 필터가 바뀌면 유실하지 않고 완료 직후 최신 조건으로 재검색한다.
#[gpui::test]
fn query_and_filter_changes_while_busy_run_latest_vault_search(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let call: Call = Arc::new(move |r| {
        seen.lock().unwrap().push(r.clone());
        Ok(Reply::AgentHistory(match r {
            Request::AgentHistory(Action::Scan) => Response::Scanned(Snapshot {
                entries: vec![sample()],
                truncated: false,
                skipped: false,
            }),
            Request::AgentHistory(Action::Search(q)) => Response::Searched(SearchSnapshot {
                hits: if q.query == "refresh"
                    && q.agents == vec![VaultAgent::Claude]
                    && q.paths == vec!["/tmp/proj-a/wt-1".to_string()]
                {
                    vec![VaultHit {
                        agent: VaultAgent::Claude,
                        id: "11111111-2222-4333-8444-555555555555".into(),
                        cwd: "/tmp/proj-a/wt-1".into(),
                        modified: 300,
                        title: "Refresh Handler".into(),
                        summary: "Token refresh".into(),
                        snippet: "refresh_token_now()".into(),
                        role: "assistant".into(),
                        score: 150,
                    }]
                } else {
                    Vec::new()
                },
                truncated: false,
                skipped: false,
            }),
            _ => unreachable!(),
        }))
    });
    let (panel, cx) = cx.add_window_view(|_, _| Panel::new(call));

    // `toggle()`이 `Action::Scan`을 시작해 `busy = true`인 같은 프레임에서
    // 검색어("r" -> "refresh")·범위(Workspace)·에이전트(Claude)를 잇달아 바꾼다.
    panel.update(cx, |p, cx| {
        p.set_scope_paths(
            Some("/tmp/proj-a/wt-1".into()),
            vec!["/tmp/proj-a/wt-1".into(), "/tmp/proj-a/wt-2".into()],
        );
        p.toggle(cx);
        assert!(p.busy);
        p.set_query("r", cx);
        p.set_scope(VaultScope::Workspace, cx);
        p.set_agent_filter(Some(VaultAgent::Claude), cx);
        p.set_query("refresh", cx);
        assert!(p.pending_search);
    });
    cx.run_until_parked();

    let requests = log.lock().unwrap().clone();
    assert_eq!(
        requests,
        vec![
            Request::AgentHistory(Action::Scan),
            Request::AgentHistory(Action::Search(SearchQuery {
                query: "refresh".into(),
                agents: vec![VaultAgent::Claude],
                paths: vec!["/tmp/proj-a/wt-1".into()],
                limit: 50,
            })),
        ]
    );
    panel.read_with(cx, |p, _| {
        assert!(!p.busy && !p.pending_search);
        let hits = p.visible_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Refresh Handler");
    });
}

/// OWE-857: 팝오버가 열리면 상단 제목·새로고침 버튼이 보이고 검색창에 자동 포커스된다.
#[gpui::test]
fn opening_panel_focuses_search_and_shows_header(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let call: Call = Arc::new(|_| {
        Ok(Reply::AgentHistory(Response::Scanned(Snapshot {
            entries: Vec::new(),
            truncated: false,
            skipped: false,
        })))
    });
    let (panel, cx) = cx.add_window_view(|_, _| Panel::new(call));
    panel.update(cx, Panel::toggle);
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("agent-history-panel").is_some(),
        "panel opens"
    );
    assert!(
        cx.debug_bounds("agent-history-title").is_some(),
        "header shows the session history title"
    );
    assert!(
        cx.debug_bounds("agent-history-refresh").is_some(),
        "header shows a refresh button, not plain text"
    );
    cx.simulate_input("hello");
    cx.run_until_parked();
    panel.read_with(cx, |p, _| assert_eq!(p.query(), "hello"));
}

/// OWE-857: 빈 상태 문구는 특정 에이전트에 고정되지 않는다.
#[test]
fn empty_message_is_agent_neutral() {
    let msg = crate::i18n::catalog::agent_history::EMPTY.get();
    assert_eq!(msg, "세션 기록이 없습니다");
    assert!(!msg.contains("Claude") && !msg.contains("Codex"));
}

/// OWE-856: 상태바 토글은 버튼 역할·이름을 노출한다.
#[gpui::test]
fn toggle_exposes_button_role_and_label(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let call: Call =
        Arc::new(|_| Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "test")));
    let (panel, cx) = cx.add_window_view(|_, _| Panel::new(call));
    let (role, label) = cx.update(|_, cx| {
        panel.update(cx, |p, cx| {
            let el = p.render_toggle(cx);
            let mut node = gpui::accesskit::Node::new(gpui::Role::Unknown);
            gpui::Element::write_a11y_info(&el, &mut node);
            (
                gpui::Element::a11y_role(&el),
                node.label().map(str::to_owned),
            )
        })
    });
    assert_eq!(role, Some(gpui::Role::Button));
    assert_eq!(label.as_deref(), Some(text::TITLE.get()));
}

#[gpui::test]
fn hidden_consumer_refreshes_query_and_changed_scope_paths(cx: &mut TestAppContext) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let call: Call = Arc::new(move |request| {
        recorded.lock().unwrap().push(request.clone());
        Ok(Reply::AgentHistory(match request {
            Request::AgentHistory(Action::Scan) => Response::Scanned(Snapshot {
                entries: Vec::new(),
                truncated: false,
                skipped: false,
            }),
            Request::AgentHistory(Action::Search(_)) => Response::Searched(SearchSnapshot {
                hits: Vec::new(),
                truncated: false,
                skipped: false,
            }),
            _ => unreachable!(),
        }))
    });
    let panel = cx.new(|_| Panel::new(call));
    panel.update(cx, Panel::scan);
    cx.run_until_parked();
    panel.update(cx, |panel, cx| {
        assert!(!panel.is_open());
        assert!(!panel.busy);
        panel.set_query("latest query", cx);
        panel.set_scope(VaultScope::Workspace, cx);
        panel.update_scope_paths(Some("/tmp/new-scope".into()), Vec::new(), cx);
    });
    cx.run_until_parked();
    assert!(matches!(requests.lock().unwrap().last(),
        Some(Request::AgentHistory(Action::Search(query)))
        if query.query == "latest query" && query.paths == ["/tmp/new-scope"]));
    panel.update(cx, Panel::close);
    let before = requests.lock().unwrap().len();
    panel.update(cx, |panel, cx| panel.set_query("closed consumer", cx));
    cx.run_until_parked();
    assert_eq!(requests.lock().unwrap().len(), before);
}
