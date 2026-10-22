use super::*;

#[test]
fn provider_details_tail_preserves_the_prior_wire_prefix() {
    let snapshot = AccountsSnapshot {
        details: Box::new(ProviderDetails {
            supported: true,
            accounts: vec![],
            handoffs: vec![],
        }),
        ..AccountsSnapshot::default()
    };
    let reply = UsageReply::Accounts(snapshot);
    let frame = reply.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
    assert_eq!(
        body,
        [
            2, 0, 0, 90, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0
        ]
    );
    assert_eq!(UsageReply::decode(kind, &body).unwrap(), reply);
    assert_eq!(
        reply.for_features(FEAT_USAGE),
        UsageReply::Accounts(AccountsSnapshot::default())
    );
    let request = UsageRequest::ImportOrcaAntigravity {
        source_dir: "o".into(),
        dry_run: true,
    };
    let frame = request.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
    assert_eq!(body, [16, 1, b'o', 1]);
    assert_eq!(UsageRequest::decode(kind, &body).unwrap(), request);
    assert!(request.is_read());
    assert_eq!(
        request.required_features(),
        FEAT_USAGE | FEAT_USAGE_AGY | FEAT_PROVIDER_DETAILS
    );
}
use crate::{control::Failure, ipc};

proptest::proptest! {
    #[test]
    fn arbitrary_usage_frames_do_not_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..2048)) {
        let _request = UsageRequest::decode(kind::USAGE_REQ, &bytes);
        let _reply = UsageReply::decode(kind::USAGE_REPLY, &bytes);
    }
}

fn fixture<T: Msg + PartialEq + std::fmt::Debug>(value: &T, expected: &[u8]) {
    assert_eq!(value.encode().unwrap(), expected);
    let (kind, body) = ipc::read_frame(&mut &expected[..]).unwrap().unwrap();
    assert_eq!(&T::decode(kind, &body).unwrap(), value);
}

/// 고정 바이트: 바뀌면 호환 위반이다(새 메시지는 줄을 추가만 한다).
#[test]
fn fixed_v1_bytes_roundtrip() {
    fixture(&UsageRequest::Get, &[2, 0, 0, 0, 0x41, 0]);
    fixture(
        &UsageReply::Current(Snapshot {
            five_hour: Some(Window {
                percent: 74,
                resets_at: Some("1791714446".to_owned()),
            }),
            seven_day: Some(Window {
                percent: 12,
                resets_at: Some("1792301246".to_owned()),
            }),
        }),
        &[
            30, 0, 0, 0, 0x61, 0, 1, 74, 1, 10, 49, 55, 57, 49, 55, 49, 52, 52, 52, 54, 1, 12, 1,
            10, 49, 55, 57, 50, 51, 48, 49, 50, 52, 54,
        ],
    );
    fixture(
        &UsageReply::Failed(Failure::Denied),
        &[3, 0, 0, 0, 0x61, 1, 3],
    );
    assert!(UsageRequest::decode(kind::USAGE_REQ, &[9]).is_err());
    assert!(UsageReply::decode(kind::USAGE_REPLY, &[9]).is_err());
}

#[test]
fn warn_at_eighty_percent() {
    let mut s = Snapshot::default();
    assert!(!s.warn());
    assert!(s.is_empty());
    assert_eq!(s.max_percent(), 0);
    s.five_hour = Some(Window {
        percent: 79,
        resets_at: None,
    });
    assert!(!s.warn());
    assert_eq!(s.max_percent(), 79);
    s.seven_day = Some(Window {
        percent: 80,
        resets_at: None,
    });
    assert!(s.warn());
    assert!(!s.is_empty());
    assert_eq!(s.max_percent(), 80);
}

#[test]
fn removed_provider_wire_slot_is_preserved_but_not_live() {
    assert_eq!(postcard::to_stdvec(&ProviderKind::Gemini).unwrap(), [2]);
    assert_eq!(
        postcard::from_bytes::<ProviderKind>(&[2]).unwrap(),
        ProviderKind::Gemini
    );
    assert_eq!(ProviderKind::parse("gemini"), None);
    assert!(!ProviderKind::ALL.contains(&ProviderKind::Gemini));
    assert!(
        UsageRequest::SwitchAccount {
            provider: ProviderKind::Gemini,
            account_id: "legacy".into(),
        }
        .contains_removed_provider()
    );
}

#[test]
fn multi_account_messages_roundtrip() {
    let req = UsageRequest::SwitchAccount {
        provider: ProviderKind::Claude,
        account_id: "claude-work".into(),
    };
    let encoded = req.encode().unwrap();
    let (k, body) = ipc::read_frame(&mut &encoded[..]).unwrap().unwrap();
    assert_eq!(UsageRequest::decode(k, &body).unwrap(), req);

    let snap = AccountsSnapshot {
        accounts: vec![AccountInfo {
            id: "claude-work".into(),
            provider: ProviderKind::Claude,
            label: "Work Max".into(),
            email: Some("owen@example.com".into()),
            profile_dir: "/Users/owen/.config/krill/profiles/claude/work".into(),
            active: true,
            host_default: false,
            stale: false,
            usage: Snapshot {
                five_hour: Some(Window {
                    percent: 42,
                    resets_at: Some("1791714446".into()),
                }),
                seven_day: None,
            },
        }],
        auto_switch: AutoSwitchPolicy::default(),
        switched: vec![AutoSwitchEvent {
            provider: ProviderKind::Claude,
            from_id: "claude-host".into(),
            from_label: "Host".into(),
            to_id: "claude-work".into(),
            to_label: "Work Max".into(),
            reason_percent: 92,
        }],
        statuses: Vec::new(),
        advanced: AccountPolicyState::default(),
        connections: Box::default(),
        details: Box::default(),
    };
    let reply = UsageReply::Accounts(snap);
    let encoded = reply.encode().unwrap();
    let (k, body) = ipc::read_frame(&mut &encoded[..]).unwrap().unwrap();
    assert_eq!(UsageReply::decode(k, &body).unwrap(), reply);
}

