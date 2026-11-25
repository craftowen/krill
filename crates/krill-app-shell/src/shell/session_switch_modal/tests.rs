use std::io::Write as _;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use krill_proto::ipc::{self, ClientHello, ServerHello};
use krill_proto::screen::Msg as _;
use krill_proto::usage::{AccountInfo, FEAT_USAGE, Snapshot, UsageReply};

use super::*;

const PREV: SessionId = SessionId(11);
const ELSEWHERE: SessionId = SessionId(12);
const CANT: [(SessionId, CantMoveReason); 5] = [
    (SessionId(21), CantMoveReason::NoSession),
    (SessionId(22), CantMoveReason::UnknownAccount),
    (SessionId(23), CantMoveReason::OwnFolder),
    (SessionId(24), CantMoveReason::SharedSession),
    (SessionId(25), CantMoveReason::PendingHandoff),
];
const NEW_FEATURES: u64 = FEAT_USAGE | FEAT_PROVIDER_DETAILS | FEAT_ACCOUNT_SWITCH_CONTROL;

fn account(id: &str, active: bool) -> AccountInfo {
    AccountInfo {
        id: id.into(),
        provider: ProviderKind::Codex,
        label: id.into(),
        email: Some(format!("{id}@example.com")),
        profile_dir: format!("/tmp/krill-switch/{id}"),
        active,
        host_default: false,
        stale: false,
        usage: Snapshot::default(),
    }
}

fn candidate(session: SessionId, group: SessionSwitchGroup, busy: bool) -> SessionSwitchCandidate {
    SessionSwitchCandidate {
        session,
        cwd: format!("/work/repo-{}", session.0 % 2),
        agent: "codex".into(),
        current_account_id: "old".into(),
        current_account_label: "old@example.com".into(),
        busy,
        group,
    }
}

fn plan() -> SessionSwitchPlan {
    let mut candidates = vec![
        candidate(PREV, SessionSwitchGroup::OnPreviousAccount, true),
        candidate(ELSEWHERE, SessionSwitchGroup::Elsewhere, false),
    ];
    candidates.extend(
        CANT.iter()
            .map(|(id, reason)| candidate(*id, SessionSwitchGroup::CantMove(*reason), false)),
    );
    SessionSwitchPlan {
        provider: ProviderKind::Codex,
        previous_account_id: Some("old".into()),
        target_account_id: "new".into(),
        candidates,
    }
}

type Log = Arc<Mutex<Vec<UsageRequest>>>;

/// Records every request; answers inspection with `plan` and everything else with accounts.
fn fake_daemon(tag: &str, features: u64, plan: SessionSwitchPlan) -> (std::path::PathBuf, Log) {
    let sock = std::path::PathBuf::from(format!("/tmp/kr-ssw-{tag}-{}.s", std::process::id()));
    std::fs::remove_file(&sock).ok();
    let listener = UnixListener::bind(&sock).unwrap();
    let log: Log = Arc::default();
    let server_log = Arc::clone(&log);
    std::thread::spawn(move || {
        let hello = ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features,
            build: "test".to_owned(),
            instance_id: 4242,
        };
        while let Ok((mut stream, _)) = listener.accept() {
            if stream.write_all(&hello.encode().unwrap()).is_err() {
                break;
            }
            let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) else {
                continue;
            };
            let Ok(client) = ClientHello::decode(kind, &body) else {
                continue;
            };
            let negotiated = ipc::negotiate(&hello, &client).unwrap().features;
            let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) else {
                continue;
            };
            let Ok(req) = UsageRequest::decode(kind, &body) else {
                continue;
            };
            let reply = if matches!(req, UsageRequest::InspectSessionSwitches { .. }) {
                UsageReply::SessionSwitchPlan(plan.clone())
            } else {
                UsageReply::Accounts(AccountsSnapshot {
                    accounts: vec![account("old", true), account("new", false)],
                    ..Default::default()
                })
            };
            server_log.lock().unwrap().push(req);
            stream
                .write_all(&reply.for_features(negotiated).encode().unwrap())
                .ok();
        }
    });
    (sock, log)
}

