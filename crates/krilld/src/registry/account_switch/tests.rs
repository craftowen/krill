use super::*;
use std::fs;
use std::io::Write as _;
use std::sync::Arc;

use crate::agent::control::LaunchAccount;
use crate::holder::{Holder, Kept, link::Link};
use crate::os::{AsFd as _, OwnedFd};
use krill_proto::agent::Permission;
use krill_proto::holder::{FromHolder, ToHolder, VERSION};
use krill_proto::os::net::UnixStream;

#[test]
fn switch_resumes_same_conversation_with_selected_credentials_before_retiring_old() {
    for (fail, cancel, unconfirmed, close_fail, process_bound, late_hook, streaming) in [
        (false, false, false, false, false, false, false),
        (true, false, false, false, false, false, false),
        (false, true, false, false, false, false, false),
        (false, false, true, false, false, false, false),
        (false, false, false, true, false, false, false),
        (false, false, false, false, true, false, false),
        (false, false, false, false, false, true, false),
        (false, false, false, false, false, false, true),
    ] {
        let root = Path::new("/tmp").join(format!(
            "kas-{}-{fail}-{cancel}-{unconfirmed}-{close_fail}-{process_bound}-{late_hook}-{streaming}",
            std::process::id()
        ));
        fs::remove_dir_all(&root).ok();
        let state = root.join("state");
        let home = root.join("home");
        let from = root.join("from");
        let to = root.join("to");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(from.join("sessions/2026/10/20")).unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(to.join("auth.json"), "selected fixture credentials").unwrap();
        let snap = accounts::add_account(
            &home,
            Some(&state),
            ProviderKind::Codex,
            "Selected",
            None,
            to.to_str().unwrap(),
        )
        .unwrap();
        let target = snap
            .accounts
            .iter()
            .find(|a| a.label == "Selected")
            .unwrap()
            .id
            .clone();
        let sid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let transcript = from.join(format!("sessions/2026/10/20/rollout-date-{sid}.jsonl"));
        fs::write(
            &transcript,
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{sid}\"}}}}\n"),
        )
        .unwrap();
        let source_process = process_bound.then(|| FileHolder::new(&transcript));
        let (daemon_end, holder_end) = UnixStream::pair().unwrap();
        let mut holder_link = Link::new(OwnedFd::from(holder_end));
        for message in [
            FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            },
            FromHolder::Ready,
        ] {
            holder_link.send(&message.encode().unwrap(), &[]).unwrap();
        }
        let (master, _old_process) = UnixStream::pair().unwrap();
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let reg = Arc::new(Registry::new(
            Some((
                holder,
                vec![Kept {
                    id: SessionId(7),
                    pid: source_process.as_ref().map_or(700, |p| p.0.id()),
                    status: None,
                    master: OwnedFd::from(master),
                }],
            )),
            Some(&state),
        ));
        reg.agents
            .remember_session(
                SessionId(7),
                &AgentLaunch {
                    profile: "codex".into(),
                    permission: Permission::Yolo,
                },
                (!process_bound && !late_hook).then(|| sid.into()),
                root.as_os_str().as_encoded_bytes().to_vec(),
                Some(LaunchAccount {
                    id: "codex-old".into(),
                    env_key: "CODEX_HOME".into(),
                    dir: from.display().to_string(),
                }),
            )
            .unwrap();
        let reader = Arc::clone(&reg);
        let reader_worker =
            std::thread::spawn(move || reader.holder.as_ref().unwrap().run(link, |_, _| {}));
        let expected_home = to.clone();
        let expected_transcript = to
            .join("sessions/2026/10/20")
            .join(transcript.file_name().unwrap());
        let reselect = cancel.then(|| Arc::clone(&reg));
        let hook_reg = Arc::clone(&reg);
        let stop_output = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let output_stop = Arc::clone(&stop_output);
        let mock = std::thread::spawn(move || {
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Spawn {
                req, id, argv, env, ..
            } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!("expected replacement spawn");
            };
            assert!(argv.iter().any(|arg| arg == b"resume"));
            assert!(argv.iter().any(|arg| arg == sid.as_bytes()));
            assert!(env.iter().any(|(key, value)| key == b"CODEX_HOME"
                && value == expected_home.as_os_str().as_encoded_bytes()));
            if let Some(reg) = reselect {
                reg.queue_account_switch(ProviderKind::Codex, "codex-old");
            }
            if fail {
                holder_link
                    .send(
                        &FromHolder::Failed {
                            req,
                            errno: libc::EACCES,
                        }
                        .encode()
                        .unwrap(),
                        &[],
                    )
                    .unwrap();
                return None;
            }
            let replacement_process = process_bound.then(|| FileHolder::new(&expected_transcript));
            let pid = replacement_process.as_ref().map_or(800, |p| p.0.id());
            let (master, mut process) = UnixStream::pair().unwrap();
            holder_link
                .send(
                    &FromHolder::Spawned { req, id, pid }.encode().unwrap(),
                    &[master.as_fd()],
                )
                .unwrap();
            process.write_all(b"resumed selected account\r\n").unwrap();
            let output_worker = streaming.then(|| {
                let mut stream = process.try_clone().unwrap();
                std::thread::spawn(move || {
                    while !output_stop.load(std::sync::atomic::Ordering::Acquire) {
                        if stream.write_all(b"continuing output\r\n").is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                })
            });
            if !unconfirmed && !process_bound {
                hook_reg.agents.record_hook_session(
                    id,
                    format!("{{\"hook_event_name\":\"SessionStart\",\"session_id\":\"{sid}\"}}")
                        .as_bytes(),
                );
            }
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Terminate { req, id: old } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!("expected old process retirement after new output");
            };
            assert_eq!(
                old,
                if cancel || unconfirmed {
                    id
                } else {
                    SessionId(7)
                }
            );
            let mut snapshot = AccountsSnapshot::default();
            hook_reg.account_switch_snapshot(&mut snapshot);
            assert!(
                snapshot.details.handoffs.iter().all(|h| h.to.is_none()),
                "clients must not rebind before retirement succeeds"
            );
            let reply = if close_fail {
                FromHolder::Failed {
                    req,
                    errno: libc::EPERM,
                }
            } else {
                FromHolder::Done { req }
            };
            holder_link.send(&reply.encode().unwrap(), &[]).unwrap();
            if !close_fail && !cancel && !unconfirmed {
                // Done only accepts teardown. Completion must remain hidden until Exited.
                std::thread::sleep(Duration::from_millis(50));
                let mut snapshot = AccountsSnapshot::default();
                hook_reg.account_switch_snapshot(&mut snapshot);
                assert!(snapshot.details.handoffs.iter().all(|h| h.to.is_none()));
                lock(&hook_reg.state).exited.insert(old, 0);
            }
            if close_fail {
                let packet = holder_link.recv().unwrap().unwrap();
                let ToHolder::Terminate { req, id: cleanup } =
                    ToHolder::decode(packet.frame()).unwrap()
                else {
                    panic!("expected failed replacement cleanup");
                };
                assert_eq!(cleanup, id);
                holder_link
                    .send(&FromHolder::Done { req }.encode().unwrap(), &[])
                    .unwrap();
            }
            // Keep the new PTY open until the parent has committed its handoff.
            Some((id, process, replacement_process, output_worker))
        });
        reg.queue_account_switch(ProviderKind::Codex, &target);
        reg.queue_account_switch(ProviderKind::Codex, "codex-old");
        assert!(
            lock(&reg.account_handoffs).is_empty(),
            "reselecting the running account cancels a queued handoff"
        );
        reg.queue_account_switch(ProviderKind::Codex, &target);
        let hook_registry = Arc::clone(&reg);
        let late_binding = late_hook.then(|| {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                hook_registry.agents.record_hook_session(
                    SessionId(7),
                    format!("{{\"hook_event_name\":\"SessionStart\",\"session_id\":\"{sid}\"}}")
                        .as_bytes(),
                );
            })
        });
        reg.process_account_work_in(&home);
        stop_output.store(true, std::sync::atomic::Ordering::Release);
        if let Some(binding) = late_binding {
            binding.join().unwrap();
        }
        let saved = load(Some(&state));
        if cancel {
            assert!(
                saved.is_empty(),
                "in-flight cancellation must not rebind the original"
            );
            assert!(!lock(&reg.state).closing.contains(&SessionId(7)));
        } else if fail || unconfirmed || close_fail {
            let result = saved.get(&SessionId(7)).unwrap();
            assert!(result.to.is_none());
            assert!(result.error.is_some());
            assert!(!lock(&reg.state).closing.contains(&SessionId(7)));
        } else {
            let result = saved.get(&SessionId(7)).unwrap();
            let new = result.to.expect("durable replacement");
            assert_eq!(
                reg.agents.stored(new).unwrap().unwrap().account.unwrap().id,
                target
            );
            assert!(lock(&reg.state).closing.contains(&SessionId(7)));
        }
        assert!(transcript.is_file());
        assert_eq!(
            fs::read_to_string(to.join("auth.json")).unwrap(),
            "selected fixture credentials"
        );
        if let Some((_, process, replacement, output_worker)) = mock.join().unwrap() {
            if let Some(worker) = output_worker {
                worker.join().unwrap();
            }
            drop((process, replacement));
        }
        reader_worker.join().unwrap();
        drop(source_process);
        drop(reg);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn switch_without_verified_identity_or_transcript_preserves_original_session() {
    use std::io::Read as _;
    for case in [
        "absent",
        "single-cwd-match",
        "ambiguous-cwd",
        "bound-missing",
        "bound-mismatched",
        "duplicate-id",
        "process-mismatch",
    ] {
        let root = Path::new("/tmp").join(format!("kas-preserve-{}-{case}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let state = root.join("state");
        let home = root.join("home");
        let from = root.join("from");
        let to = root.join("to");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(from.join("sessions/2026/10/20")).unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(to.join("auth.json"), "selected fixture credentials").unwrap();
        let snapshot = accounts::add_account(
            &home,
            Some(&state),
            ProviderKind::Codex,
            "Selected",
            None,
            to.to_str().unwrap(),
        )
        .unwrap();
        let target = snapshot
            .accounts
            .iter()
            .find(|a| a.label == "Selected")
            .unwrap()
            .id
            .clone();
        let sid = "bound-conversation";
        let cwd = root.as_os_str().as_encoded_bytes().to_vec();
        let write_transcript = |name: &str, identity: &str| {
            let path = from.join(format!("sessions/2026/10/20/rollout-{name}.jsonl"));
            fs::write(path, format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{identity}\",\"cwd\":{:?}}}}}\n", root.to_str().unwrap())).unwrap();
        };
        if matches!(case, "single-cwd-match" | "ambiguous-cwd") {
            write_transcript("unrelated-one", "unrelated-one");
        }
        if case == "ambiguous-cwd" {
            write_transcript("unrelated-two", "unrelated-two");
        }
        if case == "bound-mismatched" {
            write_transcript(sid, "unrelated-conversation");
        }
        if case == "process-mismatch" {
            write_transcript(sid, sid);
            write_transcript("wrong-conversation", "wrong-conversation");
        }
        let source_process = (case == "process-mismatch").then(|| {
            FileHolder::new(&from.join(format!("sessions/2026/10/20/rollout-{sid}.jsonl")))
        });
        if case == "duplicate-id" {
            write_transcript(&format!("one-{sid}"), sid);
            write_transcript(&format!("two-{sid}"), sid);
        }
        let (daemon_end, holder_end) = UnixStream::pair().unwrap();
        let mut observer = holder_end.try_clone().unwrap();
        observer
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let holder_link = Link::new(OwnedFd::from(holder_end));
        for message in [
            FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            },
            FromHolder::Ready,
        ] {
            holder_link.send(&message.encode().unwrap(), &[]).unwrap();
        }
        let (master, mut old_process) = UnixStream::pair().unwrap();
        let (holder, _link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let reg = Arc::new(Registry::new(
            Some((
                holder,
                vec![Kept {
                    id: SessionId(9),
                    pid: source_process.as_ref().map_or(900, |p| p.0.id()),
                    status: None,
                    master: OwnedFd::from(master),
                }],
            )),
            Some(&state),
        ));
        reg.agents
            .remember_session(
                SessionId(9),
                &AgentLaunch {
                    profile: "codex".into(),
                    permission: Permission::Yolo,
                },
                case.starts_with("bound-")
                    .then(|| sid.to_owned())
                    .or_else(|| (case == "duplicate-id").then(|| sid.to_owned()))
                    .or_else(|| {
                        (case == "process-mismatch").then(|| "wrong-conversation".to_owned())
                    }),
                cwd,
                Some(LaunchAccount {
                    id: "codex-old".into(),
                    env_key: "CODEX_HOME".into(),
                    dir: from.display().to_string(),
                }),
            )
            .unwrap();
        let launch_path = state.join("agent-launches/9.json");
        let original_launch = fs::read(&launch_path).unwrap();
        if matches!(case, "single-cwd-match" | "ambiguous-cwd") {
            // Reading chat history can use a project fallback for display, but must
            // never turn that guess into permission to replace the live agent.
            reg.chat_poll(SessionId(9), 0);
            assert_eq!(fs::read(&launch_path).unwrap(), original_launch, "{case}");
        }
        old_process
            .write_all(b"original history before handoff\r\n")
            .unwrap();
        reg.queue_account_switch(ProviderKind::Codex, &target);
        reg.process_account_work_in(&home);
        let saved = load(Some(&state));
        let handoff = saved.get(&SessionId(9)).unwrap();
        assert!(handoff.to.is_none(), "{case}");
        let error = handoff.error.as_deref().unwrap();
        assert!(
            error.contains("original session preserved"),
            "{case}: {error}"
        );
        assert!(!lock(&reg.state).closing.contains(&SessionId(9)));
        assert!(!lock(&reg.state).exited.contains_key(&SessionId(9)));
        assert_eq!(fs::read(&launch_path).unwrap(), original_launch, "{case}");
        assert!(!state.join(PREPARED_FILE).exists());
        assert_eq!(
            fs::read_dir(state.join("agent-launches")).unwrap().count(),
            1
        );
        let error = observer.read(&mut [0]).unwrap_err();
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ),
            "no Spawn or Terminate on failure: {case}"
        );
        old_process
            .write_all(b"original process still running\r\n")
            .unwrap();
        let tap = lock(&reg.state)
            .sessions
            .get(&SessionId(9))
            .unwrap()
            .session
            .tap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while tap.pulse().end < 65 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!tap.pulse().closed);
        assert!(
            tap.pulse().end >= 65,
            "old PTY continues capturing history: {case}"
        );
        drop(reg);
        drop(source_process);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn completion_snapshot_waits_for_durable_handoff_and_failed_writes_restore_pending() {
    use std::sync::mpsc;
    let root = Path::new("/tmp").join(format!("kas-durable-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();
    let reg = Arc::new(Registry::new(None, Some(&root)));
    let pending = SessionHandoff {
        from: SessionId(7),
        to: None,
        account_id: "fixture-account".into(),
        error: None,
    };
    lock(&reg.account_handoffs).insert(pending.from, pending.clone());
    let completed = SessionHandoff {
        to: Some(SessionId(8)),
        ..pending.clone()
    };
    let (entered, writing) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let writer_reg = Arc::clone(&reg);
    let writer = std::thread::spawn(move || {
        writer_reg
            .commit_account_handoff(completed, |handoffs| {
                entered.send(()).unwrap();
                proceed.recv().unwrap();
                writer_reg.write_account_handoffs(handoffs)
            })
            .unwrap();
    });
    writing.recv_timeout(Duration::from_secs(3)).unwrap();
    let (started, reading) = mpsc::channel();
    let (done, result) = mpsc::channel();
    let reader_reg = Arc::clone(&reg);
    let reader = std::thread::spawn(move || {
        started.send(()).unwrap();
        let mut snapshot = AccountsSnapshot::default();
        reader_reg.account_switch_snapshot(&mut snapshot);
        done.send(snapshot.details.handoffs).unwrap();
    });
    reading.recv_timeout(Duration::from_secs(3)).unwrap();
    let before_commit = result.recv_timeout(Duration::from_millis(50));
    release.send(()).unwrap();
    writer.join().unwrap();
    reader.join().unwrap();
    assert!(
        matches!(before_commit, Err(mpsc::RecvTimeoutError::Timeout)),
        "completion must remain hidden during the write"
    );
    let published = result.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(published[0].to, Some(SessionId(8)));
    assert_eq!(load(Some(&root))[&SessionId(7)].to, Some(SessionId(8)));
    lock(&reg.account_handoffs).insert(pending.from, pending.clone());
    assert!(
        reg.commit_account_handoff(
            SessionHandoff {
                to: Some(SessionId(9)),
                ..pending
            },
            |_| Err(std::io::ErrorKind::PermissionDenied.into()),
        )
        .is_err()
    );
    assert_eq!(lock(&reg.account_handoffs)[&SessionId(7)].to, None);
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[expect(
    clippy::unwrap_used,
    reason = "test fixture setup requires valid local sockets and files"
)]
fn kept_fixture(root: &Path, id: SessionId) -> (Arc<Registry>, UnixStream) {
    fs::create_dir_all(root).unwrap();
    let (daemon_end, holder_end) = UnixStream::pair().unwrap();
    let link = Link::new(OwnedFd::from(holder_end));
    for message in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
            inbox: false,
        },
        FromHolder::Ready,
    ] {
        link.send(&message.encode().unwrap(), &[]).unwrap();
    }
    let (master, process) = UnixStream::pair().unwrap();
    let (holder, _reader, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let reg = Arc::new(Registry::new(
        Some((
            holder,
            vec![Kept {
                id,
                pid: 800,
                status: None,
                master: OwnedFd::from(master),
            }],
        )),
        Some(root),
    ));
    (reg, process)
}

#[test]
fn recovery_keeps_only_remaining_unconfirmed_replacement_without_publishing_completion() {
    for per_session in [false, true] {
        let root = Path::new("/tmp").join(format!("kas-only-{}-{per_session}", std::process::id()));
        let journal = if per_session {
            root.join("account-replacements/8.json")
        } else {
            root.join(PREPARED_FILE)
        };
        fs::remove_dir_all(&root).ok();
        let (reg, _process) = kept_fixture(&root, SessionId(8));
        let launch = AgentLaunch {
            profile: "codex".into(),
            permission: Permission::Yolo,
        };
        for session in [SessionId(7), SessionId(8)] {
            reg.agents
                .remember_session(
                    session,
                    &launch,
                    Some("expected".into()),
                    Vec::new(),
                    Some(LaunchAccount {
                        id: "selected".into(),
                        env_key: "CODEX_HOME".into(),
                        dir: root.display().to_string(),
                    }),
                )
                .unwrap();
        }
        fs::create_dir_all(journal.parent().unwrap()).unwrap();
        fs::write(
            &journal,
            serde_json::to_vec(&Prepared {
                from: SessionId(7),
                to: SessionId(8),
                account_id: "selected".into(),
            })
            .unwrap(),
        )
        .unwrap();
        reg.recover_prepared_account();
        assert!(!journal.exists());
        let saved = load(Some(&root));
        let handoff = saved.get(&SessionId(7)).unwrap();
        assert!(handoff.to.is_none());
        assert!(handoff.error.as_deref().unwrap().contains("retained"));
        assert!(!lock(&reg.state).closing.contains(&SessionId(8)));
        let mut snapshot = AccountsSnapshot::default();
        reg.account_switch_snapshot(&mut snapshot);
        assert!(snapshot.details.handoffs.iter().all(|h| h.to.is_none()));
        // The same safety rule also applies if the original exits before a live attempt finishes.
        {
            let _replacement = Replacement {
                registry: &reg,
                original: SessionId(7),
                session: SessionId(8),
                committed: false,
            };
        }
        assert!(!lock(&reg.state).closing.contains(&SessionId(8)));
        drop(reg);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn explicit_cancel_requires_a_failed_live_original_and_preserves_untracked_account() {
    use krill_proto::roles::Role;
    use krill_proto::usage::{FEAT_ACCOUNT_CANCEL, UsageReply, UsageRequest};
    let root = Path::new("/tmp").join(format!("kas-cancel-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let session = SessionId(7);
    let (reg, _process) = kept_fixture(&root, session);
    // No recorded account is required: the explicit user choice names the original PTY.
    let handoff = SessionHandoff {
        from: session,
        to: None,
        account_id: "selected".into(),
        error: None,
    };
    lock(&reg.account_handoffs).insert(session, handoff.clone());
    assert_eq!(
        reg.cancel_failed_account_switch(session),
        Err(Failure::BadRequest)
    );
    lock(&reg.account_handoffs).get_mut(&session).unwrap().error = Some("fixture failed".into());
    let peer = Peer {
        pid: None,
        scope: Scope::Full,
    };
    let request = UsageRequest::CancelAccountSwitch { session };
    assert_eq!(
        reg.on_usage_negotiated(
            &peer,
            Role::Observer,
            request.clone(),
            krill_proto::ipc::FEATURES
        ),
        UsageReply::Failed(Failure::Denied)
    );
    assert_eq!(
        reg.on_usage_negotiated(
            &peer,
            Role::Interactive,
            request,
            krill_proto::ipc::FEATURES & !FEAT_ACCOUNT_CANCEL
        ),
        UsageReply::Failed(Failure::BadRequest)
    );
    reg.cancel_failed_account_switch(session).unwrap();
    assert!(load(Some(&root)).is_empty());
    assert!(lock(&reg.account_handoffs).is_empty());
    assert!(!lock(&reg.state).closing.contains(&session));
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn antigravity_switch_bridges_brain_and_conversations_and_confirms_without_session_start_hook() {
    for confirmed in [true, false] {
        let root = Path::new("/tmp").join(format!("kas-agy-{}-{confirmed}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let state = root.join("state");
        let home = root.join("home");
        let from = root.join("from");
        let to = root.join("to");
        fs::create_dir_all(&state).unwrap();
        let sid = "8f870a24-4055-4bdb-8c26-bd8d72c64bc3";
        let from_agy = from.join(".gemini/antigravity-cli");
        let to_agy = to.join(".gemini/antigravity-cli");
        fs::create_dir_all(
            from_agy
                .join("brain")
                .join(sid)
                .join(".system_generated/logs"),
        )
        .unwrap();
        fs::create_dir_all(from_agy.join("conversations")).unwrap();
        fs::create_dir_all(from_agy.join("annotations")).unwrap();
        fs::create_dir_all(&to_agy).unwrap();
        let tp = from_agy
            .join("brain")
            .join(sid)
            .join(".system_generated/logs/transcript.jsonl");
        fs::write(&tp, "{\"role\":\"user\",\"content\":\"hello\"}\n").unwrap();
        fs::write(
            from_agy.join("conversations").join(format!("{sid}.db")),
            b"SQLite format 3\0fixture-conversation",
        )
        .unwrap();
        fs::write(
            from_agy.join("annotations").join(format!("{sid}.pbtxt")),
            b"title: \"agy switch\"",
        )
        .unwrap();
        fs::write(
            to_agy.join("antigravity-oauth-token"),
            r#"{"access_token":"tok","expiry_date":4102444800000,"email":"agy2@example.com"}"#,
        )
        .unwrap();

        let snap = accounts::add_account(
            &home,
            Some(&state),
            ProviderKind::Antigravity,
            "Antigravity #2",
            Some("agy2@example.com".into()),
            to.to_str().unwrap(),
        )
        .unwrap();
        let target = snap
            .accounts
            .iter()
            .find(|a| a.label == "Antigravity #2")
            .unwrap()
            .id
            .clone();

        let (daemon_end, holder_end) = UnixStream::pair().unwrap();
        let mut holder_link = Link::new(OwnedFd::from(holder_end));
        for message in [
            FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            },
            FromHolder::Ready,
        ] {
            holder_link.send(&message.encode().unwrap(), &[]).unwrap();
        }
        let (master, _old_process) = UnixStream::pair().unwrap();
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let reg = Arc::new(Registry::new(
            Some((
                holder,
                vec![Kept {
                    id: SessionId(11),
                    pid: 1100,
                    status: None,
                    master: OwnedFd::from(master),
                }],
            )),
            Some(&state),
        ));
        // Simulate an agy session where session_id was not bound at launch, only discovered via transcript_path
        reg.agents
            .remember_session(
                SessionId(11),
                &AgentLaunch {
                    profile: "agy".into(),
                    permission: Permission::Yolo,
                },
                None,
                root.as_os_str().as_encoded_bytes().to_vec(),
                Some(LaunchAccount {
                    id: "agy-old".into(),
                    env_key: "ANTIGRAVITY_CLI_HOME".into(),
                    dir: from.display().to_string(),
                }),
            )
            .unwrap();
        reg.agents
            .bind_discovered_session(SessionId(11), sid, Some(&tp));

        let reader = Arc::clone(&reg);
        let reader_worker =
            std::thread::spawn(move || reader.holder.as_ref().unwrap().run(link, |_, _| {}));
        let hook_reg = Arc::clone(&reg);
        let expected_agy = to_agy.clone();
        let mock = std::thread::spawn(move || {
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Spawn {
                req, id, argv, env, ..
            } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!("expected replacement spawn");
            };
            assert!(argv.iter().any(|arg| arg == b"--conversation"));
            assert!(argv.iter().any(|arg| arg == sid.as_bytes()));
            assert!(!argv.iter().any(|arg| arg.starts_with(b"--gemini_dir=")));
            assert!(
                env.iter()
                    .any(|(key, val)| key == b"KRILL_ACCOUNT_DIR" && !val.is_empty())
            );
            let presence = expected_agy.join("presence").join(format!("{sid}.lock"));
            fs::create_dir_all(presence.parent().unwrap()).unwrap();
            fs::write(&presence, b"").unwrap();
            let replacement_process = confirmed.then(|| FileHolder::new(&presence));
            let pid = replacement_process.as_ref().map_or(u32::MAX, |p| p.0.id());
            let (master, mut process) = UnixStream::pair().unwrap();
            holder_link
                .send(
                    &FromHolder::Spawned { req, id, pid }.encode().unwrap(),
                    &[master.as_fd()],
                )
                .unwrap();
            // Notice: NO SessionStart hook is sent for Antigravity!
            process.write_all(b"Antigravity resumed prompt > ").unwrap();
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Terminate { req, id: old } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!("expected old process retirement after agy output settles");
            };
            assert_eq!(old, if confirmed { SessionId(11) } else { id });
            holder_link
                .send(&FromHolder::Done { req }.encode().unwrap(), &[])
                .unwrap();
            lock(&hook_reg.state).exited.insert(old, 0);
            Some((id, process, replacement_process))
        });

        let wal = from_agy.join("conversations").join(format!("{sid}.db-wal"));
        fs::write(&wal, b"active writer").unwrap();
        reg.queue_account_switch(ProviderKind::Antigravity, &target);
        reg.process_account_work_in(&home);
        let pending = load(Some(&state));
        let waiting = pending.get(&SessionId(11)).unwrap();
        assert!(waiting.to.is_none() && waiting.error.is_none());
        assert!(!lock(&reg.state).closing.contains(&SessionId(11)));
        // It is scheduled, not a spin loop or a visible permanent failure.
        reg.process_account_work_in(&home);
        assert!(
            lock(&reg.account_work.pending).contains_key(&SwitchScope::Session(
                SessionId(11),
                ProviderKind::Antigravity
            ))
        );
        fs::remove_file(wal).unwrap();
        lock(&reg.account_work.pending)
            .get_mut(&SwitchScope::Session(
                SessionId(11),
                ProviderKind::Antigravity,
            ))
            .unwrap()
            .not_before = None;
        reg.process_account_work_in(&home);
        let saved = load(Some(&state));
        let result = saved.get(&SessionId(11)).unwrap();
        if confirmed {
            assert!(
                result.error.is_none(),
                "unexpected error: {:?}",
                result.error
            );
            let new = result.to.expect("durable replacement for agy");
            assert_eq!(
                reg.agents.stored(new).unwrap().unwrap().account.unwrap().id,
                target
            );
        } else {
            assert!(result.to.is_none());
            assert!(result.error.as_deref().unwrap().contains("timed out"));
            assert!(!lock(&reg.state).closing.contains(&SessionId(11)));
            assert!(!lock(&reg.state).exited.contains_key(&SessionId(11)));
        }
        assert!(
            to_agy
                .join("brain")
                .join(sid)
                .join(".system_generated/logs/transcript.jsonl")
                .is_file()
        );
        assert_eq!(
            fs::read(to_agy.join("conversations").join(format!("{sid}.db"))).unwrap(),
            b"SQLite format 3\0fixture-conversation"
        );
        assert_eq!(
            fs::read(to_agy.join("annotations").join(format!("{sid}.pbtxt"))).unwrap(),
            b"title: \"agy switch\""
        );
        assert_eq!(
            fs::read_to_string(home.join(".gemini/antigravity-cli/antigravity-oauth-token"))
                .unwrap(),
            r#"{"access_token":"tok","expiry_date":4102444800000,"email":"agy2@example.com"}"#
        );
        assert_eq!(
            fs::read(
                home.join(".gemini/antigravity-cli/conversations")
                    .join(format!("{sid}.db"))
            )
            .unwrap(),
            b"SQLite format 3\0fixture-conversation"
        );

        drop(mock.join().unwrap());
        reader_worker.join().unwrap();
        drop(reg);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn switch_auto_continues_working_or_rate_limited_session_after_retirement() {
    use krill_proto::hook::{EventId, HookEvent, Input};
    use std::io::Read as _;

    for rate_limited in [false, true] {
        let root = Path::new("/tmp").join(format!(
            "kas-autocontinue-{}-{rate_limited}",
            std::process::id()
        ));
        fs::remove_dir_all(&root).ok();
        let state = root.join("state");
        let home = root.join("home");
        let from = root.join("from");
        let to = root.join("to");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(from.join("sessions/2026/10/25")).unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(to.join("auth.json"), "selected fixture credentials").unwrap();
        let snap = accounts::add_account(
            &home,
            Some(&state),
            ProviderKind::Codex,
            "Selected",
            None,
            to.to_str().unwrap(),
        )
        .unwrap();
        let target = snap
            .accounts
            .iter()
            .find(|a| a.label == "Selected")
            .unwrap()
            .id
            .clone();
        let sid = "bbbbbbbb-cccc-dddd-eeee-ffffffffffff";
        let transcript = from.join(format!("sessions/2026/10/25/rollout-date-{sid}.jsonl"));
        fs::write(
            &transcript,
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{sid}\"}}}}\n"),
        )
        .unwrap();
        let (daemon_end, holder_end) = UnixStream::pair().unwrap();
        let mut holder_link = Link::new(OwnedFd::from(holder_end));
        for message in [
            FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            },
            FromHolder::Ready,
        ] {
            holder_link.send(&message.encode().unwrap(), &[]).unwrap();
        }
        let (master, _old_process) = UnixStream::pair().unwrap();
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let reg = Arc::new(Registry::new(
            Some((
                holder,
                vec![Kept {
                    id: SessionId(21),
                    pid: 721,
                    status: None,
                    master: OwnedFd::from(master),
                }],
            )),
            Some(&state),
        ));
        reg.agents
            .remember_session(
                SessionId(21),
                &AgentLaunch {
                    profile: "codex".into(),
                    permission: Permission::Yolo,
                },
                Some(sid.into()),
                root.as_os_str().as_encoded_bytes().to_vec(),
                Some(LaunchAccount {
                    id: "codex-old".into(),
                    env_key: "CODEX_HOME".into(),
                    dir: from.display().to_string(),
                }),
            )
            .unwrap();
        // Simulate either an active Working turn (UserPromptSubmit) or a rate-limit StopFailure.
        let prompt_event = HookEvent {
            id: EventId {
                pid: 721,
                unix_ns: 1,
            },
            ppid: 721,
            agent: "codex".into(),
            input: Input::Complete,
            payload: format!(
                "{{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"{sid}\",\"prompt\":\"implement feature\"}}"
            )
            .into_bytes(),
        };
        reg.status.hook(&[(SessionId(21), &prompt_event)]).unwrap();
        if rate_limited {
            let stop_event = HookEvent {
                id: EventId {
                    pid: 721,
                    unix_ns: 2,
                },
                ppid: 721,
                agent: "codex".into(),
                input: Input::Complete,
                payload: format!(
                    "{{\"hook_event_name\":\"StopFailure\",\"session_id\":\"{sid}\",\"error\":\"rate_limit\"}}"
                )
                .into_bytes(),
            };
            reg.status.hook(&[(SessionId(21), &stop_event)]).unwrap();
            reg.agents.mark_rate_limited(SessionId(21));
        }

        let reader = Arc::clone(&reg);
        let reader_worker =
            std::thread::spawn(move || reader.holder.as_ref().unwrap().run(link, |_, _| {}));
        let hook_reg = Arc::clone(&reg);
        let mock = std::thread::spawn(move || {
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Spawn { req, id, .. } = ToHolder::decode(packet.frame()).unwrap() else {
                panic!("expected replacement spawn");
            };
            let (master, mut process) = UnixStream::pair().unwrap();
            holder_link
                .send(
                    &FromHolder::Spawned { req, id, pid: 821 }.encode().unwrap(),
                    &[master.as_fd()],
                )
                .unwrap();
            process.write_all(b"resumed prompt > ").unwrap();
            hook_reg.agents.record_hook_session(
                id,
                format!("{{\"hook_event_name\":\"SessionStart\",\"session_id\":\"{sid}\"}}")
                    .as_bytes(),
            );
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Terminate { req, id: old } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!("expected old process retirement");
            };
            assert_eq!(old, SessionId(21));
            holder_link
                .send(&FromHolder::Done { req }.encode().unwrap(), &[])
                .unwrap();
            lock(&hook_reg.state).exited.insert(old, 0);

            process
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            // A stream may split the text and its submission CR across reads.
            let mut buf = vec![0_u8; "Continue from where you left off.".len() + 1];
            process
                .read_exact(&mut buf)
                .expect("expected continuation prompt on new PTY");
            let received = String::from_utf8_lossy(&buf).into_owned();
            (id, received, process)
        });

        reg.queue_account_switch(ProviderKind::Codex, &target);
        reg.process_account_work_in(&home);

        let (_new_id, received, _process) = mock.join().unwrap();
        assert!(
            received.contains("Continue from where you left off."),
            "replacement session should auto-receive continuation prompt, got: {received:?}"
        );
        assert!(
            received.ends_with('\r'),
            "continuation prompt should be submitted with CR: {received:?}"
        );

        reader_worker.join().unwrap();
        drop(reg);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn unconfirmed_replacement_preserves_original_and_allows_a_verified_retry() {
    use std::io::Read as _;

    let root = Path::new("/tmp").join(format!("kas-unconfirmed-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let from = root.join("from");
    let to = root.join("to");
    let back = root.join("back");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(from.join("sessions/2026/10/28")).unwrap();
    let mut target_ids = Vec::new();
    for (dir, label) in [(&to, "Selected"), (&back, "Back")] {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("auth.json"), "fixture credentials").unwrap();
        let snap = accounts::add_account(
            &home,
            Some(&state),
            ProviderKind::Codex,
            label,
            None,
            dir.to_str().unwrap(),
        )
        .unwrap();
        target_ids.push(
            snap.accounts
                .iter()
                .find(|a| a.label == label)
                .unwrap()
                .id
                .clone(),
        );
    }
    let sid = "cccccccc-dddd-eeee-ffff-000000000000";
    fs::write(
        from.join(format!("sessions/2026/10/28/rollout-date-{sid}.jsonl")),
        format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{sid}\"}}}}\n"),
    )
    .unwrap();
    let (daemon_end, holder_end) = UnixStream::pair().unwrap();
    let mut holder_link = Link::new(OwnedFd::from(holder_end));
    for message in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
            inbox: false,
        },
        FromHolder::Ready,
    ] {
        holder_link.send(&message.encode().unwrap(), &[]).unwrap();
    }
    let (master, _old_process) = UnixStream::pair().unwrap();
    let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let reg = Arc::new(Registry::new(
        Some((
            holder,
            vec![Kept {
                id: SessionId(31),
                pid: 731,
                status: None,
                master: OwnedFd::from(master),
            }],
        )),
        Some(&state),
    ));
    reg.agents
        .remember_session(
            SessionId(31),
            &AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Yolo,
            },
            Some(sid.into()),
            root.as_os_str().as_encoded_bytes().to_vec(),
            Some(LaunchAccount {
                id: "codex-old".into(),
                env_key: "CODEX_HOME".into(),
                dir: from.display().to_string(),
            }),
        )
        .unwrap();
    // A rate-limited original would normally be told to continue after the switch.
    reg.agents.mark_rate_limited(SessionId(31));
    let reader = Arc::clone(&reg);
    let reader_worker =
        std::thread::spawn(move || reader.holder.as_ref().unwrap().run(link, |_, _| {}));
    let hook_reg = Arc::clone(&reg);
    let mock = std::thread::spawn(move || {
        let mut replacements = Vec::new();
        let mut processes = Vec::new();
        for round in [0, 1] {
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Spawn { req, id, argv, .. } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!("expected replacement spawn");
            };
            assert!(argv.iter().any(|arg| arg == sid.as_bytes()));
            let (master, mut process) = UnixStream::pair().unwrap();
            holder_link
                .send(
                    &FromHolder::Spawned {
                        req,
                        id,
                        pid: 831 + round,
                    }
                    .encode()
                    .unwrap(),
                    &[master.as_fd()],
                )
                .unwrap();
            // Like a login screen: output, but no hook and no open conversation.
            process.write_all(b"Sign in with ChatGPT").unwrap();
            if round == 1 {
                hook_reg.agents.record_hook_session(
                    id,
                    format!("{{\"hook_event_name\":\"SessionStart\",\"session_id\":\"{sid}\"}}")
                        .as_bytes(),
                );
            }
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Terminate { req, id: old } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!(
                    "expected rollback of an unconfirmed replacement or retirement after confirmation"
                );
            };
            let expected = if round == 0 { id } else { SessionId(31) };
            assert_eq!(old, expected);
            holder_link
                .send(&FromHolder::Done { req }.encode().unwrap(), &[])
                .unwrap();
            lock(&hook_reg.state).exited.insert(old, 0);
            replacements.push(id);
            if round == 0 {
                process
                    .set_read_timeout(Some(Duration::from_millis(1500)))
                    .unwrap();
                let mut byte = [0_u8; 1];
                assert!(
                    !matches!(process.read(&mut byte), Ok(1..)),
                    "an unconfirmed replacement must not receive the continuation prompt"
                );
            }
            processes.push(process);
        }
        (replacements, processes)
    });

    reg.queue_account_switch(ProviderKind::Codex, &target_ids[0]);
    reg.process_account_work_in(&home);
    let failed = load(Some(&state))
        .get(&SessionId(31))
        .cloned()
        .expect("failure must preserve the original's recovery record");
    assert!(failed.to.is_none() && failed.error.is_some());
    assert!(
        reg.agents.stored(SessionId(31)).unwrap().is_some(),
        "original conversation remains switchable"
    );
    let snapshot = accounts::refresh_accounts_with(&home, Some(&state), false);
    reg.queue_session_account_switch(
        SessionId(31),
        ProviderKind::Codex,
        &target_ids[1],
        &snapshot,
    )
    .unwrap();
    reg.process_account_work_in(&home);
    let (replacements, _processes) = mock.join().unwrap();
    assert_ne!(replacements[0], replacements[1]);
    assert_eq!(
        load(Some(&state)).get(&SessionId(31)).and_then(|h| h.to),
        Some(replacements[1])
    );
    reader_worker.join().unwrap();
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unmanaged_shell_skips_unidentifiable_daemon_agent_and_switches_open_transcript_agent() {
    use krill_proto::hook::{EventId, HookEvent, Input};

    let root = Path::new("/tmp").join(format!("kas-unmanaged-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let from = root.join("from");
    let to = root.join("to");
    fs::create_dir_all(state.join("shell/zsh")).unwrap();
    fs::write(state.join("shell/zsh/.zshenv"), "# stale zshenv\n").unwrap();
    fs::create_dir_all(from.join("sessions/2026/10/25")).unwrap();
    fs::create_dir_all(&to).unwrap();
    fs::write(from.join("auth.json"), "from fixture credentials").unwrap();
    fs::write(to.join("auth.json"), "selected fixture credentials").unwrap();

    let snap_from = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Codex,
        "Origin",
        None,
        from.to_str().unwrap(),
    )
    .unwrap();
    let origin_id = snap_from
        .accounts
        .iter()
        .find(|a| a.label == "Origin")
        .unwrap()
        .id
        .clone();
    let snap_to = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Codex,
        "Selected",
        None,
        to.to_str().unwrap(),
    )
    .unwrap();
    let target_id = snap_to
        .accounts
        .iter()
        .find(|a| a.label == "Selected")
        .unwrap()
        .id
        .clone();

    let sid = "cccccccc-dddd-eeee-ffff-111111111111";
    let transcript = from.join(format!("sessions/2026/10/25/rollout-date-{sid}.jsonl"));
    fs::write(
        &transcript,
        format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{sid}\"}}}}\n"),
    )
    .unwrap();

    let unidentifiable_proc = IdleChild::new();
    let open_transcript_proc = FileHolder::new(&transcript);

    let (daemon_end, holder_end) = UnixStream::pair().unwrap();
    let mut holder_link = Link::new(OwnedFd::from(holder_end));
    for message in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
            inbox: false,
        },
        FromHolder::Ready,
    ] {
        holder_link.send(&message.encode().unwrap(), &[]).unwrap();
    }
    let (master_454, _proc_454) = UnixStream::pair().unwrap();
    let (master_455, _proc_455) = UnixStream::pair().unwrap();
    let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let reg = Arc::new(Registry::new(
        Some((
            holder,
            vec![
                Kept {
                    id: SessionId(454),
                    pid: unidentifiable_proc.0.id(),
                    status: None,
                    master: OwnedFd::from(master_454),
                },
                Kept {
                    id: SessionId(455),
                    pid: open_transcript_proc.0.id(),
                    status: None,
                    master: OwnedFd::from(master_455),
                },
            ],
        )),
        Some(&state),
    ));
    let refreshed_zshenv = fs::read_to_string(state.join("shell/zsh/.zshenv")).unwrap();
    assert!(
        refreshed_zshenv.contains("--no-daemon"),
        "Registry::new should refresh existing zsh sync script on startup"
    );

    // Neither session has a StoredLaunch (`agent-launches/*.json`), simulating login-shell runs.
    for (session, pid) in [
        (SessionId(454), unidentifiable_proc.0.id()),
        (SessionId(455), open_transcript_proc.0.id()),
    ] {
        let ev = HookEvent {
            id: EventId { pid, unix_ns: 1 },
            ppid: pid,
            agent: "codex".into(),
            input: Input::Complete,
            payload: b"{\"hook_event_name\":\"Stop\"}".to_vec(),
        };
        reg.status.hook(&[(session, &ev)]).unwrap();
    }
    assert!(reg.agents.stored(SessionId(454)).unwrap().is_none());
    assert!(reg.agents.stored(SessionId(455)).unwrap().is_none());

    // Selecting the account Session 455 is already using skips both 454 and 455.
    reg.queue_account_switch(ProviderKind::Codex, &origin_id);
    assert!(lock(&reg.account_handoffs).is_empty());

    let reader = Arc::clone(&reg);
    let reader_worker =
        std::thread::spawn(move || reader.holder.as_ref().unwrap().run(link, |_, _| {}));
    let hook_reg = Arc::clone(&reg);
    let expected_transcript = to
        .join("sessions/2026/10/25")
        .join(transcript.file_name().unwrap());
    let mock = std::thread::spawn(move || {
        let packet = holder_link.recv().unwrap().unwrap();
        let ToHolder::Spawn { req, id, argv, .. } = ToHolder::decode(packet.frame()).unwrap()
        else {
            panic!("expected replacement spawn for Session 455");
        };
        assert!(argv.iter().any(|arg| arg == sid.as_bytes()));
        let replacement_proc = FileHolder::new(&expected_transcript);
        let (master, mut process) = UnixStream::pair().unwrap();
        holder_link
            .send(
                &FromHolder::Spawned {
                    req,
                    id,
                    pid: replacement_proc.0.id(),
                }
                .encode()
                .unwrap(),
                &[master.as_fd()],
            )
            .unwrap();
        process.write_all(b"resumed unmanaged session\r\n").unwrap();
        let packet = holder_link.recv().unwrap().unwrap();
        let ToHolder::Terminate { req, id: old } = ToHolder::decode(packet.frame()).unwrap() else {
            panic!("expected old process retirement for Session 455");
        };
        assert_eq!(old, SessionId(455));
        holder_link
            .send(&FromHolder::Done { req }.encode().unwrap(), &[])
            .unwrap();
        lock(&hook_reg.state).exited.insert(old, 0);
        (id, process, replacement_proc)
    });

    reg.queue_account_switch(ProviderKind::Codex, &target_id);
    assert!(
        !lock(&reg.account_handoffs).contains_key(&SessionId(454)),
        "unidentifiable unmanaged shell session must never be queued"
    );
    assert!(lock(&reg.account_handoffs).contains_key(&SessionId(455)));
    reg.process_account_work_in(&home);

    let saved = load(Some(&state));
    assert!(!saved.contains_key(&SessionId(454)));
    let switched = saved.get(&SessionId(455)).unwrap();
    assert!(switched.error.is_none());
    assert!(switched.to.is_some());

    drop(mock.join().unwrap());
    reader_worker.join().unwrap();
    drop(unidentifiable_proc);
    drop(open_transcript_proc);
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unbound_session_skips_auto_switch_and_cancelled_opt_out_until_bound_and_prunes_dead_failures() {
    let root = Path::new("/tmp").join(format!("kas-optout-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let from = root.join("from");
    let to = root.join("to");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&from).unwrap();
    fs::create_dir_all(&to).unwrap();
    fs::write(to.join("auth.json"), "selected fixture credentials").unwrap();

    let snap = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Codex,
        "Selected",
        None,
        to.to_str().unwrap(),
    )
    .unwrap();
    let target = snap
        .accounts
        .iter()
        .find(|a| a.label == "Selected")
        .unwrap()
        .id
        .clone();

    // Seed a stale failed handoff for dead Session 99 and an ineligible unmanaged Session 52.
    fs::write(
        state.join(FILE),
        serde_json::to_vec(&vec![
            SessionHandoff {
                from: SessionId(99),
                to: None,
                account_id: target.clone(),
                error: Some(
                    "Session 99: could not resume with selected account (missing); original session preserved"
                        .into(),
                ),
            },
            SessionHandoff {
                from: SessionId(52),
                to: None,
                account_id: target.clone(),
                error: Some(
                    "Session 52: could not resume with selected account (missing); original session preserved"
                        .into(),
                ),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let (daemon_end, holder_end) = UnixStream::pair().unwrap();
    let holder_link = Link::new(OwnedFd::from(holder_end));
    for message in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
            inbox: false,
        },
        FromHolder::Ready,
    ] {
        holder_link.send(&message.encode().unwrap(), &[]).unwrap();
    }
    let (master_51, _proc_51) = UnixStream::pair().unwrap();
    let (master_52, _proc_52) = UnixStream::pair().unwrap();
    let (holder, _link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let reg = Arc::new(Registry::new(
        Some((
            holder,
            vec![
                Kept {
                    id: SessionId(51),
                    pid: 951,
                    status: None,
                    master: OwnedFd::from(master_51),
                },
                Kept {
                    id: SessionId(52),
                    pid: 952,
                    status: None,
                    master: OwnedFd::from(master_52),
                },
            ],
        )),
        Some(&state),
    ));
    reg.agents
        .remember_session(
            SessionId(51),
            &AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Yolo,
            },
            None,
            root.as_os_str().as_encoded_bytes().to_vec(),
            Some(LaunchAccount {
                id: "codex-old".into(),
                env_key: "CODEX_HOME".into(),
                dir: from.display().to_string(),
            }),
        )
        .unwrap();
    // Backdate Session 51's launch record so it is not treated as mid-startup (< 5s).
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::FileTimesExt as _;
        let old_time = std::time::SystemTime::now() - Duration::from_secs(30);
        let times = fs::FileTimes::new()
            .set_created(old_time)
            .set_modified(old_time);
        fs::File::options()
            .write(true)
            .open(state.join("agent-launches/51.json"))
            .unwrap()
            .set_times(times)
            .unwrap();
    }
    #[cfg(not(target_os = "macos"))]
    std::thread::sleep(Duration::from_secs(5));

    // Dead Session 99 is immediately hidden from snapshots before recovery even runs.
    let mut snap_before = AccountsSnapshot::default();
    reg.account_switch_snapshot(&mut snap_before);
    assert!(
        !snap_before
            .details
            .handoffs
            .iter()
            .any(|h| h.from == SessionId(99))
    );

    // Automatic background switch (`prepared: false`) prunes 99 & 52 and skips unbound 51.
    lock(&reg.account_work.pending).insert(
        SwitchScope::Provider(ProviderKind::Codex),
        crate::usage::accounts::switch_work::SwitchTarget {
            account_id: target.clone(),
            prepared: false,
            not_before: None,
        },
    );
    reg.process_account_work_in(&home);
    assert!(
        load(Some(&state)).is_empty(),
        "dead/ineligible failures should be pruned and unbound session skipped on auto-switch"
    );

    // Explicit manual switch still reports failure once on unbound Session 51.
    reg.queue_account_switch(ProviderKind::Codex, &target);
    reg.process_account_work_in(&home);
    assert!(load(Some(&state))[&SessionId(51)].error.is_some());

    // Continuing on the original account opts Session 51 out of repeated failures while unbound.
    reg.cancel_failed_account_switch(SessionId(51)).unwrap();
    assert!(load(Some(&state)).is_empty());
    reg.queue_account_switch(ProviderKind::Codex, &target);
    assert!(
        lock(&reg.account_handoffs).is_empty(),
        "opted-out unbound session must not re-fail on subsequent switches"
    );

    // Once a hook binds a conversation ID, opt-out clears and manual switch enqueues again.
    reg.agents.record_hook_session(
        SessionId(51),
        b"{\"hook_event_name\":\"SessionStart\",\"session_id\":\"bound-after-optout\"}",
    );
    reg.queue_account_switch(ProviderKind::Codex, &target);
    assert!(lock(&reg.account_handoffs).contains_key(&SessionId(51)));

    // Closing a session with a non-retained failed handoff prunes its saved record.
    lock(&reg.account_handoffs).insert(
        SessionId(51),
        SessionHandoff {
            from: SessionId(51),
            to: None,
            account_id: target,
            error: Some("Session 51: could not resume; original session preserved".into()),
        },
    );
    reg.save_account_handoffs().unwrap();
    reg.remove(SessionId(51));
    assert!(load(Some(&state)).is_empty());

    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

struct IdleChild(std::process::Child);

impl IdleChild {
    #[expect(clippy::unwrap_used, reason = "real-process test fixture")]
    fn new() -> Self {
        use std::io::BufRead as _;
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'ready\n'; read -r line"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        Self(child)
    }
}

impl Drop for IdleChild {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

// A real process holds the conversation file so tests exercise the kernel lookup too.
struct FileHolder(std::process::Child);

impl FileHolder {
    #[expect(clippy::unwrap_used, reason = "real-process test fixture")]
    fn new(path: &Path) -> Self {
        use std::io::BufRead as _;
        let mut child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "exec 3<\"$1\"; printf 'ready\n'; read -r line",
                "file-holder",
            ])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        Self(child)
    }
}

impl Drop for FileHolder {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

#[test]
fn later_selection_preserves_the_durable_alias_while_original_binding_retires() {
    let root = std::env::temp_dir().join(format!("kas-retiring-select-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let profile = root.join("profile");
    fs::create_dir_all(&profile).unwrap();
    let snapshot = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Codex,
        "Later",
        None,
        profile.to_str().unwrap(),
    )
    .unwrap();
    let target = snapshot
        .accounts
        .iter()
        .find(|a| a.label == "Later")
        .unwrap()
        .id
        .clone();
    let session = SessionId(7);
    let (reg, _original) = kept_fixture(&state, session);
    reg.agents
        .remember_session(
            session,
            &AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Manual,
            },
            Some("original-conversation".into()),
            Vec::new(),
            None,
        )
        .unwrap();
    let alias = SessionHandoff {
        from: session,
        to: Some(SessionId(8)),
        account_id: "committed-selection".into(),
        error: None,
    };
    lock(&reg.account_handoffs).insert(session, alias.clone());
    reg.save_account_handoffs().unwrap();
    let before = fs::read(state.join(FILE)).unwrap();
    let retiring = reg.agents.retire_account_binding(session);
    reg.queue_account_switch(ProviderKind::Codex, &target);
    reg.queue_session_account_switch(session, ProviderKind::Codex, &target, &snapshot)
        .unwrap();
    let saved = load(Some(&state));
    assert_eq!(saved.get(&session), Some(&alias));
    assert_eq!(fs::read(state.join(FILE)).unwrap(), before);
    assert_eq!(
        lock(&reg.account_work.pending)
            .get(&SwitchScope::Session(session, ProviderKind::Codex))
            .unwrap()
            .account_id,
        target
    );
    assert!(!lock(&reg.state).closing.contains(&session));
    drop(retiring);
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_provider_handoff_is_cancelled_and_rejected_before_reading_transcripts() {
    let root = std::env::temp_dir().join(format!("kas-provider-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let from = root.join("claude");
    let to = root.join("codex");
    fs::create_dir_all(from.join("sessions")).unwrap();
    fs::create_dir_all(from.join("projects/demo")).unwrap();
    fs::create_dir_all(&to).unwrap();
    fs::write(to.join("auth.json"), "selected fixture credentials").unwrap();
    let snapshot = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Codex,
        "Selected",
        None,
        to.to_str().unwrap(),
    )
    .unwrap();
    let target = snapshot
        .accounts
        .iter()
        .find(|a| a.label == "Selected")
        .unwrap()
        .id
        .clone();
    let session = SessionId(39374);
    let (reg, mut original) = kept_fixture(&state, session);
    reg.agents
        .remember_session(
            session,
            &AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Manual,
            },
            Some("claude-conversation".into()),
            Vec::new(),
            Some(LaunchAccount {
                id: "claude-old".into(),
                env_key: "CLAUDE_CONFIG_DIR".into(),
                dir: from.display().to_string(),
            }),
        )
        .unwrap();
    let path = from.join("projects/demo/claude-conversation.jsonl");
    fs::write(&path, "{\"sessionId\":\"claude-conversation\"}\n").unwrap();
    let hook = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": "SessionStart", "session_id": "claude-conversation", "transcript_path": path
    })).unwrap();
    reg.agents
        .record_hook_session_with_context(session, &hook, Some("claude"), None);
    lock(&reg.account_handoffs).insert(
        session,
        SessionHandoff {
            from: session,
            to: None,
            account_id: target.clone(),
            error: None,
        },
    );
    let before = reg.agents.stored(session).unwrap().unwrap();
    assert_eq!(
        reg.handoff_account(
            &Peer {
                pid: None,
                scope: Scope::Full
            },
            session,
            ProviderKind::Codex,
            &target,
            &home
        ),
        Err(Failure::BadRequest)
    );
    assert_eq!(reg.agents.stored(session).unwrap().unwrap(), before);
    reg.queue_account_switch(ProviderKind::Codex, &target);
    assert!(!lock(&reg.account_handoffs).contains_key(&session));
    assert!(!load(Some(&state)).contains_key(&session));
    assert!(!lock(&reg.state).closing.contains(&session));
    original.write_all(b"original still running\r\n").unwrap();
    // Wait for the capture so the store does not write a segment while the fixture is removed.
    let tap = lock(&reg.state)
        .sessions
        .get(&session)
        .unwrap()
        .session
        .tap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while tap.pulse().end < 24 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!tap.pulse().closed);
    assert!(tap.pulse().end >= 24, "original PTY keeps capturing output");
    assert!(
        fs::read_dir(&to)
            .unwrap()
            .all(|entry| entry.unwrap().file_name() == "auth.json")
    );
    drop(reg);
    // 취소된 인계의 백그라운드 정리가 끝나기 전에 지우면 DirectoryNotEmpty가 난다(8회 반복 중 2회 재현).
    for attempt in 0..50 {
        match fs::remove_dir_all(&root) {
            Ok(()) => break,
            Err(error) if attempt == 49 => panic!("{error}"),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
}

#[expect(clippy::unwrap_used, reason = "deterministic account fixture setup")]
fn kept_sessions_fixture(root: &Path, ids: &[SessionId]) -> (Arc<Registry>, Vec<UnixStream>) {
    fs::create_dir_all(root).unwrap();
    let (daemon_end, holder_end) = UnixStream::pair().unwrap();
    let link = Link::new(OwnedFd::from(holder_end));
    for message in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
            inbox: false,
        },
        FromHolder::Ready,
    ] {
        link.send(&message.encode().unwrap(), &[]).unwrap();
    }
    let mut processes = Vec::new();
    let sessions = ids
        .iter()
        .map(|id| {
            let (master, process) = UnixStream::pair().unwrap();
            processes.push(process);
            Kept {
                id: *id,
                pid: 800,
                status: None,
                master: OwnedFd::from(master),
            }
        })
        .collect();
    let (holder, _reader, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let reg = Arc::new(Registry::new(Some((holder, sessions)), Some(root)));
    (reg, processes)
}

#[test]
fn bounded_handoff_workers_overlap_without_exceeding_limit() {
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    let entered = Arc::new(Barrier::new(MAX_HANDOFF_WORKERS + 1));
    let release = Arc::new(Barrier::new(MAX_HANDOFF_WORKERS + 1));
    let started = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let worker = {
        let (entered, release, started, completed, active, peak) = (
            Arc::clone(&entered),
            Arc::clone(&release),
            Arc::clone(&started),
            Arc::clone(&completed),
            Arc::clone(&active),
            Arc::clone(&peak),
        );
        std::thread::spawn(move || {
            run_bounded(&(0..12).collect::<Vec<_>>(), |_| {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(count, Ordering::SeqCst);
                if started.fetch_add(1, Ordering::SeqCst) < MAX_HANDOFF_WORKERS {
                    entered.wait();
                    release.wait();
                }
                completed.fetch_add(1, Ordering::SeqCst);
                active.fetch_sub(1, Ordering::SeqCst);
            });
        })
    };
    entered.wait();
    assert_eq!(started.load(Ordering::SeqCst), MAX_HANDOFF_WORKERS);
    assert_eq!(active.load(Ordering::SeqCst), MAX_HANDOFF_WORKERS);
    release.wait();
    worker.join().unwrap();
    assert_eq!(completed.load(Ordering::SeqCst), 12);
    assert_eq!(peak.load(Ordering::SeqCst), MAX_HANDOFF_WORKERS);
}

#[test]
fn session_switch_validates_exact_target_preserves_default_and_recovers_exact_scope() {
    use krill_proto::roles::Role;
    use krill_proto::usage::{FEAT_SESSION_ACCOUNT_SWITCH, UsageReply, UsageRequest};
    let root = Path::new("/tmp").join(format!("kas-session-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let profile = root.join("profile");
    fs::create_dir_all(&profile).unwrap();
    let snapshot = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Codex,
        "Chosen",
        None,
        profile.to_str().unwrap(),
    )
    .unwrap();
    let target = snapshot
        .accounts
        .iter()
        .find(|a| a.label == "Chosen")
        .unwrap()
        .id
        .clone();
    let (reg, _processes) = kept_sessions_fixture(&state, &[SessionId(7), SessionId(8)]);
    for session in [SessionId(7), SessionId(8)] {
        reg.agents
            .remember_session(
                session,
                &AgentLaunch {
                    profile: "codex".into(),
                    permission: Permission::Yolo,
                },
                Some("bound-id".into()),
                Vec::new(),
                Some(LaunchAccount {
                    id: "codex-old".into(),
                    env_key: "CODEX_HOME".into(),
                    dir: home.display().to_string(),
                }),
            )
            .unwrap();
    }
    let config = fs::read(state.join("accounts.toml")).unwrap();
    assert_eq!(
        reg.queue_session_account_switch(SessionId(99), ProviderKind::Codex, &target, &snapshot),
        Err(Failure::NoSession)
    );
    assert_eq!(
        reg.queue_session_account_switch(SessionId(7), ProviderKind::Claude, &target, &snapshot),
        Err(Failure::BadRequest)
    );
    assert_eq!(
        reg.queue_session_account_switch(SessionId(7), ProviderKind::Codex, "missing", &snapshot),
        Err(Failure::BadRequest)
    );
    assert!(lock(&reg.account_handoffs).is_empty());
    reg.queue_session_account_switch(SessionId(7), ProviderKind::Codex, &target, &snapshot)
        .unwrap();
    assert_eq!(fs::read(state.join("accounts.toml")).unwrap(), config);
    assert_eq!(
        load(Some(&state)).keys().copied().collect::<Vec<_>>(),
        [SessionId(7)]
    );
    let mut cancel_snapshot = snapshot.clone();
    let mut original = snapshot
        .accounts
        .iter()
        .find(|a| a.id == target)
        .unwrap()
        .clone();
    original.id = "codex-old".into();
    cancel_snapshot.accounts.push(original);
    reg.queue_session_account_switch(
        SessionId(7),
        ProviderKind::Codex,
        "codex-old",
        &cancel_snapshot,
    )
    .unwrap();
    assert!(lock(&reg.account_handoffs).is_empty());
    assert!(!reg.account_selection_current(
        SessionId(7),
        ProviderKind::Codex,
        &target,
        &lock(&reg.account_work.pending)
    ));
    reg.queue_session_account_switch(SessionId(7), ProviderKind::Codex, &target, &snapshot)
        .unwrap();
    let other_profile = root.join("other-profile");
    fs::create_dir_all(&other_profile).unwrap();
    let snapshot = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Codex,
        "Other",
        None,
        other_profile.to_str().unwrap(),
    )
    .unwrap();
    let other_target = snapshot
        .accounts
        .iter()
        .find(|a| a.label == "Other")
        .unwrap()
        .id
        .clone();
    reg.queue_session_account_switch(SessionId(8), ProviderKind::Codex, &other_target, &snapshot)
        .unwrap();
    let pending = lock(&reg.account_work.pending);
    assert!(reg.account_selection_current(SessionId(7), ProviderKind::Codex, &target, &pending));
    assert!(reg.account_selection_current(
        SessionId(8),
        ProviderKind::Codex,
        &other_target,
        &pending
    ));
    assert!(!reg.account_selection_current(SessionId(8), ProviderKind::Codex, &target, &pending));
    drop(pending);
    let peer = Peer {
        pid: None,
        scope: Scope::Full,
    };
    let request = UsageRequest::SwitchSessionAccount {
        session: SessionId(7),
        provider: ProviderKind::Codex,
        account_id: target.clone(),
    };
    assert_eq!(
        reg.on_usage_negotiated(
            &peer,
            Role::Observer,
            request.clone(),
            krill_proto::ipc::FEATURES
        ),
        UsageReply::Failed(Failure::Denied)
    );
    assert_eq!(
        reg.on_usage_negotiated(
            &peer,
            Role::Interactive,
            request,
            krill_proto::ipc::FEATURES & !FEAT_SESSION_ACCOUNT_SWITCH
        ),
        UsageReply::Failed(Failure::BadRequest)
    );
    lock(&reg.account_work.pending).clear();
    reg.recover_account_work(&home);
    let pending = lock(&reg.account_work.pending);
    assert_eq!(pending.len(), 2);
    assert!(pending.contains_key(&SwitchScope::Session(SessionId(7), ProviderKind::Codex)));
    assert!(!pending.contains_key(&SwitchScope::Provider(ProviderKind::Codex)));
    drop(pending);
    assert!(reg.claim_resume(SessionId(7)).is_some());
    let claim = reg.claim_resume(SessionId(7)).unwrap();
    assert!(reg.claim_resume(SessionId(7)).is_none());
    drop(claim);
    // A late click still naming the original must keep its durable alias and queue
    // against the confirmed replacement, even before retirement is fully observed.
    lock(&reg.account_handoffs).insert(
        SessionId(7),
        SessionHandoff {
            from: SessionId(7),
            to: Some(SessionId(8)),
            account_id: target.clone(),
            error: None,
        },
    );
    let retiring = reg.agents.retire_account_binding(SessionId(7));
    reg.queue_session_account_switch(SessionId(7), ProviderKind::Codex, &target, &snapshot)
        .unwrap();
    assert_eq!(load(Some(&state))[&SessionId(7)].to, Some(SessionId(8)));
    assert_eq!(load(Some(&state))[&SessionId(8)].account_id, target);
    drop(retiring);
    lock(&reg.state).closing.insert(SessionId(7));
    lock(&reg.account_replacements).insert(SessionId(8));
    reg.queue_session_account_switch(SessionId(7), ProviderKind::Codex, &target, &snapshot)
        .unwrap();
    let saved = load(Some(&state));
    assert_eq!(saved[&SessionId(7)].to, Some(SessionId(8)));
    assert_eq!(saved[&SessionId(8)].account_id, target);
    assert!(
        lock(&reg.account_work.pending)
            .contains_key(&SwitchScope::Session(SessionId(8), ProviderKind::Codex))
    );
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn replacement_journals_are_independent_and_legacy_record_is_preserved() {
    let root = Path::new("/tmp").join(format!("kas-journals-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();
    let reg = Arc::new(Registry::new(None, Some(&root)));
    for id in [SessionId(8), SessionId(9)] {
        accounts::write_atomic_0600(&replacement_journal(&root, id), b"fixture").unwrap();
    }
    fs::write(root.join(PREPARED_FILE), b"legacy").unwrap();
    drop(Replacement {
        registry: &reg,
        original: SessionId(7),
        session: SessionId(8),
        committed: true,
    });
    assert!(!replacement_journal(&root, SessionId(8)).exists());
    assert!(replacement_journal(&root, SessionId(9)).exists());
    assert!(root.join(PREPARED_FILE).exists());
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn retirement_transfer_preserves_newer_replacement_target_and_cancel() {
    check_retirement_transfer(TransferSelection::NewTarget, false);
    check_retirement_transfer(TransferSelection::Cancel, false);
}

#[test]
fn retirement_transfer_rechecks_target_and_cancel_arriving_during_refresh() {
    check_retirement_transfer(TransferSelection::NewTarget, true);
    check_retirement_transfer(TransferSelection::Cancel, true);
}

#[test]
fn retirement_transfer_applies_follow_up_when_no_newer_replacement_request_exists() {
    check_retirement_transfer(TransferSelection::FollowUp, false);
}

#[expect(clippy::unwrap_used, reason = "deterministic account fixture setup")]
fn check_retirement_transfer(selection: TransferSelection, during_refresh: bool) {
    for provider_scope in [false, true] {
        for retired in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "kas-transfer-{}-{selection:?}-{during_refresh}-{provider_scope}-{retired}",
                std::process::id()
            ));
            fs::remove_dir_all(&root).ok();
            let state = root.join("state");
            let home = root.join("home");
            let mut snapshot = AccountsSnapshot::default();
            let mut ids = Vec::new();
            for label in ["Running", "Stale", "Newest"] {
                let profile = root.join(label);
                fs::create_dir_all(&profile).unwrap();
                snapshot = accounts::add_account(
                    &home,
                    Some(&state),
                    ProviderKind::Codex,
                    label,
                    None,
                    profile.to_str().unwrap(),
                )
                .unwrap();
                ids.push(
                    snapshot
                        .accounts
                        .iter()
                        .find(|a| a.label == label)
                        .unwrap()
                        .id
                        .clone(),
                );
            }
            let mut ids = ids.into_iter();
            let running = ids.next().unwrap();
            let stale = ids.next().unwrap();
            let newest = ids.next().unwrap();
            let (reg, _processes) = kept_sessions_fixture(&state, &[SessionId(7), SessionId(8)]);
            reg.agents
                .remember_session(
                    SessionId(8),
                    &AgentLaunch {
                        profile: "codex".into(),
                        permission: Permission::Manual,
                    },
                    Some("confirmed-conversation".into()),
                    Vec::new(),
                    Some(LaunchAccount {
                        id: running.clone(),
                        env_key: "CODEX_HOME".into(),
                        dir: root.join("Running").display().to_string(),
                    }),
                )
                .unwrap();
            let alias = SessionHandoff {
                from: SessionId(7),
                to: Some(SessionId(8)),
                account_id: running.clone(),
                error: None,
            };
            lock(&reg.account_handoffs).insert(SessionId(7), alias.clone());
            reg.save_account_handoffs().unwrap();
            let retirement = reg.agents.retire_account_binding(SessionId(7));
            if retired {
                lock(&reg.state).exited.insert(SessionId(7), 0);
            }
            let scope = if provider_scope {
                SwitchScope::Provider(ProviderKind::Codex)
            } else {
                SwitchScope::Session(SessionId(7), ProviderKind::Codex)
            };
            lock(&reg.account_work.pending).insert(
                scope,
                accounts::switch_work::SwitchTarget {
                    account_id: stale.clone(),

                    prepared: true,
                    not_before: None,
                },
            );
            let chosen = match selection {
                TransferSelection::FollowUp => &stale,
                TransferSelection::NewTarget => &newest,
                TransferSelection::Cancel => &running,
            };
            let explicit = !matches!(selection, TransferSelection::FollowUp);
            let select_new = || {
                reg.queue_session_account_switch(
                    SessionId(8),
                    ProviderKind::Codex,
                    chosen,
                    &snapshot,
                )
                .unwrap();
            };
            if explicit && !during_refresh {
                select_new();
            }
            let saved_before = fs::read(state.join(FILE)).unwrap();
            let refreshed = std::cell::Cell::new(false);
            reg.transfer_account_selection(
                SessionId(7),
                SessionId(8),
                ProviderKind::Codex,
                &running,
                || {
                    refreshed.set(true);
                    // Injection point: the old/provider request exists, but discovery is
                    // still outside the mutex. The newly accepted choice must win.
                    if explicit && during_refresh {
                        select_new();
                    }
                    snapshot.clone()
                },
            )
            .unwrap();
            assert_eq!(refreshed.get(), !explicit || during_refresh);
            let pending = lock(&reg.account_work.pending);
            assert_eq!(
                pending
                    .get(&SwitchScope::Session(SessionId(8), ProviderKind::Codex))
                    .unwrap()
                    .account_id,
                *chosen
            );
            assert!(
                !pending.contains_key(&SwitchScope::Session(SessionId(7), ProviderKind::Codex))
            );
            assert_eq!(
                pending.contains_key(&SwitchScope::Provider(ProviderKind::Codex)),
                provider_scope
            );
            drop(pending);
            let saved = load(Some(&state));
            assert_eq!(saved.get(&SessionId(7)), Some(&alias));
            if matches!(selection, TransferSelection::Cancel) {
                assert!(!saved.contains_key(&SessionId(8)));
            } else {
                assert_eq!(saved.get(&SessionId(8)).unwrap().account_id, *chosen);
            }
            if explicit && !during_refresh {
                assert_eq!(fs::read(state.join(FILE)).unwrap(), saved_before);
            }
            drop(retirement);
            drop(reg);
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TransferSelection {
    FollowUp,
    NewTarget,
    Cancel,
}