#[test]
fn legacy_auto_switch_and_accounts_bytes_remain_bidirectionally_compatible() {
    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct OldPolicy {
        enabled: bool,
        threshold_percent: u8,
        switch_on_rate_limit: bool,
    }
    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct OldAccounts {
        accounts: Vec<AccountInfo>,
        auto_switch: OldPolicy,
        switched: Vec<AutoSwitchEvent>,
    }
    #[derive(Deserialize)]
    enum OldRequest {
        Get,
        ListAccounts,
        SwitchAccount {
            _provider: ProviderKind,
            _account_id: String,
        },
        AddAccount {
            _provider: ProviderKind,
            _label: String,
            _email: Option<String>,
            _profile_dir: String,
        },
        RemoveAccount {
            _provider: ProviderKind,
            _account_id: String,
        },
        SetAutoSwitch(OldPolicy),
    }
    #[derive(Deserialize)]
    enum OldReply {
        Current(Snapshot),
        Failed(Failure),
        Accounts(OldAccounts),
    }

    let policy = AutoSwitchPolicy {
        enabled: true,
        ..AutoSwitchPolicy::default()
    };
    let old_request = [5, 1, 90, 1];
    assert_eq!(
        UsageRequest::decode(kind::USAGE_REQ, &old_request).unwrap(),
        UsageRequest::SetAutoSwitch(policy.clone())
    );
    let old_accounts = [2, 0, 1, 90, 1, 0];
    let reply = UsageReply::Accounts(AccountsSnapshot {
        auto_switch: policy.clone(),
        ..AccountsSnapshot::default()
    });
    assert_eq!(
        UsageReply::decode(kind::USAGE_REPLY, &old_accounts).unwrap(),
        reply
    );
    for (frame, expected) in [
        (
            UsageRequest::SetAutoSwitch(policy).encode().unwrap(),
            old_request.as_slice(),
        ),
        (reply.encode().unwrap(), old_accounts.as_slice()),
    ] {
        let (_, body) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
        assert_eq!(body, expected);
    }

    let policy = AutoSwitchPolicy {
        rules: vec![AutoSwitchRule {
            provider: ProviderKind::Codex,
            from_account_id: "work".into(),
            enabled: true,
            threshold_percent: 85,
            metric: AutoSwitchMetric::SevenDay,
            target_account_id: None,
        }],
        ..AutoSwitchPolicy::default()
    };
    let request = UsageRequest::SetAutoSwitch(policy.clone());
    let encoded = request.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut encoded.as_slice()).unwrap().unwrap();
    assert_eq!(UsageRequest::decode(kind, &body).unwrap(), request);
    let OldRequest::SetAutoSwitch(old) = postcard::from_bytes(&body).unwrap() else {
        panic!("wrong old request");
    };
    assert_eq!(old.threshold_percent, policy.threshold_percent);

    let event = AutoSwitchEvent {
        provider: ProviderKind::Codex,
        from_id: "a".into(),
        from_label: "A".into(),
        to_id: "b".into(),
        to_label: "B".into(),
        reason_percent: 91,
    };
    let reply = UsageReply::Accounts(AccountsSnapshot {
        accounts: Vec::new(),
        auto_switch: policy,
        switched: vec![event.clone()],
        statuses: Vec::new(),
        advanced: AccountPolicyState::default(),
        connections: Box::default(),
        details: Box::default(),
    });
    let encoded = reply.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut encoded.as_slice()).unwrap().unwrap();
    assert_eq!(UsageReply::decode(kind, &body).unwrap(), reply);
    let OldReply::Accounts(old) = postcard::from_bytes(&body).unwrap() else {
        panic!("wrong old reply");
    };
    assert_eq!(old.switched, vec![event]);
    // Read the other legacy variants too so their fixture decoder remains checked.
    let OldReply::Current(current) = postcard::from_bytes(&[0, 0, 0]).unwrap() else {
        panic!("current");
    };
    assert_eq!(current, Snapshot::default());
    let OldReply::Failed(failure) = postcard::from_bytes(&[1, 3]).unwrap() else {
        panic!("failure");
    };
    assert_eq!(failure, Failure::Denied);
    assert!(UsageRequest::decode(kind::USAGE_REQ, &[5, 1, 90, 1, 1]).is_err());
    assert!(UsageReply::decode(kind::USAGE_REPLY, &[2, 0, 1, 90, 1, 0, 1]).is_err());
}

