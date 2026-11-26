use std::fs;
use std::process::Command;

use super::*;

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mk084-{}-{tag}", std::process::id()));
    fs::remove_dir_all(&d).ok();
    fs::create_dir_all(&d).unwrap();
    d
}

/// Claude local file measured format.
#[test]
fn claude_files_parse_five_hour_and_seven_day() {
    let cache = r#"{"cached_at":"2026-10-15T00:43:50.149Z","five_hour":{"utilization":3,"resets_at":"2026-10-15T04:30:00.000Z"},"seven_day":{"utilization":74,"resets_at":"2026-10-16T04:00:00.000Z"}}"#;
    let usage = parse_claude(cache);
    assert_eq!(
        usage,
        Usage {
            five_hour: Some(Window {
                percent: 3,
                resets_at: Some("2026-10-15T04:30:00.000Z".to_owned()),
            }),
            seven_day: Some(Window {
                percent: 74,
                resets_at: Some("2026-10-16T04:00:00.000Z".to_owned()),
            }),
        }
    );
    assert!(!usage.warn());

    let stdin = r#"{"rate_limits":{"five_hour":{"used_percentage":18,"resets_at":1792038600},"seven_day":{"used_percentage":83,"resets_at":1792123200}}}"#;
    let native = parse_claude(stdin);
    assert_eq!(native.five_hour.as_ref().map(|w| w.percent), Some(18));
    assert_eq!(
        native.seven_day.as_ref().and_then(|w| w.resets_at.clone()),
        Some("1792123200".to_owned())
    );
    assert!(native.warn());

    assert_eq!(parse_claude("broken {"), Usage::default());
    assert!(parse_claude("{}").is_empty());

    let home = dir("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    let orca = r#"{"accounts":[{"label":"a","host":true,"five_hour":{"utilization":8,"resets_at":"2026-10-15T04:30:00.000Z"},"seven_day":{"utilization":73,"resets_at":"2026-10-16T04:00:00.000Z"}},{"label":"b","host":false,"five_hour":{"utilization":0,"resets_at":null},"seven_day":{"utilization":100,"resets_at":"2026-10-16T04:00:00.000Z"}}]}"#;
    fs::write(home.join(".claude").join(ORCA_CACHE), orca).unwrap();
    let usage = read_claude(&home).unwrap();
    assert_eq!(usage.five_hour.as_ref().map(|w| w.percent), Some(8));
    assert!(!usage.warn());
    let single = r#"{"five_hour":{"utilization":81,"resets_at":"2026-10-15T04:30:00.000Z"}}"#;
    fs::write(home.join(".claude").join(CLAUDE_CACHE), single).unwrap();
    assert!(read_claude(&home).unwrap().warn());
    fs::remove_dir_all(&home).ok();
}

/// Codex rollout measured format.
#[test]
fn codex_rollout_parses_primary_and_secondary() {
    let line = r#"{"type":"event_msg","payload":{"type":"token_count"},"rate_limits":{"limit_id":"codex","primary":{"used_percent":74.0,"window_minutes":300,"resets_at":1791714446},"secondary":{"used_percent":12.0,"window_minutes":10080,"resets_at":1792301246},"plan_type":"plus","rate_limit_reached_type":null}}"#;
    assert_eq!(
        parse_codex_line(line),
        Some(Usage {
            five_hour: Some(Window {
                percent: 74,
                resets_at: Some("1791714446".to_owned()),
            }),
            seven_day: Some(Window {
                percent: 12,
                resets_at: Some("1792301246".to_owned()),
            }),
        })
    );
    assert!(parse_codex_line(line).is_some_and(|u| !u.warn()));
    assert_eq!(parse_codex_line("broken {"), None);

    let other = r#"{"type":"event_msg","payload":{"type":"other"}}"#;
    let text = format!("{line}\n{other}\n{line}");
    assert!(parse_codex_rollout(&text).is_some());
    assert_eq!(parse_codex_rollout("empty\nbroken {\n"), None);

    let root = dir("codex");
    let day = root.join("2026/10/11");
    fs::create_dir_all(&day).unwrap();
    let old = r#"{"rate_limits":{"primary":{"used_percent":12.0,"window_minutes":300},"secondary":{"used_percent":7.0,"window_minutes":10080}}}"#;
    fs::write(
        day.join("rollout-2026-10-11T17-45-57-old.jsonl"),
        format!("{old}\n"),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    fs::write(
        day.join("rollout-2026-10-11T17-50-22-new.jsonl"),
        format!("broken head\n{line}\n"),
    )
    .unwrap();
    let usage = read_codex(&root).unwrap();
    assert_eq!(usage.five_hour.as_ref().map(|w| w.percent), Some(74));
    assert!(read_codex(&dir("missing")).is_none());
    fs::remove_dir_all(&root).ok();
}

#[test]
fn multi_account_registry_manual_switch_and_crud() {
    use krill_proto::usage::ProviderKind;

    let home = dir("multi-home");
    let state = dir("multi-state");

    let work_dir = home.join("claude-work");
    let backup_dir = home.join("claude-backup");
    accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Work Max",
        Some("work@example.com".into()),
        &work_dir.display().to_string(),
    )
    .unwrap();
    accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Backup Pro",
        Some("backup@example.com".into()),
        &backup_dir.display().to_string(),
    )
    .unwrap();

    fs::write(
        work_dir.join(CLAUDE_CACHE),
        r#"{"five_hour":{"utilization":35},"seven_day":{"utilization":55}}"#,
    )
    .unwrap();
    fs::write(
        backup_dir.join(CLAUDE_CACHE),
        r#"{"five_hour":{"utilization":12},"seven_day":{"utilization":15}}"#,
    )
    .unwrap();

    // 호스트 계정이 92%를 넘어도 자동 전환은 일어나지 않고 호스트 계정을 유지한다.
    let host_claude = home.join(".claude");
    fs::create_dir_all(&host_claude).unwrap();
    fs::write(
        host_claude.join(CLAUDE_CACHE),
        r#"{"five_hour":{"utilization":92,"resets_at":"2099-01-01T00:00:00Z"},"seven_day":{"utilization":40}}"#,
    )
    .unwrap();

    let initial = accounts::refresh_accounts(&home, Some(&state));
    assert!(!initial.auto_switch.enabled);
    assert!(initial.switched.is_empty());
    assert!(initial.accounts.iter().any(|a| a.host_default && a.active));

    // 원클릭 수동 전환(Backup Pro로 전환) 및 CLAUDE_CONFIG_DIR 환경 변수 반영 검증.
    let switched = accounts::switch_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "claude-backup-pro",
    )
    .unwrap();
    assert!(switched.switched.is_empty());
    let active = switched
        .accounts
        .iter()
        .find(|a| a.provider == ProviderKind::Claude && a.active)
        .unwrap();
    assert_eq!(active.id, "claude-backup-pro");

    let env = accounts::active_profile_env(&home, Some(&state), "claude").unwrap();
    assert_eq!(env.0, "CLAUDE_CONFIG_DIR");
    assert_eq!(env.1, backup_dir.display().to_string());

    // 원클릭 수동 전환(Work Max로 전환) 검증.
    let switched =
        accounts::switch_account(&home, Some(&state), ProviderKind::Claude, "claude-work-max")
            .unwrap();
    let active = switched
        .accounts
        .iter()
        .find(|a| a.provider == ProviderKind::Claude && a.active)
        .unwrap();
    assert_eq!(active.id, "claude-work-max");
    assert_eq!(active.usage.max_percent(), 55);

    // 계정 삭제 검증.
    let after_rm =
        accounts::remove_account(&home, Some(&state), ProviderKind::Claude, "claude-work-max")
            .unwrap();
    assert!(!after_rm.accounts.iter().any(|a| a.id == "claude-work-max"));

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

