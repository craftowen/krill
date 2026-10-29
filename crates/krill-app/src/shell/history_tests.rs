use super::*;
use crate::shell::tests::{open, state_path};
use gpui::TestAppContext;
use krill_app::hosts::HostId;
use krill_proto::agent_history::{
    Action, Agent, Entry, Response, SearchSnapshot, Snapshot, VaultHit,
};
use krill_proto::control::{Reply, Request};
use std::sync::{Arc, Mutex};

#[gpui::test]
fn stale_cmd_j_selection_cannot_resume_on_new_history_host(cx: &mut TestAppContext) {
    let (shell, cx) = open(cx, state_path("stale-history-selection"));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    cx.update(|_, cx| {
        let history = shell.read(cx).history.clone();
        history.update(cx, |history, _| {
            history.set_call(Arc::new(move |request| {
                seen.lock().unwrap().push(request.clone());
                Ok(Reply::AgentHistory(Response::Scanned(Snapshot {
                    entries: vec![Entry {
                        agent: Agent::Codex,
                        id: "11111111-2222-4333-8444-555555555555".into(),
                        cwd: "/tmp/project".into(),
                        modified: 0,
                    }],
                    truncated: false,
                    skipped: false,
                })))
            }));
        });
    });
    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    let (palette, event) = cx.update(|_, cx| {
        let palette = shell.read(cx).jump.as_ref().unwrap().0.clone();
        let event = palette.read(cx).vault_event(0).unwrap();
        (palette, event)
    });
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            assert_eq!(palette.read(cx).vault_matches().count(), 1);
            shell.history.update(cx, |h, cx| {
                h.set_host(HostId::Remote("other".into()), None, cx);
            });
            // Deliberately select before the history observer clears the row.
            shell.on_palette_event(&palette, &event, window, cx);
        });
    });
    cx.run_until_parked();
    let seen = requests.clone();
    cx.update(|_, cx| {
        let history = shell.read(cx).history.clone();
        history.update(cx, |h, cx| {
            h.set_call(Arc::new(move |request| {
                seen.lock().unwrap().push(request.clone());
                Ok(Reply::AgentHistory(Response::Scanned(Snapshot {
                    entries: vec![Entry {
                        agent: Agent::Claude,
                        id: "22222222-2222-4333-8444-555555555555".into(),
                        cwd: "/tmp/project".into(),
                        modified: 0,
                    }],
                    truncated: false,
                    skipped: false,
                })))
            }));
            h.scan(cx);
        });
    });
    cx.run_until_parked();
    // Also deliver the old selection after the observer has replaced index 0.
    cx.update(|window, cx| {
        assert_ne!(palette.read(cx).vault_event(0).unwrap(), event);
        shell.update(cx, |shell, cx| {
            shell.on_palette_event(&palette, &event, window, cx);
        });
    });
    cx.run_until_parked();
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| { !matches!(request, Request::AgentHistory(Action::ResumeVault(_))) })
    );
}

#[gpui::test]
fn remote_history_panel_and_cmd_j_search_resume_on_source_host(cx: &mut TestAppContext) {
    let (shell, cx) = open(cx, state_path("remote-history-search"));
    let remote = HostId::Remote("studio".into());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let entry = Entry {
        agent: Agent::Codex,
        id: "11111111-2222-4333-8444-555555555555".into(),
        cwd: "/tmp/remote-project".into(),
        modified: 0,
    };
    let call: krill_app::agent_history::Call = Arc::new(move |request| {
        seen.lock().unwrap().push(request.clone());
        Ok(Reply::AgentHistory(match request {
            Request::AgentHistory(Action::Scan) => Response::Scanned(Snapshot {
                entries: vec![entry.clone()],
                truncated: false,
                skipped: false,
            }),
            Request::AgentHistory(Action::Search(query)) => {
                let mut hit = VaultHit::from(&entry);
                hit.title = query.query.clone();
                Response::Searched(SearchSnapshot {
                    hits: vec![hit],
                    truncated: false,
                    skipped: false,
                })
            }
            Request::AgentHistory(Action::ResumeVault(_)) => Response::Resumed {
                session: krill_proto::SessionId(91),
            },
            _ => unreachable!(),
        }))
    });
    shell.update(cx, |shell, cx| {
        // The same numeric ID already belongs to a local tab.
        let local = shell.layout.focused().unwrap();
        shell.layout.set_session(local, krill_proto::SessionId(91));
        shell.layout.new_tab();
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_host(pane, remote.clone());
        shell.layout.set_launch(
            pane,
            Create {
                account: None,
                program: Program::Login,
                cwd: b"/tmp/remote-project".to_vec(),
                cols: 80,
                rows: 24,
            },
        );
        shell.sync_diff_workspace(cx);
        assert_eq!(shell.history.read(cx).host(), &remote);
        shell.history.update(cx, |h, _| h.set_call(call));
        shell.toggle_history(cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-history-panel").is_some());
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).history.read(cx).visible_hits().len()),
        1
    );

    cx.simulate_keystrokes("cmd-j");
    cx.simulate_input("원격에서만 찾는 대화");
    cx.run_until_parked();
    assert!(requests.lock().unwrap().iter().any(|request| {
        matches!(request, Request::AgentHistory(Action::Search(query))
            if query.query == "원격에서만 찾는 대화")
    }));
    assert_eq!(
        cx.update(|_, cx| {
            shell
                .read(cx)
                .jump
                .as_ref()
                .unwrap()
                .0
                .read(cx)
                .vault_matches()
                .count()
        }),
        1
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    shell.read_with(cx, |shell, _| {
        let focused = shell.layout.focused().unwrap();
        assert_eq!(shell.layout.host(focused), &remote);
        assert_eq!(
            shell.layout.session(focused),
            Some(krill_proto::SessionId(91))
        );
        let local = shell.layout.tabs()[0].focused;
        assert_eq!(shell.layout.host(local), &HostId::Local);
        assert_eq!(
            shell.layout.session(local),
            Some(krill_proto::SessionId(91))
        );
    });
    shell.update(cx, |shell, cx| {
        shell.layout.activate(0);
        shell.sync_diff_workspace(cx);
        assert_eq!(shell.history.read(cx).host(), &HostId::Local);
        assert!(shell.history.read(cx).loaded_hits().is_empty());
    });
}

#[gpui::test]
fn selected_remote_history_scope_without_focused_pane_uses_remote_path(cx: &mut TestAppContext) {
    use krill_app::hosts::HostStatus;
    use krill_proto::agent_history::VaultScope;
    let (shell, cx) = open(cx, state_path("remote-history-no-pane"));
    shell.update(cx, |shell, cx| {
        shell.layout.close_tab(0);
        assert!(shell.layout.focused().is_none());
        shell.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "studio",
                HostStatus::Connected,
                sidebar::Snapshot {
                    sessions: Vec::new(),
                    trees: [("/remote/repo".into(), vec!["/remote/repo".into()])].into(),
                },
                cx,
            );
            sidebar.select_worktree(Some("/remote/repo".into()), cx);
        });
        shell
            .history
            .update(cx, |h, cx| h.set_scope(VaultScope::Workspace, cx));
        assert_eq!(shell.sync_history_host(cx).as_deref(), Some("/remote/repo"));
        let history = shell.history.read(cx);
        assert_eq!(history.host(), &HostId::Remote("studio".into()));
        assert_eq!(history.build_search_query().paths, ["/remote/repo"]);
    });
}
