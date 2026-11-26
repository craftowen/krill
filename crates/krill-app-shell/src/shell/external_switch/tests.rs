use super::*;
use crate::shell::tests::{open, state_path};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use krill_proto::SessionId;
use krill_proto::ipc::{ClientHello, ServerHello};
use krill_proto::screen::Msg as _;
use krill_proto::usage::{
    AccountInfo, AutoSwitchEvent, CantMoveReason, FEAT_ACCOUNT_SWITCH_CONTROL,
    FEAT_PROVIDER_DETAILS, FEAT_USAGE, SessionSwitchCandidate, Snapshot, SwitchRecord, UsageReply,
    UsageRequest,
};
use std::io::Write as _;
use std::sync::Mutex;

type Requests = Arc<Mutex<Vec<UsageRequest>>>;

fn snapshot(active: &str) -> AccountsSnapshot {
    let account = |id: &str| AccountInfo {
        id: id.into(),
        provider: ProviderKind::Claude,
        label: id.to_uppercase(),
        email: None,
        profile_dir: format!("/tmp/{id}"),
        active: id == active,
        host_default: false,
        stale: false,
        usage: Snapshot::default(),
    };
    AccountsSnapshot {
        accounts: vec![account("a"), account("b")],
        ..AccountsSnapshot::default()
    }
}

fn candidate(session: u64, group: SessionSwitchGroup) -> SessionSwitchCandidate {
    SessionSwitchCandidate {
        session: SessionId(session),
        cwd: "/tmp".into(),
        agent: "claude".into(),
        current_account_id: "a".into(),
        current_account_label: "A".into(),
        busy: false,
        group,
    }
}

/// 사용량 요청마다 연결 하나. 진단에는 이동 가능 1개·이동 불가 1개를 답한다.
#[expect(clippy::unwrap_used, reason = "테스트 헬퍼")]
fn fake_daemon(name: &str) -> (PathBuf, Requests) {
    let root = std::env::temp_dir().join(format!("ext-switch-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    let socket = root.join("d.sock");
    let listener = krill_proto::os::net::UnixListener::bind(&socket).unwrap();
    let requests = Requests::default();
    let seen = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            serve(&mut stream, &seen);
        }
    });
    (socket, requests)
}

fn serve(stream: &mut UnixStream, seen: &Requests) -> Option<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let hello = ServerHello {
        proto_min: ipc::PROTO_MIN,
        proto_max: ipc::PROTO_MAX,
        features: FEAT_USAGE | FEAT_PROVIDER_DETAILS | FEAT_ACCOUNT_SWITCH_CONTROL,
        build: "synthetic".into(),
        instance_id: 9_301,
    };
    stream.write_all(&hello.encode().ok()?).ok()?;
    let (kind, body) = ipc::read_frame(stream).ok()??;
    let client = ClientHello::decode(kind, &body).ok()?;
    let negotiated = ipc::negotiate(&hello, &client)?;
    let (kind, body) = ipc::read_frame(stream).ok()??;
    let req = UsageRequest::decode(kind, &body).ok()?;
    let reply = match &req {
        UsageRequest::InspectSessionSwitches {
            provider,
            previous_account_id,
            target_account_id,
        } => UsageReply::SessionSwitchPlan(SessionSwitchPlan {
            provider: *provider,
            previous_account_id: previous_account_id.clone(),
            target_account_id: target_account_id.clone(),
            candidates: vec![
                candidate(1, SessionSwitchGroup::OnPreviousAccount),
                candidate(2, SessionSwitchGroup::CantMove(CantMoveReason::NoSession)),
            ],
        }),
        _ => UsageReply::Accounts(AccountsSnapshot::default()),
    };
    seen.lock().ok()?.push(req);
    stream
        .write_all(&reply.for_features(negotiated.features).encode().ok()?)
        .ok()
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..8 {
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }
}

fn set_policy(cx: &mut VisualTestContext, policy: RunningSessionSwitchPolicy) {
    cx.update(|_, cx| krill_app_core::settings::set_running_session_switch_policy(policy, cx));
}

#[expect(clippy::unwrap_used, reason = "테스트 헬퍼")]
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap().center();
    cx.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
}

fn inspect_a_to_b() -> UsageRequest {
    UsageRequest::InspectSessionSwitches {
        provider: ProviderKind::Claude,
        previous_account_id: Some("a".into()),
        target_account_id: "b".into(),
    }
}