#[test]
fn legacy_accounts_toml_ignores_auto_switch_and_advanced_policy_tables() {
    let file: accounts::AccountsFile = toml::from_str(
        "[auto_switch]\nenabled = true\nthreshold_percent = 90\nswitch_on_rate_limit = true\n[[auto_switch.rules]]\nprovider = 'Claude'\nfrom_account_id = 'claude-host'\nenabled = true\nthreshold_percent = 80\n[advanced_policy]\npredict_exhaustion = true\n",
    )
    .unwrap();
    assert!(file.accounts.is_empty());
    assert!(file.provisioning);
}

/// K-03: `accounts.toml` 파싱 실패는 기본값으로 대체하지 않는다. 원문을 보존하고 쓰기를 거부한다.
#[test]
fn corrupt_accounts_toml_is_preserved_and_writes_refused() {
    use krill_proto::usage::ProviderKind;

    let home = dir("k03-home");
    let state = dir("k03-state");
    let profile = home.join("claude-keep");
    accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Keep",
        None,
        &profile.display().to_string(),
    )
    .unwrap();
    let path = state.join("accounts.toml");
    let before = fs::read(&path).unwrap();
    let corrupt = String::from_utf8(before.clone())
        .unwrap()
        .replace("provisioning = true", "provisioning = 'corrupt'");
    assert_ne!(corrupt.as_bytes(), before.as_slice());
    fs::write(&path, &corrupt).unwrap();

    // 읽기 경로는 관대하게 기본값으로 떨어진다.
    assert!(accounts::load_file(&home, Some(&state)).accounts.is_empty());
    // 쓰기 경로는 전부 거부하고 원문을 그대로 둔다.
    assert!(
        accounts::add_account(&home, Some(&state), ProviderKind::Claude, "New", None, "",).is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), corrupt.as_bytes());
    assert!(
        accounts::switch_account(&home, Some(&state), ProviderKind::Claude, "claude-keep").is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), corrupt.as_bytes());
    // 조회는 기본값 스냅샷만 내보내고 저장하지 않는다.
    let _ = accounts::refresh_accounts(&home, Some(&state));
    assert_eq!(fs::read(&path).unwrap(), corrupt.as_bytes());

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

/// K-03: 과거 `[auto_switch]` 일부 필드 누락(부분 쓰기·구버전 파일) 때도 파싱이 깨지지 않는다.
#[test]
fn partial_legacy_auto_switch_table_parses_cleanly() {
    let file: accounts::AccountsFile = toml::from_str("[auto_switch]\n").unwrap();
    assert!(file.accounts.is_empty());
}