struct Fixture {
    shell: Entity<Shell>,
    handle: gpui::AnyWindowHandle,
    host: HostId,
    log: Log,
    state: std::path::PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.state).ok();
    }
}

fn setup(
    cx: &mut TestAppContext,
    tag: &str,
    features: u64,
    policy: RunningSessionSwitchPolicy,
    plan: SessionSwitchPlan,
) -> Fixture {
    let state = std::env::temp_dir().join(format!("kr-ssw-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&state).ok();
    std::fs::create_dir_all(&state).unwrap();
    let (sock, log) = fake_daemon(tag, features, plan);
    let settings_dir = state.clone();
    cx.update(|cx| {
        gpui_component::init(cx);
        super::super::bind_keys(cx);
        krill_app_core::settings::init_at(Some(settings_dir), cx);
        krill_app_core::settings::set_running_session_switch_policy(policy, cx);
    });
    let window = cx.add_window(|window, cx| {
        let shell = Shell::new(None, cx);
        window.focus(shell.focus_handle(), cx);
        shell
    });
    let shell = window.update(cx, |_, _, cx| cx.entity()).unwrap();
    let handle: gpui::AnyWindowHandle = window.into();
    // Startup host sync drops targets it doesn't know; register ours afterwards.
    cx.run_until_parked();
    let host = HostId::Remote(format!("iroh:{tag}"));
    let target = HostTarget::remote(format!("iroh:{tag}"), sock);
    shell.update(cx, |s, cx| {
        s.host_targets.insert(host.clone(), target);
        s.host_statuses.push((
            krill_app_core::hosts::RemoteHostSpec {
                id: format!("iroh:{tag}"),
                label: tag.to_owned(),
                address: format!("iroh:{tag}"),
                port: None,
                identity_file: None,
                remote_socket: None,
            },
            krill_app_core::hosts::HostStatus::Connected,
        ));
        s.remote_accounts.insert(
            host.clone(),
            std::sync::Arc::new(AccountsSnapshot {
                accounts: vec![account("old", true), account("new", false)],
                ..Default::default()
            }),
        );
        s.sidebar.update(cx, |sb, cx| {
            sb.set_session_account_features(host.clone(), features, cx);
        });
    });
    cx.run_until_parked();
    Fixture {
        shell,
        handle,
        host,
        log,
        state,
    }
}

fn switch(cx: &mut TestAppContext, f: &Fixture) {
    let host = f.host.clone();
    f.shell.update(cx, |s, cx| {
        s.switch_provider_account(host, ProviderKind::Codex, "new".into(), cx);
    });
    cx.run_until_parked();
}

fn requests(f: &Fixture) -> Vec<UsageRequest> {
    f.log.lock().unwrap().clone()
}

fn is_switch_only(req: &UsageRequest) -> bool {
    matches!(req, UsageRequest::SwitchAccountOnly { account_id, .. } if account_id == "new")
}

fn is_inspect(req: &UsageRequest) -> bool {
    matches!(
        req,
        UsageRequest::InspectSessionSwitches { previous_account_id: Some(prev), target_account_id, .. }
            if prev == "old" && target_account_id == "new"
    )
}

fn selected_sessions(reqs: &[UsageRequest]) -> Vec<Vec<SessionId>> {
    reqs.iter()
        .filter_map(|r| match r {
            UsageRequest::SwitchSelectedSessions {
                account_id,
                sessions,
                interrupt_busy: true,
                ..
            } if account_id == "new" => Some(sessions.clone()),
            _ => None,
        })
        .collect()
}

fn modal_open(cx: &mut TestAppContext, f: &Fixture) -> bool {
    f.shell.read_with(cx, |s, _| s.session_switch.is_some())
}

#[gpui::test]
fn session_switch_keep_policy_only_changes_default(cx: &mut TestAppContext) {
    let f = setup(
        cx,
        "keep",
        NEW_FEATURES,
        RunningSessionSwitchPolicy::Keep,
        plan(),
    );
    switch(cx, &f);
    let reqs = requests(&f);
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    assert!(is_switch_only(&reqs[0]), "{reqs:?}");
    assert!(!modal_open(cx, &f));
}

#[gpui::test]
fn session_switch_restart_policy_moves_every_movable_session(cx: &mut TestAppContext) {
    let f = setup(
        cx,
        "restart",
        NEW_FEATURES,
        RunningSessionSwitchPolicy::Restart,
        plan(),
    );
    switch(cx, &f);
    let reqs = requests(&f);
    assert!(reqs.iter().any(is_switch_only), "{reqs:?}");
    assert!(reqs.iter().any(is_inspect), "{reqs:?}");
    assert_eq!(selected_sessions(&reqs), vec![vec![PREV, ELSEWHERE]]);
    assert!(
        !reqs
            .iter()
            .any(|r| matches!(r, UsageRequest::SwitchAccount { .. }))
    );
    assert!(!modal_open(cx, &f));
}

#[gpui::test]
fn session_switch_ask_policy_opens_modal_with_previous_account_checked(cx: &mut TestAppContext) {
    let f = setup(
        cx,
        "ask",
        NEW_FEATURES,
        RunningSessionSwitchPolicy::Ask,
        plan(),
    );
    switch(cx, &f);
    let reqs = requests(&f);
    assert!(reqs.iter().any(is_switch_only), "{reqs:?}");
    assert!(reqs.iter().any(is_inspect), "{reqs:?}");
    assert!(selected_sessions(&reqs).is_empty(), "{reqs:?}");
    f.shell.read_with(cx, |s, _| {
        let modal = s.session_switch.as_ref().unwrap();
        assert_eq!(modal.selected, BTreeSet::from([PREV]));
        assert_eq!(modal.previous_account_label, "old@example.com");
        assert_eq!(modal.next_account_label, "new@example.com");
        assert!(!modal.review);
    });
}

#[gpui::test]
fn session_switch_old_daemon_falls_back_to_switch_account(cx: &mut TestAppContext) {
    let f = setup(
        cx,
        "legacy",
        FEAT_USAGE | FEAT_PROVIDER_DETAILS,
        RunningSessionSwitchPolicy::Ask,
        plan(),
    );
    switch(cx, &f);
    let reqs = requests(&f);
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    assert!(
        matches!(&reqs[0], UsageRequest::SwitchAccount { account_id, .. } if account_id == "new"),
        "{reqs:?}"
    );
    assert!(!modal_open(cx, &f));
}

fn open_modal(
    cx: &mut TestAppContext,
    f: &Fixture,
    review: bool,
) -> &'static mut VisualTestContext {
    let host = f.host.clone();
    f.shell.update(cx, |s, cx| {
        s.open_session_switch_modal(host, plan(), review, cx);
    });
    let visual = VisualTestContext::from_window(f.handle, cx).into_mut();
    visual.simulate_resize(gpui::size(px(1200.), px(1400.)));
    visual.run_until_parked();
    visual
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} must render"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
}

