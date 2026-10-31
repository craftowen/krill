use super::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("krill-ids-{label}-{}-{n}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn recording_independent_reservations_survive_reopen_and_independent_allocators() {
    let fixture = Fixture::new("reopen");
    fs::write(fixture.0.join("history.toml"), b"recording = false\n").unwrap();
    let mut first = SessionIds::open(Some(&fixture.0), std::iter::empty());
    let mut second = SessionIds::open(Some(&fixture.0), std::iter::empty());
    assert_eq!(first.reserve_after(None, 0).unwrap(), SessionId(1));
    assert_eq!(second.reserve_after(None, 0).unwrap(), SessionId(2));
    drop(first);
    drop(second);
    let mut reopened = SessionIds::open(Some(&fixture.0), std::iter::empty());
    assert_eq!(
        reopened.reserve_after(Some(SessionId(99)), 0).unwrap(),
        SessionId(100)
    );
    assert!(!fixture.0.join("sessions").exists());
    assert!(!fixture.0.join("agent-launches").exists());
    fs::remove_file(fixture.0.join(FILE)).unwrap();
    assert!(
        reopened.reserve_after(None, 0).is_err(),
        "missing ledger must never reset an active allocator"
    );
}

#[test]
fn bootstrap_imports_kept_disk_and_creator_ids_before_retention_prunes_them() {
    let fixture = Fixture::new("bootstrap");
    fs::create_dir_all(fixture.0.join("sessions/40.noindex")).unwrap();
    fs::create_dir_all(fixture.0.join("agent-launches")).unwrap();
    fs::write(fixture.0.join("agent-launches/80.json"), b"fixture").unwrap();
    fs::write(fixture.0.join("session-creators"), b"[[90,120]]").unwrap();
    for (kept, expected) in [(0, 120), (150, 150)] {
        if fixture.0.join(FILE).exists() {
            fs::remove_file(fixture.0.join(FILE)).unwrap();
        }
        let ids = SessionIds::open(Some(&fixture.0), [SessionId(kept)].into_iter());
        assert!(ids.ready());
        assert_eq!(
            read_sequence(&fixture.0.join(FILE)).unwrap().last_reserved,
            expected
        );
    }
    let ids = SessionIds::open(Some(&fixture.0), [SessionId(150)].into_iter());
    assert!(ids.ready());
    assert_eq!(
        read_sequence(&fixture.0.join(FILE)).unwrap().last_reserved,
        150
    );
    drop(ids);
    fs::remove_dir_all(fixture.0.join("sessions")).unwrap();
    fs::remove_dir_all(fixture.0.join("agent-launches")).unwrap();
    fs::remove_file(fixture.0.join("session-creators")).unwrap();
    let mut reopened = SessionIds::open(Some(&fixture.0), std::iter::empty());
    assert_eq!(reopened.reserve_after(None, 0).unwrap(), SessionId(151));
}