#[test]
fn usage_extensions_are_filtered_and_requests_require_negotiation() {
    let account = AccountInfo {
        id: "agy-work".into(),
        provider: ProviderKind::Antigravity,
        label: "Work".into(),
        email: None,
        profile_dir: "/tmp/agy-work".into(),
        active: true,
        host_default: false,
        stale: false,
        usage: Snapshot::default(),
    };
    let reply = UsageReply::Accounts(AccountsSnapshot {
        accounts: vec![account],
        auto_switch: AutoSwitchPolicy {
            enabled: true,
            rules: vec![AutoSwitchRule {
                provider: ProviderKind::Antigravity,
                ..rule()
            }],
            ..AutoSwitchPolicy::default()
        },
        switched: vec![AutoSwitchEvent {
            provider: ProviderKind::Antigravity,
            from_id: "a".into(),
            from_label: "A".into(),
            to_id: "b".into(),
            to_label: "B".into(),
            reason_percent: 90,
        }],
        statuses: Vec::new(),
        advanced: AccountPolicyState::default(),
        connections: Box::default(),
        details: Box::default(),
    });
    let legacy = reply.clone().for_features(FEAT_USAGE);
    let frame = legacy.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
    // This exact pre-extension fixture is also decoded by the old client.
    assert_eq!(body, [2, 0, 1, 90, 1, 0]);
    assert_eq!(UsageReply::decode(kind, &body).unwrap(), legacy);
    let modern = reply
        .clone()
        .for_features(FEAT_USAGE | FEAT_USAGE_AGY | FEAT_USAGE_RULES);
    assert_eq!(modern, reply);
    let frame = modern.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
    assert_eq!(UsageReply::decode(kind, &body).unwrap(), modern);
    assert_eq!(UsageRequest::ListAccounts.required_features(), FEAT_USAGE);
    assert_eq!(
        UsageRequest::SwitchAccount {
            provider: ProviderKind::Antigravity,
            account_id: "work".into()
        }
        .required_features(),
        FEAT_USAGE | FEAT_USAGE_AGY
    );
    assert_eq!(
        UsageRequest::UpsertAutoSwitchRule(rule()).required_features(),
        FEAT_USAGE | FEAT_USAGE_RULES
    );
}

#[test]
fn auto_switch_metrics_parse_and_evaluate_independent_windows() {
    let usage = Snapshot {
        five_hour: Some(Window {
            percent: 55,
            resets_at: None,
        }),
        seven_day: Some(Window {
            percent: 94,
            resets_at: None,
        }),
    };
    for (metric, aliases, value, label) in [
        (AutoSwitchMetric::Max, ["max", "Max"], 94, "Max(5h/7d)"),
        (AutoSwitchMetric::FiveHour, ["5h", "five_hour"], 55, "5h"),
        (AutoSwitchMetric::SevenDay, ["7d", "seven_day"], 94, "7d"),
    ] {
        for alias in aliases {
            assert_eq!(AutoSwitchMetric::parse(alias), Some(metric));
        }
        assert_eq!(AutoSwitchMetric::parse(metric.short_code()), Some(metric));
        assert_eq!(metric.label(), label);
        assert_eq!(metric.evaluate(&usage), value);
        assert_eq!(metric.evaluate(&Snapshot::default()), 0);
    }
    assert_eq!(AutoSwitchMetric::parse("daily"), None);
}

fn rule() -> AutoSwitchRule {
    AutoSwitchRule {
        provider: ProviderKind::Claude,
        from_account_id: "host".into(),
        enabled: true,
        threshold_percent: 90,
        metric: AutoSwitchMetric::FiveHour,
        target_account_id: Some("work".into()),
    }
}

fn status_account() -> AccountInfo {
    AccountInfo {
        id: "claude-work".into(),
        provider: ProviderKind::Claude,
        label: "Work".into(),
        email: None,
        profile_dir: "/tmp/claude-work".into(),
        active: true,
        host_default: false,
        stale: false,
        usage: Snapshot {
            five_hour: Some(Window {
                percent: 42,
                resets_at: None,
            }),
            seven_day: None,
        },
    }
}