/// K-04: 비ASCII 라벨은 서로 다른 id·프로필 디렉터리를 받는다. 같은 라벨 재등록은 멱등하다.
#[test]
fn non_ascii_labels_get_unique_ids_and_profile_dirs() {
    use krill_proto::usage::ProviderKind;

    let home = dir("k04-home");
    let state = dir("k04-state");
    let first =
        accounts::add_account(&home, Some(&state), ProviderKind::Claude, "회사", None, "").unwrap();
    let second =
        accounts::add_account(&home, Some(&state), ProviderKind::Claude, "개인", None, "").unwrap();
    let ids: Vec<&str> = second
        .accounts
        .iter()
        .filter(|a| a.provider == ProviderKind::Claude && !a.host_default)
        .map(|a| a.id.as_str())
        .collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_ne!(ids[0], ids[1]);
    let dirs: Vec<&str> = second
        .accounts
        .iter()
        .filter(|a| a.provider == ProviderKind::Claude && !a.host_default)
        .map(|a| a.profile_dir.as_str())
        .collect();
    assert_ne!(dirs[0], dirs[1]);
    assert!(first.accounts.iter().any(|a| a.label == "회사"));
    assert!(second.accounts.iter().any(|a| a.label == "회사"));
    assert!(second.accounts.iter().any(|a| a.label == "개인"));

    // 같은 라벨 재등록은 새 항목을 만들지 않고 기존 id를 유지한다(하위호환).
    let third =
        accounts::add_account(&home, Some(&state), ProviderKind::Claude, "회사", None, "").unwrap();
    assert_eq!(
        third
            .accounts
            .iter()
            .filter(|a| a.provider == ProviderKind::Claude && !a.host_default)
            .count(),
        2
    );

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

/// K-04: 대소문자만 다른 라벨도 충돌하지 않는다.
#[test]
fn case_only_labels_do_not_collide() {
    use krill_proto::usage::ProviderKind;

    let home = dir("k04case-home");
    let state = dir("k04case-state");
    accounts::add_account(&home, Some(&state), ProviderKind::Claude, "Work", None, "").unwrap();
    let snap =
        accounts::add_account(&home, Some(&state), ProviderKind::Claude, "work", None, "").unwrap();
    let mut ids: Vec<&str> = snap
        .accounts
        .iter()
        .filter(|a| a.provider == ProviderKind::Claude && !a.host_default)
        .map(|a| a.id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

/// K-09: 동시에 저장해도 갱신이 유실되지 않고 TOML이 깨지지 않는다.
#[test]
fn concurrent_add_account_keeps_all_entries_and_valid_toml() {
    use krill_proto::usage::ProviderKind;
    use std::sync::Barrier;

    let home = dir("k09-home");
    let state = dir("k09-state");
    let barrier = std::sync::Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|n| {
            let (home, state, barrier) =
                (home.clone(), state.clone(), std::sync::Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                accounts::add_account(
                    &home,
                    Some(&state),
                    ProviderKind::Claude,
                    &format!("Worker {n}"),
                    None,
                    "",
                )
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    let file = accounts::load_file(&home, Some(&state));
    assert_eq!(file.accounts.len(), 8, "{file:?}");
    let mut ids: Vec<&str> = file.accounts.iter().map(|a| a.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 8);

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

/// K-11: `resets_at`은 epoch 초·RFC 3339(Z·오프셋·소수초)를 푼다. 모르면 `None`.
#[test]
fn reset_epoch_parses_rfc3339_and_epoch_seconds() {
    use crate::usage::accounts::parse_reset_epoch;

    assert_eq!(parse_reset_epoch("1792123200"), Some(1_792_123_200));
    assert_eq!(parse_reset_epoch("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_reset_epoch("2001-01-01T00:00:00Z"), Some(978_307_200));
    assert_eq!(
        parse_reset_epoch("2026-10-15T04:30:00.000Z"),
        Some(1_792_038_600)
    );
    assert_eq!(
        parse_reset_epoch("2099-01-01T00:00:00Z"),
        parse_reset_epoch("2099-01-01T09:00:00+09:00")
    );
    assert!(
        parse_reset_epoch("2099-01-01T00:00:00Z").unwrap()
            > parse_reset_epoch("2001-01-01T00:00:00Z").unwrap()
    );
    for bad in [
        "",
        "not-a-date",
        "2026-13-01T00:00:00Z",
        "2026-10-16",
        "12:00:00",
    ] {
        assert_eq!(parse_reset_epoch(bad), None, "{bad:?}");
    }
}

/// K-28: `active-accounts.env`는 셸에 안전하게 인용된다. source해도 값이 그대로다.
#[test]
fn active_env_shell_quotes_special_chars() {
    use krill_proto::usage::ProviderKind;

    let home = dir("k28-home");
    let state = dir("k28-state");
    let nasty = home.join("a$b`c'd\"e");
    accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Nasty",
        None,
        &nasty.display().to_string(),
    )
    .unwrap();
    let added = accounts::load_file(&home, Some(&state));
    let id = added
        .accounts
        .iter()
        .find(|a| a.label == "Nasty")
        .map(|a| a.id.clone())
        .unwrap();
    accounts::switch_account(&home, Some(&state), ProviderKind::Claude, &id).unwrap();
    let env_path = state.join("active-accounts.env");
    let content = fs::read_to_string(&env_path).unwrap();
    let line = content
        .lines()
        .find(|l| l.starts_with("export CLAUDE_CONFIG_DIR="))
        .unwrap();
    assert!(line.starts_with("export CLAUDE_CONFIG_DIR='"), "{line:?}");
    assert!(line.ends_with('\''), "{line:?}");
    // 셸로 읽어도 디렉터리 그대로다.
    let out = Command::new("sh")
        .args([
            "-c",
            &format!(
                ". '{}'; printf '%s' \"$CLAUDE_CONFIG_DIR\"",
                env_path.display()
            ),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, nasty.display().to_string().as_bytes());

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

#[test]
fn cross_review_reset_dates_reject_overflow_and_invalid_components() {
    for date in [
        "9223372036854775807-01-01T00:00:00Z",
        "2026-02-30T00:00:00Z",
        "2026-01-01T-1:00:00Z",
        "2026-01-01T00:00:00+-1:00",
    ] {
        assert_eq!(accounts::parse_reset_epoch(date), None, "{date}");
    }
}

proptest::proptest! {
    #[test]
    fn cross_review_reset_parser_never_panics(
        text in ".{0,256}",
        year in proptest::num::i64::ANY,
    ) {
        core::hint::black_box(accounts::parse_reset_epoch(&text));
        core::hint::black_box(accounts::parse_reset_epoch(&format!(
            "{year}-01-01T00:00:00Z"
        )));
    }
}

#[test]
fn cross_review_reregister_preserves_suffixed_id_and_custom_directory() {
    use krill_proto::usage::ProviderKind;
    let home = dir("cross-id-home");
    let state = dir("cross-id-state");
    accounts::add_account(&home, Some(&state), ProviderKind::Claude, "회사", None, "").unwrap();
    let custom = home.join("custom-profile").display().to_string();
    accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "개인",
        None,
        &custom,
    )
    .unwrap();
    accounts::remove_account(&home, Some(&state), ProviderKind::Claude, "claude-account").unwrap();
    accounts::add_account(&home, Some(&state), ProviderKind::Claude, "개인", None, "").unwrap();
    let file = accounts::load_file(&home, Some(&state));
    assert_eq!(file.accounts.len(), 1);
    assert_eq!(file.accounts[0].id, "claude-account-2");
    assert_eq!(file.accounts[0].profile_dir, custom);
    fs::remove_dir_all(home).unwrap();
    fs::remove_dir_all(state).unwrap();
}

/// OWE-703 1단계: Orca 캐시의 `error`·`detail`·최상위 `updated_at`을 읽는다.
#[test]
fn orca_cache_parses_error_detail_and_updated_at() {
    let rows = parse_orca(
        r#"{"updated_at":"2026-10-16T12:00:00Z","accounts":[
        {"id":"u-1","label":"A","host":true,"error":"재로그인","detail":"token 400","cooldown_until":1790991920613,"stale":true,"five_hour":{"utilization":42}}
    ]}"#,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].error.as_deref(), Some("재로그인"));
    assert_eq!(rows[0].detail.as_deref(), Some("token 400"));
    assert!(rows[0].stale);
    let expected =
        u64::try_from(accounts::parse_reset_epoch("2026-10-16T12:00:00Z").unwrap()).unwrap();
    assert_eq!(rows[0].updated_at, Some(expected));
    // 빈 문자열 error는 없음으로 둔다. 최상위 시각이 없어도 행은 읽힌다.
    let rows =
        parse_orca(r#"{"accounts":[{"label":"B","error":"","five_hour":{"utilization":1}}]}"#);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].error.is_none());
    assert!(rows[0].detail.is_none());
    assert!(rows[0].updated_at.is_none());
    // 깨진 캐시는 빈 목록(기존 동작 유지).
    assert!(parse_orca("broken {").is_empty());
}

/// OWE-703 1단계: 상태 꼬리 분류(모름/stale/로그인 필요/오류/정상) + 갱신 시각.
#[test]
fn account_statuses_classify_unknown_stale_login_error_ok() {
    use krill_proto::usage::{AccountStatus, ProviderKind};

    let home = dir("status-home");
    let state = dir("status-state");

    // 호스트: 신선한 캐시 → 정상 + 갱신 시각 있음.
    let host = home.join(".claude");
    fs::create_dir_all(&host).unwrap();
    fs::write(
        host.join(CLAUDE_CACHE),
        r#"{"five_hour":{"utilization":20}}"#,
    )
    .unwrap();

    // Orca 캐시: stale 행, 로그인 필요 행, 오류 행, 정상 행.
    fs::write(
        host.join(ORCA_CACHE),
        r#"{"updated_at":"2026-10-16T12:00:00Z","accounts":[
        {"id":"u-stale","label":"Stale","stale":true,"five_hour":{"utilization":10}},
        {"id":"u-login","label":"Login","error":"재로그인","detail":"token 400","five_hour":{"utilization":0}},
        {"id":"u-error","label":"Broken","error":"server 500","five_hour":{"utilization":0}},
        {"id":"u-fresh","label":"FreshRemote","five_hour":{"utilization":5}}
    ]}"#,
    )
    .unwrap();

    // 등록 계정: 파일 없음 → 모름.
    let fresh = home.join("claude-fresh");
    accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Fresh",
        None,
        &fresh.display().to_string(),
    )
    .unwrap();

    let snap = accounts::refresh_accounts(&home, Some(&state));
    let status_of = |id: &str| {
        snap.statuses
            .iter()
            .find(|s| s.account_id == id)
            .map(|s| s.status)
    };
    assert_eq!(status_of("claude-host"), Some(AccountStatus::Ok));
    // Orca 감지 계정은 캐시 행 uuid 기반 안정 id다.
    assert_eq!(status_of("claude-orca-u-stale"), Some(AccountStatus::Stale));
    assert_eq!(
        status_of("claude-orca-u-login"),
        Some(AccountStatus::NeedsLogin)
    );
    assert_eq!(status_of("claude-orca-u-error"), Some(AccountStatus::Error));
    assert_eq!(status_of("claude-orca-u-fresh"), Some(AccountStatus::Ok));
    assert_eq!(status_of("claude-fresh"), Some(AccountStatus::Unknown));
    // 갱신 시각: 파일·캐시 근거가 있으면 있다.
    assert!(
        snap.statuses
            .iter()
            .find(|s| s.account_id == "claude-host")
            .unwrap()
            .updated_at
            .is_some()
    );
    assert!(
        snap.statuses
            .iter()
            .find(|s| s.account_id == "claude-orca-u-stale")
            .unwrap()
            .updated_at
            .is_some()
    );
    assert!(
        snap.statuses
            .iter()
            .find(|s| s.account_id == "claude-fresh")
            .unwrap()
            .updated_at
            .is_none()
    );
    // 로그인 필요 행은 안내 원문을 유지한다.
    assert_eq!(
        snap.statuses
            .iter()
            .find(|s| s.account_id == "claude-orca-u-login")
            .unwrap()
            .detail
            .as_deref(),
        Some("token 400")
    );

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

/// OWE-703 2단계 지시: 한도 기록 계정은 상태 꼬리에 `RateLimited`로 표시되고,
/// 수동 전환이나 clear 시 해제된다(자동 전환은 일어나지 않는다).
#[test]
fn rate_limited_status_is_recorded_and_cleared_on_manual_switch() {
    use krill_proto::usage::{AccountStatus, ProviderKind};

    let home = dir("ratelim-home");
    let state = dir("ratelim-state");
    for (label, five) in [("RateLim", 10), ("RateBak", 15)] {
        let profile = home.join(format!("claude-{label}"));
        accounts::add_account(
            &home,
            Some(&state),
            ProviderKind::Claude,
            label,
            None,
            &profile.display().to_string(),
        )
        .unwrap();
        fs::write(
            profile.join(CLAUDE_CACHE),
            format!(r#"{{"five_hour":{{"utilization":{five}}}}}"#),
        )
        .unwrap();
    }
    let host = home.join(".claude");
    fs::create_dir_all(&host).unwrap();
    fs::write(
        host.join(CLAUDE_CACHE),
        r#"{"five_hour":{"utilization":40}}"#,
    )
    .unwrap();

    accounts::switch_account(&home, Some(&state), ProviderKind::Claude, "claude-ratelim").unwrap();
    accounts::record_rate_limited(ProviderKind::Claude, "claude-ratelim");
    let snap = accounts::refresh_accounts(&home, Some(&state));
    assert!(snap.switched.is_empty());
    assert!(
        snap.accounts
            .iter()
            .any(|a| a.id == "claude-ratelim" && a.active)
    );
    let status_of = |id: &str| {
        snap.statuses
            .iter()
            .find(|s| s.account_id == id)
            .map(|s| s.status)
    };
    assert_eq!(
        status_of("claude-ratelim"),
        Some(AccountStatus::RateLimited)
    );
    assert!(
        snap.statuses
            .iter()
            .find(|s| s.account_id == "claude-ratelim")
            .unwrap()
            .updated_at
            .is_some()
    );
    assert_eq!(status_of("claude-ratebak"), Some(AccountStatus::Ok));

    // 사용자가 직접 선택하면 이전 한도 기록이 지워진다.
    let snap =
        accounts::switch_account(&home, Some(&state), ProviderKind::Claude, "claude-ratelim")
            .unwrap();
    assert!(snap.switched.is_empty(), "{snap:?}");
    assert_ne!(
        snap.statuses
            .iter()
            .find(|s| s.account_id == "claude-ratelim")
            .map(|s| s.status),
        Some(AccountStatus::RateLimited)
    );

    assert!(!accounts::clear_rate_limited(
        ProviderKind::Claude,
        "claude-ratelim"
    ));
    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

#[test]
fn orca_detected_accounts_have_stable_uuid_ids_and_never_launch() {
    use krill_proto::usage::ProviderKind;
    let home = dir("owe696-home");
    let state = dir("owe696-state");
    let host = home.join(".claude");
    fs::create_dir_all(&host).unwrap();
    fs::write(
        host.join(CLAUDE_CACHE),
        r#"{"five_hour":{"utilization":95,"resets_at":"2099-01-01T00:00:00Z"}}"#,
    )
    .unwrap();
    let cache = |rows: &[(&str, &str)]| {
        let body: Vec<String> = rows
            .iter()
            .map(|(id, label)| {
                format!(r#"{{"id":"{id}","label":"{label}","five_hour":{{"utilization":5}}}}"#)
            })
            .collect();
        fs::write(
            host.join(ORCA_CACHE),
            format!(r#"{{"accounts":[{}]}}"#, body.join(",")),
        )
        .unwrap();
    };
    let a = ("96f8c826-8b89-4470-a327-f4918efb8177", "a@example.com");
    let b = ("432d5922-d39e-4df0-9255-6de3443b3df0", "b@example.com");
    cache(&[a, b]);
    let mut file = accounts::AccountsFile::default();
    // 구버전처럼 Orca 계정이 활성으로 저장돼 있어도 실행 env는 주입하지 않는다.
    let b_id = format!("claude-orca-{}", b.0);
    file.active.insert("claude".into(), b_id.clone());
    accounts::save_file(&home, Some(&state), &file).unwrap();

    let snap = accounts::refresh_accounts(&home, Some(&state));
    // 호스트 95%·Orca 5%여도 폴더 없는 Orca 계정으로는 자동전환하지 않는다.
    assert!(snap.switched.is_empty(), "{:?}", snap.switched);
    let orca: Vec<_> = snap
        .accounts
        .iter()
        .filter(|x| x.id.starts_with("claude-orca-"))
        .collect();
    assert_eq!(orca.len(), 2);
    assert!(orca.iter().all(|x| x.profile_dir.is_empty()));
    assert!(snap.accounts.iter().any(|x| x.id == b_id && x.active));
    assert_eq!(
        accounts::active_profile_env(&home, Some(&state), "claude"),
        None,
        "orca account must not launch with a made-up profile dir"
    );
    let env = fs::read_to_string(state.join("active-accounts.env")).unwrap();
    assert!(
        env.contains(&format!("'{}'", host.display())),
        "env file must match the real launch dir (host): {env:?}"
    );

    // 순번이 바뀌어도 같은 uuid → 같은 id, 활성 계정 유지.
    cache(&[b, a]);
    let snap = accounts::refresh_accounts(&home, Some(&state));
    let active = snap
        .accounts
        .iter()
        .find(|x| x.provider == ProviderKind::Claude && x.active)
        .unwrap();
    assert_eq!(active.id, b_id);
    assert_eq!(active.label, b.1);

    // 수동 전환·제거도 거부한다(데몬이 관리하지 않는 계정).
    let a_id = format!("claude-orca-{}", a.0);
    accounts::switch_account(&home, Some(&state), ProviderKind::Claude, &a_id)
        .expect_err("orca-managed account cannot be activated");
    accounts::remove_account(&home, Some(&state), ProviderKind::Claude, &a_id)
        .expect_err("orca-managed account is not in accounts.toml");

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

/// OWE-896: Orca 감지 행(`claude-orca-<uuid>`)은 등록 계정 프로필 경로 속 uuid와
/// 라벨·이메일 형식 무관하게 병합된다(중복 표시 제거). uuid가 없는 레거시 행은
/// `"email (조직)"` 라벨과 이메일 기준으로 병합된다.
#[test]
fn orca_detected_row_merges_with_imported_profile_by_uuid_and_email() {
    use krill_proto::usage::ProviderKind;
    let home = dir("owe896-home");
    let state = dir("owe896-state");
    let host = home.join(".claude");
    fs::create_dir_all(&host).unwrap();

    // uuid 행: 라벨이 표시명이라 기존 매칭(라벨·이메일 일치)으로는 병합 불가.
    let uuid_a = "96f8c826-8b89-4470-a327-f4918efb8177";
    let profile_a = state
        .join("orca")
        .join("claude-profiles")
        .join(uuid_a)
        .join("home");
    fs::create_dir_all(&profile_a).unwrap();
    fs::write(
        profile_a.join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"a@example.com","organizationUuid":"org-1","organizationName":"Org A"}}"#,
    )
    .unwrap();
    // 레거시 행: uuid 없이 이메일 라벨. 이메일 없는 등록 항목과 병합되어야 한다.
    let profile_b = state.join("custom-b");
    fs::create_dir_all(&profile_b).unwrap();
    fs::write(
        host.join(ORCA_CACHE),
        format!(
            r#"{{"accounts":[{{"id":"{uuid_a}","label":"표시명","five_hour":{{"utilization":5}}}},{{"id":"","label":"b@example.com","five_hour":{{"utilization":7}}}}]}}"#,
        ),
    )
    .unwrap();

    accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "a@example.com (Org A)",
        Some("a@example.com".to_owned()),
        &profile_a.display().to_string(),
    )
    .unwrap();
    accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "b@example.com (Org B)",
        None,
        &profile_b.display().to_string(),
    )
    .unwrap();

    let snap = accounts::refresh_accounts(&home, Some(&state));
    // 감지 행은 등록 계정에 흡수되어 `claude-orca-*` id가 남지 않는다.
    assert!(
        !snap
            .accounts
            .iter()
            .any(|a| a.id.starts_with("claude-orca-")),
        "{:?}",
        snap.accounts.iter().map(|a| &a.id).collect::<Vec<_>>()
    );
    let merged: Vec<_> = snap
        .accounts
        .iter()
        .filter(|a| a.provider == ProviderKind::Claude && !a.host_default)
        .collect();
    assert_eq!(merged.len(), 2);
    assert!(merged.iter().all(|a| !a.profile_dir.is_empty()));
    // 행의 사용량이 등록 계정으로 이어진다.
    let usage_of = |dir: &str| {
        merged
            .iter()
            .find(|a| a.profile_dir == dir)
            .and_then(|a| a.usage.five_hour.as_ref().map(|w| w.percent))
    };
    assert_eq!(usage_of(&profile_a.display().to_string()), Some(5));
    assert_eq!(usage_of(&profile_b.display().to_string()), Some(7));

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

/// OWE-696: 등록 계정이라도 프로필 폴더가 사라지면 자동전환 후보·실행 env에서 빠지고,
/// `active-accounts.env`는 실행과 같은 호스트 경로를 적는다.
#[test]
fn registered_account_with_missing_dir_is_not_a_candidate() {
    use krill_proto::usage::ProviderKind;
    let home = dir("owe696-missing-home");
    let state = dir("owe696-missing-state");
    let host = home.join(".claude");
    fs::create_dir_all(&host).unwrap();
    fs::write(
        host.join(CLAUDE_CACHE),
        r#"{"five_hour":{"utilization":95,"resets_at":"2099-01-01T00:00:00Z"}}"#,
    )
    .unwrap();
    let gone = home.join("gone");
    accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Gone",
        None,
        &gone.display().to_string(),
    )
    .unwrap();
    // 폴더가 없어도 Orca 캐시(라벨 일치)에서 여유 사용량을 받는 상황.
    fs::write(
        host.join(ORCA_CACHE),
        r#"{"accounts":[{"id":"u-1","label":"Gone","five_hour":{"utilization":5}}]}"#,
    )
    .unwrap();
    accounts::switch_account(&home, Some(&state), ProviderKind::Claude, "claude-gone").unwrap();
    assert_eq!(
        accounts::active_profile_env(&home, Some(&state), "claude").map(|e| e.1),
        Some(gone.display().to_string())
    );
    fs::remove_dir_all(&gone).unwrap();
    assert_eq!(
        accounts::active_profile_env(&home, Some(&state), "claude"),
        None
    );
    // 다음 저장 때 env 파일도 실행과 같은 호스트 경로로 맞춰진다.
    let mut file = accounts::load_file(&home, Some(&state));
    accounts::save_file(&home, Some(&state), &file).unwrap();
    let env = fs::read_to_string(state.join("active-accounts.env")).unwrap();
    assert!(env.contains(&format!("'{}'", host.display())), "{env:?}");
    file.active.insert("claude".into(), "claude-host".into());
    accounts::save_file(&home, Some(&state), &file).unwrap();
    let snap = accounts::refresh_accounts(&home, Some(&state));
    assert!(snap.switched.is_empty(), "{:?}", snap.switched);
    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

/// OWE-705: 라벨을 바꿔도 id·프로필 폴더·활성 계정이 그대로다. 이메일은 `None`이면 유지된다.
#[test]
fn rename_account_keeps_id_dir_and_active() {
    use accounts::{add_account, rename_account, switch_account};
    use krill_proto::usage::ProviderKind;
    let home = dir("owe705-rename");
    let state = home.join("state");
    let snap = add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Claude Profile #1",
        None,
        "",
    )
    .unwrap();
    let before = snap
        .accounts
        .iter()
        .find(|a| !a.host_default)
        .unwrap()
        .clone();
    switch_account(&home, Some(&state), ProviderKind::Claude, &before.id).unwrap();
    let snap = rename_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        &before.id,
        " Work ",
        Some("w@example.com".into()),
    )
    .unwrap();
    let after = snap.accounts.iter().find(|a| a.id == before.id).unwrap();
    assert_eq!(after.label, "Work");
    assert_eq!(after.email.as_deref(), Some("w@example.com"));
    assert_eq!(after.profile_dir, before.profile_dir);
    assert!(after.active);
    let snap = rename_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        &before.id,
        "Work 2",
        None,
    )
    .unwrap();
    let after = snap.accounts.iter().find(|a| a.id == before.id).unwrap();
    assert_eq!(after.email.as_deref(), Some("w@example.com"));
    assert!(
        rename_account(
            &home,
            Some(&state),
            ProviderKind::Claude,
            &before.id,
            " ",
            None
        )
        .is_err()
    );
    assert_eq!(
        rename_account(
            &home,
            Some(&state),
            ProviderKind::Codex,
            &before.id,
            "x",
            None
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::NotFound
    );
    fs::remove_dir_all(home).ok();
}

/// Orca 감지 계정 삭제는 Krill 목록에서만 숨긴다. Orca 캐시 파일은 그대로 두고 새로고침해도 다시 보이지 않는다.
#[test]
fn delete_orca_detected_account_hides_it_without_touching_orca_cache() {
    use accounts::{delete_account, refresh_accounts};
    use krill_proto::usage::ProviderKind;
    let home = dir("orca-hide");
    let state = home.join("state");
    fs::create_dir_all(home.join(".claude")).unwrap();
    let cache = home.join(".claude/orca-usage-cache.json");
    let body = r#"{"accounts":[
        {"id":"aaaa-1111","label":"keep@example.com"},
        {"id":"bbbb-2222","label":"gone@example.com"}
    ]}"#;
    fs::write(&cache, body).unwrap();
    let ids = |snap: &krill_proto::usage::AccountsSnapshot| -> Vec<String> {
        snap.accounts
            .iter()
            .filter(|a| a.id.starts_with("claude-orca-"))
            .map(|a| a.id.clone())
            .collect()
    };
    let before = refresh_accounts(&home, Some(&state));
    let gone = ids(&before)
        .into_iter()
        .find(|id| id.contains("bbbb"))
        .unwrap();
    let snap = delete_account(&home, Some(&state), ProviderKind::Claude, &gone, &[]).unwrap();
    assert!(!ids(&snap).contains(&gone), "{:?}", ids(&snap));
    assert_eq!(ids(&snap).len(), 1);
    assert_eq!(fs::read_to_string(&cache).unwrap(), body);
    let again = refresh_accounts(&home, Some(&state));
    assert!(!ids(&again).contains(&gone));
    fs::remove_dir_all(home).ok();
}

/// OWE-704: 세션이 쓰는 프로필이면 EBUSY로 거부(등록·폴더 그대로), 아니면 등록을 지우고 관리 폴더는
/// 휴지통으로 옮긴다. 사용자가 지정한 관리 루트 밖 폴더는 등록만 지운다. 고아 정리는 세션 폴더를 건너뛴다.
#[test]
fn delete_account_refuses_in_use_and_trashes_managed_profile() {
    use accounts::{add_account, delete_account, orphan_profiles, purge_orphan_profiles};
    use krill_proto::usage::ProviderKind;
    use std::path::PathBuf;
    let home = dir("owe704-delete");
    let state = home.join("state");
    let custom = home.join("custom-claude");
    add_account(&home, Some(&state), ProviderKind::Claude, "Work", None, "").unwrap();
    let snap = add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Mine",
        None,
        &custom.display().to_string(),
    )
    .unwrap();
    let work = snap
        .accounts
        .iter()
        .find(|a| a.label == "Work")
        .unwrap()
        .clone();
    let work_dir = PathBuf::from(&work.profile_dir);
    assert!(krill_proto::usage::profiles::is_managed(&home, &work_dir));

    let busy = delete_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        &work.id,
        std::slice::from_ref(&work_dir),
    )
    .unwrap_err();
    assert_eq!(busy.raw_os_error(), Some(libc::EBUSY));
    assert!(work_dir.is_dir());
    assert!(
        accounts::refresh_accounts(&home, Some(&state))
            .accounts
            .iter()
            .any(|a| a.id == work.id)
    );

    let snap = delete_account(&home, Some(&state), ProviderKind::Claude, &work.id, &[]).unwrap();
    assert!(snap.accounts.iter().all(|a| a.id != work.id));
    assert!(!work_dir.exists());
    assert!(home.join(".Trash").join("krill-claude-work").is_dir());

    let mine = snap
        .accounts
        .iter()
        .find(|a| a.label == "Mine")
        .unwrap()
        .id
        .clone();
    delete_account(&home, Some(&state), ProviderKind::Claude, &mine, &[]).unwrap();
    assert!(
        custom.is_dir(),
        "folders outside the managed root are never moved"
    );

    // 고아 폴더(이전 "제거"가 남긴 것) 정리: 세션이 쓰는 폴더는 남는다.
    let root = krill_proto::usage::profiles::managed_root(&home);
    let agy = root.join("agy").join("antigravity-profile-1");
    let oc = root.join("opencode").join("opencode-profile-2");
    fs::create_dir_all(&agy).unwrap();
    fs::create_dir_all(&oc).unwrap();
    assert_eq!(orphan_profiles(&home, Some(&state)).len(), 2);
    let left = purge_orphan_profiles(&home, Some(&state), std::slice::from_ref(&oc)).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].dir, oc.display().to_string());
    assert!(!agy.exists());
    assert!(oc.is_dir());
    fs::remove_dir_all(home).ok();
}