#[test]
fn malformed_or_future_ledger_is_preserved_and_new_allocations_fail_closed() {
    let fixture = Fixture::new("invalid");
    let path = fixture.0.join(FILE);
    for bytes in [
        b"".to_vec(),
        b"broken".to_vec(),
        b"{\"version\":2,\"last_reserved\":80}".to_vec(),
        b"{\"version\":1}".to_vec(),
        vec![b' '; 4097],
    ] {
        fs::write(&path, &bytes).unwrap();
        let mut ids = SessionIds::open(Some(&fixture.0), [SessionId(99)].into_iter());
        assert!(!ids.ready());
        assert!(ids.reserve_after(None, 0).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    let mut no_root = SessionIds::open(None, std::iter::empty());
    assert!(no_root.reserve_after(None, 0).is_err());
}

#[test]
fn allocation_commit_failure_never_returns_id_or_rolls_back_a_completed_rename() {
    let fixture = Fixture::new("failure");
    let mut ids = SessionIds::open(Some(&fixture.0), std::iter::empty());
    let path = fixture.0.join(FILE);
    let before = fs::read(&path).unwrap();
    assert!(
        ids.reserve_using(None, 0, |_, _| Err(io::ErrorKind::StorageFull.into()))
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(
        ids.reserve_using(None, 0, |path, id| {
            persist(path, id)?;
            Err(io::ErrorKind::PermissionDenied.into())
        })
        .is_err()
    );
    let committed = read_sequence(&path).unwrap().last_reserved;
    assert!(committed > 0);
    let mut reopened = SessionIds::open(Some(&fixture.0), std::iter::empty());
    assert!(reopened.reserve_after(None, 0).unwrap().0 > committed);
    assert!(ids.reserve_after(None, u64::MAX).is_err());
}

#[test]
fn bootstrap_inventory_errors_preserve_existing_evidence_and_disable_creation() {
    let fixture = Fixture::new("inventory");
    fs::write(
        fixture.0.join("session-creators"),
        b"broken legacy metadata",
    )
    .unwrap();
    let mut ids = SessionIds::open(Some(&fixture.0), std::iter::empty());
    assert!(!ids.ready());
    assert!(ids.reserve_after(None, 0).is_err());
    assert!(!fixture.0.join(FILE).exists());
    fs::remove_file(fixture.0.join("session-creators")).unwrap();
    fs::write(fixture.0.join("sessions"), b"not a directory").unwrap();
    assert!(!SessionIds::open(Some(&fixture.0), std::iter::empty()).ready());
}

#[cfg(unix)]
#[test]
fn recording_disabled_create_and_resume_commit_before_spawn_and_kept_survives_bad_ledger() {
    use crate::holder::{Holder, Kept, link::Link};
    use crate::os::{AsFd as _, OwnedFd};
    use crate::registry::{Peer, Registry, Scope, lock};
    use krill_proto::control::{Create, Program, Reply};
    use krill_proto::holder::{FromHolder, ToHolder, VERSION};
    use krill_proto::os::net::UnixStream;
    use std::io::Write as _;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const ROLE: &str = "KRILL_TEST_ID_RESERVATIONS";
    if std::env::var_os(ROLE).is_none() {
        let fixture = Fixture::new("registry");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry::session_ids::tests::recording_disabled_create_and_resume_commit_before_spawn_and_kept_survives_bad_ledger", "--nocapture"])
            .env(ROLE, "1").env("KRILL_STATE_DIR", &fixture.0).env("HOME", fixture.0.join("home"))
            .env("DO_NOT_TRACK", "1").env_remove("CLAUDE_CONFIG_DIR").env_remove("CODEX_HOME")
            .env_remove("KRILL_ACCOUNT_PROVIDER").env_remove("KRILL_ACCOUNT_DIR")
            .status().unwrap();
        assert!(status.success());
        return;
    }

    let root = PathBuf::from(std::env::var_os("KRILL_STATE_DIR").unwrap());
    fs::create_dir_all(root.join("home")).unwrap();
    fs::write(root.join("history.toml"), b"recording = false\n").unwrap();
    let mut prior = None;
    for round in 0..2 {
        let (a, b) = UnixStream::pair().unwrap();
        let mut peer = Link::new(OwnedFd::from(b));
        for message in [
            FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            },
            FromHolder::Ready,
        ] {
            peer.send(&message.encode().unwrap(), &[]).unwrap();
        }
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(a)).unwrap();
        let reg = Arc::new(Registry::new(Some((holder, vec![])), Some(&root)));
        let r = Arc::clone(&reg);
        let reader = std::thread::spawn(move || r.holder.as_ref().unwrap().run(link, |_, _| {}));
        let state = root.clone();
        let mock = std::thread::spawn(move || {
            let packet = peer.recv().unwrap().unwrap();
            let ToHolder::Spawn { req, id, .. } = ToHolder::decode(packet.frame()).unwrap() else {
                panic!("expected Spawn")
            };
            assert!(
                read_sequence(&state.join(FILE)).unwrap().last_reserved >= id.0,
                "Spawn preceded durable reservation"
            );
            let (master, process) = UnixStream::pair().unwrap();
            peer.send(
                &FromHolder::Spawned {
                    req,
                    id,
                    pid: std::process::id(),
                }
                .encode()
                .unwrap(),
                &[master.as_fd()],
            )
            .unwrap();
            process
        });
        let c = Create {
            program: Program::Login,
            cwd: root.as_os_str().as_encoded_bytes().to_vec(),
            cols: 80,
            rows: 24,
            account: None,
        };
        let full = Peer {
            pid: None,
            scope: Scope::Full,
        };
        let reply = if round == 0 {
            reg.create(&full, c)
        } else {
            reg.resume(&full, prior.unwrap(), c)
        }
        .unwrap();
        let id = match reply {
            Reply::Created { session, .. } | Reply::Resumed { session, .. } => session,
            other => panic!("unexpected {other:?}"),
        };
        assert!(prior.is_none_or(|old: SessionId| id.0 > old.0));
        prior = Some(id);
        let process = mock.join().unwrap();
        reader.join().unwrap();
        assert!(!root.join("sessions").exists());
        assert!(!root.join("agent-launches").exists());
        drop(process);
        drop(reg);
    }
    fs::write(root.join(FILE), b"corrupt reservation").unwrap();
    let (a, b) = UnixStream::pair().unwrap();
    let peer = Link::new(OwnedFd::from(b));
    for message in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
            inbox: false,
        },
        FromHolder::Ready,
    ] {
        peer.send(&message.encode().unwrap(), &[]).unwrap();
    }
    let (holder, _link, _, _) = Holder::connect(OwnedFd::from(a)).unwrap();
    let (master, mut process) = UnixStream::pair().unwrap();
    let id = prior.unwrap();
    let reg = Registry::new(
        Some((
            holder,
            vec![Kept {
                id,
                pid: std::process::id(),
                status: None,
                master: OwnedFd::from(master),
            }],
        )),
        Some(&root),
    );
    assert!(reg.reserve_session_id(None).is_err());
    let tap = lock(&reg.state).sessions[&id].session.tap();
    process.write_all(b"still alive").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while tap.pulse().end < 11 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(fs::read(root.join(FILE)).unwrap(), b"corrupt reservation");
    lock(&reg.state).closing.insert(id);
    reg.on_exited(id, 0);
    assert!(
        lock(&reg.state).sessions.contains_key(&id),
        "keep the final holder inventory evidence until the ID floor is durable"
    );
    assert!(lock(&reg.state).closed.contains(&id));
    fs::remove_file(root.join(FILE)).unwrap();
    assert!(reg.reserve_session_id(None).unwrap().0 > id.0);
    reg.gc_due(
        Instant::now() + crate::registry::GC_INTERVAL,
        std::time::SystemTime::now(),
    );
    assert!(
        !lock(&reg.state).sessions.contains_key(&id),
        "retire deferred closed inventory after durable recovery"
    );
}

