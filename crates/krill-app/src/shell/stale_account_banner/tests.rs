use super::*;
use crate::shell::tests::{open, state_path};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use krill_app::hosts::{HostStatus, HostTarget};
use krill_proto::SessionId;
use krill_proto::control::{AgentState, AgentStatus, SessionAccount};
use krill_proto::ipc::{self, ClientHello, ServerHello};
use krill_proto::screen::Msg as _;
use krill_proto::usage::{
    AccountInfo, AccountStatus, AccountStatusInfo, AccountsSnapshot, FEAT_PROVIDER_DETAILS,
    FEAT_SESSION_ACCOUNT_SWITCH, FEAT_USAGE, ProviderKind, SessionHandoff, UsageReply,
    UsageRequest,
};
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

const SESSION: SessionId = SessionId(410);
const FEATURES: u64 = FEAT_USAGE | FEAT_SESSION_ACCOUNT_SWITCH | FEAT_PROVIDER_DETAILS;

fn studio() -> HostId {
    HostId::Remote("studio".into())
}

fn accounts(active: &str) -> AccountsSnapshot {
    AccountsSnapshot {
        accounts: ["old", "new", "third"]
            .into_iter()
            .map(|id| AccountInfo {
                id: id.into(),
                provider: ProviderKind::Claude,
                label: id.into(),
                email: Some(format!("{id}@example.test")),
                profile_dir: format!("/synthetic/{id}"),
                active: id == active,
                host_default: false,
                stale: false,
                usage: krill_proto::usage::Snapshot::default(),
            })
            .collect(),
        ..AccountsSnapshot::default()
    }
}

/// A live remote pane whose session runs on `old`; the caller keeps the peer open.
fn setup<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
) -> (Entity<Shell>, &'a mut VisualTestContext, PaneId, UnixStream) {
    let (view, cx) = open(cx, state_path(name));
    let (ours, peer) = UnixStream::pair().unwrap();
    let pane = view.update(cx, |s, cx| {
        let pane = s.layout.focused().unwrap();
        s.layout.set_host(pane, studio());
        s.layout.set_session(pane, SESSION);
        s.attach_test(pane, SESSION, ours, cx);
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "studio",
                HostStatus::Connected,
                sidebar::Snapshot {
                    sessions: vec![sidebar::Session {
                        id: SESSION,
                        status: AgentStatus {
                            state: AgentState::Working,
                            agent: "claude".into(),
                            ..AgentStatus::UNKNOWN
                        },
                        worktree: None,
                        cwd: None,
                        account: Some(SessionAccount {
                            session: SESSION,
                            account_id: "old".into(),
                            label: "Old".into(),
                        }),
                    }],
                    trees: std::collections::BTreeMap::new(),
                },
                cx,
            );
            sidebar.set_session_account_features(studio(), FEATURES, cx);
        });
        pane
    });
    cx.run_until_parked();
    (view, cx, pane, peer)
}

fn apply(view: &Entity<Shell>, cx: &mut VisualTestContext, host: HostId, snap: AccountsSnapshot) {
    view.update(cx, |s, cx| {
        s.apply_accounts_snapshot_for_host(host, snap, cx);
    });
    cx.run_until_parked();
}

fn shown(cx: &mut VisualTestContext, pane: PaneId) -> bool {
    let switch = cx
        .debug_bounds(Box::leak(
            format!("stale-account-switch-{}", pane.get()).into_boxed_str(),
        ))
        .is_some();
    let dismiss = cx
        .debug_bounds(Box::leak(
            format!("stale-account-dismiss-{}", pane.get()).into_boxed_str(),
        ))
        .is_some();
    assert_eq!(switch, dismiss, "both banner actions appear together");
    switch
}

fn click(cx: &mut VisualTestContext, selector: String) {
    let at = cx
        .debug_bounds(Box::leak(selector.into_boxed_str()))
        .unwrap()
        .center();
    cx.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
}

#[gpui::test]
fn stale_account_banner_follows_the_hosts_active_account(cx: &mut TestAppContext) {
    let (view, cx, pane, _peer) = setup(cx, "stale-account-shown");
    apply(&view, cx, studio(), accounts("old"));
    assert!(!shown(cx, pane), "matching accounts need no banner");

    apply(&view, cx, studio(), accounts("new"));
    assert!(shown(cx, pane));

    let mut handoff = accounts("new");
    handoff.details.supported = true;
    handoff.details.handoffs.push(SessionHandoff {
        from: SESSION,
        to: None,
        account_id: "new".into(),
        error: None,
    });
    apply(&view, cx, studio(), handoff);
    assert!(
        !shown(cx, pane),
        "a running handoff already moves the session"
    );

    for status in [AccountStatus::RateLimited, AccountStatus::NeedsLogin] {
        let mut blocked = accounts("new");
        blocked.statuses.push(AccountStatusInfo {
            account_id: "new".into(),
            status,
            updated_at: None,
            detail: None,
        });
        apply(&view, cx, studio(), blocked);
        assert!(
            !shown(cx, pane),
            "{status:?} target cannot take the session"
        );
    }

    apply(&view, cx, studio(), accounts("new"));
    assert!(shown(cx, pane));
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_session_account_features(studio(), FEAT_USAGE, cx);
        });
    });
    cx.run_until_parked();
    assert!(!shown(cx, pane), "hosts without session switching hide it");
}

