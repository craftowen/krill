use super::outbox::{Lock, MAX_ENTRY_BYTES, QUEUE_LOCK, flush_with};
use super::*;
use std::fs;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// 테스트마다 따로 쓰는 상태 디렉터리(끝나면 지운다).
struct State(PathBuf);

impl State {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "krill-errrep-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// 동의하고 CI 환경 변수와 무관하게 현재 동의를 돌려준다.
    fn consent(&self) -> Consent {
        enable(&self.0).unwrap();
        consent_with(&self.0, &Env::stub(&[])).unwrap().unwrap()
    }

    fn outbox(&self) -> PathBuf {
        dir(&self.0).join("outbox")
    }

    fn retry_file(&self) -> PathBuf {
        dir(&self.0).join("retry.json")
    }

    /// 대기열에 남은 이벤트 ID(정렬).
    fn entries(&self) -> Vec<String> {
        let Ok(items) = fs::read_dir(self.outbox()) else {
            return Vec::new();
        };
        let mut ids: Vec<String> = items
            .map(|item| item.unwrap().file_name().into_string().unwrap())
            .filter_map(|name| name.strip_suffix(".json").map(str::to_owned))
            .collect();
        ids.sort();
        ids
    }

    fn edit_entry(&self, id: &str, edit: impl FnOnce(&mut serde_json::Value)) {
        let path = self.outbox().join(format!("{id}.json"));
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        edit(&mut value);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    }
}

impl Drop for State {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

/// 종류별 필수 필드를 채운 이벤트.
fn event(code: Code) -> Event {
    let location = format!(
        "crates/krill/src/main.rs:{}",
        NEXT.fetch_add(1, Ordering::Relaxed).saturating_add(1)
    );
    let mut event = Event::new(Process::Cli, code, Some(location)).unwrap();
    match code.kind() {
        Kind::UiHang => event.hang_bucket_ms = Some(1500),
        Kind::NativeCrash => event.native = Some(NativeData::unknown()),
        Kind::OperationError | Kind::Panic | Kind::AbnormalExit => {}
    }
    event
}

/// `n`건을 기록하고 ID를 정렬해 돌려준다.
fn queue(state: &State, consent: &Consent, n: usize) -> Vec<String> {
    let mut ids: Vec<String> = (0..n)
        .map(|_| {
            let event = event(Code::UpdateFailed);
            let id = event.event_id.clone();
            assert!(record_with_consent(state.path(), event, consent).unwrap());
            id
        })
        .collect();
    ids.sort();
    ids
}

fn sent_ids(body: &[u8]) -> Vec<String> {
    serde_json::from_slice::<Request>(body)
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.event_id)
        .collect()
}

fn ack(accepted: &[String], duplicate: &[String]) -> Reply {
    let body = serde_json::to_vec(&Ack {
        schema_version: SCHEMA_VERSION,
        accepted_ids: accepted.to_vec(),
        duplicate_ids: duplicate.to_vec(),
    })
    .unwrap();
    Reply::Http {
        status: 200,
        retry_after_secs: None,
        body,
    }
}

fn http(status: u16) -> Reply {
    Reply::Http {
        status,
        retry_after_secs: None,
        body: Vec::new(),
    }
}

/// 시계를 `now`로 고정하고 지터는 최대값(20%)으로 flush한다.
fn flush_at(
    state: &State,
    consent: &Consent,
    now: u64,
    mut transport: impl FnMut(&[u8]) -> Reply,
) -> FlushReport {
    let flight = UploadLock::try_acquire(state.path()).unwrap().unwrap();
    flush_with(
        state.path(),
        consent,
        &flight,
        &mut transport,
        &mut || now,
        &mut |max| max,
    )
    .unwrap()
}

const WIRE_REQUEST: &str = r#"{"schema_version":1,"events":[{"event_id":"0123456789abcdef0123456789abcdef","process":"krill-app","kind":"operation_error","error_code":"update_failed","release":"0.1.42","build_id":"0.1.42+dev.e7f7f0edcae739d1","os":"macos","arch":"aarch64","source_loc":null,"hang_bucket_ms":null,"native":null}]}"#;
const WIRE_NATIVE: &str = r#"{"exception":"EXC_BAD_ACCESS","signal":"SIGSEGV","frames":[{"image_uuid":"0123456789abcdef0123456789abcdef","offset":1234}]}"#;

#[test]
fn repeated_fingerprint_is_suppressed_after_ack_and_reset_by_new_consent() {
    let state = State::new();
    let consent = state.consent();
    let mut repeated = event(Code::SchedulerFailed);
    assert!(record_with_consent(state.path(), repeated.clone(), &consent).unwrap());
    let report = flush_at(&state, &consent, now_ms(), |body| ack(&sent_ids(body), &[]));
    assert_eq!(report.accepted, 1);
    repeated.event_id = event(Code::SchedulerFailed).event_id;
    for _ in 0..100 {
        assert!(!record_with_consent(state.path(), repeated.clone(), &consent).unwrap());
    }
    assert!(state.entries().is_empty());
    disable(state.path()).unwrap();
    assert!(!dir(state.path()).join(super::rate::FILE).exists());
    let consent = state.consent();
    assert!(record_with_consent(state.path(), repeated, &consent).unwrap());
}

#[test]
fn fingerprint_window_expires_without_requiring_queue_replay() {
    let state = State::new();
    let consent = state.consent();
    let event = event(Code::Panic);
    let now = now_ms();
    let folder = dir(state.path());
    assert!(super::rate::admit(&folder, &consent, &event, now).unwrap());
    assert!(!super::rate::admit(&folder, &consent, &event, now + 3_599_999).unwrap());
    assert!(super::rate::admit(&folder, &consent, &event, now + 3_600_000).unwrap());
    for _ in 0..3 {
        assert!(
            super::rate::admit(&folder, &consent, &self::event(Code::NativeCrash), now).unwrap()
        );
    }
}

#[test]
fn withdrawal_removes_consent_published_while_waiting_for_queue_lock() {
    use std::time::Duration;
    let state = State::new();
    let consent = state.consent();
    let held = Lock::try_acquire(&dir(state.path()), QUEUE_LOCK)
        .unwrap()
        .unwrap();
    let path = state.0.clone();
    let worker = std::thread::spawn(move || disable(&path));
    let deadline = Instant::now() + Duration::from_secs(2);
    while dir(state.path()).join(CONSENT_FILE).exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    consent::write_atomic(
        &dir(state.path()),
        CONSENT_FILE,
        &serde_json::to_vec(&consent).unwrap(),
    )
    .unwrap();
    drop(held);
    worker.join().unwrap().unwrap();
    assert!(
        consent_with(state.path(), &Env::stub(&[]))
            .unwrap()
            .is_none()
    );
}

#[test]
fn short_retry_after_never_resets_exponential_backoff() {
    let state = State::new();
    let consent = state.consent();
    queue(&state, &consent, 1);
    let mut now = now_ms();
    let mut delays = Vec::new();
    for _ in 0..4 {
        let report = flush_at(&state, &consent, now, |_| Reply::Http {
            status: 429,
            retry_after_secs: Some(60),
            body: Vec::new(),
        });
        let next = report.next_attempt_at_ms.unwrap();
        delays.push(next - now);
        now = next;
    }
    assert_eq!(delays, [72_000, 144_000, 288_000, 576_000]);
}