#[test]
fn failed_bootstrap_preserves_legacy_id_evidence_when_session_is_removed() {
    let fixture = Fixture::new("remove-evidence");
    fs::create_dir_all(fixture.0.join("agent-launches")).unwrap();
    let launch = fixture.0.join("agent-launches/80.json");
    fs::write(&launch, b"legacy metadata").unwrap();
    let creators = fixture.0.join("session-creators");
    fs::write(&creators, b"[[80,120]]").unwrap();
    fs::write(fixture.0.join(FILE), b"unreadable reservation").unwrap();
    let reg = crate::registry::Registry::new(None, Some(&fixture.0));
    reg.remove(SessionId(80));
    assert_eq!(fs::read(&launch).unwrap(), b"legacy metadata");
    assert_eq!(fs::read(&creators).unwrap(), b"[[80,120]]");
    drop(reg);
    fs::remove_file(fixture.0.join(FILE)).unwrap();
    let mut ids = SessionIds::open(Some(&fixture.0), std::iter::empty());
    assert_eq!(ids.reserve_after(None, 0).unwrap(), SessionId(121));
}

#[test]
fn bootstrap_imports_each_evidence_source_without_a_larger_kept_floor_hiding_it() {
    for (source, floor) in [
        ("sessions", 77),
        ("agent-launches", 92),
        ("parent", 109),
        ("child", 130),
        ("kept", 151),
    ] {
        let fixture = Fixture::new(source);
        match source {
            "sessions" => fs::create_dir_all(fixture.0.join("sessions/77.noindex")).unwrap(),
            "agent-launches" => {
                fs::create_dir_all(fixture.0.join("agent-launches")).unwrap();
                fs::write(fixture.0.join("agent-launches/92.json"), b"fixture").unwrap();
            }
            "parent" => fs::write(fixture.0.join("session-creators"), b"[[1,109]]").unwrap(),
            "child" => fs::write(fixture.0.join("session-creators"), b"[[130,1]]").unwrap(),
            _ => {}
        }
        let kept = (source == "kept").then_some(SessionId(floor));
        let mut ids = SessionIds::open(Some(&fixture.0), kept.into_iter());
        assert_eq!(
            ids.reserve_after(None, 0).unwrap(),
            SessionId(floor + 1),
            "source {source}"
        );
    }
}

