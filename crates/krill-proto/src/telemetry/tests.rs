use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub(super) fn state_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mk-telemetry-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn all_events() -> Vec<Event> {
    vec![
        Event::AppOpened(AppOpened {}),
        Event::DaemonReattached(DaemonReattached {
            recovered: true,
            delay_ms: 320,
        }),
        Event::AgentStatusSource(AgentStatusSource {
            hook: 7,
            process: 2,
            title: 1,
            screen: 0,
        }),
        Event::AgentWaitingMissed(AgentWaitingMissed { corrections: 3 }),
        Event::CrashDetected(CrashDetected {
            process: CrashProcess::Daemon,
        }),
        Event::UpdateResult(UpdateResult { success: true }),
        Event::MemorySample(MemorySample {
            app: RssBucket::Mb50To100,
            daemon: RssBucket::Mb0To50,
        }),
    ]
}

#[test]
fn default_enabled_without_legacy_consent() {
    let dir = state_dir();
    assert_eq!(consent(Some(&dir), &Env::default()), Consent::Enabled);
    assert!(!dir.join(CONSENT_FILE).exists());
    disable(&dir).unwrap();
    assert_eq!(
        consent(Some(&dir), &Env::default()),
        Consent::Disabled(Reason::UserDisabled)
    );
    let mut recorder = Recorder::default();
    assert!(!emit(
        &consent(Some(&dir), &Env::default()),
        all_events().remove(0),
        1,
        &mut recorder
    ));
    assert!(recorder.sent.is_empty());
    fs::remove_dir_all(dir).ok();
}

/// 거부 변수가 동의 파일보다 이긴다. 거짓값은 거부하지 않는다.
#[test]
fn env_veto_wins_over_consent_file() {
    let dir = state_dir();
    enable(&dir).unwrap();
    for var in [
        DO_NOT_TRACK,
        KRILL_DISABLED,
        "CI",
        "GITHUB_ACTIONS",
        "GITLAB_CI",
        "JENKINS_URL",
        "TEAMCITY_VERSION",
        "BUILDKITE",
        "CIRCLECI",
        "TRAVIS",
    ] {
        let env = Env::stub(&[(var, "1")]);
        let consent = consent(Some(&dir), &env);
        assert_eq!(consent, Consent::Disabled(Reason::EnvVeto(var)));
        let mut recorder = Recorder::default();
        assert!(!emit(&consent, all_events().remove(0), 1, &mut recorder));
        assert!(recorder.sent.is_empty());
    }
    for falsy in ["", "0", "false", "False", "no", "off"] {
        let env = Env::stub(&[(DO_NOT_TRACK, falsy), ("CI", falsy)]);
        assert_eq!(consent(Some(&dir), &env), Consent::Enabled);
    }
    fs::remove_dir_all(&dir).ok();
}

/// 켜져 있으면 봉투 하나가 나가고, 키는 고정·값은 허용 목록 안에 있다.
#[test]
fn enabled_emits_strict_envelope() {
    let dir = state_dir();
    enable(&dir).unwrap();
    let consent = consent(Some(&dir), &Env::stub(&[]));
    assert_eq!(consent, Consent::Enabled);
    let mut recorder = Recorder::default();
    for event in all_events() {
        let name = event.name().to_owned();
        assert!(emit(&consent, event, 1_700_000_000, &mut recorder));
        let body = recorder.sent.last().unwrap();
        let value: serde_json::Value = serde_json::from_str(body).unwrap();
        // `preserve_order` 유무에 따라 순서가 바뀌므로 정렬 후 비교한다.
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["app_version", "arch", "event", "os", "ts_unix", "v"]);
        assert_eq!(value["v"], SCHEMA_VERSION);
        assert_eq!(value["ts_unix"], 1_700_000_000);
        assert_eq!(value["event"]["name"], name.as_str());
        assert!(Event::ALL.contains(&name.as_str()));
        // 같은 본문은 다시 읽힌다(왕복).
        let back: Envelope = serde_json::from_str(body).unwrap();
        assert_eq!(back.event.name(), name.as_str());
    }
    assert_eq!(recorder.sent.len(), all_events().len());
    fs::remove_dir_all(&dir).ok();
}

/// 모르는 키가 하나라도 있으면 봉투·페이로드 모두 거부한다(엄격 스키마).
#[test]
fn unknown_fields_are_rejected() {
    let body =
        serde_json::to_string(&envelope(Event::AppOpened(AppOpened {}), 1_700_000_000)).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&body).unwrap();
    value["bogus"] = serde_json::Value::Bool(true);
    assert!(serde_json::from_value::<Envelope>(value).is_err());
    let mut data = serde_json::to_value(Event::DaemonReattached(DaemonReattached {
        recovered: true,
        delay_ms: 1,
    }))
    .unwrap();
    data["data"]["prompt"] = serde_json::Value::from("do not send me");
    assert!(serde_json::from_value::<Event>(data).is_err());
}

/// 이벤트 이름은 10개 이하·중복 없음·변형과 일치.
#[test]
fn event_allowlist_is_capped() {
    assert!(Event::ALL.len() <= MAX_EVENTS);
    let mut names = Event::ALL.to_vec();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), Event::ALL.len());
    let mut covered: Vec<&str> = all_events().iter().map(Event::name).collect();
    covered.sort_unstable();
    assert_eq!(covered, names);
}

/// 자유 텍스트 금지: 직렬화된 문자열 값은 고정 집합(버전·OS·아키·구간·이름) 안에만 있다.
#[test]
fn payloads_carry_no_free_text() {
    let mut allowed = vec![
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        "0-50MB",
        "50-100MB",
        "100-150MB",
        "150-200MB",
        "200MB+",
        "app",
        "daemon",
        "holder",
        "cli",
    ];
    allowed.extend(Event::ALL);
    for event in all_events() {
        let body = serde_json::to_string(&envelope(event, 1)).unwrap();
        assert!(!body.contains('/'), "{body}");
        assert!(!body.contains('\n'), "{body}");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        let mut stack = vec![&value];
        while let Some(next) = stack.pop() {
            match next {
                serde_json::Value::String(s) => assert!(allowed.contains(&s.as_str()), "{s}"),
                serde_json::Value::Array(items) => stack.extend(items),
                serde_json::Value::Object(map) => stack.extend(map.values()),
                _ => {}
            }
        }
    }
}

/// 동의 파일 만들기·지우기 왕복. 파일 권한은 0600.
#[test]
fn enable_disable_roundtrip() {
    let dir = state_dir();
    let target = dir.join("nested");
    enable(&target).unwrap();
    disable(&target).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(target.join(DISABLED_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    enable(&target).unwrap();
    assert_eq!(consent(Some(&target), &Env::stub(&[])), Consent::Enabled);
    disable(&target).unwrap();
    disable(&target).unwrap();
    assert_eq!(
        consent(Some(&target), &Env::stub(&[])),
        Consent::Disabled(Reason::UserDisabled)
    );
    fs::remove_dir_all(&dir).ok();
}

/// 상태 디렉터리를 모르면 꺼짐.
#[test]
fn no_state_dir_is_off() {
    assert_eq!(
        consent(None, &Env::stub(&[])),
        Consent::Disabled(Reason::NoConsentFile)
    );
}