fn selected(visual: &mut VisualTestContext, f: &Fixture) -> BTreeSet<SessionId> {
    f.shell.read_with(visual, |s, _| {
        s.session_switch
            .as_ref()
            .map(|m| m.selected.clone())
            .unwrap_or_default()
    })
}

#[gpui::test]
fn session_switch_modal_renders_groups_reasons_and_states(cx: &mut TestAppContext) {
    let f = setup(
        cx,
        "render",
        NEW_FEATURES,
        RunningSessionSwitchPolicy::Ask,
        plan(),
    );
    let visual = open_modal(cx, &f, false);
    for selector in [
        "session-switch-modal",
        "session-switch-detail",
        "session-switch-group-previous",
        "session-switch-group-elsewhere",
        "session-switch-group-cant-move",
        "session-switch-state-11-busy",
        "session-switch-state-12-idle",
        "session-switch-reason-21-no-session",
        "session-switch-reason-22-unknown-account",
        "session-switch-reason-23-own-folder",
        "session-switch-reason-24-shared-session",
        "session-switch-reason-25-pending-handoff",
    ] {
        assert!(visual.debug_bounds(selector).is_some(), "{selector}");
    }
    assert!(
        visual
            .debug_bounds("session-switch-detail-review")
            .is_none()
    );
    assert_eq!(selected(visual, &f), BTreeSet::from([PREV]));

    // Can't-move rows and their header stay unselectable.
    click(visual, "session-switch-row-21");
    click(visual, "session-switch-group-cant-move");
    assert_eq!(selected(visual, &f), BTreeSet::from([PREV]));

    click(visual, "session-switch-group-elsewhere");
    assert_eq!(selected(visual, &f), BTreeSet::from([PREV, ELSEWHERE]));
    click(visual, "session-switch-group-previous");
    assert_eq!(selected(visual, &f), BTreeSet::from([ELSEWHERE]));
    click(visual, "session-switch-row-12");
    assert!(selected(visual, &f).is_empty());

    // Nothing selected: the restart button is inert.
    click(visual, "session-switch-restart");
    assert!(f.shell.read_with(visual, |s, _| s.session_switch.is_some()));
    assert!(requests(&f).is_empty());
}