fn switch_session_1(interrupt_busy: bool) -> UsageRequest {
    UsageRequest::SwitchSelectedSessions {
        provider: ProviderKind::Claude,
        account_id: "b".into(),
        sessions: vec![SessionId(1)],
        interrupt_busy,
    }
}

/// 카드의 리뷰 버튼이 이동 불가 사유까지 보이는 review 모드 모달을 연다.
fn open_review_modal(cx: &mut VisualTestContext) {
    click(cx, "external-switch-review-action");
    settle(cx);
    assert!(cx.debug_bounds("session-switch-modal").is_some());
    assert!(cx.debug_bounds("session-switch-detail-review").is_some());
    assert!(cx.debug_bounds("external-switch-notice").is_none());
}

/// 원격 호스트에서 a → b 외부 전환을 일으킨다.
fn remote_switch<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    policy: RunningSessionSwitchPolicy,
) -> (Entity<Shell>, &'a mut VisualTestContext, Requests) {
    let (socket, requests) = fake_daemon(name);
    // Toast entry/exit animations would move click targets between frames.
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, state_path(name));
    set_policy(cx, policy);
    let host = HostId::Remote("studio".into());
    view.update(cx, |s, cx| {
        s.host_targets.insert(
            host.clone(),
            krill_app_core::hosts::HostTarget::remote("studio", socket),
        );
        s.apply_accounts_snapshot_for_host(host.clone(), snapshot("a"), cx);
        s.apply_accounts_snapshot_for_host(host, snapshot("b"), cx);
    });
    cx.run_until_parked();
    settle(cx);
    (view, cx, requests)
}

#[gpui::test]
fn first_in_app_and_automatic_switches_stay_silent(cx: &mut TestAppContext) {
    // Toast entry/exit animations would move click targets between frames.
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, state_path("ext-switch-silent"));
    view.update(cx, |s, cx| {
        // Local may already hold a startup fallback snapshot; an unseen host starts clean.
        let host = HostId::Remote("quiet".into());
        s.apply_accounts_snapshot_for_host(host.clone(), snapshot("a"), cx);
        assert!(
            s.external_switch.pending_review.is_none(),
            "first observation"
        );

        // The shell marks its own switch request even when the host is gone.
        s.refresh_accounts_for_host(
            HostId::Remote("gone".into()),
            UsageRequest::SwitchAccount {
                provider: ProviderKind::Claude,
                account_id: "b".into(),
            },
            cx,
        );
        assert!(
            cx.global::<krill_app_core::accounts::InAppSwitches>()
                .0
                .iter()
                .any(|(h, _, id, _)| h.is_remote() && id == "b")
        );
        krill_app_core::accounts::mark_in_app_switch(
            host.clone(),
            ProviderKind::Claude,
            "b".into(),
            cx,
        );
        s.apply_accounts_snapshot_for_host(host.clone(), snapshot("b"), cx);
        assert!(s.external_switch.pending_review.is_none(), "in-app switch");

        let mut auto = snapshot("a");
        auto.switched.push(AutoSwitchEvent {
            provider: ProviderKind::Claude,
            from_id: "b".into(),
            from_label: "B".into(),
            to_id: "a".into(),
            to_label: "A".into(),
            reason_percent: 95,
        });
        s.apply_accounts_snapshot_for_host(host.clone(), auto, cx);
        assert!(
            s.external_switch.pending_review.is_none(),
            "daemon switched"
        );

        let mut history = snapshot("b");
        history.advanced.history.push(SwitchRecord {
            sequence: 5,
            at: 0,
            event: AutoSwitchEvent {
                provider: ProviderKind::Claude,
                from_id: "a".into(),
                from_label: "A".into(),
                to_id: "b".into(),
                to_label: "B".into(),
                reason_percent: 95,
            },
            predicted: false,
        });
        s.apply_accounts_snapshot_for_host(host.clone(), history.clone(), cx);
        assert!(
            s.external_switch.pending_review.is_none(),
            "new history sequence"
        );

        // An already-seen sequence no longer explains a later switch.
        let mut back = snapshot("a");
        back.advanced = history.advanced;
        s.apply_accounts_snapshot_for_host(host.clone(), back, cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |s, _| {
        let review = s.external_switch.pending_review.as_ref().unwrap();
        assert_eq!(
            (
                review.previous_account_id.as_str(),
                review.next_account_id.as_str()
            ),
            ("b", "a")
        );
        assert!(review.plan.is_none(), "no daemon target");
    });
}

#[gpui::test]
fn local_ask_opens_review_modal_and_switches_selected_sessions(cx: &mut TestAppContext) {
    let (socket, requests) = fake_daemon("local-ask");
    // Toast entry/exit animations would move click targets between frames.
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, state_path("ext-switch-local-ask"));
    set_policy(cx, RunningSessionSwitchPolicy::Ask);
    view.update(cx, |s, cx| {
        s.paths = Some(Paths {
            holder: socket.with_file_name("holder"),
            krilld: socket.with_file_name("krilld"),
            socket,
        });
        s.link = Link::Up;
        s.apply_accounts_snapshot(snapshot("a"), cx);
        s.apply_accounts_snapshot(snapshot("b"), cx);
    });
    cx.run_until_parked();
    settle(cx);
    assert!(requests.lock().unwrap().contains(&inspect_a_to_b()));
    assert!(cx.debug_bounds("external-switch-notice").is_some());
    assert!(cx.debug_bounds("external-switch-dismiss").is_some());
    open_review_modal(cx);
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, UsageRequest::SwitchSelectedSessions { .. })),
        "the review modal decides, not the card"
    );
    click(cx, "session-switch-restart");
    settle(cx);
    assert!(requests.lock().unwrap().contains(&switch_session_1(true)));
    assert!(cx.debug_bounds("session-switch-modal").is_none());
}