#[test]
fn wire_example_round_trips_and_validates() {
    let request: Request = serde_json::from_str(WIRE_REQUEST).unwrap();
    for event in &request.events {
        event.validate().unwrap();
    }
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::from_str::<serde_json::Value>(WIRE_REQUEST).unwrap()
    );
    let mut crash = request.events[0].clone();
    crash.kind = Kind::NativeCrash;
    crash.error_code = Code::NativeCrash;
    crash.native = Some(serde_json::from_str(WIRE_NATIVE).unwrap());
    crash.validate().unwrap();
    assert_eq!(
        serde_json::to_value(crash.native.as_ref().unwrap()).unwrap(),
        serde_json::from_str::<serde_json::Value>(WIRE_NATIVE).unwrap()
    );
}

#[test]
fn enum_names_match_wire_lists() {
    fn names<T: serde::Serialize>(values: &[T]) -> Vec<String> {
        values
            .iter()
            .map(|v| {
                serde_json::to_value(v)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }
    assert_eq!(
        names(&Process::ALL),
        ["krill-app", "krilld", "krill-cli", "krill-holder"]
    );
    assert_eq!(
        names(&[
            Kind::OperationError,
            Kind::Panic,
            Kind::UiHang,
            Kind::NativeCrash,
            Kind::AbnormalExit
        ]),
        [
            "operation_error",
            "panic",
            "ui_hang",
            "native_crash",
            "abnormal_exit"
        ]
    );
    assert_eq!(
        names(&Code::ALL),
        [
            "session_start_failed",
            "daemon_listener_failed",
            "scheduler_failed",
            "recording_failed",
            "update_failed",
            "daemon_connect_failed",
            "panic",
            "ui_stall",
            "native_crash",
            "unclean_exit"
        ]
    );
    assert_eq!(
        names(&[Os::Macos, Os::Windows, Os::Linux]),
        ["macos", "windows", "linux"]
    );
    assert_eq!(
        names(&[
            Arch::Aarch64,
            Arch::X86_64,
            Arch::X86,
            Arch::Arm,
            Arch::Other
        ]),
        ["aarch64", "x86_64", "x86", "arm", "other"]
    );
    assert_eq!(
        names(&Exception::ALL),
        [
            "EXC_BAD_ACCESS",
            "EXC_BAD_INSTRUCTION",
            "EXC_ARITHMETIC",
            "EXC_BREAKPOINT",
            "EXC_CRASH",
            "EXC_RESOURCE",
            "EXC_GUARD",
            "UNKNOWN"
        ]
    );
    assert_eq!(
        names(&Signal::ALL),
        [
            "SIGSEGV", "SIGABRT", "SIGBUS", "SIGILL", "SIGFPE", "SIGTRAP", "SIGKILL", "UNKNOWN"
        ]
    );
    for e in Exception::ALL {
        assert_eq!(Exception::from_name(e.as_str()), e);
        assert_eq!(names(&[e]), [e.as_str()]);
    }
    for s in Signal::ALL {
        assert_eq!(Signal::from_name(s.as_str()), s);
        assert_eq!(names(&[s]), [s.as_str()]);
    }
    assert_eq!(
        Exception::from_name("EXC_SOMETHING_NEW"),
        Exception::Unknown
    );
    assert_eq!(Signal::from_name("SIGUSR1"), Signal::Unknown);
    for p in Process::ALL {
        assert_eq!(names(&[p]), [p.as_str()]);
    }
}

#[test]
fn codes_pair_with_one_kind() {
    let kinds: Vec<Kind> = Code::ALL.iter().map(|c| c.kind()).collect();
    assert_eq!(
        kinds,
        [
            Kind::OperationError,
            Kind::OperationError,
            Kind::OperationError,
            Kind::OperationError,
            Kind::OperationError,
            Kind::OperationError,
            Kind::Panic,
            Kind::UiHang,
            Kind::NativeCrash,
            Kind::AbnormalExit
        ]
    );
    for code in Code::ALL {
        let mut event = event(code);
        event.validate().unwrap();
        event.kind = if code.kind() == Kind::Panic {
            Kind::OperationError
        } else {
            Kind::Panic
        };
        assert!(event.validate().is_err(), "{code:?}");
    }
}

#[test]
fn unknown_fields_and_values_are_rejected() {
    let mut value: serde_json::Value = serde_json::from_str(WIRE_REQUEST).unwrap();
    value["events"][0]["message"] = "boom".into();
    assert!(serde_json::from_value::<Request>(value).is_err());
    let mut value: serde_json::Value = serde_json::from_str(WIRE_REQUEST).unwrap();
    value["installation_id"] = "x".into();
    assert!(serde_json::from_value::<Request>(value).is_err());
    let mut value: serde_json::Value = serde_json::from_str(WIRE_REQUEST).unwrap();
    value["events"][0]["kind"] = "fatal".into();
    assert!(serde_json::from_value::<Request>(value).is_err());
    assert!(
        serde_json::from_str::<NativeData>(
            r#"{"exception":"UNKNOWN","signal":"UNKNOWN","frames":[],"path":"/x"}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<NativeFrame>(
            r#"{"image_uuid":"0123456789abcdef0123456789abcdef","offset":1,"symbol":"main"}"#
        )
        .is_err()
    );
}

#[test]
fn validate_enforces_kind_specific_fields_and_bounds() {
    let base = event(Code::UpdateFailed);
    base.validate().unwrap();
    let mut with_hang = base.clone();
    with_hang.hang_bucket_ms = Some(500);
    assert!(with_hang.validate().is_err());
    let mut with_native = base.clone();
    with_native.native = Some(NativeData::unknown());
    assert!(with_native.validate().is_err());

    let mut hang = event(Code::UiStall);
    hang.validate().unwrap();
    for bad in [None, Some(0), Some(700), Some(60_000)] {
        hang.hang_bucket_ms = bad;
        assert!(hang.validate().is_err(), "{bad:?}");
    }

    let mut crash = event(Code::NativeCrash);
    crash.validate().unwrap();
    crash.native = None;
    assert!(crash.validate().is_err());
    let frame = NativeFrame {
        image_uuid: "0123456789abcdef0123456789abcdef".into(),
        offset: MAX_SAFE_OFFSET,
    };
    crash.native = Some(NativeData {
        frames: vec![frame.clone(); MAX_NATIVE_FRAMES],
        ..NativeData::unknown()
    });
    crash.validate().unwrap();
    for frames in [
        vec![frame.clone(); MAX_NATIVE_FRAMES + 1],
        vec![NativeFrame {
            offset: MAX_SAFE_OFFSET + 1,
            ..frame.clone()
        }],
        vec![NativeFrame {
            image_uuid: "0123456789ABCDEF0123456789ABCDEF".into(),
            offset: 0,
        }],
        vec![NativeFrame {
            image_uuid: "../../Users/me".into(),
            offset: 0,
        }],
    ] {
        crash.native = Some(NativeData {
            frames,
            ..NativeData::unknown()
        });
        assert!(crash.validate().is_err());
    }

    for bad in ["", "0123", "0123456789abcdef0123456789abcdeg", "../x"] {
        let mut e = base.clone();
        e.event_id = bad.into();
        assert!(e.validate().is_err(), "{bad}");
    }
    for bad in [
        "",
        "1.2",
        "01.2.3",
        "1.2.3+build",
        "1.2.3-",
        "1.2.3-a..b",
        "1.2.3 ",
        "v1.2.3",
    ] {
        let mut e = base.clone();
        e.release = bad.into();
        assert!(e.validate().is_err(), "{bad}");
    }
    let mut e = base.clone();
    e.release = "1.2.3-rc.1".into();
    e.validate().unwrap();
    for bad in [
        String::new(),
        "x".repeat(MAX_BUILD_ID + 1),
        "0.1.0+dev/../x".into(),
        "0.1.0 dev".into(),
    ] {
        let mut e = base.clone();
        e.build_id = bad;
        assert!(e.validate().is_err());
    }
}

#[test]
fn new_event_carries_build_metadata_and_random_ids() {
    let a = Event::new(
        Process::App,
        Code::Panic,
        Some("crates/krill-app/src/main.rs:10".into()),
    )
    .unwrap();
    let b = Event::new(Process::App, Code::Panic, None).unwrap();
    assert_ne!(a.event_id, b.event_id);
    assert_eq!(a.kind, Kind::Panic);
    assert_eq!(a.release, env!("CARGO_PKG_VERSION"));
    assert_eq!(a.build_id, crate::BUILD);
    assert_eq!(Some(a.os), Os::current());
    assert_eq!(a.arch, Arch::current());
    assert_eq!((a.hang_bucket_ms, a.native.is_none()), (None, true));
    a.validate().unwrap();
    assert!(
        Event::new(
            Process::App,
            Code::Panic,
            Some("/Users/me/krill/crates/krill-app/src/main.rs:1".into())
        )
        .is_err()
    );
}

#[test]
fn source_location_keeps_only_public_relative_paths() {
    assert_eq!(
        source_location("crates/krill-app/src/main.rs", 42).as_deref(),
        Some("crates/krill-app/src/main.rs:42")
    );
    assert_eq!(
        source_location(r"crates\krilld\src\session\spawn.rs", 7).as_deref(),
        Some("crates/krilld/src/session/spawn.rs:7")
    );
    for bad in [
        "/Users/me/krill/crates/krill-app/src/main.rs",
        "C:/krill/crates/krill/src/main.rs",
        r"C:\krill\crates\krill\src\main.rs",
        "crates/krill-app/src/../../secret.rs",
        "crates/krill-app/src/./main.rs",
        "crates/krill-app/src//main.rs",
        "crates/other/src/lib.rs",
        "crates/krill-app/tests/e2e.rs",
        "vendor/vte/src/lib.rs",
        "/rustc/abc/library/std/src/panicking.rs",
        "crates/krill-app/src/.hidden.rs",
        "crates/krill-app/src/main.txt",
        "crates/Krill-App/src/main.rs",
        "crates/krill-app/src/my file.rs",
        "crates/krill-app/src/",
        "",
    ] {
        assert_eq!(source_location(bad, 1), None, "{bad}");
    }
    assert_eq!(source_location("crates/krill-app/src/main.rs", 0), None);
    let long = format!("crates/krill-app/src/{}.rs", "a".repeat(MAX_SOURCE_LOC));
    assert_eq!(source_location(&long, 1), None);

    let mut e = event(Code::UpdateFailed);
    e.source_loc = Some("crates/krill/src/main.rs:3:9".into());
    e.validate().unwrap();
    for bad in [
        "crates/krill/src/main.rs",
        "crates/krill/src/main.rs:0",
        "crates/krill/src/main.rs:01",
        "crates/krill/src/main.rs:1:2:3",
        "crates/krill/src/main.rs:-1",
        "crates/krill/src/main.rs:99999999999",
        "crates/krill/src/main.rs:1:",
    ] {
        e.source_loc = Some(bad.into());
        assert!(e.validate().is_err(), "{bad}");
    }
}

#[test]
fn event_id_is_deterministic_and_domain_separated() {
    let a = event_id("native", b"incident-1");
    // SHA-256("krill-error-reporting/v1\0" || be64(6) || "native" || "incident-1")의 앞 128비트.
    assert_eq!(a, "83646872b935d73fbef18c66324aeeea");
    assert_eq!(a, event_id("native", b"incident-1"));
    assert_ne!(a, event_id("native", b"incident-2"));
    assert_ne!(a, event_id("abnormal", b"incident-1"));
    assert_ne!(event_id("ab", b"c"), event_id("a", b"bc"));
}

#[test]
fn ack_accepts_only_requested_unique_ids() {
    let ids = vec!["a".repeat(32), "b".repeat(32)];
    let other = "c".repeat(32);
    let body = |accepted: &[&String], duplicate: &[&String]| {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "accepted_ids": accepted,
            "duplicate_ids": duplicate,
        }))
        .unwrap()
    };
    let ack = Ack::parse(&body(&[&ids[0]], &[&ids[1]]), &ids).unwrap();
    assert_eq!(
        ack.acked().collect::<Vec<_>>(),
        [ids[0].as_str(), ids[1].as_str()]
    );
    assert!(Ack::parse(&body(&[], &[]), &ids).is_some());
    for bad in [
        body(&[&other], &[]),
        body(&[&ids[0]], &[&ids[0]]),
        body(&[&ids[0], &ids[0]], &[]),
        br#"{"schema_version":2,"accepted_ids":[],"duplicate_ids":[]}"#.to_vec(),
        br#"{"schema_version":1,"accepted_ids":[],"duplicate_ids":[],"rejected_ids":[]}"#.to_vec(),
        br#"{"schema_version":1,"accepted_ids":[]}"#.to_vec(),
        b"not json".to_vec(),
        Vec::new(),
        vec![b' '; MAX_REQUEST_BYTES + 1],
    ] {
        assert!(Ack::parse(&bad, &ids).is_none(), "{bad:?}");
    }
}