/// OWE-701: `krill statusline`이 쓰는 모양(`rate_limits` 포장 + `cached_at`)의
/// `usage_cache.json`이 등록 계정의 `ListAccounts`(`refresh_accounts`) 사용량에 반영된다.
#[test]
fn statusline_cache_file_flows_into_list_accounts() {
    use krill_proto::usage::ProviderKind;

    let home = dir("owe701-statusline-home");
    let state = dir("owe701-statusline-state");
    let profile = home.join("profiles").join("claude-work");
    accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Work",
        None,
        &profile.display().to_string(),
    )
    .unwrap();
    fs::write(
        profile.join("usage_cache.json"),
        r#"{"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":"2099-01-01T00:00:00Z"},"seven_day":{"used_percentage":67,"resets_at":"2099-01-08T00:00:00Z"}},"cached_at":"1792123200"}"#,
    )
    .unwrap();

    let snap = accounts::refresh_accounts(&home, Some(&state));
    let work = snap
        .accounts
        .iter()
        .find(|a| a.id == "claude-work")
        .unwrap();
    assert_eq!(work.usage.five_hour.as_ref().map(|w| w.percent), Some(42));
    assert_eq!(work.usage.seven_day.as_ref().map(|w| w.percent), Some(67));
    assert!(!work.stale);
    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

