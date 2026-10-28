use super::*;
use crate::sidebar::{HostStatus, Meta, Selected};
use gpui::{AppContext as _, TestAppContext};
use krill_proto::control::{AgentState, SessionAccount};
use krill_proto::usage::{AccountStatusInfo, SessionHandoff};
use std::cell::RefCell;
use std::rc::Rc;

fn accounts() -> AccountsSnapshot {
    AccountsSnapshot {
        accounts: ["main", "other"]
            .into_iter()
            .map(|id| AccountInfo {
                id: id.into(),
                provider: ProviderKind::Claude,
                label: id.into(),
                email: None,
                profile_dir: format!("/synthetic/{id}"),
                active: id == "main",
                host_default: false,
                stale: false,
                usage: usage::Snapshot::default(),
            })
            .collect(),
        ..AccountsSnapshot::default()
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        sessions: [10, 20, 30]
            .into_iter()
            .map(|id| {
                let mut s = Session {
                    id: SessionId(id),
                    status: krill_proto::control::AgentStatus {
                        state: AgentState::Done,
                        agent: "claude".into(),
                        ..krill_proto::control::AgentStatus::UNKNOWN
                    },
                    worktree: Some("/repo/tree".into()),
                    cwd: Some("/repo/tree".into()),
                    account: None,
                };
                s.account = Some(SessionAccount {
                    session: s.id,
                    account_id: "main".into(),
                    label: "Main".into(),
                });
                s
            })
            .collect(),
        trees: [("/repo".into(), vec!["/repo/tree".into()])].into(),
    }
}

fn row_sessions(sidebar: &Sidebar, host: &HostId) -> Vec<SessionId> {
    sidebar
        .rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            Row::Session(s, _) if sidebar.row_host(i) == host => Some(s.id),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn capability_pending_and_account_readiness_gate_session_profile_menu(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let view = cx.new(|_| Sidebar::new_loaded(None));
    view.update(cx, |s, cx| {
        s.apply(Ok((snapshot(), Meta::new())), BTreeMap::new(), cx);
        s.sync_session_accounts(&HostId::Local, accounts().into(), cx);
        let row = s.snapshot.sessions[0].clone();
        assert!(s.session_account_choices(&HostId::Local, &row).is_empty());
        s.set_session_account_features(
            HostId::Local,
            usage::FEAT_USAGE | usage::FEAT_SESSION_ACCOUNT_SWITCH | usage::FEAT_PROVIDER_DETAILS,
            cx,
        );
        assert_eq!(s.session_account_choices(&HostId::Local, &row).len(), 2);
        let mut snap = accounts();
        snap.accounts[1].host_default = true;
        snap.accounts[1].profile_dir.clear();
        s.sync_session_accounts(&HostId::Local, snap.clone().into(), cx);
        assert_eq!(
            s.session_account_choices(&HostId::Local, &row).len(),
            2,
            "host-default profiles remain usable without a managed profile directory"
        );
        snap.statuses.push(AccountStatusInfo {
            account_id: "other".into(),
            status: AccountStatus::NeedsLogin,
            updated_at: None,
            detail: None,
        });
        s.sync_session_accounts(&HostId::Local, snap.clone().into(), cx);
        assert_eq!(s.session_account_choices(&HostId::Local, &row).len(), 1);
        snap.details.handoffs.push(SessionHandoff {
            from: row.id,
            to: None,
            account_id: "other".into(),
            error: None,
        });
        s.sync_session_accounts(&HostId::Local, snap.into(), cx);
        assert!(s.session_account_choices(&HostId::Local, &row).is_empty());
    });
}

#[gpui::test]
fn clicked_session_profile_target_is_host_scoped_and_does_not_activate_a_row(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let view = cx.new(|_| Sidebar::new_loaded(None));
    let emitted = Rc::new(RefCell::new(Vec::new()));
    let selected = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        let emitted = emitted.clone();
        cx.subscribe(&view, move |_, e: &SwitchSessionAccount, _| {
            emitted.borrow_mut().push(e.clone());
        })
        .detach();
        let selected = selected.clone();
        cx.subscribe(&view, move |_, e: &Selected, _| {
            selected.borrow_mut().push(e.0);
        })
        .detach();
    });
    let host = HostId::Remote("studio".into());
    let target = SwitchSessionAccount {
        host: host.clone(),
        session: SessionId(20),
        provider: ProviderKind::Claude,
        account_id: "other".into(),
    };
    view.update(cx, |s, cx| {
        s.apply(Ok((snapshot(), Meta::new())), BTreeMap::new(), cx);
        s.set_remote_snapshot_for_test("studio", HostStatus::Connected, snapshot(), cx);
        s.sync_session_accounts(&host, accounts().into(), cx);
        s.set_session_account_features(
            host.clone(),
            usage::FEAT_USAGE | usage::FEAT_SESSION_ACCOUNT_SWITCH | usage::FEAT_PROVIDER_DETAILS,
            cx,
        );
        s.focused_host = HostId::Local;
        s.focused = Some(SessionId(10));
        s.switch_session_account(target.clone(), cx);
        assert!(s.session_account_pending(&host, SessionId(20)));
        s.switch_session_account(target.clone(), cx); // duplicate is suppressed until the reply
        s.finish_session_account_switch(&host, SessionId(20), cx);
        assert!(!s.session_account_pending(&host, SessionId(20)));
        assert_eq!(s.focused, Some(SessionId(10)));
        assert_eq!(s.focused_host, HostId::Local);
        assert!(s.action_host.is_none());
    });
    assert_eq!(*emitted.borrow(), vec![target]);
    assert!(selected.borrow().is_empty());
}