#[test]
fn hang_buckets_round_down_to_the_crossed_threshold() {
    let ms = Duration::from_millis;
    assert_eq!(hang_bucket_ms(ms(499)), None);
    assert_eq!(hang_bucket_ms(ms(500)), Some(500));
    assert_eq!(hang_bucket_ms(ms(1499)), Some(500));
    assert_eq!(hang_bucket_ms(ms(1500)), Some(1500));
    assert_eq!(hang_bucket_ms(ms(29_999)), Some(5000));
    assert_eq!(hang_bucket_ms(Duration::from_secs(3600)), Some(30_000));
}

#[test]
fn consent_is_a_separate_opt_in_with_a_stable_generation() {
    let state = State::new();
    let none = Env::stub(&[]);
    assert_eq!(consent_with(state.path(), &none).unwrap(), None);
    // 텔레메트리 동의는 오류 보고 동의가 아니다.
    crate::telemetry::enable(state.path()).unwrap();
    assert_eq!(consent_with(state.path(), &none).unwrap(), None);
    assert!(
        !dir(state.path()).exists(),
        "읽기만으로 디렉터리를 만들지 않는다"
    );

    enable(state.path()).unwrap();
    let first = consent_with(state.path(), &none).unwrap().unwrap();
    assert!(first.enabled_at_ms > 0);
    assert_eq!(first.generation.len(), 32);
    enable(state.path()).unwrap();
    assert_eq!(
        consent_with(state.path(), &none).unwrap().as_ref(),
        Some(&first),
        "이미 동의했으면 세대를 유지한다"
    );
    disable(state.path()).unwrap();
    disable(state.path()).unwrap();
    assert_eq!(consent_with(state.path(), &none).unwrap(), None);
    enable(state.path()).unwrap();
    let second = consent_with(state.path(), &none).unwrap().unwrap();
    assert_ne!(second.generation, first.generation, "재동의는 새 세대");
}

