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
fn comparison_aligns_columns_and_details_does_not_switch_or_close_menu(
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
        cx.notify();
    });
    cx.run_until_parked();
    let visual = &mut VisualTestContext::from_window(handle, cx);
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
    let toggle = visual
        .debug_bounds("status-provider-account-codex-host-details-toggle")
        .unwrap();
    visual.simulate_click(toggle.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("status-provider-table-header")
            .is_some(),
        "상세 클릭은 팝업을 닫지 않는다"
    );
    assert!(
        visual
            .debug_bounds("status-provider-profile-saved")
            .is_some(),
        "다른 프로필을 숨겨 버리지 않는다"
    );
    assert!(visual.debug_bounds("status-provider-use-saved").is_some());
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
            "상세 클릭은 계정 전환 IPC를 보내지 않는다"
        );
    });
    let toggle = visual
        .debug_bounds("status-provider-account-codex-host-details-toggle")
        .unwrap();
    visual.simulate_click(toggle.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("status-provider-profile-saved")
            .is_none()
    );
    assert!(
        visual
            .debug_bounds("status-provider-table-header")
            .is_some()
    );
}
