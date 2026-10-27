use super::*;
use krill_proto::usage::{AccountMeters, Snapshot, UsageMeter};

fn profile(id: &str, email: Option<&str>) -> AccountInfo {
    AccountInfo {
        id: id.into(),
        provider: ProviderKind::Codex,
        label: id.into(),
        email: email.map(str::to_owned),
        profile_dir: format!("/tmp/krill-comparison/{id}"),
        active: false,
        host_default: false,
        stale: false,
        usage: Snapshot::default(),
    }
}

#[test]
fn grouping_preserves_profiles_and_uses_active_profile_without_combining_usage() {
    let mut host = profile("host", Some("same@example.com"));
    host.host_default = true;
    host.usage.seven_day = Some(UsageWindow {
        percent: 5,
        resets_at: None,
    });
    let mut saved = profile("saved", Some(" SAME@example.com "));
    saved.active = true;
    saved.usage.seven_day = Some(UsageWindow {
        percent: 80,
        resets_at: None,
    });
    let mut other_provider = saved.clone();
    other_provider.provider = ProviderKind::Claude;
    let snap = AccountsSnapshot {
        accounts: vec![
            profile("other", Some("other@example.com")),
            saved,
            host,
            profile("unknown-1", None),
            profile("unknown-2", Some("  ")),
            profile("claude-orca-external", Some("same@example.com")),
            other_provider,
        ],
        ..Default::default()
    };
    let groups = account_groups(&snap, ProviderKind::Codex);
    assert_eq!(groups.len(), 4);
    let grouped = &groups[0];
    assert_eq!(
        grouped.profiles.len(),
        2,
        "묶음은 원본 프로필을 모두 보존한다"
    );
    let selected = grouped.selected().unwrap();
    assert_eq!(
        selected.id, "saved",
        "호스트보다 실제 활성 프로필의 값을 표시한다"
    );
    assert_eq!(account_name(selected), "SAME@example.com");
    assert_eq!(
        comparison_window(&snap, selected, "7d").unwrap().percent,
        80
    );
    assert_eq!(
        grouped
            .profiles
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        ["saved", "host"]
    );
    assert_eq!(groups.iter().map(|g| g.profiles.len()).sum::<usize>(), 5);
}

#[test]
fn comparison_keeps_missing_windows_unknown_and_preserves_provider_specific_pools() {
    let mut a = profile("a", Some("a@example.com"));
    a.usage.five_hour = Some(UsageWindow {
        percent: 10,
        resets_at: None,
    });
    let mut snap = AccountsSnapshot::default();
    assert_eq!(comparison_window(&snap, &a, "5h").unwrap().percent, 10);
    snap.details.accounts.push(AccountMeters {
        account_id: a.id.clone(),
        plan: None,
        meters: vec![
            UsageMeter {
                key: "credits".into(),
                label: "Credits".into(),
                window: None,
                value: Some("42.123456".into()),
            },
            UsageMeter {
                key: "monthly".into(),
                label: "30d".into(),
                window: Some(UsageWindow {
                    percent: 80,
                    resets_at: None,
                }),
                value: None,
            },
        ],
    });
    assert!(
        comparison_window(&snap, &a, "5h").is_none(),
        "꼬리에서 보고하지 않은 기간을 이전 요약으로 채우지 않는다"
    );
    assert!(comparison_window(&snap, &a, "7d").is_none());
    snap.details.accounts[0].meters.push(UsageMeter {
        key: "rolling".into(),
        label: "5h".into(),
        value: None,
        window: Some(UsageWindow {
            percent: 97,
            resets_at: Some("1".into()),
        }),
    });
    let window = comparison_window(&snap, &a, "5h").unwrap();
    assert_eq!(window.percent, 97);
    assert_eq!(
        krill_app::accounts::display::window(Some(window), 2).percent,
        None,
        "리셋이 지난 값은 사용 가능한 0%가 아니다"
    );
    assert_eq!(
        snap.details.accounts[0].meters[0].value.as_deref(),
        Some("42.123456")
    );
}