#[gpui::test]
fn replacement_preserves_sidebar_slot_cursor_and_host_across_poll_gap_and_chain(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let view = cx.new(|_| Sidebar::new_loaded(None));
    view.update(cx, |s, cx| {
        s.apply(Ok((snapshot(), Meta::new())), BTreeMap::new(), cx);
        s.set_remote_snapshot_for_test("studio", HostStatus::Connected, snapshot(), cx);
        s.toggle_card_sessions(std::path::Path::new("/repo/tree"), cx);
        let before = row_sessions(s, &HostId::Local);
        assert_eq!(before, vec![SessionId(10), SessionId(20), SessionId(30)]);
        let cursor = s
            .rows
            .iter()
            .position(|r| matches!(r, Row::Session(row, _) if row.id == SessionId(20)))
            .unwrap();
        s.cursor = Some(cursor);
        let mut snap = accounts();
        snap.details.handoffs.push(SessionHandoff {
            from: SessionId(20),
            to: Some(SessionId(90)),
            account_id: "other".into(),
            error: None,
        });
        s.sync_session_accounts(&HostId::Local, snap.clone().into(), cx);
        assert_eq!(
            row_sessions(s, &HostId::Local),
            vec![SessionId(10), SessionId(90), SessionId(30)]
        );
        assert_eq!(s.cursor, Some(cursor));
        assert_eq!(row_sessions(s, &HostId::Remote("studio".into())), before);
        let mut gap = snapshot();
        gap.sessions.retain(|s| s.id != SessionId(20));
        s.apply(Ok((gap, Meta::new())), BTreeMap::new(), cx);
        assert_eq!(
            row_sessions(s, &HostId::Local),
            vec![SessionId(10), SessionId(90), SessionId(30)]
        );
        let mut after = snapshot();
        after.sessions[1].id = SessionId(90);
        after.sessions.swap(1, 2); // IPC sorts by new id.
        s.apply(Ok((after, Meta::new())), BTreeMap::new(), cx);
        assert_eq!(
            row_sessions(s, &HostId::Local),
            vec![SessionId(10), SessionId(90), SessionId(30)]
        );
        snap.details.handoffs.push(SessionHandoff {
            from: SessionId(90),
            to: Some(SessionId(100)),
            account_id: "main".into(),
            error: None,
        });
        s.sync_session_accounts(&HostId::Local, snap.into(), cx);
        assert_eq!(
            row_sessions(s, &HostId::Local),
            vec![SessionId(10), SessionId(100), SessionId(30)]
        );
        // Once observed, genuinely removed replacement sessions must not be resurrected.
        let mut ended = snapshot();
        ended.sessions.retain(|s| s.id != SessionId(20));
        let mut replacement = ended.sessions[0].clone();
        replacement.id = SessionId(100);
        ended.sessions.push(replacement);
        s.apply(Ok((ended.clone(), Meta::new())), BTreeMap::new(), cx);
        ended.sessions.retain(|s| s.id != SessionId(100));
        s.apply(Ok((ended, Meta::new())), BTreeMap::new(), cx);
        assert_eq!(
            row_sessions(s, &HostId::Local),
            vec![SessionId(10), SessionId(30)]
        );
    });
}

