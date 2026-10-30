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
fn switching_history_hosts_clears_results_and_never_uses_local_call(cx: &mut TestAppContext) {
    let calls = Arc::new(Mutex::new(0));
    let seen = calls.clone();
    let panel = cx.new(|_| {
        Panel::new(Arc::new(move |_| {
            *seen.lock().unwrap() += 1;
            Ok(Reply::AgentHistory(Response::Scanned(Snapshot {
                entries: vec![sample()],
                truncated: false,
                skipped: false,
            })))
        }))
    });
    panel.update(cx, Panel::scan);
    cx.run_until_parked();
    assert_eq!(panel.read_with(cx, |p, _| p.loaded_hits().len()), 1);
    panel.update(cx, |p, cx| {
        p.set_host(HostId::Remote("studio".into()), None, cx);
        assert!(p.loaded_hits().is_empty());
        assert!(p.list_hits.is_empty());
    });
    cx.run_until_parked();
    assert_eq!(*calls.lock().unwrap(), 1, "no local fallback");
    panel.read_with(cx, |p, _| {
        assert!(p.error.is_some());
        assert!(!p.has_loaded());
    });
}

#[gpui::test]
fn remote_history_resume_preserves_host_and_switch_discards_pending_reply(cx: &mut TestAppContext) {
    let panel = cx.new(|_| Panel::new(Arc::new(|_| unreachable!())));
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    cx.update(|cx| {
        cx.subscribe(&panel, move |_, event: &Open, _| {
            seen.lock()
                .unwrap()
                .push((event.host.clone(), event.session));
        })
        .detach();
    });
    let host = HostId::Remote("studio".into());
    let call: Call = Arc::new(|_| {
        Ok(Reply::AgentHistory(Response::Resumed {
            session: SessionId(91),
        }))
    });
    panel.update(cx, |p, cx| {
        p.set_host(host.clone(), None, cx);
        p.set_call(call.clone());
        p.resume_vault(VaultHit::from(&sample()).target(), cx);
    });
    cx.run_until_parked();
    assert_eq!(*events.lock().unwrap(), [(host, SessionId(91))]);

    // A request from the old host must not publish into the new host's view.
    panel.update(cx, |p, cx| {
        p.resume(sample(), cx);
        p.set_host(HostId::Local, None, cx);
    });
    cx.run_until_parked();
    assert_eq!(events.lock().unwrap().len(), 1);
}

#[gpui::test]
fn history_target_reconnect_invalidates_same_host_cache(cx: &mut TestAppContext) {
    let panel = cx.new(|_| Panel::new(Arc::new(|_| unreachable!())));
    let target = HostTarget::remote("studio", "/tmp/history-old.sock".into());
    panel.update(cx, |p, cx| {
        p.set_host(target.host().clone(), Some(target.clone()), cx);
        p.data = Some(Snapshot {
            entries: vec![sample()],
            truncated: false,
            skipped: false,
        });
        let generation = p.generation;
        p.set_host(target.host().clone(), Some(target), cx);
        assert_eq!(p.generation, generation);
        assert_eq!(p.loaded_hits().len(), 1);
        let replacement = HostTarget::remote("studio", "/tmp/history-new.sock".into());
        p.set_host(replacement.host().clone(), Some(replacement), cx);
        assert!(p.loaded_hits().is_empty());
        assert_ne!(p.generation, generation);
    });
}