/// OWE-726: `krill import orca`가 `UsageRequest::AddAccount` / `SwitchAccount`로 등록한
/// Claude·Codex 다계정은 `orca-usage-cache.json` 행과 병합되어 프로필 경로가 연결되고
/// 즉시 실행 가능(`active_profile_env`)하며 `switch_account`로 원클릭 전환할 수 있다.
#[test]
fn orca_imported_accounts_are_immediately_runnable_and_switchable() {
    use krill_proto::usage::ProviderKind;

    let home = dir("owe726-usage-home");
    let state = dir("owe726-usage-state");
    let orca = home.join("Library/Application Support/orca");

    let claude_uuid_a = "11111111-aaaa-bbbb-cccc-222222222222";
    let claude_uuid_b = "33333333-dddd-eeee-ffff-444444444444";
    let codex_uuid = "55555555-6666-7777-8888-999999999999";

    let claude_dir_a = orca
        .join("claude-profiles")
        .join(claude_uuid_a)
        .join("home");
    let claude_dir_b = orca
        .join("claude-profiles")
        .join(claude_uuid_b)
        .join("home");
    let codex_dir = orca.join("codex-accounts").join(codex_uuid).join("home");
    fs::create_dir_all(&claude_dir_a).unwrap();
    fs::create_dir_all(&claude_dir_b).unwrap();
    fs::create_dir_all(&codex_dir).unwrap();

    // `~/.claude/orca-usage-cache.json` also exists with usage for both accounts
    let host_claude = home.join(".claude");
    fs::create_dir_all(&host_claude).unwrap();
    fs::write(
        host_claude.join(ORCA_CACHE),
        format!(
            r#"{{"accounts":[{{"id":"{claude_uuid_a}","label":"a@example.com","five_hour":{{"utilization":25}}}},{{"id":"{claude_uuid_b}","label":"b@example.com","five_hour":{{"utilization":10}}}}]}}"#
        ),
    )
    .unwrap();

    // Register via `add_account` and `switch_account` (same path as `UsageRequest::AddAccount` / `SwitchAccount`)
    let snap = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "a@example.com (Org A)",
        Some("a@example.com".to_owned()),
        &claude_dir_a.display().to_string(),
    )
    .unwrap();
    let a_id = snap
        .accounts
        .iter()
        .find(|a| a.profile_dir == claude_dir_a.display().to_string())
        .unwrap()
        .id
        .clone();

    let snap = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "b@example.com (Org B)",
        Some("b@example.com".to_owned()),
        &claude_dir_b.display().to_string(),
    )
    .unwrap();
    let b_id = snap
        .accounts
        .iter()
        .find(|a| a.profile_dir == claude_dir_b.display().to_string())
        .unwrap()
        .id
        .clone();

    let snap = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Codex,
        "codex@example.com (Codex WS)",
        Some("codex@example.com".to_owned()),
        &codex_dir.display().to_string(),
    )
    .unwrap();
    let codex_id = snap
        .accounts
        .iter()
        .find(|a| a.profile_dir == codex_dir.display().to_string())
        .unwrap()
        .id
        .clone();

    accounts::switch_account(&home, Some(&state), ProviderKind::Claude, &a_id).unwrap();
    accounts::switch_account(&home, Some(&state), ProviderKind::Codex, &codex_id).unwrap();

    // `active_profile_env` immediately resolves to the active Orca account's profile directory
    assert_eq!(
        accounts::active_profile_env(&home, Some(&state), "claude"),
        Some(("CLAUDE_CONFIG_DIR", claude_dir_a.display().to_string()))
    );
    assert_eq!(
        accounts::active_profile_env(&home, Some(&state), "codex"),
        Some(("CODEX_HOME", codex_dir.display().to_string()))
    );

    // Manual switch to the second imported Claude account succeeds because its profile_dir is runnable
    let switched_snap =
        accounts::switch_account(&home, Some(&state), ProviderKind::Claude, &b_id).unwrap();
    let active_claude = switched_snap
        .accounts
        .iter()
        .find(|a| a.provider == ProviderKind::Claude && a.active)
        .unwrap();
    assert_eq!(active_claude.id, b_id);
    assert_eq!(active_claude.label, "b@example.com (Org B)");
    assert_eq!(
        accounts::active_profile_env(&home, Some(&state), "claude"),
        Some(("CLAUDE_CONFIG_DIR", claude_dir_b.display().to_string()))
    );

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