#[gpui::test]
fn stale_account_banner_reads_only_its_own_hosts_snapshot(cx: &mut TestAppContext) {
    let (view, cx, pane, _peer) = setup(cx, "stale-account-remote");
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_session_account_features(HostId::Local, FEATURES, cx);
        });
    });
    apply(&view, cx, HostId::Local, accounts("new"));
    assert!(
        !shown(cx, pane),
        "the local snapshot says nothing about studio"
    );
    apply(&view, cx, studio(), accounts("new"));
    assert!(shown(cx, pane));
}

#[gpui::test]
fn keeping_the_account_hides_until_a_newer_target_appears(cx: &mut TestAppContext) {
    let (view, cx, pane, _peer) = setup(cx, "stale-account-dismiss");
    apply(&view, cx, studio(), accounts("new"));
    click(cx, format!("stale-account-dismiss-{}", pane.get()));
    assert!(!shown(cx, pane));
    apply(&view, cx, studio(), accounts("new"));
    assert!(!shown(cx, pane), "the same target stays dismissed");
    apply(&view, cx, studio(), accounts("third"));
    assert!(shown(cx, pane), "a different active account asks again");
}

#[gpui::test]
fn switch_sends_the_session_to_the_active_account(cx: &mut TestAppContext) {
    let root = std::env::temp_dir().join(format!("stale-account-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let socket = root.join("daemon.sock");
    let listener = krill_proto::os::net::UnixListener::bind(&socket).unwrap();
    let captured = Arc::new(Mutex::new(None));
    let capture = captured.clone();
    // Other account reads may reach the daemon first; answer each until the switch arrives.
    let server = std::thread::spawn(move || {
        for _ in 0..8 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let hello = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: FEATURES,
                build: "synthetic".into(),
                instance_id: 9996,
            };
            stream.write_all(&hello.encode().unwrap()).unwrap();
            let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
            let client = ClientHello::decode(kind, &body).unwrap();
            let negotiated = ipc::negotiate(&hello, &client).unwrap();
            let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) else {
                continue;
            };
            let request = UsageRequest::decode(kind, &body);
            let reply = UsageReply::Accounts(accounts("new")).for_features(negotiated.features);
            stream.write_all(&reply.encode().unwrap()).ok();
            if let Ok(request @ UsageRequest::SwitchSessionAccount { .. }) = request {
                *capture.lock().unwrap() = Some(request);
                return;
            }
        }
    });
    let (view, cx, pane, _peer) = setup(cx, "stale-account-switch");
    view.update(cx, |s, _| {
        s.host_targets
            .insert(studio(), HostTarget::remote("studio", socket));
    });
    apply(&view, cx, studio(), accounts("new"));
    click(cx, format!("stale-account-switch-{}", pane.get()));
    server.join().unwrap();
    cx.run_until_parked();
    assert_eq!(
        *captured.lock().unwrap(),
        Some(UsageRequest::SwitchSessionAccount {
            session: SESSION,
            provider: ProviderKind::Claude,
            account_id: "new".into(),
        })
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn switching_banner_shows_in_place_progress_while_pending_and_clears_after_rebind(
    cx: &mut TestAppContext,
) {
    let (view, cx, pane, _peer) = setup(cx, "stale-account-switching-banner");
    view.update(cx, |s, _| {
        s.host_targets.insert(
            studio(),
            HostTarget::remote(
                "studio",
                state_path("stale-account-switching-banner").join("daemon.sock"),
            ),
        );
    });
    apply(&view, cx, studio(), accounts("new"));
    assert!(shown(cx, pane));
    let switching_sel: &'static str =
        Box::leak(format!("account-switching-banner-{}", pane.get()).into_boxed_str());
    assert!(cx.debug_bounds(switching_sel).is_none());

    // Pending handoff replaces action buttons with an in-place switching notice in the same pane.
    let mut handoff = accounts("new");
    handoff.details.supported = true;
    handoff.details.handoffs.push(SessionHandoff {
        from: SESSION,
        to: None,
        account_id: "new".into(),
        error: None,
    });
    apply(&view, cx, studio(), handoff);
    assert!(!shown(cx, pane));
    assert!(
        cx.debug_bounds(switching_sel).is_some(),
        "in-place switching status banner must appear while handoff is active"
    );

    // Once rebound to the replacement running on "new", both banners clear in the same pane.
    let replacement = SessionId(411);
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "studio",
                HostStatus::Connected,
                sidebar::Snapshot {
                    sessions: vec![sidebar::Session {
                        id: replacement,
                        status: AgentStatus {
                            state: AgentState::Done,
                            agent: "claude".into(),
                            ..AgentStatus::UNKNOWN
                        },
                        worktree: None,
                        cwd: None,
                        account: Some(SessionAccount {
                            session: replacement,
                            account_id: "new".into(),
                            label: "New".into(),
                        }),
                    }],
                    trees: std::collections::BTreeMap::new(),
                },
                cx,
            );
        });
    });
    let mut done = accounts("new");
    done.details.supported = true;
    done.details.handoffs.push(SessionHandoff {
        from: SESSION,
        to: Some(replacement),
        account_id: "new".into(),
        error: None,
    });
    apply(&view, cx, studio(), done);
    assert!(!shown(cx, pane));
    assert!(
        cx.debug_bounds(switching_sel).is_none(),
        "switching status banner must disappear once rebound onto target account"
    );
    view.read_with(cx, |s, _| {
        assert_eq!(s.layout.session(pane), Some(replacement));
    });
}
