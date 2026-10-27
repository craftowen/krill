use super::tests::state_dir;
use super::*;
use std::fs;
fn env() -> Env {
    Env::default()
}
fn queued(dir: &Path) -> Batch {
    let mut batch = None;
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("telemetry/state.json")).unwrap()).unwrap();
    let now = value["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["ts_unix"].as_u64())
        .max()
        .unwrap();
    flush_with(dir, &env(), now, |body| {
        batch = Some(serde_json::from_slice(body).unwrap());
        Err(io::Error::other("offline"))
    })
    .unwrap();
    batch.unwrap()
}
#[test]
fn identity_persists_and_concurrent_creation_is_single() {
    let dir = state_dir();
    let jobs: Vec<_> = (0..8)
        .map(|_| {
            let dir = dir.clone();
            std::thread::spawn(move || record_open(&dir, &env(), 1000).unwrap())
        })
        .collect();
    for job in jobs {
        job.join().unwrap();
    }
    let identity = status_with(&dir, &env()).unwrap().install_id.unwrap();
    assert!(usage::valid_uuid(&identity));
    let batch = queued(&dir);
    assert_eq!(
        batch
            .events
            .iter()
            .filter(|e| e.name == UsageName::FirstOpen)
            .count(),
        1
    );
    assert_eq!(
        batch
            .events
            .iter()
            .filter(|e| e.name == UsageName::AppOpened)
            .count(),
        8
    );
    assert!(
        batch
            .events
            .iter()
            .all(|e| e.install_id == identity && usage::valid_uuid(&e.id))
    );
    disable(&dir).unwrap();
    enable(&dir).unwrap();
    record_open(&dir, &env(), 2000).unwrap();
    assert_eq!(
        status_with(&dir, &env()).unwrap().install_id.as_deref(),
        Some(identity.as_str())
    );
    let batch = queued(&dir);
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].name, UsageName::AppOpened);
    fs::remove_dir_all(dir).ok();
}
#[test]
fn active_day_requires_activity_and_rolls_over_utc() {
    let dir = state_dir();
    record_open(&dir, &env(), 86_399).unwrap();
    assert_eq!(status_with(&dir, &env()).unwrap().pending, 2);
    for time in [86_399, 86_399, 86_400, 86_401, 1, 172_800] {
        record_activity(&dir, &env(), time).unwrap();
    }
    let batch = queued(&dir);
    let days: Vec<_> = batch
        .events
        .iter()
        .filter(|e| e.name == UsageName::ActiveDay)
        .map(|e| e.ts_unix)
        .collect();
    assert_eq!(days, [86_399, 86_400, 172_800]);
    fs::remove_dir_all(dir).ok();
}
#[test]
fn disabled_and_env_veto_do_not_create_identity_or_events() {
    let dir = state_dir();
    for var in [DO_NOT_TRACK, KRILL_DISABLED, "CI"] {
        record_open(&dir, &Env::stub(&[(var, "1")]), 1000).unwrap();
        assert!(status_with(&dir, &env()).unwrap().install_id.is_none());
    }
    disable(&dir).unwrap();
    record_open(&dir, &env(), 1000).unwrap();
    assert_eq!(status_with(&dir, &env()).unwrap().pending, 0);
    assert_eq!(
        consent(Some(&dir), &env()),
        Consent::Disabled(Reason::UserDisabled)
    );
    fs::write(dir.join(CONSENT_FILE), "legacy").unwrap();
    assert!(!consent(Some(&dir), &env()).enabled());
    enable(&dir).unwrap();
    assert!(!consent(Some(&dir), &Env::stub(&[(DO_NOT_TRACK, "1")])).enabled());
    assert!(consent(Some(&dir), &env()).enabled());
    fs::remove_dir_all(dir).ok();
}
#[test]
fn retries_stable_ids_ack_exact_and_queue_bounded() {
    let dir = state_dir();
    for i in 0..200 {
        record_open(&dir, &env(), i).unwrap();
    }
    assert_eq!(status_with(&dir, &env()).unwrap().pending, MAX_PENDING);
    let first = queued(&dir);
    assert_eq!(first.events.len(), MAX_BATCH);
    assert!(serde_json::to_vec(&first).unwrap().len() <= MAX_REQUEST_BYTES);
    let now = 199;
    let mut calls = 0;
    assert_eq!(
        flush_with(&dir, &env(), now, |_| {
            calls += 1;
            unreachable!()
        })
        .unwrap()
        .outcome,
        Outcome::Backoff
    );
    assert_eq!(calls, 0);
    let report = flush_with(&dir, &env(), now + 40, |body| {
        assert_eq!(serde_json::from_slice::<Batch>(body).unwrap(), first);
        Ok((200, br#"{"accepted":49}"#.to_vec()))
    })
    .unwrap();
    assert_eq!(report.outcome, Outcome::Deferred);
    let report = flush_with(&dir, &env(), now + 200, |body| {
        assert_eq!(serde_json::from_slice::<Batch>(body).unwrap(), first);
        Ok((200, br#"{"accepted":50}"#.to_vec()))
    })
    .unwrap();
    assert_eq!(report.accepted, 50);
    assert_eq!(report.pending, MAX_PENDING - 50);
    assert_eq!(
        status_with(&dir, &env()).unwrap().last_success_unix,
        Some(now + 200)
    );
    fs::remove_dir_all(dir).ok();
}
#[test]
fn malformed_non_200_or_partial_ack_never_dequeues() {
    for (status, body) in [
        (201, r#"{"accepted":2}"#),
        (200, r#"{"accepted":1}"#),
        (200, r#"{"accepted":2,"extra":true}"#),
        (200, "not-json"),
        (429, r#"{"accepted":2}"#),
    ] {
        let dir = state_dir();
        record_open(&dir, &env(), 1000).unwrap();
        let report = flush_with(&dir, &env(), 1000, |_| {
            Ok((status, body.as_bytes().to_vec()))
        })
        .unwrap();
        assert_eq!(report.outcome, Outcome::Deferred);
        assert_eq!(report.pending, 2);
        fs::remove_dir_all(dir).ok();
    }
}
#[test]
fn disable_fences_inflight_response_and_clears_queue() {
    let dir = state_dir();
    record_open(&dir, &env(), 1000).unwrap();
    let original = status_with(&dir, &env()).unwrap().install_id;
    let report = flush_with(&dir, &env(), 1000, |_| {
        disable(&dir).unwrap();
        assert_eq!(status_with(&dir, &env()).unwrap().pending, 0);
        enable(&dir).unwrap();
        record_activity(&dir, &env(), 1001).unwrap();
        Ok((200, br#"{"accepted":2}"#.to_vec()))
    })
    .unwrap();
    assert_eq!(report.outcome, Outcome::Withdrawn);
    let status = status_with(&dir, &env()).unwrap();
    assert_eq!(status.pending, 1);
    assert_eq!(status.install_id, original);
    fs::remove_dir_all(dir).ok();
}
#[test]
fn closed_schema_fixture_and_validation() {
    let raw = r#"{"v":1,"events":[{"id":"12345678-1234-4234-8234-123456789abc","install_id":"12345678-1234-4234-8234-123456789abc","name":"active_day","ts_unix":1793000000,"app_version":"0.2.7","os":"macos","arch":"aarch64"}]}"#;
    let batch: Batch = serde_json::from_str(raw).unwrap();
    assert!(batch.valid());
    assert_eq!(serde_json::to_string(&batch).unwrap(), raw);
    let mut value = serde_json::to_value(&batch).unwrap();
    value["events"][0]["path"] = "/private".into();
    assert!(serde_json::from_value::<Batch>(value).is_err());
    for (key, value) in [
        ("id", "not-uuid"),
        ("install_id", "12345678-1234-1234-8234-123456789abc"),
        ("os", "darwin"),
        ("arch", "riscv64"),
        ("app_version", "/private/path"),
    ] {
        let mut json = serde_json::to_value(&batch).unwrap();
        json["events"][0][key] = value.into();
        assert!(!serde_json::from_value::<Batch>(json).unwrap().valid());
    }
}
proptest::proptest! {
    #[test]
    fn arbitrary_wire_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
        if let Ok(batch) = serde_json::from_slice::<Batch>(&bytes) { let _ = batch.valid(); }
        drop(serde_json::from_slice::<Ack>(&bytes));
    }
}

#[test]
fn enable_is_idempotent_and_keeps_pending_install_event() {
    let dir = state_dir();
    record_open(&dir, &env(), 1000).unwrap();
    let before = fs::read(dir.join("telemetry/state.json")).unwrap();
    enable(&dir).unwrap();
    enable(&dir).unwrap();
    assert_eq!(fs::read(dir.join("telemetry/state.json")).unwrap(), before);
    fs::remove_dir_all(dir).ok();
}
#[test]
fn invalid_old_and_future_disk_entries_are_pruned_without_poisoning_batch() {
    let dir = state_dir();
    let now = 1_793_000_000;
    record_open(&dir, &env(), now).unwrap();
    let file = dir.join("telemetry/state.json");
    let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let valid = json["events"][0].clone();
    let entries = json["events"].as_array_mut().unwrap();
    for (key, value) in [
        ("os", serde_json::json!("private")),
        ("app_version", serde_json::json!("broken")),
        ("ts_unix", serde_json::json!(now - 30 * 86400 - 1)),
        ("ts_unix", serde_json::json!(now + 301)),
        ("path", serde_json::json!("/private")),
        ("install_id", serde_json::json!(random_uuid().unwrap())),
    ] {
        let mut bad = valid.clone();
        bad[key] = value;
        entries.push(bad);
    }
    fs::write(&file, serde_json::to_vec(&json).unwrap()).unwrap();
    let report = flush_with(&dir, &env(), now, |body| {
        let batch: Batch = serde_json::from_slice(body).unwrap();
        assert!(batch.valid());
        assert_eq!(batch.events.len(), 2);
        Ok((200, br#"{"accepted":2}"#.to_vec()))
    })
    .unwrap();
    assert_eq!(report.outcome, Outcome::Sent);
    assert_eq!(report.pending, 0);
    fs::remove_dir_all(dir).ok();
}
#[test]
fn thirty_day_offline_retention_is_bounded() {
    let dir = state_dir();
    record_open(&dir, &env(), 1000).unwrap();
    let report = flush_with(&dir, &env(), 1000 + 30 * 86400 + 1, |_| unreachable!()).unwrap();
    assert_eq!(report.outcome, Outcome::Empty);
    assert_eq!(status_with(&dir, &env()).unwrap().pending, 0);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn generation_bound_input_rejects_same_timestamp_across_every_toggle() {
    let dir = state_dir();
    let original = activity_generation(&dir, &env()).unwrap().unwrap();
    let install = status_with(&dir, &env()).unwrap().install_id;
    // Every signal has exactly the same timestamp: policy epochs cannot rely on
    // seconds, queue existence, first-open state or the user's install UUID.
    let input_time = 86_399;
    for _ in 0..3 {
        disable(&dir).unwrap();
        enable(&dir).unwrap();
        assert!(!record_activity_for_generation(&dir, &env(), input_time, &original).unwrap());
        assert_eq!(status_with(&dir, &env()).unwrap().pending, 0);
    }
    let fresh = activity_generation(&dir, &env()).unwrap().unwrap();
    assert_ne!(fresh, original);
    assert!(record_activity_for_generation(&dir, &env(), input_time, &fresh).unwrap());
    assert_eq!(status_with(&dir, &env()).unwrap().install_id, install);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn disabled_or_missing_policy_never_uses_new_identity_as_input_fallback() {
    let dir = state_dir();
    disable(&dir).unwrap();
    assert!(activity_generation(&dir, &env()).unwrap().is_none());
    assert!(status_with(&dir, &env()).unwrap().install_id.is_none());
    enable(&dir).unwrap();
    let token = activity_generation(&dir, &env()).unwrap().unwrap();
    fs::remove_file(dir.join("telemetry/state.json")).unwrap();
    assert!(!record_activity_for_generation(&dir, &env(), 1000, &token).unwrap());
    assert!(status_with(&dir, &env()).unwrap().install_id.is_none());
    for var in [DO_NOT_TRACK, KRILL_DISABLED, "CI"] {
        assert!(
            activity_generation(&dir, &Env::stub(&[(var, "1")]))
                .unwrap()
                .is_none()
        );
        assert!(
            !record_activity_for_generation(&dir, &Env::stub(&[(var, "1")]), 1000, &token).unwrap()
        );
    }
    fs::remove_dir_all(dir).ok();
}

#[test]
fn enable_queue_write_failure_keeps_veto_and_old_events_cannot_revive() {
    let dir = state_dir();
    record_open(&dir, &env(), 1000).unwrap();
    let token = activity_generation(&dir, &env()).unwrap().unwrap();
    let install = status_with(&dir, &env()).unwrap().install_id;
    // Real deterministic IO failure: the atomic-write destination is a directory.
    // Disable publishes its veto before failing to clear the old queued events.
    fs::create_dir(dir.join("telemetry/state.tmp")).unwrap();
    assert!(disable(&dir).is_err());
    assert_eq!(status_with(&dir, &env()).unwrap().pending, 2);
    assert!(!consent(Some(&dir), &env()).enabled());
    assert!(enable(&dir).is_err());
    assert!(
        !consent(Some(&dir), &env()).enabled(),
        "failed enable must remain disabled"
    );
    assert!(!record_activity_for_generation(&dir, &env(), 1000, &token).unwrap());
    let report = flush_with(&dir, &env(), 1000, |_| {
        unreachable!("disabled queue must not upload")
    })
    .unwrap();
    assert_eq!(report.outcome, Outcome::Disabled);
    fs::remove_dir(dir.join("telemetry/state.tmp")).unwrap();
    enable(&dir).unwrap();
    assert!(consent(Some(&dir), &env()).enabled());
    assert_eq!(status_with(&dir, &env()).unwrap().pending, 0);
    assert_eq!(status_with(&dir, &env()).unwrap().install_id, install);
    assert!(!record_activity_for_generation(&dir, &env(), 1000, &token).unwrap());
    fs::remove_dir_all(dir).ok();
}

#[test]
fn initial_enable_write_failure_retains_disabled_marker_without_identity() {
    let dir = state_dir();
    disable(&dir).unwrap();
    fs::create_dir(dir.join("telemetry/state.tmp")).unwrap();
    assert!(enable(&dir).is_err());
    assert!(!consent(Some(&dir), &env()).enabled());
    assert!(status_with(&dir, &env()).unwrap().install_id.is_none());
    assert!(activity_generation(&dir, &env()).unwrap().is_none());
    fs::remove_dir_all(dir).ok();
}
