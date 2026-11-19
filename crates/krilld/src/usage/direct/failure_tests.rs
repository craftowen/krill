use super::*;
use std::sync::Arc;
use tracing::{Event, Metadata, Subscriber, field::Visit, span};

/// Captures events as `(level, " field=value" ...)` for log-volume assertions.
#[derive(Clone, Default)]
pub(crate) struct Events(pub(crate) Arc<Mutex<Vec<(tracing::Level, String)>>>);

impl Visit for Events {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if let Some((_, text)) = lock(&self.0).last_mut() {
            use std::fmt::Write as _;
            write!(text, " {}={value:?}", field.name()).unwrap();
        }
    }
}

impl Subscriber for Events {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
        lock(&self.0).push((*event.metadata().level(), String::new()));
        event.record(&mut self.clone());
    }
}

#[test]
fn reports_each_profile_and_reason_streak_once_and_recovers_once() {
    let home = PathBuf::from("/fixture-usage-failure-streak");
    let first = (
        home.clone(),
        ProviderKind::Claude,
        home.join("private-email-profile"),
    );
    let second = (
        home.clone(),
        ProviderKind::Claude,
        home.join("another-profile"),
    );
    let events = Events::default();
    tracing::subscriber::with_default(events.clone(), || {
        for count in 1..=3 {
            assert!(apply_result(&first, Fetched::Failed(Failure::Http(401)), Some(42)).is_none());
            let usage = lock(&CACHE)[&first].1.clone();
            assert_eq!(usage.error.as_deref(), Some("Refresh failed"));
            assert_eq!(usage.detail, Some(Failure::Http(401).detail()));
            assert_eq!(lock(&RETRY)[&first].failures, count);
        }
        apply_result(
            &first,
            Fetched::RateLimited(Some(Duration::from_secs(123))),
            Some(42),
        );
        apply_result(&first, Fetched::Failed(Failure::Http(401)), Some(42));
        assert_eq!(
            lock(&RETRY)[&first].failures,
            4,
            "429 must not reset the streak"
        );
        apply_result(&second, Fetched::Failed(Failure::Http(401)), None);
        apply_result(&first, Fetched::Failed(Failure::Http(403)), None);
        for _ in 0..2 {
            let usage = apply_result(
                &first,
                Fetched::Usage(
                    Usage::default(),
                    krill_proto::usage::AccountMeters::default(),
                ),
                Some(43),
            )
            .unwrap();
            assert!(usage.error.is_none());
            assert!(!lock(&RETRY).contains_key(&first));
        }
        apply_result(&first, Fetched::Failed(Failure::Expired), None);
    });
    let captured = lock(&events.0);
    assert_eq!(
        captured
            .iter()
            .filter(|(level, _)| *level == tracing::Level::WARN)
            .count(),
        4
    );
    assert_eq!(
        captured
            .iter()
            .filter(|(level, _)| *level == tracing::Level::INFO)
            .count(),
        1
    );
    assert!(
        captured
            .iter()
            .all(|(_, text)| !text.contains("private-email-profile"))
    );
    assert!(
        captured
            .iter()
            .any(|(_, text)| text.contains(&profile_id(&first.2)))
    );
    assert!(
        captured
            .iter()
            .any(|(_, text)| text.contains(&profile_id(&second.2)))
    );
    lock(&RETRY).remove(&first);
    lock(&RETRY).remove(&second);
    lock(&CACHE).remove(&first);
    super::super::meters::evict_profile(&home, first.1, &first.2);
    super::super::meters::evict_profile(&home, second.1, &second.2);
}

#[test]
fn retry_deadline_doubles_and_caps_until_reason_changes() {
    let now = Instant::now();
    let mut retry = Retry::new();
    for (count, secs) in [30, 60, 120, 240, 480, 900, 900].into_iter().enumerate() {
        assert_eq!(retry.fail(Failure::Expired, now), count == 0);
        assert_eq!(retry.until.duration_since(now), Duration::from_secs(secs));
    }
    assert!(retry.fail(Failure::Http(401), now));
    assert_eq!(retry.until.duration_since(now), FAILURE_RETRY);
}