#[cfg(unix)]
#[test]
fn consent_and_queue_files_are_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let state = State::new();
    let consent = state.consent();
    let ids = queue(&state, &consent, 1);
    assert_eq!(mode(&dir(state.path())), 0o700);
    assert_eq!(mode(&dir(state.path()).join(CONSENT_FILE)), 0o600);
    assert_eq!(mode(&state.outbox()), 0o700);
    assert_eq!(
        mode(&state.outbox().join(format!("{}.json", ids[0]))),
        0o600
    );
}

#[test]
fn env_veto_disables_reporting_and_status_names_it() {
    let state = State::new();
    enable(state.path()).unwrap();
    for (var, value) in [
        (DISABLED_VAR, "1"),
        ("DO_NOT_TRACK", "1"),
        ("KRILL_TELEMETRY_DISABLED", "true"),
        ("CI", "true"),
        ("GITHUB_ACTIONS", "true"),
    ] {
        let env = Env::stub(&[(var, value)]);
        assert_eq!(consent_with(state.path(), &env).unwrap(), None, "{var}");
        let status = status_with(state.path(), &env).unwrap();
        assert!(status.consented && !status.enabled, "{var}");
        assert_eq!(status.veto.as_deref(), Some(var));
        capture_with(
            state.path(),
            &env,
            Process::App,
            Code::Panic,
            None,
            "crates/krill-app/src/main.rs",
            1,
        );
        assert!(state.entries().is_empty(), "{var}");
    }
    let off = Env::stub(&[(DISABLED_VAR, "0"), ("CI", "false"), ("DO_NOT_TRACK", "")]);
    assert!(consent_with(state.path(), &off).unwrap().is_some());
}

#[test]
fn damaged_consent_is_not_consent() {
    let state = State::new();
    enable(state.path()).unwrap();
    let file = dir(state.path()).join(CONSENT_FILE);
    for bad in [
        b"{}".to_vec(),
        b"not json".to_vec(),
        br#"{"generation":"XYZ","enabled_at_ms":1}"#.to_vec(),
        br#"{"generation":"0123456789abcdef0123456789abcdef","enabled_at_ms":1,"extra":1}"#
            .to_vec(),
        vec![b' '; 600],
    ] {
        fs::write(&file, bad).unwrap();
        assert_eq!(consent_with(state.path(), &Env::stub(&[])).unwrap(), None);
    }
    enable(state.path()).unwrap();
    assert!(
        consent_with(state.path(), &Env::stub(&[]))
            .unwrap()
            .is_some()
    );
}

#[test]
fn status_reports_queue_and_backoff_with_fixed_keys() {
    let state = State::new();
    let none = Env::stub(&[]);
    assert_eq!(
        status_with(state.path(), &none).unwrap(),
        Status {
            consented: false,
            enabled: false,
            pending: 0,
            veto: None,
            enabled_at_ms: None,
            next_attempt_at_ms: None,
        }
    );
    let consent = state.consent();
    queue(&state, &consent, 2);
    let report = flush_at(&state, &consent, now_ms(), |_| Reply::Transport);
    let status = status_with(state.path(), &none).unwrap();
    assert!(status.consented && status.enabled);
    assert_eq!(status.pending, 2);
    assert_eq!(status.enabled_at_ms, Some(consent.enabled_at_ms));
    assert_eq!(status.next_attempt_at_ms, report.next_attempt_at_ms);
    let json = serde_json::to_value(&status).unwrap();
    let mut keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "consented",
            "enabled",
            "enabled_at_ms",
            "next_attempt_at_ms",
            "pending",
            "veto"
        ]
    );
    disable(state.path()).unwrap();
    let status = status_with(state.path(), &none).unwrap();
    assert_eq!((status.consented, status.pending), (false, 0));
    assert!(!state.retry_file().exists());
}

#[test]
fn capture_completes_optional_fields_and_respects_consent() {
    let state = State::new();
    let none = Env::stub(&[]);
    let file = "crates/krilld/src/main.rs";
    capture_with(
        state.path(),
        &none,
        Process::Daemon,
        Code::Panic,
        None,
        file,
        9,
    );
    assert!(
        !dir(state.path()).exists(),
        "동의 전에는 아무것도 쓰지 않는다"
    );
    state.consent();
    capture_with(
        state.path(),
        &none,
        Process::Daemon,
        Code::Panic,
        None,
        file,
        9,
    );
    capture_with(
        state.path(),
        &none,
        Process::App,
        Code::UiStall,
        None,
        file,
        1,
    );
    capture_with(
        state.path(),
        &none,
        Process::App,
        Code::UiStall,
        Some(5000),
        file,
        1,
    );
    capture_with(
        state.path(),
        &none,
        Process::App,
        Code::NativeCrash,
        None,
        file,
        1,
    );
    capture_with(
        state.path(),
        &none,
        Process::Cli,
        Code::UpdateFailed,
        None,
        "/abs/x.rs",
        1,
    );
    let events: Vec<Event> = state
        .entries()
        .iter()
        .map(|id| {
            let raw = fs::read(state.outbox().join(format!("{id}.json"))).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
            serde_json::from_value(value["event"].clone()).unwrap()
        })
        .collect();
    assert_eq!(events.len(), 5);
    for event in &events {
        event.validate().unwrap();
    }
    let panic = events.iter().find(|e| e.kind == Kind::Panic).unwrap();
    assert_eq!(
        panic.source_loc.as_deref(),
        Some("crates/krilld/src/main.rs:9")
    );
    assert_eq!(panic.process, Process::Daemon);
    let mut buckets: Vec<u32> = events.iter().filter_map(|e| e.hang_bucket_ms).collect();
    buckets.sort_unstable();
    assert_eq!(buckets, [500, 5000]);
    let crash = events.iter().find(|e| e.kind == Kind::NativeCrash).unwrap();
    assert_eq!(crash.native, Some(NativeData::unknown()));
    let op = events
        .iter()
        .find(|e| e.kind == Kind::OperationError)
        .unwrap();
    assert_eq!(op.source_loc, None, "비공개 경로는 위치 없이 기록한다");
}

#[test]
fn record_needs_the_current_consent_and_never_resurrects_withdrawn_data() {
    let state = State::new();
    let foreign = Consent {
        generation: "0".repeat(32),
        enabled_at_ms: 1,
    };
    assert!(!record_with_consent(state.path(), event(Code::Panic), &foreign).unwrap());
    assert!(!dir(state.path()).exists());
    let consent = state.consent();
    let stale = Consent {
        enabled_at_ms: consent.enabled_at_ms.saturating_add(1),
        ..consent.clone()
    };
    assert!(!record_with_consent(state.path(), event(Code::Panic), &stale).unwrap());
    assert!(record_with_consent(state.path(), event(Code::Panic), &consent).unwrap());
    disable(state.path()).unwrap();
    assert!(!state.outbox().exists());
    assert!(!record_with_consent(state.path(), event(Code::Panic), &consent).unwrap());
    assert!(
        !state.outbox().exists(),
        "철회 뒤 대기열을 다시 만들지 않는다"
    );
    enable(state.path()).unwrap();
    assert!(
        !record_with_consent(state.path(), event(Code::Panic), &consent).unwrap(),
        "재동의 뒤 옛 세대로는 기록하지 않는다"
    );
}

