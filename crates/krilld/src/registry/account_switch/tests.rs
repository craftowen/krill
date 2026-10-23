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
    for (fail, cancel, unconfirmed, close_fail) in [
        (false, false, false, false),
        (true, false, false, false),
        (false, true, false, false),
        (false, false, true, false),
        (false, false, false, true),
    ] {
        let root = Path::new("/tmp").join(format!(
            "kas-{}-{fail}-{cancel}-{unconfirmed}-{close_fail}",
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
                    pid: 700,
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
                Some(sid.into()),
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
        let reselect = cancel.then(|| Arc::clone(&reg));
        let hook_reg = Arc::clone(&reg);
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
            let (master, mut process) = UnixStream::pair().unwrap();
            holder_link
                .send(
                    &FromHolder::Spawned { req, id, pid: 800 }.encode().unwrap(),
                    &[master.as_fd()],
                )
                .unwrap();
            process.write_all(b"resumed selected account\r\n").unwrap();
            if !unconfirmed {
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
            Some((id, process))
        });
        reg.queue_account_switch(ProviderKind::Codex, &target);
        reg.queue_account_switch(ProviderKind::Codex, "codex-old");
        assert!(
            lock(&reg.account_handoffs).is_empty(),
            "reselecting the running account cancels a queued handoff"
        );
        reg.queue_account_switch(ProviderKind::Codex, &target);
        reg.process_account_work_in(&home);
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
        drop(mock.join().unwrap());
        reader_worker.join().unwrap();
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
        let reg = Registry::new(
            Some((
                holder,
                vec![Kept {
                    id: SessionId(9),
                    pid: 900,
                    status: None,
                    master: OwnedFd::from(master),
                }],
            )),
            Some(&state),
        );
        reg.agents
            .remember_session(
                SessionId(9),
                &AgentLaunch {
                    profile: "codex".into(),
                    permission: Permission::Yolo,
                },
                case.starts_with("bound-")
                    .then(|| sid.to_owned())
                    .or_else(|| (case == "duplicate-id").then(|| sid.to_owned())),
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
fn kept_fixture(root: &Path, id: SessionId) -> (Registry, UnixStream) {
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
    let reg = Registry::new(
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
    );
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