#[gpui::test]
fn session_switch_modal_restart_sends_selection_and_saves_dont_ask(cx: &mut TestAppContext) {
    let f = setup(
        cx,
        "confirm",
        NEW_FEATURES,
        RunningSessionSwitchPolicy::Ask,
        plan(),
    );
    let visual = open_modal(cx, &f, false);
    click(visual, "session-switch-row-12");
    click(visual, "session-switch-dont-ask");
    click(visual, "session-switch-restart");
    assert!(f.shell.read_with(visual, |s, _| s.session_switch.is_none()));
    assert_eq!(
        selected_sessions(&requests(&f)),
        vec![vec![PREV, ELSEWHERE]]
    );
    visual.update(|_, cx| {
        assert_eq!(
            krill_app_core::settings::running_session_switch_policy(cx),
            RunningSessionSwitchPolicy::Restart
        );
    });
}

#[gpui::test]
fn session_switch_modal_keep_closes_without_switching(cx: &mut TestAppContext) {
    let f = setup(
        cx,
        "close",
        NEW_FEATURES,
        RunningSessionSwitchPolicy::Ask,
        plan(),
    );
    let visual = open_modal(cx, &f, false);
    click(visual, "session-switch-dont-ask");
    click(visual, "session-switch-keep");
    assert!(f.shell.read_with(visual, |s, _| s.session_switch.is_none()));
    assert!(requests(&f).is_empty());
    visual.update(|_, cx| {
        assert_eq!(
            krill_app_core::settings::running_session_switch_policy(cx),
            RunningSessionSwitchPolicy::Ask
        );
    });
}

#[gpui::test]
fn session_switch_modal_review_mode_and_empty_plan(cx: &mut TestAppContext) {
    let f = setup(
        cx,
        "review",
        NEW_FEATURES,
        RunningSessionSwitchPolicy::Ask,
        plan(),
    );
    let host = f.host.clone();
    f.shell.update(cx, |s, cx| {
        let mut empty = plan();
        empty.candidates.clear();
        s.open_session_switch_modal(host, empty, false, cx);
    });
    assert!(!modal_open(cx, &f), "an empty plan never opens the modal");
    let visual = open_modal(cx, &f, true);
    assert!(
        visual
            .debug_bounds("session-switch-detail-review")
            .is_some()
    );
    assert!(visual.debug_bounds("session-switch-detail").is_none());
}