#[test]
fn record_dedups_validates_and_caps_the_queue() {
    let state = State::new();
    let consent = state.consent();
    let e = event(Code::RecordingFailed);
    assert!(record_with_consent(state.path(), e.clone(), &consent).unwrap());
    assert!(
        !record_with_consent(state.path(), e, &consent).unwrap(),
        "같은 ID"
    );
    let mut bad = event(Code::RecordingFailed);
    bad.kind = Kind::Panic;
    assert_eq!(
        record_with_consent(state.path(), bad, &consent)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    queue(&state, &consent, MAX_PENDING - 1);
    assert_eq!(state.entries().len(), MAX_PENDING);
    assert!(!record_with_consent(state.path(), event(Code::Panic), &consent).unwrap());
    // 만료된 항목은 기록할 때 치워 자리를 만든다.
    let oldest = state.entries()[0].clone();
    state.edit_entry(&oldest, |v| v["created_at_ms"] = 1.into());
    assert!(record_with_consent(state.path(), event(Code::Panic), &consent).unwrap());
    assert_eq!(state.entries().len(), MAX_PENDING);
    assert!(!state.entries().contains(&oldest));
}

#[test]
fn record_never_waits_for_the_queue_lock() {
    let state = State::new();
    let consent = state.consent();
    let held = Lock::try_acquire(&dir(state.path()), QUEUE_LOCK)
        .unwrap()
        .unwrap();
    let started = Instant::now();
    assert!(!record_with_consent(state.path(), event(Code::Panic), &consent).unwrap());
    capture_with(
        state.path(),
        &Env::stub(&[]),
        Process::App,
        Code::Panic,
        None,
        "crates/krill-app/src/main.rs",
        1,
    );
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(state.entries().is_empty());
    drop(held);
    assert!(record_with_consent(state.path(), event(Code::Panic), &consent).unwrap());
}

#[test]
fn flush_sends_at_most_four_requests_of_sixteen() {
    let state = State::new();
    let consent = state.consent();
    queue(&state, &consent, MAX_PENDING);
    let mut sizes = Vec::new();
    let report = flush_at(&state, &consent, now_ms(), |body| {
        assert!(body.len() <= MAX_REQUEST_BYTES);
        let ids = sent_ids(body);
        sizes.push(ids.len());
        ack(&ids, &[])
    });
    assert_eq!(sizes, [MAX_BATCH; MAX_FLUSH_REQUESTS]);
    assert_eq!(
        (
            report.outcome,
            report.requests,
            report.accepted,
            report.pending
        ),
        (FlushOutcome::Sent, MAX_FLUSH_REQUESTS, MAX_PENDING, 0)
    );
    assert!(!state.retry_file().exists());
    let mut called = false;
    let report = flush_at(&state, &consent, now_ms(), |_| {
        called = true;
        Reply::Transport
    });
    assert!(!called);
    assert_eq!(report.outcome, FlushOutcome::Empty);
}

#[test]
fn queue_stays_writable_while_a_request_is_in_flight() {
    let state = State::new();
    let consent = state.consent();
    queue(&state, &consent, 1);
    let mut late = None;
    let report = flush_at(&state, &consent, now_ms(), |body| {
        if late.is_none() {
            let event = event(Code::SchedulerFailed);
            late = Some(event.event_id.clone());
            assert!(record_with_consent(state.path(), event, &consent).unwrap());
        }
        ack(&sent_ids(body), &[])
    });
    assert!(late.is_some());
    assert_eq!(
        (
            report.outcome,
            report.requests,
            report.accepted,
            report.pending
        ),
        (FlushOutcome::Sent, 2, 2, 0)
    );
}

#[test]
fn attempts_are_counted_before_the_request_leaves() {
    let state = State::new();
    let consent = state.consent();
    let ids = queue(&state, &consent, 1);
    let path = state.outbox().join(format!("{}.json", ids[0]));
    let mut seen = Vec::new();
    flush_at(&state, &consent, now_ms(), |_| {
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        seen.push(value["attempts"].as_u64().unwrap());
        Reply::Transport
    });
    assert_eq!(seen, [1]);
}

#[test]
fn partial_ack_removes_only_matching_ids_and_backs_off_the_rest() {
    let state = State::new();
    let consent = state.consent();
    queue(&state, &consent, 3);
    let now = now_ms();
    let mut sent = Vec::new();
    let report = flush_at(&state, &consent, now, |body| {
        sent = sent_ids(body);
        ack(&sent[..1], &sent[1..2])
    });
    assert_eq!(
        (
            report.outcome,
            report.requests,
            report.accepted,
            report.duplicates,
            report.pending
        ),
        (FlushOutcome::Deferred, 1, 1, 1, 1)
    );
    assert_eq!(state.entries(), [sent[2].clone()]);
    assert_eq!(report.next_attempt_at_ms, Some(now.saturating_add(72_000)));
    let mut called = false;
    let report = flush_at(&state, &consent, now, |_| {
        called = true;
        Reply::Transport
    });
    assert!(!called, "재시도 시각 전에는 보내지 않는다");
    assert_eq!(report.outcome, FlushOutcome::Backoff);
}

/// 받은 ID로 응답을 만드는 가짜 서버.
type Case<'a> = Box<dyn Fn(&[String]) -> Reply + 'a>;

#[test]
fn unusable_acks_keep_the_whole_batch() {
    let unknown = "f".repeat(32);
    let cases: Vec<Case<'_>> = vec![
        Box::new(|ids: &[String]| ack(&[ids[0].clone(), unknown.clone()], &[])),
        Box::new(|ids: &[String]| ack(&ids[..1], &ids[..1])),
        Box::new(|ids: &[String]| ack(&[ids[0].clone(), ids[0].clone()], &[])),
        Box::new(|_: &[String]| Reply::Http {
            status: 200,
            retry_after_secs: None,
            body: br#"{"schema_version":2,"accepted_ids":[],"duplicate_ids":[]}"#.to_vec(),
        }),
        Box::new(|_: &[String]| Reply::Http {
            status: 200,
            retry_after_secs: None,
            body: b"<html>ok</html>".to_vec(),
        }),
        Box::new(|ids: &[String]| match ack(ids, &[]) {
            Reply::Http { body, .. } => Reply::Http {
                status: 201,
                retry_after_secs: None,
                body,
            },
            Reply::Transport => Reply::Transport,
        }),
    ];
    for (i, case) in cases.iter().enumerate() {
        let state = State::new();
        let consent = state.consent();
        let ids = queue(&state, &consent, 2);
        let report = flush_at(&state, &consent, now_ms(), |body| case(&sent_ids(body)));
        assert_eq!(
            (
                report.outcome,
                report.requests,
                report.accepted,
                report.pending
            ),
            (FlushOutcome::Deferred, 1, 0, 2),
            "case {i}"
        );
        assert_eq!(state.entries(), ids, "case {i}");
    }
}