/// OWE-703 1단계: 상태 꼬리는 N-1 양방향 호환이다.
/// 구버전 바이트 → 신버전은 모름, 신버전 바이트 → 구버전 구조는 꼬리 무시다.
#[test]
fn account_status_tail_is_n1_bidirectionally_compatible() {
    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct OldPolicy {
        enabled: bool,
        threshold_percent: u8,
        switch_on_rate_limit: bool,
    }
    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct OldAccounts {
        accounts: Vec<AccountInfo>,
        auto_switch: OldPolicy,
        switched: Vec<AutoSwitchEvent>,
    }
    #[derive(Deserialize)]
    enum OldReply {
        Current(Snapshot),
        Failed(Failure),
        Accounts(OldAccounts),
    }
    type LegacyAccounts = (u8, Vec<AccountInfo>, (bool, u8, bool), Vec<AutoSwitchEvent>);

    // 구버전 바이트(상태 꼬리 없음) → 신버전 디코딩: 상태는 비어 있음 = 전부 모름.
    let old_bytes = [2, 0, 1, 90, 1, 0];
    let UsageReply::Accounts(snap) = UsageReply::decode(kind::USAGE_REPLY, &old_bytes).unwrap()
    else {
        panic!("accounts");
    };
    assert!(snap.statuses.is_empty());
    assert!(snap.status_for("claude-work").is_none());

    // 신버전(상태 꼬리 포함) → 구버전 구조 디코딩: 앞부분만 읽고 꼬리를 무시한다.
    let full = UsageReply::Accounts(AccountsSnapshot {
        accounts: vec![status_account()],
        auto_switch: AutoSwitchPolicy::default(),
        switched: Vec::new(),
        advanced: AccountPolicyState::default(),
        connections: Box::default(),
        details: Box::default(),
        statuses: vec![
            AccountStatusInfo {
                account_id: "claude-work".into(),
                status: AccountStatus::Ok,
                updated_at: Some(1_792_123_200),
                detail: None,
            },
            AccountStatusInfo {
                account_id: "claude-gone".into(),
                status: AccountStatus::NeedsLogin,
                updated_at: None,
                detail: Some("재로그인".into()),
            },
        ],
    });
    let frame = full.clone().encode().unwrap();
    let (k, body) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
    assert_eq!(UsageReply::decode(k, &body).unwrap(), full);
    let OldReply::Accounts(old) = postcard::from_bytes(&body).unwrap() else {
        panic!("old reply");
    };
    assert_eq!(old.accounts, vec![status_account()]);
    assert!(!old.auto_switch.enabled);

    // rules 인식 구버전 디코더는 새 꼬리(튜플)의 첫 목록만 읽는다.
    let ruled = UsageReply::Accounts(AccountsSnapshot {
        auto_switch: AutoSwitchPolicy {
            rules: vec![rule()],
            ..AutoSwitchPolicy::default()
        },
        statuses: vec![AccountStatusInfo {
            account_id: "x".into(),
            status: AccountStatus::Stale,
            updated_at: Some(1_792_123_200),
            detail: None,
        }],
        ..AccountsSnapshot::default()
    });
    let frame = ruled.encode().unwrap();
    let (_, ruled_body) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
    let (_, ruled_tail): (LegacyAccounts, &[u8]) = postcard::take_from_bytes(&ruled_body).unwrap();
    assert_eq!(
        postcard::from_bytes::<Vec<AutoSwitchRule>>(ruled_tail).unwrap(),
        vec![rule()]
    );
    assert_eq!(
        UsageReply::decode(kind::USAGE_REPLY, &ruled_body).unwrap(),
        ruled
    );
    // 다른 구버전 variant 디코더도 살아 있다.
    let OldReply::Current(current) = postcard::from_bytes(&[0, 0, 0]).unwrap() else {
        panic!("current");
    };
    assert_eq!(current, Snapshot::default());
    let OldReply::Failed(failure) = postcard::from_bytes(&[1, 3]).unwrap() else {
        panic!("failure");
    };
    assert_eq!(failure, Failure::Denied);

    // 상태 비트 없는 피어에게는 꼬리를 비워 보낸다(규칙 비트와 독립).
    let stripped = full
        .clone()
        .for_features(FEAT_USAGE | FEAT_USAGE_AGY | FEAT_USAGE_RULES);
    let UsageReply::Accounts(stripped_snap) = stripped else {
        panic!("stripped");
    };
    assert!(stripped_snap.statuses.is_empty());
    assert_eq!(stripped_snap.accounts, vec![status_account()]);
    let modern = full
        .clone()
        .for_features(FEAT_USAGE | FEAT_USAGE_AGY | FEAT_USAGE_RULES | FEAT_USAGE_STATUS);
    assert_eq!(modern, full);

    // 규칙도 상태도 없으면 구버전 바이트와 같은 몸통이다.
    let bare = UsageReply::Accounts(AccountsSnapshot {
        auto_switch: AutoSwitchPolicy {
            enabled: true,
            ..AutoSwitchPolicy::default()
        },
        statuses: vec![AccountStatusInfo {
            account_id: "a".into(),
            status: AccountStatus::Ok,
            updated_at: None,
            detail: None,
        }],
        ..AccountsSnapshot::default()
    })
    .for_features(FEAT_USAGE);
    let frame = bare.encode().unwrap();
    let (_, body) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
    assert_eq!(body, [2, 0, 1, 90, 1, 0]);

    // 깨진 꼬리는 거부한다(규칙 꼬리·상태 꼬리 모두).
    assert!(UsageReply::decode(kind::USAGE_REPLY, &[2, 0, 1, 90, 1, 0, 1]).is_err());
    let frame = full.encode().unwrap();
    let (_, full_body) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
    let mut truncated = full_body.clone();
    truncated.pop();
    assert!(UsageReply::decode(kind::USAGE_REPLY, &truncated).is_err());
}