#[test]
fn model_pools_get_real_columns_instead_of_invented_five_hour_windows() {
    let mut account = profile("agy", Some("a@example.com"));
    account.provider = ProviderKind::Antigravity;
    let mut snap = AccountsSnapshot {
        accounts: vec![account],
        ..Default::default()
    };
    snap.details.accounts.push(AccountMeters {
        account_id: "agy".into(),
        plan: None,
        meters: ["Gemini · 7d", "Claude and GPT · 7d", "Other · 7d"]
            .into_iter()
            .map(|label| UsageMeter {
                key: label.into(),
                label: label.into(),
                value: None,
                window: Some(UsageWindow {
                    percent: 20,
                    resets_at: None,
                }),
            })
            .collect(),
    });
    assert_eq!(
        comparison_columns(&snap, ProviderKind::Antigravity),
        ["Gemini · 7d", "Claude and GPT · 7d"]
    );
    assert_eq!(
        snap.details.accounts[0].meters.len(),
        3,
        "추가 풀은 상세에 남는다"
    );
}

#[gpui::test]
fn comparison_opens_side_details_on_hover_and_click_without_switching(
    cx: &mut gpui::TestAppContext,
) {
    use gpui::VisualTestContext;
    let (shell, handle) = super::super::tests::open(cx);
    shell.update(cx, |s, cx| {
        cx.set_global(krill_app::accounts::installed::Installed(Some(vec![
            ProviderKind::Codex,
        ])));
        let mut host = profile("host", Some("same@example.com"));
        host.host_default = true;
        host.active = true;
        let mut saved = profile("saved", Some("same@example.com"));
        saved.label = "Saved workspace profile".into();
        std::sync::Arc::make_mut(&mut s.accounts_snapshot).accounts =
            vec![host, saved, profile("other", Some("other@example.com"))];
        std::sync::Arc::make_mut(&mut s.accounts_snapshot)
            .statuses
            .push(krill_proto::usage::AccountStatusInfo {
                account_id: "other".into(),
                status: AccountStatus::Error,
                updated_at: None,
                detail: Some("usage API returned HTTP 403".into()),
            });
        std::sync::Arc::make_mut(&mut s.accounts_snapshot)
            .details
            .accounts
            .push(AccountMeters {
                account_id: "host".into(),
                plan: Some("Plus".into()),
                meters: ["5h", "7d", "model-pool"]
                    .into_iter()
                    .map(|key| UsageMeter {
                        key: key.into(),
                        label: key.into(),
                        value: None,
                        window: Some(UsageWindow {
                            percent: 20,
                            resets_at: None,
                        }),
                    })
                    .collect(),
            });
        cx.notify();
    });
    cx.run_until_parked();
    let visual = &mut VisualTestContext::from_window(handle, cx);
    visual.simulate_resize(gpui::size(gpui::px(890.), gpui::px(700.)));
    visual.run_until_parked();
    let pill = visual.debug_bounds("status-provider-pill-codex").unwrap();
    visual.simulate_click(pill.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    let header = visual.debug_bounds("status-provider-table-header").unwrap();
    assert!(
        visual.debug_bounds("status-provider-card-codex").is_none(),
        "중복 상단 요약을 표시하지 않는다"
    );
    assert!(
        visual
            .debug_bounds("status-provider-account-codex-saved")
            .is_none(),
        "같은 이메일은 한 행에 모은다"
    );
    let first = visual
        .debug_bounds("status-provider-account-codex-host-5h")
        .unwrap();
    let second = visual
        .debug_bounds("status-provider-account-codex-other-5h")
        .unwrap();
    let weekly = visual
        .debug_bounds("status-provider-account-codex-host-7d")
        .unwrap();
    assert_eq!(first.origin.x, second.origin.x);
    assert_eq!(first.size.width, second.size.width);
    assert!(first.right() <= weekly.origin.x);
    assert!(weekly.right() <= header.right());
    let row = visual
        .debug_bounds("status-provider-account-codex-host")
        .unwrap();
    let other = visual
        .debug_bounds("status-provider-account-codex-other")
        .unwrap();
    assert!(
        visual
            .debug_bounds("status-provider-account-codex-other-status")
            .is_some(),
        "사용량이 없는 계정은 모름·갱신 기록 상태를 보여 준다"
    );
    let diagnostic = visual
        .debug_bounds("status-provider-account-codex-other-status-detail")
        .expect("failed usage must expose its diagnostic in the account card");
    assert!(diagnostic.size.width > gpui::px(0.));
    assert!(diagnostic.origin.x >= other.origin.x);
    assert!(diagnostic.right() <= other.right());
    assert!(diagnostic.bottom() <= other.bottom());
    assert!(
        row.bottom() < other.origin.y,
        "계정 영역 사이에 간격이 있다"
    );
    assert!(
        visual
            .debug_bounds("status-provider-details-host")
            .is_some(),
        "시안 D는 팝오버를 열면 우측 인라인 상세 패널에 활성 계정을 바로 표시한다"
    );
    assert!(
        visual
            .debug_bounds("status-provider-details-other")
            .is_none()
    );
    assert!(
        visual
            .debug_bounds("status-provider-account-codex-host-details-toggle")
            .is_none(),
        "별도의 상세 버튼을 표시하지 않는다"
    );
    visual.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
    visual.run_until_parked();
    let panel = visual.debug_bounds("status-provider-details-host").unwrap();
    assert!(
        panel.origin.x > row.right(),
        "상세는 행 오른쪽 패널에 열린다"
    );
    assert!(
        panel.right() <= gpui::px(890.),
        "상세 패널이 창 안에 들어간다"
    );
    assert_eq!(
        visual.debug_bounds("status-provider-account-codex-host"),
        Some(row)
    );
    assert_eq!(
        visual.debug_bounds("status-provider-account-codex-other"),
        Some(other)
    );
    assert!(
        visual
            .debug_bounds("status-provider-profile-saved")
            .is_some()
    );
    assert!(
        visual
            .debug_bounds("status-provider-detail-host-model-pool")
            .is_some()
    );
    assert!(
        visual
            .debug_bounds("status-provider-detail-reset-host")
            .is_some()
    );
    let saved_reset = visual
        .debug_bounds("status-provider-detail-reset-saved")
        .expect("inactive profile in details panel must also expose reset usage button");
    visual.simulate_click(saved_reset.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("status-provider-detail-reset-confirm-saved")
            .is_some(),
        "clicking detail reset button arms confirmation in-place without closing panel"
    );
    let cancel_reset = visual
        .debug_bounds("status-provider-detail-reset-cancel-saved")
        .unwrap();
    visual.simulate_click(cancel_reset.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("status-provider-detail-reset-confirm-saved")
            .is_none()
    );
    let saved = visual.debug_bounds("status-provider-use-saved").unwrap();
    visual.simulate_mouse_move(saved.center(), None, gpui::Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("status-provider-details-host")
            .is_some(),
        "행에서 상세 패널로 이동해도 패널이 유지된다"
    );
    let header_before = visual.debug_bounds("status-provider-table-header").unwrap();
    let toggle_before = visual
        .debug_bounds("status-provider-toggle-bar-codex")
        .unwrap();
    let open_accounts_before = visual
        .debug_bounds("status-provider-open-accounts")
        .unwrap();
    visual.simulate_click(other.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("status-provider-details-host")
            .is_none()
    );
    assert!(
        visual
            .debug_bounds("status-provider-details-other")
            .is_some()
    );
    assert_eq!(
        visual.debug_bounds("status-provider-table-header"),
        Some(header_before),
        "계정 선택이 바뀌어도 상단 헤더 위치가 흔들리지 않는다 (CONVENTIONS G7)"
    );
    assert_eq!(
        visual.debug_bounds("status-provider-toggle-bar-codex"),
        Some(toggle_before),
        "상세 패널 길이가 다른 계정으로 바꿔도 하단 메뉴 항목 위치가 흔들리지 않는다 (CONVENTIONS G7)"
    );
    assert_eq!(
        visual.debug_bounds("status-provider-open-accounts"),
        Some(open_accounts_before),
        "상세 패널 전환 시 팝오버 전체 높이가 변하지 않는다 (CONVENTIONS G7)"
    );
    shell.read_with(visual, |s, _| {
        assert!(
            s.accounts_snapshot
                .accounts
                .iter()
                .find(|a| a.id == "host")
                .unwrap()
                .active
        );
        assert!(
            s.notice.is_none(),
            "계정 행 클릭은 전환 IPC를 보내지 않는다"
        );
    });
    visual.simulate_keystrokes("right");
    visual.run_until_parked();
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("status-provider-details-other")
            .is_none()
    );
    assert!(
        visual
            .debug_bounds("status-provider-table-header")
            .is_none()
    );

    // 사용 버튼은 여전히 전환을 요청하고 팝업을 닫는다. 실제 계정 파일 대신 실패하는 IPC를 쓴다.
    visual.simulate_click(pill.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    let row = visual
        .debug_bounds("status-provider-account-codex-host")
        .unwrap();
    visual.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
    visual.run_until_parked();
    let saved = visual.debug_bounds("status-provider-use-saved").unwrap();
    shell.update(visual, |s, _| {
        let root = std::env::temp_dir().join("krill-provider-details-missing-socket");
        s.paths = Some(krill_proto::daemon::Paths {
            socket: root.join("krilld.sock"),
            holder: root.join("holder"),
            krilld: root.join("krilld"),
        });
        s.link = super::super::super::Link::Up;
    });
    visual.simulate_click(saved.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    visual
        .executor()
        .advance_clock(std::time::Duration::from_secs(2));
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("status-provider-table-header")
            .is_none()
    );
    shell.read_with(visual, |s, _| {
        assert!(s.notice.is_some(), "사용 버튼은 전환 IPC를 요청한다");
    });
}

#[gpui::test]
fn remote_host_provider_popup_switches_remote_profile_and_rebinds_remote_handoff(
    cx: &mut gpui::TestAppContext,
) {
    use gpui::VisualTestContext;
    use krill_proto::ipc::{self, ClientHello, ServerHello};
    use krill_proto::screen::Msg as _;
    use krill_proto::usage::{
        FEAT_PROVIDER_DETAILS, FEAT_USAGE, FEAT_USAGE_STATUS, SessionHandoff, UsageReply,
    };
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};

    let sock_path = std::path::PathBuf::from(format!("/tmp/kr-rpop-{}.s", std::process::id()));
    std::fs::remove_file(&sock_path).ok();
    let listener = UnixListener::bind(&sock_path).unwrap();

    let mut primary = profile("remote-main", Some("main@remote.example"));
    primary.active = true;
    primary.host_default = true;
    let secondary = profile("remote-alt", Some("alt@remote.example"));
    let remote_snap = Arc::new(Mutex::new(AccountsSnapshot {
        accounts: vec![primary, secondary],
        ..Default::default()
    }));
    let server_snap = Arc::clone(&remote_snap);
    let server = std::thread::spawn(move || {
        let server_hello = ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features: FEAT_USAGE | FEAT_USAGE_STATUS | FEAT_PROVIDER_DETAILS,
            build: "test".to_owned(),
            instance_id: 7777,
        };
        while let Ok((mut stream, _)) = listener.accept() {
            if stream.write_all(&server_hello.encode().unwrap()).is_err() {
                break;
            }
            let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) else {
                break;
            };
            let Ok(client) = ClientHello::decode(kind, &body) else {
                break;
            };
            let features = ipc::negotiate(&server_hello, &client).unwrap().features;
            let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) else {
                continue;
            };
            let Ok(req) = UsageRequest::decode(kind, &body) else {
                continue;
            };
            let mut snap = server_snap.lock().unwrap();
            if let UsageRequest::SwitchAccount {
                provider,
                account_id,
            } = req
            {
                for acc in &mut snap.accounts {
                    if acc.provider == provider {
                        acc.active = acc.id == account_id;
                    }
                }
                snap.details.supported = true;
                snap.details.handoffs = vec![SessionHandoff {
                    from: krill_proto::SessionId(11),
                    to: Some(krill_proto::SessionId(22)),
                    account_id,
                    error: None,
                }];
            }
            let reply = UsageReply::Accounts(snap.clone()).for_features(features);
            stream.write_all(&reply.encode().unwrap()).ok();
        }
    });

    let (shell, handle) = super::super::tests::open(cx);
    let remote_host = krill_app::hosts::HostId::Remote("iroh:remote-pc".into());
    let remote_target = krill_app::hosts::HostTarget::remote("iroh:remote-pc", sock_path.clone());
    shell.update(cx, |s, cx| {
        cx.set_global(krill_app::accounts::installed::Installed(Some(vec![])));
        let pane = s.layout.focused().unwrap();
        s.layout.set_host(pane, remote_host.clone());
        s.layout.set_session(pane, krill_proto::SessionId(11));
        s.host_targets.insert(remote_host.clone(), remote_target);
        s.host_statuses.push((
            krill_app::hosts::RemoteHostSpec {
                id: "iroh:remote-pc".into(),
                label: "Remote PC".into(),
                address: "iroh:remote-pc".into(),
                port: None,
                identity_file: None,
                remote_socket: None,
            },
            krill_app::hosts::HostStatus::Connected,
        ));
        s.remote_accounts.insert(
            remote_host.clone(),
            Arc::new(remote_snap.lock().unwrap().clone()),
        );
        cx.notify();
    });
    cx.run_until_parked();

    let visual = &mut VisualTestContext::from_window(handle, cx);
    visual.simulate_resize(gpui::size(gpui::px(890.), gpui::px(700.)));
    visual.run_until_parked();

    let pill = visual
        .debug_bounds("status-provider-pill-codex")
        .expect("remote codex pill must be shown even when codex is not installed locally");
    visual.simulate_click(pill.center(), gpui::Modifiers::default());
    visual.run_until_parked();

    assert!(
        visual.debug_bounds("status-provider-active-host").is_some(),
        "remote host badge must appear in provider popup header"
    );
    assert!(
        visual.debug_bounds("status-provider-tab-local").is_some(),
        "local host tab in Variant D header"
    );
    let tab_remote = visual
        .debug_bounds("status-provider-tab-remote")
        .expect("remote hosts tab in Variant D header");
    assert!(
        visual
            .debug_bounds("status-provider-remote-hosts-view")
            .is_none()
    );
    let header_before_tab = visual.debug_bounds("status-provider-table-header").unwrap();
    let toggle_before_tab = visual
        .debug_bounds("status-provider-toggle-bar-codex")
        .unwrap();
    let open_accounts_before_tab = visual
        .debug_bounds("status-provider-open-accounts")
        .unwrap();
    visual.simulate_click(tab_remote.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("status-provider-remote-hosts-view")
            .is_some(),
        "clicking remote tab switches popover body to remote hosts view"
    );
    assert_eq!(
        visual.debug_bounds("status-provider-table-header"),
        Some(header_before_tab),
        "로컬↔원격 탭 전환 시 상단 헤더 높이와 위치가 흔들리지 않는다 (CONVENTIONS G7)"
    );
    assert_eq!(
        visual.debug_bounds("status-provider-toggle-bar-codex"),
        Some(toggle_before_tab),
        "로컬↔원격 탭 전환 시 하단 메뉴 항목 위치가 흔들리지 않는다 (CONVENTIONS G7)"
    );
    assert_eq!(
        visual.debug_bounds("status-provider-open-accounts"),
        Some(open_accounts_before_tab),
        "로컬↔원격 탭 전환 시 팝오버 전체 높이가 유지된다 (CONVENTIONS G7)"
    );
    let tab_local = visual
        .debug_bounds("status-provider-tab-local")
        .expect("local host tab after switching to remote tab");
    visual.simulate_click(tab_local.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("status-provider-remote-hosts-view")
            .is_none(),
        "clicking local tab returns to master-detail account view"
    );
    assert_eq!(
        visual.debug_bounds("status-provider-table-header"),
        Some(header_before_tab)
    );
    assert_eq!(
        visual.debug_bounds("status-provider-open-accounts"),
        Some(open_accounts_before_tab)
    );
    let use_alt = visual
        .debug_bounds("status-provider-use-remote-alt")
        .expect("use button for inactive remote profile");
    visual.simulate_click(use_alt.center(), gpui::Modifiers::default());
    visual.run_until_parked();

    for _ in 0..20 {
        let switched = shell.read_with(visual, |s, _| {
            s.accounts_for_host(&remote_host)
                .accounts
                .iter()
                .find(|a| a.id == "remote-alt")
                .is_some_and(|a| a.active)
        });
        if switched {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        visual.run_until_parked();
    }

    shell.read_with(visual, |s, _| {
        assert!(
            s.accounts_for_host(&remote_host)
                .accounts
                .iter()
                .find(|a| a.id == "remote-alt")
                .is_some_and(|a| a.active),
            "remote profile must be switched via status bar popup"
        );
        let pane = s.layout.focused().unwrap();
        assert_eq!(
            s.layout.session(pane),
            Some(krill_proto::SessionId(22)),
            "remote session handoff must rebind the remote pane session"
        );
    });

    std::os::unix::net::UnixStream::connect(&sock_path).ok();
    server.join().ok();
    std::fs::remove_file(&sock_path).ok();
}