#[test]
fn retryable_statuses_keep_the_batch_and_back_off() {
    for reply in [
        Reply::Transport,
        http(500),
        http(503),
        http(301),
        http(401),
        http(403),
        http(404),
        http(204),
    ] {
        let state = State::new();
        let consent = state.consent();
        let ids = queue(&state, &consent, 2);
        let now = now_ms();
        let report = flush_at(&state, &consent, now, |_| reply.clone());
        assert_eq!(
            (report.outcome, report.requests, report.pending),
            (FlushOutcome::Deferred, 1, 2),
            "{reply:?}"
        );
        assert_eq!(report.next_attempt_at_ms, Some(now.saturating_add(72_000)));
        assert_eq!(state.entries(), ids);
    }
}

#[test]
fn rate_limit_holds_the_whole_queue_until_retry_after() {
    let state = State::new();
    let consent = state.consent();
    let ids = queue(&state, &consent, 3);
    let now = now_ms();
    let report = flush_at(&state, &consent, now, |_| Reply::Http {
        status: 429,
        retry_after_secs: Some(120),
        body: Vec::new(),
    });
    assert_eq!(
        (report.outcome, report.requests, report.pending),
        (FlushOutcome::Deferred, 1, 3)
    );
    // 120초 + 지터 상한 20%.
    let until = now.saturating_add(144_000);
    assert_eq!(report.next_attempt_at_ms, Some(until));
    assert_eq!(state.entries(), ids);
    let mut called = false;
    let report = flush_at(&state, &consent, until.saturating_sub(1), |_| {
        called = true;
        Reply::Transport
    });
    assert!(!called);
    assert_eq!(
        (report.outcome, report.next_attempt_at_ms),
        (FlushOutcome::Backoff, Some(until))
    );
    let report = flush_at(&state, &consent, until, |body| ack(&sent_ids(body), &[]));
    assert_eq!(
        (report.outcome, report.accepted, report.pending),
        (FlushOutcome::Sent, 3, 0)
    );
    assert!(
        !state.retry_file().exists(),
        "성공하면 재시도 상태를 지운다"
    );
}

#[test]
fn retry_after_applies_to_server_errors_too() {
    let state = State::new();
    let consent = state.consent();
    queue(&state, &consent, 1);
    let now = now_ms();
    let report = flush_at(&state, &consent, now, |_| Reply::Http {
        status: 503,
        retry_after_secs: Some(600),
        body: Vec::new(),
    });
    assert_eq!(report.next_attempt_at_ms, Some(now.saturating_add(720_000)));
}

#[test]
fn backoff_stays_between_a_minute_and_a_day() {
    for (retry_after, delay) in [
        (Some(0), 72_000),
        (Some(30), 72_000),
        (Some(10 * 86_400), RETRY_MAX_MS),
        (None, 72_000),
    ] {
        let state = State::new();
        let consent = state.consent();
        queue(&state, &consent, 1);
        let now = now_ms();
        let report = flush_at(&state, &consent, now, |_| Reply::Http {
            status: 429,
            retry_after_secs: retry_after,
            body: Vec::new(),
        });
        assert_eq!(
            report.next_attempt_at_ms,
            Some(now.saturating_add(delay)),
            "{retry_after:?}"
        );
    }
    let state = State::new();
    let consent = state.consent();
    queue(&state, &consent, 1);
    let mut now = now_ms();
    let mut delays = Vec::new();
    for _ in 0..3 {
        let report = flush_at(&state, &consent, now, |_| http(503));
        let next = report.next_attempt_at_ms.unwrap();
        delays.push(next.saturating_sub(now));
        now = next;
    }
    assert_eq!(delays, [72_000, 144_000, 288_000], "60초부터 두 배씩(+20%)");
}

#[test]
fn each_event_is_sent_at_most_ten_times() {
    let state = State::new();
    let consent = state.consent();
    queue(&state, &consent, 1);
    let mut now = now_ms();
    let mut calls = Vec::new();
    for _ in 0..MAX_ATTEMPTS {
        let report = flush_at(&state, &consent, now, |_| {
            calls.push(());
            http(503)
        });
        assert_eq!(report.outcome, FlushOutcome::Deferred);
        now = report.next_attempt_at_ms.unwrap();
    }
    let report = flush_at(&state, &consent, now, |_| {
        calls.push(());
        http(503)
    });
    assert_eq!(calls.len(), usize::try_from(MAX_ATTEMPTS).unwrap());
    assert_eq!(
        (report.outcome, report.dropped, report.pending),
        (FlushOutcome::Empty, 1, 0)
    );
}

#[test]
fn expired_future_and_foreign_entries_never_reach_the_wire() {
    let state = State::new();
    let consent = state.consent();
    let ids = queue(&state, &consent, 4);
    let now = now_ms();
    state.edit_entry(&ids[0], |v| {
        v["created_at_ms"] = now.saturating_sub(MAX_AGE_MS).saturating_sub(1).into();
    });
    state.edit_entry(&ids[1], |v| {
        v["created_at_ms"] = now.saturating_add(2 * 86_400_000).into();
    });
    state.edit_entry(&ids[2], |v| v["generation"] = "f".repeat(32).into());
    let mut sent = Vec::new();
    let report = flush_at(&state, &consent, now, |body| {
        let batch = sent_ids(body);
        sent.extend(batch.clone());
        ack(&batch, &[])
    });
    assert_eq!(sent, [ids[3].clone()]);
    assert_eq!((report.dropped, report.accepted, report.pending), (3, 1, 0));
}

#[test]
fn damaged_entries_are_removed_one_by_one() {
    let state = State::new();
    let consent = state.consent();
    let ids = queue(&state, &consent, 2);
    let outbox = state.outbox();
    fs::write(
        outbox.join(format!("{}.json", "a".repeat(32))),
        b"{not json",
    )
    .unwrap();
    fs::write(
        outbox.join(format!("{}.json", "b".repeat(32))),
        vec![b' '; MAX_EVENT_BYTES + 1],
    )
    .unwrap();
    fs::copy(
        outbox.join(format!("{}.json", ids[0])),
        outbox.join(format!("{}.json", "c".repeat(32))),
    )
    .unwrap();
    fs::write(outbox.join(".leftover.tmp"), b"x").unwrap();
    fs::write(outbox.join("notes.txt"), b"x").unwrap();
    fs::create_dir(outbox.join("subdir")).unwrap();
    let mut sent = Vec::new();
    let report = flush_at(&state, &consent, now_ms(), |body| {
        let batch = sent_ids(body);
        sent.extend(batch.clone());
        ack(&batch, &[])
    });
    sent.sort();
    assert_eq!(sent, ids);
    assert_eq!((report.dropped, report.pending), (3, 0));
    assert!(!outbox.join(".leftover.tmp").exists());
    assert!(!outbox.join("notes.txt").exists());
    assert!(outbox.join("subdir").is_dir());
}

#[test]
fn rejected_batch_is_split_and_only_the_rejected_single_is_dropped() {
    let state = State::new();
    let consent = state.consent();
    let ids = queue(&state, &consent, 3);
    let bad = ids[1].clone();
    let mut requests: Vec<Vec<String>> = Vec::new();
    let report = flush_at(&state, &consent, now_ms(), |body| {
        let batch = sent_ids(body);
        requests.push(batch.clone());
        if batch.len() > 1 || batch.contains(&bad) {
            http(400)
        } else {
            ack(&batch, &[])
        }
    });
    assert_eq!(requests.len(), MAX_FLUSH_REQUESTS);
    assert_eq!(requests[0].len(), 3);
    assert!(requests[1..].iter().all(|r| r.len() == 1));
    assert_eq!(
        (
            report.outcome,
            report.accepted,
            report.rejected,
            report.pending
        ),
        (FlushOutcome::Sent, 2, 1, 0)
    );
}