/// OWE-703 1단계: 2단계 UI가 쓸 공개 API(상태 라벨·갱신 시각·창 표기).
#[test]
fn account_status_display_helpers_for_step_two_ui() {
    for (status, label, describe) in [
        (AccountStatus::Unknown, "모름", "사용량을 알 수 없음"),
        (AccountStatus::Stale, "오래됨", "오래된 사용량 값"),
        (AccountStatus::NeedsLogin, "로그인 필요", "다시 로그인 필요"),
        (AccountStatus::Error, "오류", "사용량 조회 오류"),
        (AccountStatus::Ok, "정상", "정상"),
    ] {
        assert_eq!(status.label_ko(), label);
        assert_eq!(status.describe_ko(), describe);
    }
    assert_eq!(AccountStatus::default(), AccountStatus::Unknown);

    let now = 1_792_123_200_u64;
    assert_eq!(format_updated_ago_ko(None, now), "갱신 없음");
    assert_eq!(format_updated_ago_ko(Some(now), now), "방금 전");
    assert_eq!(format_updated_ago_ko(Some(now + 60), now), "방금 전");
    assert_eq!(format_updated_ago_ko(Some(now - 45), now), "45초 전");
    assert_eq!(format_updated_ago_ko(Some(now - 120), now), "2분 전");
    assert_eq!(format_updated_ago_ko(Some(now - 7_200), now), "2시간 전");
    assert_eq!(format_updated_ago_ko(Some(now - 172_800), now), "2일 전");
    assert!(!is_updated_stale(Some(now - 60), now));
    assert!(is_updated_stale(Some(now - 1_801), now));
    assert!(is_updated_stale(None, now));

    let now_i64 = 1_792_123_200_i64;
    assert_eq!(format_window_percent_ko(None, now_i64), "—");
    assert_eq!(
        format_window_percent_ko(
            Some(&Window {
                percent: 42,
                resets_at: None
            }),
            now_i64
        ),
        "42%"
    );
    assert_eq!(
        format_window_percent_ko(
            Some(&Window {
                percent: 95,
                resets_at: Some("2001-01-01T00:00:00Z".into())
            }),
            now_i64
        ),
        "리셋됨"
    );
    assert_eq!(
        format_window_percent_ko(
            Some(&Window {
                percent: 95,
                resets_at: Some("2099-01-01T00:00:00Z".into())
            }),
            now_i64
        ),
        "95%"
    );

    let snap = AccountsSnapshot {
        statuses: vec![AccountStatusInfo {
            account_id: "a".into(),
            status: AccountStatus::Stale,
            updated_at: Some(now),
            detail: None,
        }],
        ..AccountsSnapshot::default()
    };
    assert_eq!(
        snap.status_for("a").map(|s| s.status),
        Some(AccountStatus::Stale)
    );
    assert!(snap.status_for("b").is_none());
}

/// OWE-703 1단계: 모름·리셋 지남은 `None`이다(0% 취급 금지).
#[test]
fn effective_percent_never_zero_for_unknown() {
    let now = 1_792_123_200_i64;
    assert_eq!(
        AutoSwitchMetric::Max.effective_percent(&Snapshot::default(), now),
        None
    );
    let expired = Snapshot {
        five_hour: Some(Window {
            percent: 95,
            resets_at: Some("2001-01-01T00:00:00Z".into()),
        }),
        seven_day: Some(Window {
            percent: 40,
            resets_at: None,
        }),
    };
    assert_eq!(
        AutoSwitchMetric::FiveHour.effective_percent(&expired, now),
        None
    );
    assert_eq!(
        AutoSwitchMetric::Max.effective_percent(&expired, now),
        Some(40)
    );
    let fresh = Snapshot {
        five_hour: Some(Window {
            percent: 92,
            resets_at: Some("2099-01-01T00:00:00Z".into()),
        }),
        seven_day: None,
    };
    assert_eq!(
        AutoSwitchMetric::Max.effective_percent(&fresh, now),
        Some(92)
    );
    // 구경로 `evaluate`는 호환용으로만 0으로 접는다.
    assert_eq!(AutoSwitchMetric::Max.evaluate(&Snapshot::default()), 0);
}