#[gpui::test]
fn remote_restart_moves_sessions_and_reports_the_rest(cx: &mut TestAppContext) {
    let (_view, cx, requests) = remote_switch(
        cx,
        "ext-switch-remote-restart",
        RunningSessionSwitchPolicy::Restart,
    );
    let seen = requests.lock().unwrap().clone();
    assert!(seen.contains(&inspect_a_to_b()));
    assert!(seen.contains(&switch_session_1(false)));
    assert!(cx.debug_bounds("external-switch-notice").is_some());
    assert!(cx.debug_bounds("external-switch-dismiss").is_some());
    open_review_modal(cx);
}

#[gpui::test]
fn remote_keep_only_notifies(cx: &mut TestAppContext) {
    let (_view, cx, requests) = remote_switch(
        cx,
        "ext-switch-remote-keep",
        RunningSessionSwitchPolicy::Keep,
    );
    let seen = requests.lock().unwrap().clone();
    assert!(seen.contains(&inspect_a_to_b()));
    assert!(
        !seen
            .iter()
            .any(|r| matches!(r, UsageRequest::SwitchSelectedSessions { .. }))
    );
    assert!(cx.debug_bounds("external-switch-notice").is_some());
    assert!(cx.debug_bounds("external-switch-dismiss").is_none());
}

#[gpui::test]
fn kept_switch_stays_dismissed_when_the_same_snapshot_returns(cx: &mut TestAppContext) {
    let (view, cx, requests) = remote_switch(
        cx,
        "ext-switch-remote-dismiss",
        RunningSessionSwitchPolicy::Ask,
    );
    click(cx, "external-switch-dismiss");
    settle(cx);
    assert!(cx.debug_bounds("external-switch-notice").is_none());
    let before = requests.lock().unwrap().len();
    view.update(cx, |s, cx| {
        s.apply_accounts_snapshot_for_host(HostId::Remote("studio".into()), snapshot("b"), cx);
    });
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("external-switch-notice").is_none());
    assert_eq!(requests.lock().unwrap().len(), before);
    view.read_with(cx, |s, _| {
        assert!(s.external_switch.pending_review.is_none());
    });
}

#[gpui::test]
fn settings_window_switch_is_not_an_external_switch(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("ext-switch-settings"));
    let settings = cx.new(krill_app_core::settings::SettingsView::new);
    view.update(cx, |s, cx| {
        // Local may already hold a startup fallback snapshot; start from a known baseline.
        s.external_switch = ExternalSwitchTracker::default();
        s.apply_accounts_snapshot(snapshot("a"), cx);
        settings.update(cx, |settings, cx| {
            settings.apply_account_request(
                UsageRequest::SwitchAccount {
                    provider: ProviderKind::Claude,
                    account_id: "b".into(),
                },
                cx,
            );
        });
        s.apply_accounts_snapshot(snapshot("b"), cx);
        assert!(s.external_switch.pending_review.is_none());
    });
}