#[test]
fn isolation_continues_across_flushes() {
    let state = State::new();
    let consent = state.consent();
    queue(&state, &consent, 6);
    let mut bad: Option<String> = None;
    let mut requests: Vec<Vec<String>> = Vec::new();
    let mut transport = |body: &[u8]| -> Reply {
        let batch = sent_ids(body);
        requests.push(batch.clone());
        if bad.is_none() {
            bad = batch.last().cloned();
        }
        if batch.len() > 1 || bad.as_ref().is_some_and(|b| batch.contains(b)) {
            http(413)
        } else {
            ack(&batch, &[])
        }
    };
    let first = flush_at(&state, &consent, now_ms(), &mut transport);
    assert_eq!(
        (first.requests, first.accepted, first.pending),
        (MAX_FLUSH_REQUESTS, 3, 3)
    );
    let second = flush_at(&state, &consent, now_ms(), &mut transport);
    assert_eq!(
        (second.accepted, second.rejected, second.pending),
        (2, 1, 0)
    );
    assert_eq!(requests[0].len(), 6);
    assert!(
        requests[1..].iter().all(|r| r.len() == 1),
        "격리 중에는 단건만 보낸다: {requests:?}"
    );
    assert!(!state.retry_file().exists());
}

#[test]
fn withdrawal_during_upload_never_recreates_deleted_files() {
    for acked in [true, false] {
        let state = State::new();
        let consent = state.consent();
        queue(&state, &consent, 2);
        let report = flush_at(&state, &consent, now_ms(), |body| {
            disable(state.path()).unwrap();
            if acked {
                ack(&sent_ids(body), &[])
            } else {
                Reply::Transport
            }
        });
        assert_eq!(
            (
                report.outcome,
                report.requests,
                report.accepted,
                report.pending
            ),
            (FlushOutcome::Withdrawn, 1, 0, 0)
        );
        assert!(!state.outbox().exists());
        assert!(!state.retry_file().exists());
        assert!(!dir(state.path()).join(CONSENT_FILE).exists());
    }
}

#[test]
fn reconsent_during_upload_does_not_mix_generations() {
    let state = State::new();
    let consent = state.consent();
    queue(&state, &consent, 2);
    let report = flush_at(&state, &consent, now_ms(), |_| {
        disable(state.path()).unwrap();
        enable(state.path()).unwrap();
        http(503)
    });
    assert_eq!(report.outcome, FlushOutcome::Withdrawn);
    assert!(state.entries().is_empty());
    assert!(!state.retry_file().exists());
    let fresh = consent_with(state.path(), &Env::stub(&[]))
        .unwrap()
        .unwrap();
    assert_ne!(fresh.generation, consent.generation);
    let mut called = false;
    let report = flush_at(&state, &consent, now_ms(), |_| {
        called = true;
        Reply::Transport
    });
    assert!(!called, "옛 동의로는 보내지 않는다");
    assert_eq!(report.outcome, FlushOutcome::Withdrawn);
}

#[test]
fn reenable_drops_previous_generation_leftovers() {
    let state = State::new();
    let consent = state.consent();
    let ids = queue(&state, &consent, 1);
    let leftover = fs::read(state.outbox().join(format!("{}.json", ids[0]))).unwrap();
    let native = dir(state.path()).join(NATIVE_DIR);
    let runs = dir(state.path()).join(RUNS_DIR);
    fs::create_dir_all(&native).unwrap();
    fs::create_dir_all(&runs).unwrap();
    disable(state.path()).unwrap();
    assert!(!native.exists() && !runs.exists() && !state.outbox().exists());
    // 철회 정리가 끝나지 못한 옛 세대 파일이 남아 있어도 보내지 않는다.
    let consent = state.consent();
    fs::create_dir_all(state.outbox()).unwrap();
    fs::write(state.outbox().join(format!("{}.json", ids[0])), leftover).unwrap();
    let mut called = false;
    let report = flush_at(&state, &consent, now_ms(), |_| {
        called = true;
        Reply::Transport
    });
    assert!(!called);
    assert_eq!((report.dropped, report.pending), (1, 0));
}

#[test]
fn only_one_uploader_runs_at_a_time() {
    let state = State::new();
    state.consent();
    let first = UploadLock::try_acquire(state.path()).unwrap();
    assert!(first.is_some());
    assert!(UploadLock::try_acquire(state.path()).unwrap().is_none());
    drop(first);
    assert!(UploadLock::try_acquire(state.path()).unwrap().is_some());
}

#[test]
fn flush_gives_up_when_the_queue_lock_stays_busy() {
    let state = State::new();
    let consent = state.consent();
    queue(&state, &consent, 1);
    let held = Lock::try_acquire(&dir(state.path()), QUEUE_LOCK)
        .unwrap()
        .unwrap();
    let mut called = false;
    let report = flush_at(&state, &consent, now_ms(), |_| {
        called = true;
        Reply::Transport
    });
    assert!(!called);
    assert_eq!((report.outcome, report.pending), (FlushOutcome::Busy, 1));
    drop(held);
}

#[test]
fn largest_valid_event_fits_the_wire_and_queue_file_limits() {
    let state = State::new();
    let consent = state.consent();
    let mut event = event(Code::NativeCrash);
    event.process = Process::Holder;
    // 21 + 154 + 3 + 22 = 200바이트.
    event.source_loc = Some(format!(
        "crates/krill-app/src/{}.rs:4294967295:4294967295",
        "a".repeat(154)
    ));
    event.release = format!("999999999.999999999.999999999-{}", "r".repeat(34));
    event.build_id = "b".repeat(MAX_BUILD_ID);
    event.native = Some(NativeData {
        exception: Exception::BadInstruction,
        signal: Signal::Unknown,
        frames: vec![
            NativeFrame {
                image_uuid: "f".repeat(32),
                offset: MAX_SAFE_OFFSET,
            };
            MAX_NATIVE_FRAMES
        ],
    });
    assert_eq!(event.source_loc.as_ref().unwrap().len(), MAX_SOURCE_LOC);
    assert_eq!(event.release.len(), 64);
    event.validate().unwrap();
    let event_bytes = serde_json::to_vec(&event).unwrap().len();
    assert!(event_bytes <= MAX_EVENT_BYTES, "{event_bytes}");
    let id = event.event_id.clone();
    assert!(record_with_consent(state.path(), event, &consent).unwrap());
    let entry_bytes = fs::metadata(state.outbox().join(format!("{id}.json")))
        .unwrap()
        .len();
    let entry_bytes = usize::try_from(entry_bytes).unwrap();
    assert!(entry_bytes > event_bytes, "봉투가 붙는다");
    assert!(entry_bytes <= MAX_ENTRY_BYTES, "{entry_bytes}");
    // 상한을 넘는 파일은 끝까지 읽지 않고 버린다.
    fs::write(
        state.outbox().join(format!("{}.json", "c".repeat(32))),
        vec![b' '; MAX_ENTRY_BYTES + 1],
    )
    .unwrap();
    let report = flush_at(&state, &consent, now_ms(), |body| {
        assert_eq!(sent_ids(body), std::slice::from_ref(&id));
        ack(&sent_ids(body), &[])
    });
    assert_eq!((report.accepted, report.dropped, report.pending), (1, 1, 0));
}