#[test]
fn account_status_json_defaults_for_legacy_documents() {
    let snap: AccountsSnapshot = serde_json::from_str(
        r#"{"accounts":[],"auto_switch":{"enabled":true,"threshold_percent":90,"switch_on_rate_limit":true},"switched":[]}"#,
    )
    .unwrap();
    assert!(snap.statuses.is_empty());
    let info: AccountStatusInfo = serde_json::from_str(r#"{"account_id":"a"}"#).unwrap();
    assert_eq!(info.status, AccountStatus::Unknown);
    assert_eq!(info.updated_at, None);
    assert_eq!(info.detail, None);
}

/// OWE-703 2단계 지시: 한도 구분 비트가 없으면 기존 표현(`Error`)으로 폴백한다.
/// enum에 variant가 늘어도 꼬리째 무시하는 구버전 디코더는 깨지지 않는다.
#[test]
fn rate_limited_falls_back_to_error_without_feature_bit() {
    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct OldPolicyLite {
        enabled: bool,
        threshold_percent: u8,
        switch_on_rate_limit: bool,
    }
    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct OldAccountsLite {
        accounts: Vec<AccountInfo>,
        auto_switch: OldPolicyLite,
        switched: Vec<AutoSwitchEvent>,
    }
    #[derive(Deserialize)]
    enum OldReplyLite {
        Current(Snapshot),
        Failed(Failure),
        Accounts(OldAccountsLite),
    }
    assert_eq!(
        ProviderKind::from_env_var("CLAUDE_CONFIG_DIR"),
        Some(ProviderKind::Claude)
    );
    assert_eq!(
        ProviderKind::from_env_var("CODEX_HOME"),
        Some(ProviderKind::Codex)
    );
    assert_eq!(ProviderKind::from_env_var("UNKNOWN_VAR"), None);

    let full = UsageReply::Accounts(AccountsSnapshot {
        statuses: vec![AccountStatusInfo {
            account_id: "a".into(),
            status: AccountStatus::RateLimited,
            updated_at: Some(1_792_123_200),
            detail: None,
        }],
        ..AccountsSnapshot::default()
    });
    let legacy = full.clone().for_features(FEAT_USAGE | FEAT_USAGE_STATUS);
    let UsageReply::Accounts(legacy_snap) = legacy else {
        panic!("legacy");
    };
    assert_eq!(legacy_snap.statuses[0].status, AccountStatus::Error);
    assert_eq!(
        legacy_snap.statuses[0].detail.as_deref(),
        Some("rate_limit")
    );
    let modern = full
        .clone()
        .for_features(FEAT_USAGE | FEAT_USAGE_STATUS | FEAT_USAGE_RATELIMIT);
    assert_eq!(modern, full);
    // 구버전 구조 디코더는 새 variant가 든 꼬리도 통째로 무시한다.
    let frame = full.encode().unwrap();
    let (_, body) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
    let OldReplyLite::Accounts(old) = postcard::from_bytes(&body).unwrap() else {
        panic!("old reply");
    };
    assert!(old.accounts.is_empty());
    let OldReplyLite::Current(current) = postcard::from_bytes(&[0, 0, 0]).unwrap() else {
        panic!("current");
    };
    assert_eq!(current, Snapshot::default());
    let OldReplyLite::Failed(failure) = postcard::from_bytes(&[1, 3]).unwrap() else {
        panic!("failure");
    };
    assert_eq!(failure, Failure::Denied);
}

#[test]
fn auto_switch_policy_upsert_clamps_and_keys_by_provider_and_account() {
    let mut policy = AutoSwitchPolicy::default();
    let mut first = rule();
    // K-27: 문서 범위(1~100)를 따른다. 50~99 고정은 사용자 설정을 말없이 바꿔버린다.
    first.threshold_percent = 1;
    policy.upsert_rule(first.clone());
    assert_eq!(
        policy
            .rule_for(first.provider, "host")
            .unwrap()
            .threshold_percent,
        1
    );
    first.threshold_percent = 100;
    policy.upsert_rule(first.clone());
    assert_eq!(policy.rules.len(), 1);
    assert_eq!(policy.rules[0].threshold_percent, 100);
    first.provider = ProviderKind::Codex;
    policy.upsert_rule(first);
    assert_eq!(policy.rules.len(), 2);
    assert!(policy.remove_rule(ProviderKind::Claude, "host"));
    assert!(!policy.remove_rule(ProviderKind::Claude, "host"));
    assert!(policy.rule_for(ProviderKind::Codex, "host").is_some());
}

#[test]
fn auto_switch_json_defaults_for_legacy_policy_and_rule() {
    let policy: AutoSwitchPolicy = serde_json::from_str(
        r#"{"enabled":true,"threshold_percent":90,"switch_on_rate_limit":true}"#,
    )
    .unwrap();
    assert!(policy.rules.is_empty());
    let rule: AutoSwitchRule = serde_json::from_str(
        r#"{"provider":"Claude","from_account_id":"host","enabled":true,"threshold_percent":90}"#,
    )
    .unwrap();
    assert_eq!(rule.metric, AutoSwitchMetric::Max);
    assert_eq!(rule.target_account_id, None);
    assert_eq!(
        serde_json::to_string(&AutoSwitchMetric::FiveHour).unwrap(),
        "\"five_hour\""
    );
}

#[test]
fn rule_requests_append_variant_tags_and_roundtrip() {
    fixture(&UsageRequest::ListAccounts, &[2, 0, 0, 0, 0x41, 1]);
    fixture(
        &UsageRequest::SwitchAccount {
            provider: ProviderKind::Claude,
            account_id: "a".into(),
        },
        &[5, 0, 0, 0, 0x41, 2, 0, 1, b'a'],
    );
    fixture(
        &UsageRequest::AddAccount {
            provider: ProviderKind::Claude,
            label: "a".into(),
            email: None,
            profile_dir: "b".into(),
        },
        &[8, 0, 0, 0, 0x41, 3, 0, 1, b'a', 0, 1, b'b'],
    );
    fixture(
        &UsageRequest::RemoveAccount {
            provider: ProviderKind::Claude,
            account_id: "a".into(),
        },
        &[5, 0, 0, 0, 0x41, 4, 0, 1, b'a'],
    );
    fixture(
        &UsageRequest::SetAutoSwitch(AutoSwitchPolicy {
            enabled: true,
            ..AutoSwitchPolicy::default()
        }),
        &[5, 0, 0, 0, 0x41, 5, 1, 90, 1],
    );
    fixture(
        &UsageRequest::UpsertAutoSwitchRule(AutoSwitchRule {
            provider: ProviderKind::Claude,
            from_account_id: "a".into(),
            enabled: true,
            threshold_percent: 90,
            metric: AutoSwitchMetric::Max,
            target_account_id: None,
        }),
        &[9, 0, 0, 0, 0x41, 6, 0, 1, b'a', 1, 90, 0, 0],
    );
    fixture(
        &UsageRequest::RemoveAutoSwitchRule {
            provider: ProviderKind::Claude,
            from_account_id: "a".into(),
        },
        &[5, 0, 0, 0, 0x41, 7, 0, 1, b'a'],
    );
    let req = UsageRequest::UpsertAutoSwitchRule(rule());
    let encoded = req.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut &encoded[..]).unwrap().unwrap();
    assert_eq!(UsageRequest::decode(kind, &body).unwrap(), req);
}