#[test]
fn expired_selected_keychain_is_not_replaced_by_another_account_and_is_reread() {
    let expired = br#"{"claudeAiOauth":{"accessToken":"expired-fixture","refreshToken":"fixture-only","expiresAt":1000}}"#;
    let renewed = br#"{"claudeAiOauth":{"accessToken":"renewed-fixture","expiresAt":2000}}"#;
    let dir = Path::new("/fixture-keychain-profile");
    let expected_service = claude_service(Some(dir)).unwrap();
    let reads = std::cell::Cell::new(0);
    let current = std::cell::Cell::new(expired.as_slice());
    let read = |service: &str, account: &str| {
        assert_eq!(service, expected_service);
        assert_eq!(
            account,
            claude_keychain_user(),
            "must not fall back from an expired selected account"
        );
        reads.set(reads.get() + 1);
        Some(Zeroizing::new(current.get().to_vec()))
    };
    assert!(matches!(
        claude_credentials_at(dir, false, read, 1000),
        Err(Failure::Expired)
    ));
    current.set(renewed);
    let (token, _) = claude_credentials_at(dir, false, read, 1000).unwrap();
    assert_eq!(&*token, "renewed-fixture");
    assert_eq!(reads.get(), 2);
}

#[test]
fn credential_expiry_handles_files_bom_and_missing_expiry_without_writing() {
    let root = std::env::temp_dir().join(format!("krill-usage-expiry-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(".credentials.json");
    let expired = br#"{"claudeAiOauth":{"accessToken":"old-fixture","expiresAt":999}}"#;
    std::fs::write(&path, expired).unwrap();
    assert!(matches!(
        claude_credentials_at(&root, true, |_, _| None, 1000),
        Err(Failure::Expired)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), expired);
    let valid = br#"{"claudeAiOauth":{"accessToken":"new-fixture","expiresAt":1001}}"#;
    std::fs::write(&path, valid).unwrap();
    let (token, _) =
        claude_credentials_at(&root, true, |_, _| Some(b"malformed".to_vec().into()), 1000)
            .unwrap();
    assert_eq!(&*token, "new-fixture");
    let mut utf16 = vec![0xff, 0xfe];
    for unit in std::str::from_utf8(expired).unwrap().encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    assert!(matches!(
        claude_credential(&utf16, 1000),
        Some(Err(Failure::Expired))
    ));
    assert!(matches!(
        claude_credential(br#"{"claudeAiOauth":{"accessToken":"fixture"}}"#, 1000),
        Some(Ok(_))
    ));
    std::fs::remove_dir_all(root).unwrap();
}

/// 외부 스크립트가 다른 계정 값을 쓴 `statusline_cache.json`은 이 프로필의 API 값이나
/// `krill statusline` 기록을 이기지 못한다.
#[test]
fn unattributed_statusline_file_never_beats_profile_usage() {
    let root = std::env::temp_dir().join(format!("krill-usage-owner-{}", std::process::id()));
    let dir = root.join("profile");
    std::fs::create_dir_all(&dir).unwrap();
    let percent = |u: ProfileUsage| u.usage.seven_day.map(|w| w.percent);
    let read = || accounts::read_profile_usage(&root, ProviderKind::Claude, &dir, "A", None, false);
    let write_recorded = |value: u8| {
        std::fs::write(
            dir.join(krill_proto::statusline::CACHE_FILE),
            format!(r#"{{"rate_limits":{{"seven_day":{{"used_percentage":{value}}}}}}}"#),
        )
        .unwrap();
    };
    std::fs::write(
        dir.join("statusline_cache.json"),
        r#"{"seven_day":{"utilization":97}}"#,
    )
    .unwrap();
    assert_eq!(percent(read()), Some(97), "다른 값이 없으면 쓴다");
    assert!(!live_fresh(ProviderKind::Claude, &dir));

    // 더 오래된 프로필 기록이 먼저 만든 파일보다 우선한다.
    write_recorded(66);
    let stale = std::fs::File::options()
        .write(true)
        .open(dir.join(krill_proto::statusline::CACHE_FILE))
        .unwrap();
    stale
        .set_modified(SystemTime::now() - Duration::from_hours(3))
        .unwrap();
    assert_eq!(percent(read()), Some(66));

    // 직접 조회 값은 방금 쓴 외부 파일보다, 더 새 프로필 기록은 직접 조회보다 우선한다.
    let direct_at = now_epoch_u64().unwrap() - 60;
    lock(&CACHE).insert(
        (root.clone(), ProviderKind::Claude, dir.clone()),
        (
            Instant::now(),
            ProfileUsage {
                usage: Usage {
                    seven_day: Some(Window {
                        percent: 100,
                        resets_at: None,
                    }),
                    ..Usage::default()
                },
                updated_at: Some(direct_at),
                ..ProfileUsage::default()
            },
        ),
    );
    assert_eq!(percent(read()), Some(100));
    write_recorded(50);
    assert_eq!(percent(read()), Some(50));

    evict_profile(&root, ProviderKind::Claude, &dir);
    std::fs::remove_dir_all(root).unwrap();
}