#[gpui::test]
fn account_badge_opens_inline_menu_without_selecting_session(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, _| Sidebar::new_loaded(None));
    let switched = Rc::new(RefCell::new(Vec::new()));
    let selected = Rc::new(RefCell::new(0));
    vcx.update(|_, cx| {
        let switched = switched.clone();
        cx.subscribe(&view, move |_, e: &SwitchSessionAccount, _| {
            switched.borrow_mut().push(e.clone());
        })
        .detach();
        let selected = selected.clone();
        cx.subscribe(&view, move |_, _: &Selected, _| *selected.borrow_mut() += 1)
            .detach();
    });
    view.update(vcx, |s, cx| {
        s.apply(Ok((snapshot(), Meta::new())), BTreeMap::new(), cx);
        s.sync_session_accounts(&HostId::Local, accounts().into(), cx);
        s.set_session_account_features(
            HostId::Local,
            usage::FEAT_USAGE | usage::FEAT_SESSION_ACCOUNT_SWITCH | usage::FEAT_PROVIDER_DETAILS,
            cx,
        );
        s.toggle_card_sessions(std::path::Path::new("/repo/tree"), cx);
    });
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    let index = view.read_with(vcx, |s, _| {
        s.rows
            .iter()
            .position(|r| matches!(r, Row::Session(row, _) if row.id == SessionId(20)))
            .unwrap()
    });
    let button = vcx
        .debug_bounds(format!("account-badge-{index}").leak())
        .expect("account button visible");
    vcx.simulate_click(button.center(), gpui::Modifiers::default());
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(*selected.borrow(), 0);
    // PopupMenu's id is not a debug selector; selecting its first enabled item
    // exercises the real dropdown and its callback without relying on geometry.
    vcx.run_until_parked();
    vcx.simulate_keystrokes("up enter");
    assert_eq!(
        *switched.borrow(),
        vec![SwitchSessionAccount {
            host: HostId::Local,
            session: SessionId(20),
            provider: ProviderKind::Claude,
            account_id: "other".into(),
        }]
    );
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(view.read_with(vcx, |s, _| {
        s.session_account_pending(&HostId::Local, SessionId(20))
    }));
    assert!(!view.read_with(vcx, |s, _| {
        s.session_account_loading(&HostId::Local, SessionId(20))
    }));
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(view.read_with(vcx, |s, _| {
        s.session_account_loading(&HostId::Local, SessionId(20))
    }));
    let pending_button = vcx
        .debug_bounds(format!("account-badge-{index}").leak())
        .unwrap();
    assert_eq!(
        button.size, pending_button.size,
        "spinner reserves the idle chevron footprint"
    );
    assert_eq!(*selected.borrow(), 0);
}

#[gpui::test]
fn unseen_replacement_is_removed_after_one_fresh_poll_gap(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let view = cx.new(|_| Sidebar::new_loaded(None));
    view.update(cx, |s, cx| {
        s.apply(Ok((snapshot(), Meta::new())), BTreeMap::new(), cx);
        s.toggle_card_sessions(std::path::Path::new("/repo/tree"), cx);
        let mut snap = accounts();
        snap.details.handoffs.push(SessionHandoff {
            from: SessionId(20),
            to: Some(SessionId(90)),
            account_id: "other".into(),
            error: None,
        });
        s.sync_session_accounts(&HostId::Local, snap.into(), cx);
        let mut gap = snapshot();
        gap.sessions.retain(|s| s.id != SessionId(20));
        s.apply(Ok((gap.clone(), Meta::new())), BTreeMap::new(), cx);
        assert!(row_sessions(s, &HostId::Local).contains(&SessionId(90)));
        // Redraws and settings changes do not consume the status-poll grace.
        s.rebuild(cx);
        s.rebuild(cx);
        assert!(row_sessions(s, &HostId::Local).contains(&SessionId(90)));
        s.apply(Ok((gap, Meta::new())), BTreeMap::new(), cx);
        assert!(!row_sessions(s, &HostId::Local).contains(&SessionId(90)));
    });
}