#[test]
fn exhaustion_forecasts_use_whole_hours() {
    let now = krill_app::accounts::display::now();
    let account = profile("forecast", None);
    let mut snap = AccountsSnapshot::default();
    snap.advanced.supported = true;
    snap.advanced.forecasts = vec![krill_proto::usage::UsageForecast {
        account_id: account.id.clone(),
        window: "5h".into(),
        percent_per_hour: 12,
        exhausts_at: now + 7_199,
        resets_at: now + 18_000,
    }];
    let notes = advanced_notes(&snap, &account);
    assert_eq!(
        notes,
        vec![krill_app::i18n::catalog::shell::ACCOUNT_FORECAST.format(&[
            ("window", "5h"),
            ("hours", "2"),
            ("rate", "12"),
        ])]
    );
}

#[gpui::test]
fn remote_model_limits_and_resets_are_visible_without_moving_tab_controls(
    cx: &mut gpui::TestAppContext,
) {
    use gpui::VisualTestContext;
    use krill_app::hosts::{HostId, HostStatus, RemoteHostSpec};
    use std::sync::Arc;
    let (shell, handle) = super::super::tests::open(cx);
    let mut local = profile("host", None);
    local.provider = ProviderKind::Antigravity;
    local.active = true;
    let mut remote = local.clone();
    remote.usage.five_hour = Some(UsageWindow {
        percent: 5,
        resets_at: None,
    });
    let mut snap = AccountsSnapshot {
        accounts: vec![remote],
        ..Default::default()
    };
    snap.details.supported = true;
    snap.details.accounts.push(AccountMeters {
        account_id: "host".into(),
        plan: None,
        meters: vec![UsageMeter {
            key: "gemini_weekly".into(),
            label: "Gemini · 7d".into(),
            value: None,
            window: Some(UsageWindow {
                percent: 73,
                resets_at: Some("4102444800".into()),
            }),
        }],
    });
    shell.update(cx, |s, cx| {
        cx.set_global(krill_app::accounts::installed::Installed(Some(vec![
            ProviderKind::Antigravity,
        ])));
        Arc::make_mut(&mut s.accounts_snapshot).accounts = vec![local];
        s.host_statuses.push((
            RemoteHostSpec {
                id: "quota-box".into(),
                label: "Quota Box".into(),
                address: "quota-box".into(),
                port: None,
                identity_file: None,
                remote_socket: None,
            },
            HostStatus::Connected,
        ));
        s.remote_accounts
            .insert(HostId::Remote("quota-box".into()), Arc::new(snap));
        cx.notify();
    });
    cx.run_until_parked();
    let visual = &mut VisualTestContext::from_window(handle, cx);
    visual.simulate_resize(gpui::size(gpui::px(1000.), gpui::px(850.)));
    visual.run_until_parked();
    let pill = visual.debug_bounds("status-provider-pill-agy").unwrap();
    visual.simulate_click(pill.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    let footer = visual
        .debug_bounds("status-provider-open-accounts")
        .unwrap();
    let tab = visual.debug_bounds("status-provider-tab-remote").unwrap();
    visual.simulate_click(tab.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("status-provider-remote-quota-box-host-gemini_weekly-percent")
            .is_some()
    );
    assert!(
        visual
            .debug_bounds("status-provider-remote-quota-box-host-gemini_weekly-reset")
            .is_some()
    );
    assert!(
        visual
            .debug_bounds("status-provider-remote-quota-box-host-status")
            .is_some()
    );
    assert!(
        visual
            .debug_bounds("status-provider-remote-quota-box-host-5h-percent")
            .is_none(),
        "원격 모델 미터를 로컬/요약 창으로 대체하지 않는다"
    );
    assert_eq!(
        visual.debug_bounds("status-provider-open-accounts"),
        Some(footer)
    );
}