#[cfg(unix)]
fn mkfifo(path: &Path) {
    assert!(
        std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}

/// 다른 스레드에서 돌려 5초 안에 끝나는지 본다(FIFO 등에서 막히면 실패).
#[cfg(unix)]
fn finishes<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tx.send(f()).ok();
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("blocked on a special file")
}

#[cfg(unix)]
#[test]
fn fifo_consent_file_never_blocks_capture_or_status() {
    let state = State::new();
    let consent = state.consent();
    let file = dir(state.path()).join(CONSENT_FILE);
    fs::remove_file(&file).unwrap();
    mkfifo(&file);
    let path = state.path().to_path_buf();
    let (read, status) = finishes(move || {
        (
            consent_with(&path, &Env::stub(&[])).unwrap(),
            status_with(&path, &Env::stub(&[])).unwrap(),
        )
    });
    assert_eq!(read, None, "FIFO는 동의가 아니다");
    assert!(!status.consented);
    let path = state.path().to_path_buf();
    finishes(move || {
        capture_with(
            &path,
            &Env::stub(&[]),
            Process::App,
            Code::Panic,
            None,
            "crates/krill-app/src/main.rs",
            1,
        );
    });
    let path = state.path().to_path_buf();
    let stale = consent.clone();
    assert!(!finishes(move || record_with_consent(
        &path,
        event(Code::Panic),
        &stale
    )
    .unwrap()));
    assert!(state.entries().is_empty());
    let path = state.path().to_path_buf();
    finishes(move || disable(&path).unwrap());
    assert!(fs::symlink_metadata(&file).is_err());
}

#[cfg(unix)]
#[test]
fn fifo_queue_and_retry_files_are_dropped_without_being_opened() {
    let state = State::new();
    let consent = state.consent();
    let ids = queue(&state, &consent, 1);
    mkfifo(&state.outbox().join(format!("{}.json", "e".repeat(32))));
    mkfifo(&state.retry_file());
    let path = state.path().to_path_buf();
    let expected = consent.clone();
    let (report, sent) = finishes(move || {
        let flight = UploadLock::try_acquire(&path).unwrap().unwrap();
        let mut sent = Vec::new();
        let report = flush_with(
            &path,
            &expected,
            &flight,
            &mut |body: &[u8]| {
                sent.extend(sent_ids(body));
                ack(&sent_ids(body), &[])
            },
            &mut now_ms,
            &mut |max| max,
        )
        .unwrap();
        (report, sent)
    });
    assert_eq!(sent, ids);
    assert_eq!(
        (
            report.outcome,
            report.accepted,
            report.dropped,
            report.pending
        ),
        (FlushOutcome::Sent, 1, 1, 0)
    );
    assert!(fs::symlink_metadata(state.retry_file()).is_err());
}

#[cfg(unix)]
#[test]
fn special_lock_files_fail_fast() {
    let state = State::new();
    let consent = state.consent();
    let lock = dir(state.path()).join(QUEUE_LOCK);
    fs::remove_file(&lock).unwrap();
    mkfifo(&lock);
    let path = state.path().to_path_buf();
    let expected = consent.clone();
    let result = finishes(move || record_with_consent(&path, event(Code::Panic), &expected));
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    let path = state.path().to_path_buf();
    finishes(move || {
        capture_with(
            &path,
            &Env::stub(&[]),
            Process::App,
            Code::Panic,
            None,
            "crates/krill-app/src/main.rs",
            1,
        );
    });
    mkfifo(&dir(state.path()).join("upload.lock"));
    let path = state.path().to_path_buf();
    assert_eq!(
        finishes(move || UploadLock::try_acquire(&path).map(|l| l.is_some()))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    // 철회는 잠금 자리가 이상해도 끝나고 동의를 지운다.
    let path = state.path().to_path_buf();
    finishes(move || disable(&path).unwrap());
    assert_eq!(consent_with(state.path(), &Env::stub(&[])).unwrap(), None);
}

#[cfg(unix)]
#[test]
fn symlinks_are_never_followed() {
    use std::os::unix::fs::symlink;
    // 다른 곳의 유효한 동의 파일을 가리키는 링크는 동의가 아니다.
    let state = State::new();
    state.consent();
    let elsewhere = State::new();
    elsewhere.consent();
    let file = dir(state.path()).join(CONSENT_FILE);
    fs::remove_file(&file).unwrap();
    symlink(dir(elsewhere.path()).join(CONSENT_FILE), &file).unwrap();
    assert_eq!(consent_with(state.path(), &Env::stub(&[])).unwrap(), None);

    // 대기열 자리가 링크면 기록·정리·철회가 링크 너머를 건드리지 않는다.
    let state = State::new();
    let consent = state.consent();
    let decoy = State::new();
    let junk = decoy.path().join(format!("{}.json", "a".repeat(32)));
    fs::write(decoy.path().join("keep.txt"), b"x").unwrap();
    fs::write(&junk, b"junk").unwrap();
    symlink(decoy.path(), state.outbox()).unwrap();
    assert!(!record_with_consent(state.path(), event(Code::Panic), &consent).unwrap());
    let report = flush_at(&state, &consent, now_ms(), |_| {
        panic!("sent through a symlinked queue")
    });
    assert_eq!(
        (report.outcome, report.dropped, report.pending),
        (FlushOutcome::Empty, 0, 0)
    );
    assert!(decoy.path().join("keep.txt").exists() && junk.exists());
    disable(state.path()).unwrap();
    assert!(fs::symlink_metadata(state.outbox()).is_err());
    assert!(decoy.path().join("keep.txt").exists() && junk.exists());

    // 항목 하나가 링크여도 따라가지 않고 링크만 지운다.
    let consent = state.consent();
    let ids = queue(&state, &consent, 1);
    let entry = state.outbox().join(format!("{}.json", ids[0]));
    let outside = decoy.path().join("entry.json");
    fs::rename(&entry, &outside).unwrap();
    symlink(&outside, &entry).unwrap();
    let report = flush_at(&state, &consent, now_ms(), |_| {
        panic!("sent a symlinked entry")
    });
    assert_eq!((report.dropped, report.pending), (1, 0));
    assert!(outside.exists(), "링크 대상은 지우지 않는다");
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    #[test]
    fn ack_parser_never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..512)) {
        let ids = ["0".repeat(32)];
        if let Some(ack) = Ack::parse(&bytes, &ids) {
            proptest::prop_assert!(ack.acked().all(|id| id == ids[0]));
        }
    }

    #[test]
    fn source_locations_are_always_public_and_relative(file in "[ -~]{0,80}", line in 0u32..100) {
        if let Some(location) = source_location(&file, line) {
            proptest::prop_assert!(location.starts_with("crates/krill"));
            proptest::prop_assert!(!location.contains("..") && !location.contains("//"));
            proptest::prop_assert!(location.len() <= MAX_SOURCE_LOC);
        }
    }

    #[test]
    fn arbitrary_entry_bytes_never_reach_the_wire(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..600)) {
        let state = State::new();
        let consent = state.consent();
        fs::create_dir_all(state.outbox()).unwrap();
        fs::write(state.outbox().join(format!("{}.json", "d".repeat(32))), &bytes).unwrap();
        let report = flush_at(&state, &consent, now_ms(), |_| panic!("damaged entry was sent"));
        proptest::prop_assert_eq!((report.dropped, report.pending), (1, 0));
    }
}