/// OWE-704·705: 관리 요청은 enum 끝에 붙어 기존 variant 바이트가 그대로이고, 고정 바이트로 왕복하며
/// `FEAT_USAGE_MANAGE`를 협상해야 보낸다. 구 디코더(variant 0~7만 아는 데몬)는 새 `tag`를 거부한다.
#[test]
fn manage_requests_append_tags_roundtrip_and_require_feature() {
    // N-1: 관리 기능 이전 데몬의 요청 enum.
    #[derive(Debug, Deserialize)]
    #[expect(dead_code, reason = "디코딩 호환 검사용 구 enum")]
    enum LegacyRequest {
        Get,
        ListAccounts,
        SwitchAccount {
            provider: ProviderKind,
            account_id: String,
        },
        AddAccount {
            provider: ProviderKind,
            label: String,
            email: Option<String>,
            profile_dir: String,
        },
        RemoveAccount {
            provider: ProviderKind,
            account_id: String,
        },
        SetAutoSwitch(AutoSwitchPolicy),
        UpsertAutoSwitchRule(AutoSwitchRule),
        RemoveAutoSwitchRule {
            provider: ProviderKind,
            from_account_id: String,
        },
    }
    fixture(
        &UsageRequest::RenameAccount {
            provider: ProviderKind::Claude,
            account_id: "a".into(),
            label: "b".into(),
            email: Some("c".into()),
        },
        &[10, 0, 0, 0, 0x41, 8, 0, 1, b'a', 1, b'b', 1, 1, b'c'],
    );
    fixture(
        &UsageRequest::DeleteAccount {
            provider: ProviderKind::Claude,
            account_id: "a".into(),
        },
        &[5, 0, 0, 0, 0x41, 9, 0, 1, b'a'],
    );
    fixture(&UsageRequest::ListOrphanProfiles, &[2, 0, 0, 0, 0x41, 10]);
    fixture(&UsageRequest::PurgeOrphanProfiles, &[2, 0, 0, 0, 0x41, 11]);
    fixture(
        &UsageReply::Orphans(vec![OrphanProfile {
            provider: ProviderKind::OpenCode,
            dir: "d".into(),
        }]),
        &[6, 0, 0, 0, 0x61, 3, 1, 3, 1, b'd'],
    );
    assert!(UsageRequest::ListOrphanProfiles.is_read());
    assert!(!UsageRequest::PurgeOrphanProfiles.is_read());
    assert_eq!(
        UsageRequest::PurgeOrphanProfiles.required_features(),
        FEAT_USAGE | FEAT_USAGE_MANAGE
    );
    assert_eq!(
        UsageRequest::DeleteAccount {
            provider: ProviderKind::Antigravity,
            account_id: "a".into(),
        }
        .required_features(),
        FEAT_USAGE | FEAT_USAGE_MANAGE | FEAT_USAGE_AGY
    );

    let frame = UsageRequest::DeleteAccount {
        provider: ProviderKind::Claude,
        account_id: "a".into(),
    }
    .encode()
    .unwrap();
    let (_, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
    assert!(postcard::from_bytes::<LegacyRequest>(&body).is_err());
    let frame = UsageRequest::RemoveAccount {
        provider: ProviderKind::Claude,
        account_id: "a".into(),
    }
    .encode()
    .unwrap();
    let (_, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
    assert!(postcard::from_bytes::<LegacyRequest>(&body).is_ok());
}

#[test]
fn advanced_policy_tail_is_gated_and_legacy_prefix_is_unchanged() {
    let mut snapshot = AccountsSnapshot::default();
    snapshot.advanced.supported = true;
    snapshot.advanced.policy.predict_exhaustion = true;
    let reply = UsageReply::Accounts(snapshot.clone());
    let bytes = reply.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut bytes.as_slice()).unwrap().unwrap();
    let (legacy, _) = postcard::take_from_bytes::<(
        u8,
        Vec<AccountInfo>,
        (bool, u8, bool),
        Vec<AutoSwitchEvent>,
    )>(&body)
    .unwrap();
    assert_eq!(legacy.0, 2);
    assert!(legacy.1.is_empty());
    assert!(legacy.3.is_empty());
    assert_eq!(UsageReply::decode(kind, &body).unwrap(), reply);
    let gated = reply.for_features(FEAT_USAGE | FEAT_USAGE_RULES | FEAT_USAGE_STATUS);
    let UsageReply::Accounts(gated_snapshot) = &gated else {
        panic!("accounts")
    };
    assert_eq!(gated_snapshot.advanced, AccountPolicyState::default());
    let bytes = gated.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut bytes.as_slice()).unwrap().unwrap();
    assert_eq!(UsageReply::decode(kind, &body).unwrap(), gated);
    assert_eq!(
        UsageRequest::SetAdvancedPolicy(AdvancedPolicy::default()).required_features(),
        FEAT_USAGE | FEAT_ACCOUNT_POLICY
    );
}