#[test]
fn account_handoff_ids_remain_reserved_after_terminal_history_is_gone() {
    for (source, raw, floor) in [
        (
            "account-handoffs.json",
            r#"[{"from":220,"to":null,"account_id":"selected","error":"replacement unavailable"}]"#,
            220,
        ),
        (
            "account-handoffs.json",
            r#"[{"from":10,"to":427,"account_id":"selected","error":null}]"#,
            427,
        ),
        (
            "account-replacement.json",
            r#"{"from":529,"to":12,"account_id":"selected"}"#,
            529,
        ),
        (
            "account-replacements/7.json",
            r#"{"from":7,"to":601,"account_id":"selected"}"#,
            601,
        ),
    ] {
        let fixture = Fixture::new("handoff-evidence");
        let path = fixture.0.join(source);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, raw).unwrap();
        let mut ids = SessionIds::open(Some(&fixture.0), std::iter::empty());
        assert_eq!(
            ids.reserve_after(None, 0).unwrap(),
            SessionId(floor + 1),
            "must not attach an old account handoff to a new shell: {source}"
        );
        assert_eq!(fs::read_to_string(path).unwrap(), raw);
        // Retention may remove the last other reference after bootstrap.
        drop(ids);
        fs::remove_file(fixture.0.join(source)).unwrap();
        let mut reopened = SessionIds::open(Some(&fixture.0), std::iter::empty());
        assert_eq!(
            reopened.reserve_after(None, 0).unwrap(),
            SessionId(floor + 2)
        );
    }
}

#[test]
fn unreadable_account_handoff_inventory_never_resets_the_id_floor() {
    for source in [
        "account-handoffs.json",
        "account-replacement.json",
        "account-replacements/7.json",
    ] {
        let fixture = Fixture::new("bad-handoff-evidence");
        persist(&fixture.0.join(FILE), 20).unwrap();
        let before = fs::read(fixture.0.join(FILE)).unwrap();
        let path = fixture.0.join(source);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"broken handoff evidence").unwrap();
        let mut ids = SessionIds::open(Some(&fixture.0), std::iter::empty());
        assert!(!ids.ready(), "incomplete evidence: {source}");
        assert!(ids.reserve_after(None, 0).is_err());
        assert_eq!(fs::read(fixture.0.join(FILE)).unwrap(), before);
        assert_eq!(fs::read(&path).unwrap(), b"broken handoff evidence");
    }
}

#[cfg(unix)]
#[test]
fn nonregular_account_handoff_evidence_does_not_block_daemon_initialization() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new("nonregular-handoff-evidence");
    let path = fixture.0.join("account-handoffs.json");
    fs::create_dir(&path).unwrap();
    assert!(!SessionIds::open(Some(&fixture.0), std::iter::empty()).ready());
    fs::remove_dir(&path).unwrap();

    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    assert!(!SessionIds::open(Some(&fixture.0), std::iter::empty()).ready());
    fs::remove_file(&path).unwrap();

    let other = fixture.0.join("other.json");
    fs::write(&other, b"[]").unwrap();
    symlink(&other, &path).unwrap();
    assert!(!SessionIds::open(Some(&fixture.0), std::iter::empty()).ready());
    assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
    assert_eq!(fs::read(&other).unwrap(), b"[]");
    assert!(!fixture.0.join(FILE).exists());
}

#[test]
fn large_valid_account_history_does_not_disable_new_shells() {
    let fixture = Fixture::new("large-handoff-evidence");
    let raw = serde_json::to_vec(&serde_json::json!([{
        "from": 220,
        "to": 427,
        "account_id": "selected",
        "error": "x".repeat(1_048_576),
    }]))
    .unwrap();
    fs::write(fixture.0.join("account-handoffs.json"), &raw).unwrap();
    let mut ids = SessionIds::open(Some(&fixture.0), std::iter::empty());
    assert_eq!(ids.reserve_after(None, 0).unwrap(), SessionId(428));
    assert_eq!(
        fs::read(fixture.0.join("account-handoffs.json")).unwrap(),
        raw
    );
}