#[test]
fn delete_registered_orca_account_hides_orca_cache_ghost_and_readd_unhides_it() {
    use krill_proto::usage::ProviderKind;

    let home = dir("orca-registered-delete-home");
    let state = dir("orca-registered-delete-state");
    let orca = home.join("Library/Application Support/orca");
    let uuid = "26015f11-6f96-4bc5-835d-ab10baa7fa74";
    let profile_dir = orca.join("claude-profiles").join(uuid).join("home");
    fs::create_dir_all(&profile_dir).unwrap();

    let host_claude = home.join(".claude");
    fs::create_dir_all(&host_claude).unwrap();
    fs::write(
        host_claude.join(ORCA_CACHE),
        format!(
            r#"{{"accounts":[{{"id":"{uuid}","name":"My Org","email":"fixture-1@example.com","five_hour":{{"utilization":20}}}}]}}"#
        ),
    )
    .unwrap();

    let snap = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "fixture-1@example.com (My Org)",
        Some("fixture-1@example.com".to_owned()),
        &profile_dir.display().to_string(),
    )
    .unwrap();
    let registered_id = snap
        .accounts
        .iter()
        .find(|a| !a.host_default && a.provider == ProviderKind::Claude)
        .unwrap()
        .id
        .clone();
    assert!(
        !snap
            .accounts
            .iter()
            .any(|a| a.id.starts_with("claude-orca-")),
        "registered Orca account should merge with cache row rather than duplicating it"
    );

    let after_delete = accounts::delete_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        &registered_id,
        &[],
    )
    .unwrap();
    assert!(
        !after_delete
            .accounts
            .iter()
            .any(|a| a.id == registered_id || a.id.starts_with("claude-orca-")),
        "deleted registered account must not reappear as an Orca-only ghost: {:?}",
        after_delete.accounts
    );
    let refreshed = accounts::refresh_accounts(&home, Some(&state));
    assert!(
        !refreshed
            .accounts
            .iter()
            .any(|a| a.id == registered_id || a.id.starts_with("claude-orca-"))
    );

    let readded = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "fixture-1@example.com (My Org)",
        Some("fixture-1@example.com".to_owned()),
        &profile_dir.display().to_string(),
    )
    .unwrap();
    assert!(
        readded
            .accounts
            .iter()
            .any(|a| a.profile_dir == profile_dir.display().to_string())
    );

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}