#[test]
fn menu_names_distinguish_profiles_without_exposing_directories() {
    let mut profiles = accounts().accounts;
    for profile in &mut profiles {
        profile.email = Some("shared@example.test".into());
    }
    assert_eq!(
        account_label(&profiles[0], &profiles),
        "main · shared@example.test"
    );
    assert_eq!(
        account_label(&profiles[1], &profiles),
        "other · shared@example.test"
    );
    profiles[1].label = profiles[0].label.clone();
    assert_eq!(
        account_label(&profiles[0], &profiles),
        "main · shared@example.test · main"
    );
    assert_eq!(
        account_label(&profiles[1], &profiles),
        "main · shared@example.test · other"
    );
}

#[gpui::test]
fn menu_covers_every_live_provider_with_its_negotiated_requirements(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let view = cx.new(|_| Sidebar::new_loaded(None));
    view.update(cx, |s, cx| {
        s.apply(Ok((snapshot(), Meta::new())), BTreeMap::new(), cx);
        let mut row = s.snapshot.sessions[0].clone();
        for provider in ProviderKind::ALL {
            let mut snap = accounts();
            for account in &mut snap.accounts {
                account.provider = provider;
            }
            row.status.agent = provider.as_str().into();
            s.sync_session_accounts(&HostId::Local, snap.into(), cx);
            let required = usage::UsageRequest::SwitchSessionAccount {
                session: row.id,
                provider,
                account_id: "other".into(),
            }
            .required_features();
            s.set_session_account_features(HostId::Local, required, cx);
            assert_eq!(
                s.session_account_choices(&HostId::Local, &row).len(),
                2,
                "{provider:?}"
            );
            if provider == ProviderKind::Antigravity {
                s.set_session_account_features(
                    HostId::Local,
                    required & !usage::FEAT_USAGE_AGY,
                    cx,
                );
                assert!(s.session_account_choices(&HostId::Local, &row).is_empty());
            }
        }
        let mut removed = accounts();
        for account in &mut removed.accounts {
            account.provider = ProviderKind::Gemini;
        }
        s.sync_session_accounts(&HostId::Local, removed.into(), cx);
        s.set_session_account_features(HostId::Local, u64::MAX, cx);
        assert!(
            s.session_account_choices(&HostId::Local, &row).is_empty(),
            "retired provider stays hidden"
        );
    });
}