#[cfg(unix)]
#[test]
fn remote_history_transport_sends_scan_search_and_resume_to_forwarded_socket() {
    use krill_proto::ipc::{ClientHello, ServerHello};
    use krill_proto::os::net::UnixListener;

    let dir = std::env::temp_dir().join(format!("krill-history-wire-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("remote.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let requests = [
        Request::AgentHistory(Action::Scan),
        Request::AgentHistory(Action::Search(SearchQuery {
            query: "원격 검색".into(),
            paths: vec![sample().cwd],
            ..SearchQuery::default()
        })),
        Request::AgentHistory(Action::ResumeVault(VaultHit::from(&sample()).target())),
    ];
    let expected = requests.clone();
    let server = std::thread::spawn(move || {
        for request in expected {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .write_all(
                    &ServerHello {
                        proto_min: ipc::PROTO_MIN,
                        proto_max: ipc::PROTO_MAX,
                        features: FEAT_CONTROL | FEAT_AGENT_HISTORY | FEAT_AI_VAULT,
                        build: "remote-history-test".into(),
                        instance_id: 1,
                    }
                    .encode()
                    .unwrap(),
                )
                .unwrap();
            let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
            ClientHello::decode(kind, &body).unwrap();
            let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
            assert_eq!(Request::decode(kind, &body).unwrap(), request);
            stream
                .write_all(
                    &Reply::AgentHistory(Response::Resumed {
                        session: SessionId(91),
                    })
                    .encode()
                    .unwrap(),
                )
                .unwrap();
        }
    });
    let call = daemon_call_for(Some(HostTarget::remote("studio", socket)));
    for request in requests {
        assert_eq!(
            call(&request).unwrap(),
            Reply::AgentHistory(Response::Resumed {
                session: SessionId(91)
            })
        );
    }
    server.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
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
        cx.subscribe(&panel, move |_, event: &Open, _| {
            events.lock().unwrap().push(event.session);
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
            Request::AgentHistory(Action::Search(q)) => Response::Searched(SearchSnapshot {
                hits: hits_for_call
                    .iter()
                    .filter(|hit| {
                        (q.agents.is_empty() || q.agents.contains(&hit.agent))
                            && path_matches_scope(&hit.cwd, &q.paths)
                    })
                    .cloned()
                    .collect(),
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
        cx.subscribe(&panel, move |_, event: &Open, _| {
            events.lock().unwrap().push(event.session);
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

#[gpui::test]
fn history_rows_are_virtualized_and_same_count_replacements_refresh(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);
    let (panel, cx) = cx.add_window_view(|_, _| {
        let mut panel = Panel::new(Arc::new(|_| Err(io::ErrorKind::NotConnected.into())));
        panel.open = true;
        panel.data = Some(Snapshot {
            entries: (0..10_000)
                .map(|i| Entry {
                    agent: krill_proto::agent_history::Agent::Claude,
                    id: format!("conversation-{i}"),
                    cwd: "/repo".into(),
                    modified: 0,
                })
                .collect(),
            truncated: false,
            skipped: false,
        });
        panel
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-history-resume-0").is_some());
    assert!(cx.debug_bounds("agent-history-resume-9999").is_none());
    panel.update(cx, |panel, cx| {
        panel.data.as_mut().unwrap().entries[0].id = "replaced".into();
        cx.notify();
    });
    cx.run_until_parked();
    panel.read_with(cx, |panel, _| assert_eq!(panel.list_hits[0].id, "replaced"));
}

#[test]
fn server_search_hits_survive_incomplete_snippets_and_remote_path_spelling() {
    let mut panel = Panel::new(Arc::new(|_| unreachable!()));
    panel.query = "alpha beta".into();
    panel.scope = VaultScope::Workspace;
    panel.workspace_path = Some(r"C:\Repo".into());
    let mut hit = VaultHit::from(&sample());
    hit.cwd = "c:/repo/sub".into();
    hit.snippet = "alpha".into();
    panel.vault = Some(SearchSnapshot {
        hits: vec![hit.clone()],
        ..Default::default()
    });
    assert_eq!(panel.visible_hits(), vec![hit]);
}

#[gpui::test]
fn resumed_reply_is_not_dropped_when_a_search_is_pending(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let call: Call = Arc::new(|r| {
        Ok(Reply::AgentHistory(match r {
            Request::AgentHistory(Action::Scan) => Response::Scanned(Snapshot {
                entries: vec![sample()],
                truncated: false,
                skipped: false,
            }),
            Request::AgentHistory(Action::Search(_)) => Response::Searched(SearchSnapshot {
                hits: Vec::new(),
                truncated: false,
                skipped: false,
            }),
            _ => Response::Resumed {
                session: SessionId(5),
            },
        }))
    });
    let (panel, cx) = cx.add_window_view(|_, _| Panel::new(call));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let events = opened.clone();
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, event: &Open, _| {
            events.lock().unwrap().push(event.session);
        })
        .detach();
    });
    panel.update(cx, Panel::toggle);
    cx.run_until_parked();
    panel.update(cx, |p, cx| {
        p.resume(sample(), cx);
        p.set_query("typed while resuming", cx);
    });
    cx.run_until_parked();
    assert_eq!(*opened.lock().unwrap(), [SessionId(5)]);
}