#[test]
fn connection_tail_negotiation_roundtrip_and_n_minus_one_prefix() {
    type Legacy = (u8, Vec<AccountInfo>, (bool, u8, bool), Vec<AutoSwitchEvent>);
    let snap = AccountsSnapshot {
        auto_switch: AutoSwitchPolicy {
            enabled: true,
            ..AutoSwitchPolicy::default()
        },
        connections: Box::new(ConnectionsSnapshot {
            supported: true,
            direct_usage: true,
            accounts: vec![AccountConnection {
                account_id: "codex-host".into(),
                status: ConnectionStatus::CheckFailed,
            }],
        }),
        ..AccountsSnapshot::default()
    };
    assert_eq!(
        snap.connection_for("codex-host"),
        ConnectionStatus::CheckFailed
    );
    let reply = UsageReply::Accounts(snap);
    let bytes = reply.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut bytes.as_slice()).unwrap().unwrap();
    assert_eq!(UsageReply::decode(kind, &body).unwrap(), reply);
    let (prefix, _) = postcard::take_from_bytes::<Legacy>(&body).unwrap();
    assert_eq!(prefix, (2, vec![], (true, 90, true), vec![]));
    let old = reply.for_features(FEAT_USAGE);
    let bytes = old.encode().unwrap();
    let (_, body) = ipc::read_frame(&mut bytes.as_slice()).unwrap().unwrap();
    assert_eq!(body, [2, 0, 1, 90, 1, 0]);
    let request = UsageRequest::SetDirectUsage(false);
    assert_eq!(
        request.required_features(),
        FEAT_USAGE | FEAT_ACCOUNT_CONNECTION
    );
    assert!(!request.is_read());
    let bytes = request.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut bytes.as_slice()).unwrap().unwrap();
    assert_eq!(UsageRequest::decode(kind, &body).unwrap(), request);
}

/// OWE-722: 미출시 관리 비트로 보호하며 기존 enum 인덱스를 바꾸지 않는다.
#[test]
fn provisioning_fixed_bytes_roundtrip_and_feature_gate() {
    fixture(
        &UsageRequest::SetProvisioning(true),
        &[3, 0, 0, 0, 0x41, 14, 1],
    );
    fixture(
        &UsageRequest::SetProvisioning(false),
        &[3, 0, 0, 0, 0x41, 14, 0],
    );
    fixture(&UsageRequest::GetProvisioning, &[2, 0, 0, 0, 0x41, 15]);
    fixture(&UsageReply::Provisioning(true), &[3, 0, 0, 0, 0x61, 4, 1]);
    fixture(&UsageReply::Provisioning(false), &[3, 0, 0, 0, 0x61, 4, 0]);
    for req in [
        UsageRequest::SetProvisioning(true),
        UsageRequest::GetProvisioning,
    ] {
        assert_eq!(req.required_features(), FEAT_USAGE | FEAT_USAGE_MANAGE);
    }
    assert!(UsageRequest::GetProvisioning.is_read());
    assert!(!UsageRequest::SetProvisioning(false).is_read());
}

#[test]
fn connection_tail_has_frozen_bytes_and_prior_policy_decoder_can_skip_it() {
    type Prefix = (
        u8,
        Vec<AccountInfo>,
        (bool, u8, bool),
        Vec<AutoSwitchEvent>,
        Vec<AutoSwitchRule>,
        Vec<AccountStatusInfo>,
    );
    let reply = UsageReply::Accounts(AccountsSnapshot {
        auto_switch: AutoSwitchPolicy {
            enabled: true,
            ..AutoSwitchPolicy::default()
        },
        connections: Box::new(ConnectionsSnapshot {
            supported: true,
            direct_usage: true,
            accounts: vec![],
        }),
        ..AccountsSnapshot::default()
    });
    let frame = reply.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
    assert_eq!(
        body,
        [2, 0, 1, 90, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0]
    );
    let (_, rest) = postcard::take_from_bytes::<Prefix>(&body).unwrap();
    let (old_policy, tail) = postcard::take_from_bytes::<AccountPolicyState>(rest).unwrap();
    assert_eq!(old_policy, AccountPolicyState::default());
    assert_eq!(tail, [1, 1, 0]);
    assert_eq!(UsageReply::decode(kind, &body).unwrap(), reply);
}

#[test]
fn reset_credit_request_reply_roundtrip_and_feature_gate() {
    // 요청은 enum 끝에 붙는다(Codex discriminant 1, 계정 id "a").
    fixture(
        &UsageRequest::ConsumeResetCredit {
            provider: ProviderKind::Codex,
            account_id: "a".into(),
        },
        &[5, 0, 0, 0, 0x41, 17, 1, 1, b'a'],
    );
    let request = UsageRequest::ConsumeResetCredit {
        provider: ProviderKind::Codex,
        account_id: "a".into(),
    };
    assert!(!request.is_read());
    assert!(!request.contains_removed_provider());
    assert_eq!(request.required_features(), FEAT_USAGE | FEAT_USAGE_RESET);
    assert_eq!(
        UsageRequest::ConsumeResetCredit {
            provider: ProviderKind::Gemini,
            account_id: "a".into(),
        }
        .required_features(),
        FEAT_USAGE | FEAT_USAGE_RESET
    );
    assert!(
        UsageRequest::ConsumeResetCredit {
            provider: ProviderKind::Gemini,
            account_id: "a".into(),
        }
        .contains_removed_provider()
    );

    let reply = UsageReply::ResetConsumed {
        snapshot: AccountsSnapshot::default(),
        outcome: ResetConsumeOutcome::Reset,
        windows_reset: 2,
    };
    let frame = reply.encode().unwrap();
    let (kind, body) = ipc::read_frame(&mut frame.as_slice()).unwrap().unwrap();
    // 응답도 끝에 붙는다(5): 기본 스냅샷 바이트 뒤에 결과(0)·창 수(2).
    assert!(body.starts_with(&[5]));
    assert!(body.ends_with(&[0, 2]));
    assert_eq!(UsageReply::decode(kind, &body).unwrap(), reply);
    // 새 비트를 모르는 피어에는 스냅샷만 남는다.
    let gated = reply.for_features(FEAT_USAGE);
    assert_eq!(gated, UsageReply::Accounts(AccountsSnapshot::default()));
}