/// 공유 세션 폴더(symlink)의 rollout은 다른 계정의 사용량으로 귀속하지 않는다.
#[cfg(unix)]
#[test]
fn shared_rollout_is_not_attributed_to_other_codex_profile() {
    let root = dir("codex-shared-rollout");
    let (home, state) = (root.join("home"), root.join("state"));
    let tnlvof = root.join("tnlvof");
    fs::create_dir_all(&tnlvof).unwrap();
    let shared = root.join("shared");
    fs::create_dir_all(shared.join("2026/10/28")).unwrap();
    fs::write(
        shared.join("2026/10/28/rollout-2026-10-28T15-35-18-x.jsonl"),
        "{\"rate_limits\":{\"primary\":{\"used_percent\":10.0,\"window_minutes\":10080},\"secondary\":null}}\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&shared, tnlvof.join("sessions")).unwrap();

    let file = accounts::AccountsFile {
        accounts: vec![accounts::StoredAccount {
            id: "codex-tnlvof".into(),
            provider: "codex".into(),
            label: "codex-tnlvof".into(),
            email: None,
            profile_dir: tnlvof.display().to_string(),
        }],
        ..Default::default()
    };
    accounts::save_file(&home, Some(&state), &file).unwrap();
    let snap = accounts::refresh_accounts(&home, Some(&state));
    let dead = snap
        .accounts
        .iter()
        .find(|a| a.id == "codex-tnlvof")
        .unwrap();
    assert!(dead.usage.is_empty(), "{dead:?}");

    fs::remove_dir_all(&root).ok();
}

/// 멈춘 Orca의 오래된 캐시 행이 호스트 계정의 현재 로그인 이름이 되지 않는다.
#[test]
fn stale_orca_row_does_not_name_host_account() {
    use krill_proto::usage::ProviderKind;

    let home = dir("orca-host-email-home");
    let state = dir("orca-host-email-state");
    fs::create_dir_all(home.join(".claude")).unwrap();
    let cache = home.join(".claude").join(ORCA_CACHE);
    fs::write(
        &cache,
        r#"{"accounts":[{"host":true,"label":"old@example.dev"}]}"#,
    )
    .unwrap();
    let host_email = || {
        accounts::refresh_accounts(&home, Some(&state))
            .accounts
            .into_iter()
            .find(|a| a.provider == ProviderKind::Claude && a.host_default)
            .unwrap()
            .email
    };
    assert_eq!(host_email().as_deref(), Some("old@example.dev"));
    fs::File::options()
        .write(true)
        .open(&cache)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_hours(2))
        .unwrap();
    assert_eq!(host_email(), None);
    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&state).ok();
}