#[gpui::test]
fn unopened_remote_session_failure_stays_on_its_row_and_allows_retry(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let view = cx.new(|_| Sidebar::new_loaded(None));
    let emitted = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        let emitted = emitted.clone();
        cx.subscribe(&view, move |_, event: &SwitchSessionAccount, _| {
            emitted.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let host = HostId::Remote("unopened".into());
    view.update(cx, |s, cx| {
        s.apply(Ok((snapshot(), Meta::new())), BTreeMap::new(), cx);
        s.set_remote_snapshot_for_test("unopened", HostStatus::Connected, snapshot(), cx);
        s.focused_host = HostId::Local;
        s.focused = Some(SessionId(10));
        let mut failed = accounts();
        failed.details.handoffs.push(SessionHandoff {
            from: SessionId(20),
            to: None,
            account_id: "other".into(),
            error: Some("resume failed".into()),
        });
        s.sync_session_accounts(&host, failed.into(), cx);
        let target = SwitchSessionAccount {
            host: host.clone(),
            session: SessionId(20),
            provider: ProviderKind::Claude,
            account_id: "other".into(),
        };
        s.set_session_account_features(
            host.clone(),
            usage::UsageRequest::SwitchSessionAccount {
                session: target.session,
                provider: target.provider,
                account_id: target.account_id.clone(),
            }
            .required_features(),
            cx,
        );
        assert_eq!(
            s.session_account_error(&host, SessionId(20)),
            Some("resume failed")
        );
        assert_eq!(s.session_account_error(&HostId::Local, SessionId(20)), None);
        assert_eq!(s.session_account_error(&host, SessionId(10)), None);
        assert!(!s.session_account_pending(&host, SessionId(20)));
        assert_eq!(
            s.session_on(&host, SessionId(20))
                .unwrap()
                .account
                .as_ref()
                .unwrap()
                .account_id,
            "main"
        );
        s.switch_session_account(target, cx);
        assert!(
            s.session_account_pending(&host, SessionId(20)),
            "failed row can retry without being opened"
        );
        assert_eq!(s.focused_host, HostId::Local);
        assert_eq!(s.focused, Some(SessionId(10)));
    });
    assert_eq!(emitted.borrow().len(), 1);
    assert_eq!(emitted.borrow()[0].host, host);
    assert_eq!(emitted.borrow()[0].session, SessionId(20));
}

fn spinner_view(cx: &mut TestAppContext) -> gpui::Entity<Sidebar> {
    cx.update(gpui_component::init);
    let view = cx.new(|_| Sidebar::new_loaded(None));
    view.update(cx, |s, cx| {
        s.apply(Ok((snapshot(), Meta::new())), BTreeMap::new(), cx);
        s.sync_session_accounts(&HostId::Local, accounts().into(), cx);
        s.set_session_account_features(
            HostId::Local,
            usage::FEAT_USAGE | usage::FEAT_SESSION_ACCOUNT_SWITCH | usage::FEAT_PROVIDER_DETAILS,
            cx,
        );
    });
    view
}

fn spinner_request(s: &mut Sidebar, cx: &mut gpui::Context<Sidebar>) {
    s.switch_session_account(
        SwitchSessionAccount {
            host: HostId::Local,
            session: SessionId(20),
            provider: ProviderKind::Claude,
            account_id: "other".into(),
        },
        cx,
    );
}

fn spinner_handoff() -> AccountsSnapshot {
    let mut pending = accounts();
    pending.details.supported = true;
    pending.details.handoffs.push(SessionHandoff {
        from: SessionId(20),
        to: None,
        account_id: "other".into(),
        error: None,
    });
    pending
}

#[gpui::test]
fn spinner_grace_spans_api_and_handoff_and_notifies_without_another_poll(cx: &mut TestAppContext) {
    let view = spinner_view(cx);
    let notices = Rc::new(std::cell::Cell::new(0));
    cx.update(|cx| {
        let notices = notices.clone();
        cx.observe(&view, move |_, _| notices.set(notices.get() + 1))
            .detach();
    });
    view.update(cx, spinner_request);
    cx.run_until_parked();
    let generation = view.read_with(cx, |s, _| {
        s.profiles.hosts[&HostId::Local].spinner_grace[&SessionId(20)].generation
    });
    assert!(view.read_with(cx, |s, _| {
        s.session_account_pending(&HostId::Local, SessionId(20))
    }));
    assert!(!view.read_with(cx, |s, _| {
        s.session_account_loading(&HostId::Local, SessionId(20))
    }));
    cx.executor().advance_clock(Duration::from_millis(400));
    view.update(cx, |s, cx| {
        s.sync_session_accounts(&HostId::Local, spinner_handoff().into(), cx);
        s.finish_session_account_switch(&HostId::Local, SessionId(20), cx);
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    view.update(cx, |s, cx| {
        s.sync_session_accounts(&HostId::Local, spinner_handoff().into(), cx);
    });
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |s, _| s.profiles.hosts[&HostId::Local].spinner_grace
            [&SessionId(20)]
            .generation),
        generation,
        "API completion and repeated daemon snapshots keep the original deadline"
    );
    cx.executor().advance_clock(Duration::from_millis(199));
    cx.run_until_parked();
    assert!(!view.read_with(cx, |s, _| {
        s.session_account_loading(&HostId::Local, SessionId(20))
    }));
    assert_eq!(
        view.read_with(cx, |s, _| s.session_account_tooltip(
            &HostId::Local,
            SessionId(20),
            "Main"
        )),
        crate::i18n::catalog::sidebar::SESSION_ACCOUNT.format(&[("label", "Main")]),
        "999ms retains the neutral current-account tooltip"
    );
    let before = notices.get();
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    assert!(view.read_with(cx, |s, _| {
        s.session_account_loading(&HostId::Local, SessionId(20))
    }));
    assert_eq!(
        view.read_with(cx, |s, _| s.session_account_tooltip(
            &HostId::Local,
            SessionId(20),
            "Main"
        )),
        crate::i18n::catalog::settings::ACCOUNT_SESSION_SWITCHING.get(),
        "progress text shares the spinner's one-second threshold"
    );
    assert!(
        notices.get() > before,
        "the one-second timer notifies without a fresh snapshot"
    );
}

#[gpui::test]
fn spinner_grace_fresh_request_rejects_the_previous_deadline(cx: &mut TestAppContext) {
    let view = spinner_view(cx);
    view.update(cx, spinner_request);
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    view.update(cx, |s, cx| {
        s.finish_session_account_switch(&HostId::Local, SessionId(20), cx);
        let mut failed = spinner_handoff();
        failed.details.handoffs[0].error = Some("previous attempt failed".into());
        s.sync_session_accounts(&HostId::Local, failed.into(), cx);
        spinner_request(s, cx);
        assert_eq!(
            s.session_account_tooltip(&HostId::Local, SessionId(20), "Main"),
            crate::i18n::catalog::sidebar::SESSION_ACCOUNT.format(&[("label", "Main")]),
            "retry grace suppresses the old error and progress text"
        );
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(600)); // old request's deadline
    cx.run_until_parked();
    assert!(!view.read_with(cx, |s, _| {
        s.session_account_loading(&HostId::Local, SessionId(20))
    }));
    cx.executor().advance_clock(Duration::from_millis(399));
    cx.run_until_parked();
    assert!(!view.read_with(cx, |s, _| {
        s.session_account_loading(&HostId::Local, SessionId(20))
    }));
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    assert!(view.read_with(cx, |s, _| {
        s.session_account_loading(&HostId::Local, SessionId(20))
    }));
}

#[gpui::test]
fn spinner_grace_completed_request_has_no_late_spinner_or_notification(cx: &mut TestAppContext) {
    let view = spinner_view(cx);
    let notices = Rc::new(std::cell::Cell::new(0));
    cx.update(|cx| {
        let notices = notices.clone();
        cx.observe(&view, move |_, _| notices.set(notices.get() + 1))
            .detach();
    });
    view.update(cx, spinner_request);
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(200));
    view.update(cx, |s, cx| {
        s.finish_session_account_switch(&HostId::Local, SessionId(20), cx);
    });
    cx.run_until_parked();
    let before = notices.get();
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert!(!view.read_with(cx, |s, _| {
        s.session_account_pending(&HostId::Local, SessionId(20))
    }));
    assert!(!view.read_with(cx, |s, _| {
        s.session_account_loading(&HostId::Local, SessionId(20))
    }));
    assert_eq!(
        notices.get(),
        before,
        "settled requests cancel their delayed notification"
    );
}

#[gpui::test]
fn spinner_grace_global_handoff_is_host_and_session_scoped(cx: &mut TestAppContext) {
    let view = spinner_view(cx);
    view.update(cx, spinner_request);
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(250));
    let remote = HostId::Remote("global".into());
    view.update(cx, |s, cx| {
        s.set_remote_snapshot_for_test("global", HostStatus::Connected, snapshot(), cx);
        s.sync_session_accounts(&remote, spinner_handoff().into(), cx);
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(750));
    cx.run_until_parked();
    assert!(view.read_with(cx, |s, _| {
        s.session_account_loading(&HostId::Local, SessionId(20))
    }));
    assert!(!view.read_with(cx, |s, _| s.session_account_loading(&remote, SessionId(20))));
    assert!(!view.read_with(cx, |s, _| {
        s.session_account_loading(&HostId::Local, SessionId(30))
    }));
    cx.executor().advance_clock(Duration::from_millis(250));
    cx.run_until_parked();
    assert!(view.read_with(cx, |s, _| s.session_account_loading(&remote, SessionId(20))));
    view.update(cx, |s, cx| {
        s.sync_session_accounts(&remote, accounts().into(), cx);
    });
    assert!(!view.read_with(cx, |s, _| s.session_account_loading(&remote, SessionId(20))));
    assert!(view.read_with(cx, |s, _| {
        s.session_account_loading(&HostId::Local, SessionId(20))
    }));
}
