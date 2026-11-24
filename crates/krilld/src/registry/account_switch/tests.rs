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
    for (fail, cancel, unconfirmed, close_fail, process_bound, late_hook, became_busy) in [
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
            "kas-{}-{fail}-{cancel}-{unconfirmed}-{close_fail}-{process_bound}-{late_hook}-{became_busy}",
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
                    size: None,
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
            if became_busy {
                use krill_proto::hook::{EventId, HookEvent, Input};
                let event = HookEvent {
                    id: EventId {
                        pid: 700,
                        unix_ns: 1,
                    },
                    ppid: 700,
                    agent: "codex".into(),
                    input: Input::Complete,
                    payload: br#"{"hook_event_name":"UserPromptSubmit"}"#.to_vec(),
                };
                hook_reg.status.hook(&[(SessionId(7), &event)]).unwrap();
            }
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
                if cancel || became_busy {
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
            if !close_fail {
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
                lock(&hook_reg.state).exited.insert(cleanup, 0);
            }
            // Keep the new PTY open until the parent has committed its handoff.
            Some((id, process, replacement_process))
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
        } else if became_busy {
            let result = saved.get(&SessionId(7)).unwrap();
            assert!(result.to.is_none() && result.error.is_none());
            assert!(!lock(&reg.state).closing.contains(&SessionId(7)));
            assert!(
                lock(&reg.account_work.pending)
                    .contains_key(&SwitchScope::Session(SessionId(7), ProviderKind::Codex))
            );
        } else if fail || close_fail {
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
                    size: None,
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
pub(super) fn kept_fixture(root: &Path, id: SessionId) -> (Arc<Registry>, UnixStream) {
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
                size: None,
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
                disposition: ReplacementDisposition::Unresolved,
                bot_grant_token: None,
            };
        }
        assert!(!lock(&reg.state).closing.contains(&SessionId(8)));
        drop(reg);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn explicit_cancel_accepts_pending_or_failed_live_original_and_preserves_untracked_account() {
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
    lock(&reg.account_work.pending).insert(
        SwitchScope::Session(session, ProviderKind::Codex),
        accounts::switch_work::SwitchTarget {
            account_id: "selected".into(),
            prepared: true,
            not_before: None,
            interrupt_busy: false,
        },
    );
    reg.cancel_failed_account_switch(session).unwrap();
    assert!(lock(&reg.account_work.pending).is_empty());
    assert!(lock(&reg.account_handoffs).is_empty());
    lock(&reg.account_handoffs).insert(
        session,
        SessionHandoff {
            error: Some("fixture failed".into()),
            ..handoff
        },
    );
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
                    size: None,
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
            assert_eq!(old, SessionId(11));
            holder_link
                .send(&FromHolder::Done { req }.encode().unwrap(), &[])
                .unwrap();
            lock(&hook_reg.state).exited.insert(old, 0);
            Some((id, process, replacement_process))
        });

        // The live original keeps its WAL; only an open rollback journal means a write.
        let conversation = from_agy.join("conversations");
        fs::write(conversation.join(format!("{sid}.db-wal")), b"frames").unwrap();
        fs::write(conversation.join(format!("{sid}.db-shm")), b"index").unwrap();
        let journal = conversation.join(format!("{sid}.db-journal"));
        fs::write(&journal, b"active writer").unwrap();
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
        fs::remove_file(journal).unwrap();
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
            fs::read(to_agy.join("conversations").join(format!("{sid}.db-wal"))).unwrap(),
            b"frames"
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
fn switch_waits_for_working_turn_and_never_injects_continuation() {
    use krill_proto::hook::{EventId, HookEvent, Input};
    use std::io::Read as _;

    for (rate_limited, typed) in [(false, false), (true, false), (true, true)] {
        let root = Path::new("/tmp").join(format!(
            "kas-autocontinue-{}-{rate_limited}-{typed}",
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
                    size: None,
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
            if typed {
                let deadline = Instant::now() + Duration::from_secs(2);
                let tap = loop {
                    if let Some(tap) = lock(&hook_reg.state)
                        .sessions
                        .get(&id)
                        .map(|e| e.session.tap())
                    {
                        break tap;
                    }
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                };
                tap.write_input(b"user draft", None).unwrap();
                process
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut draft = [0; 10];
                process.read_exact(&mut draft).unwrap();
                assert_eq!(&draft, b"user draft");
            }
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
                .set_read_timeout(Some(Duration::from_millis(300)))
                .unwrap();
            let mut byte = [0; 1];
            assert!(
                process.read(&mut byte).is_err(),
                "held app prompts must never be preceded by synthetic text or Enter"
            );
            (id, process)
        });

        reg.queue_account_switch(ProviderKind::Codex, &target);
        reg.process_account_work_in(&home);
        if !rate_limited {
            assert_eq!(
                lock(&reg.state).sessions.len(),
                1,
                "working session must not spawn a replacement"
            );
            assert!(!lock(&reg.state).closing.contains(&SessionId(21)));
            assert!(
                load(Some(&state))
                    .get(&SessionId(21))
                    .is_some_and(|h| h.to.is_none() && h.error.is_none())
            );
            let stop = HookEvent {
                id: EventId {
                    pid: 721,
                    unix_ns: 3,
                },
                ppid: 721,
                agent: "codex".into(),
                input: Input::Complete,
                payload: format!("{{\"hook_event_name\":\"Stop\",\"session_id\":\"{sid}\"}}")
                    .into_bytes(),
            };
            reg.status.hook(&[(SessionId(21), &stop)]).unwrap();
            lock(&reg.account_work.pending)
                .get_mut(&SwitchScope::Session(SessionId(21), ProviderKind::Codex))
                .unwrap()
                .not_before = None;
            reg.process_account_work_in(&home);
        }

        let (_new_id, _process) = mock.join().unwrap();

        reader_worker.join().unwrap();
        drop(reg);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn unconfirmed_replacement_retires_original_immediately_without_input() {
    use std::io::Read as _;

    let root = Path::new("/tmp").join(format!("kas-unconfirmed-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let from = root.join("from");
    let to = root.join("to");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(from.join("sessions/2026/10/28")).unwrap();
    fs::create_dir_all(&to).unwrap();
    fs::write(to.join("auth.json"), "fixture credentials").unwrap();
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
                size: None,
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
    reg.agents.mark_rate_limited(SessionId(31));
    let reader = Arc::clone(&reg);
    let reader_worker =
        std::thread::spawn(move || reader.holder.as_ref().unwrap().run(link, |_, _| {}));
    let hook_reg = Arc::clone(&reg);
    let mock = std::thread::spawn(move || {
        let packet = holder_link.recv().unwrap().unwrap();
        let ToHolder::Spawn { req, id, argv, .. } = ToHolder::decode(packet.frame()).unwrap()
        else {
            panic!("expected replacement spawn");
        };
        assert!(argv.iter().any(|arg| arg == sid.as_bytes()));
        let (master, mut process) = UnixStream::pair().unwrap();
        holder_link
            .send(
                &FromHolder::Spawned { req, id, pid: 831 }.encode().unwrap(),
                &[master.as_fd()],
            )
            .unwrap();
        // An idle Codex resume: output, but no hook and no open rollout file.
        process.write_all(b"Resuming session").unwrap();
        let packet = holder_link.recv().unwrap().unwrap();
        let ToHolder::Terminate { req, id: old } = ToHolder::decode(packet.frame()).unwrap() else {
            panic!("expected immediate retirement of the original");
        };
        assert_eq!(old, SessionId(31));
        holder_link
            .send(&FromHolder::Done { req }.encode().unwrap(), &[])
            .unwrap();
        lock(&hook_reg.state).exited.insert(old, 0);
        process
            .set_read_timeout(Some(Duration::from_millis(1500)))
            .unwrap();
        let mut byte = [0_u8; 1];
        assert!(
            process.read(&mut byte).is_err(),
            "the replacement must not receive injected input"
        );
        (id, process)
    });

    reg.queue_account_switch(ProviderKind::Codex, &target);
    reg.process_account_work_in(&home);
    let (replacement, _process) = mock.join().unwrap();
    let saved = load(Some(&state)).get(&SessionId(31)).cloned().unwrap();
    assert!(saved.error.is_none(), "unexpected error: {:?}", saved.error);
    assert_eq!(saved.to, Some(replacement));
    assert_eq!(
        reg.agents
            .stored(replacement)
            .unwrap()
            .unwrap()
            .account
            .unwrap()
            .id,
        target
    );
    reader_worker.join().unwrap();
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn closing_a_session_cancels_its_queued_switch_and_stops_an_unconfirmed_replacement() {
    for in_flight in [true, false] {
        let root = Path::new("/tmp").join(format!(
            "kas-close-queued-{}-{in_flight}",
            std::process::id()
        ));
        fs::remove_dir_all(&root).ok();
        let state = root.join("state");
        let home = root.join("home");
        let from = root.join("from");
        let to = root.join("to");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(from.join("sessions/2026/10/28")).unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(to.join("auth.json"), "fixture credentials").unwrap();
        let target = accounts::add_account(
            &home,
            Some(&state),
            ProviderKind::Codex,
            "Selected",
            None,
            to.to_str().unwrap(),
        )
        .unwrap()
        .accounts
        .into_iter()
        .find(|a| a.label == "Selected")
        .unwrap()
        .id;
        let sid = "cccccccc-dddd-eeee-ffff-000000000001";
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
        let old = SessionId(31);
        let reg = Arc::new(Registry::new(
            Some((
                holder,
                vec![Kept {
                    id: old,
                    pid: 731,
                    status: None,
                    size: None,
                    master: OwnedFd::from(master),
                }],
            )),
            Some(&state),
        ));
        reg.agents
            .remember_session(
                old,
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
        reg.agents.mark_rate_limited(old);
        let reader = Arc::clone(&reg);
        let reader_worker =
            std::thread::spawn(move || reader.holder.as_ref().unwrap().run(link, |_, _| {}));
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        let close_old = {
            let reg = Arc::clone(&reg);
            move || reg.close(&peer, old).unwrap()
        };
        let next_terminate = |link: &mut Link| loop {
            let packet = link.recv().unwrap().unwrap();
            match ToHolder::decode(packet.frame()).unwrap() {
                ToHolder::Release { .. } => {}
                ToHolder::Terminate { req, id } => {
                    link.send(&FromHolder::Done { req }.encode().unwrap(), &[])
                        .unwrap();
                    return id;
                }
                other => panic!("unexpected holder request: {other:?}"),
            }
        };

        reg.queue_account_switch(ProviderKind::Codex, &target);
        let mock = if in_flight {
            let mock_reg = Arc::clone(&reg);
            std::thread::spawn(move || {
                let packet = holder_link.recv().unwrap().unwrap();
                let ToHolder::Spawn { req, id: new, .. } =
                    ToHolder::decode(packet.frame()).unwrap()
                else {
                    panic!("expected replacement spawn");
                };
                let (master, mut process) = UnixStream::pair().unwrap();
                holder_link
                    .send(
                        &FromHolder::Spawned {
                            req,
                            id: new,
                            pid: 831,
                        }
                        .encode()
                        .unwrap(),
                        &[master.as_fd()],
                    )
                    .unwrap();
                let closer = std::thread::spawn(close_old);
                // Cancelling lets the attempt stop its replacement before the close reaches the holder.
                let first = next_terminate(&mut holder_link);
                mock_reg.on_exited(first, 0);
                process.write_all(b"Resuming session").ok();
                let second = next_terminate(&mut holder_link);
                mock_reg.on_exited(second, 0);
                closer.join().unwrap();
                let mut ended = [first, second];
                ended.sort();
                assert_eq!(ended, [old, new]);
            })
        } else {
            let closer = std::thread::spawn(close_old);
            assert_eq!(next_terminate(&mut holder_link), old);
            closer.join().unwrap();
            std::thread::spawn(move || drop(holder_link))
        };

        reg.process_account_work_in(&home);

        let saved = load(Some(&state));
        assert!(
            !saved.contains_key(&old),
            "in_flight={in_flight}: closed session keeps a switch record: {:?}",
            saved.get(&old)
        );
        assert!(lock(&reg.account_work.pending).is_empty());
        assert!(lock(&reg.account_replacements).is_empty());
        mock.join().unwrap();
        assert!(
            !fs::read_dir(state.join("account-replacements"))
                .is_ok_and(|mut journals| journals.next().is_some()),
            "in_flight={in_flight}: replacement journal survived"
        );
        reader_worker.join().unwrap();
        drop(reg);
        fs::remove_dir_all(root).unwrap();
    }
}

// Codex 0.166 shares `thread-writer-locks` across profiles. A resumed TUI that finds the
// original still writing shows a lock prompt and opens nothing until the user presses r.
#[test]
fn codex_replacement_blocked_by_the_original_writer_takes_over_after_retirement() {
    check_codex_writer_takeover(false);
}

// Measured with codex-cli 0.166.0: first output at about 15 ms, lock prompt at about 410 ms. The
// output gate retires the original first, so the prompt can appear only after retirement began.
#[test]
fn codex_lock_prompt_painted_after_retirement_still_gets_the_retry_key() {
    check_codex_writer_takeover(true);
}

#[expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "test fixture setup requires valid local sockets and files"
)]
fn check_codex_writer_takeover(prompt_after_terminate: bool) {
    use std::io::Read as _;

    let root = Path::new("/tmp").join(format!(
        "kas-writer-lock-{prompt_after_terminate}-{}",
        std::process::id()
    ));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let from = root.join("from");
    let to = root.join("to");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(from.join("sessions/2026/11/03")).unwrap();
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
    let sid = "cccccccc-dddd-eeee-ffff-222222222222";
    let transcript = from.join(format!("sessions/2026/11/03/rollout-date-{sid}.jsonl"));
    fs::write(
        &transcript,
        format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{sid}\"}}}}\n"),
    )
    .unwrap();
    let original_proc = FileHolder::new(&transcript);
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
                id: SessionId(41),
                pid: original_proc.0.id(),
                status: None,
                size: None,
                master: OwnedFd::from(master),
            }],
        )),
        Some(&state),
    ));
    reg.agents
        .remember_session(
            SessionId(41),
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
    let hook_reg = Arc::clone(&reg);
    let expected_transcript = to
        .join("sessions/2026/11/03")
        .join(transcript.file_name().unwrap());
    let released = root.join("original-released");
    let resumed_transcript = expected_transcript.clone();
    let mock = std::thread::spawn(move || {
        let packet = holder_link.recv().unwrap().unwrap();
        let ToHolder::Spawn { req, id, argv, .. } = ToHolder::decode(packet.frame()).unwrap()
        else {
            panic!("expected replacement spawn");
        };
        assert!(argv.iter().any(|arg| arg == sid.as_bytes()));
        // The resumed CLI opens the conversation only once the writer lock is free.
        let replacement_proc = FileHolder(
            std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    "while [ ! -e \"$1\" ]; do sleep 0.02; done; exec 3<\"$2\"; read -r line",
                    "blocked-resume",
                ])
                .arg(&released)
                .arg(&resumed_transcript)
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
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
        let prompt: &[u8] =
            b"This conversation is open in another app\r\nClose it there and press R to continue here.\r\n";
        process
            .write_all(if prompt_after_terminate {
                b"Resuming session"
            } else {
                prompt
            })
            .unwrap();
        let packet = holder_link.recv().unwrap().unwrap();
        let ToHolder::Terminate { req, id: retired } = ToHolder::decode(packet.frame()).unwrap()
        else {
            panic!("expected a terminate request");
        };
        holder_link
            .send(&FromHolder::Done { req }.encode().unwrap(), &[])
            .unwrap();
        lock(&hook_reg.state).exited.insert(retired, 0);
        drop(original_proc);
        if prompt_after_terminate {
            process.write_all(prompt).unwrap();
        }
        process
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut key = [0_u8; 1];
        let retried = process
            .read(&mut key)
            .is_ok_and(|n| n == 1 && key[0] == b'r');
        if retried {
            fs::write(&released, b"").unwrap();
        }
        (id, retired, retried, process, replacement_proc)
    });

    reg.queue_account_switch(ProviderKind::Codex, &target);
    reg.process_account_work_in(&home);
    let (replacement, retired, retried, _process, _replacement_proc) = mock.join().unwrap();
    assert_eq!(
        retired,
        SessionId(41),
        "the original writer must be retired, not the waiting replacement"
    );
    assert!(
        retried,
        "the replacement must be told to retry once the lock is free"
    );
    let saved = load(Some(&state));
    let handoff = saved.get(&SessionId(41)).unwrap();
    assert_eq!(handoff.error, None);
    assert_eq!(handoff.to, Some(replacement));
    let bound = reg.agents.stored(replacement).unwrap().unwrap();
    assert_eq!(
        bound.transcript_path.map(|p| fs::canonicalize(p).unwrap()),
        Some(expected_transcript.canonicalize().unwrap()),
        "the replacement's held conversation is bound after takeover"
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
                    size: None,
                    master: OwnedFd::from(master_454),
                },
                Kept {
                    id: SessionId(455),
                    pid: open_transcript_proc.0.id(),
                    status: None,
                    size: None,
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
                    size: None,
                    master: OwnedFd::from(master_51),
                },
                Kept {
                    id: SessionId(52),
                    pid: 952,
                    status: None,
                    size: None,
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
    // Backdate Session 51's launch record (both JSON `launched_at` and file timestamps) so it is
    // not treated as mid-startup (< 5s).
    let launch_path = state.join("agent-launches/51.json");
    let mut stored: serde_json::Value =
        serde_json::from_slice(&fs::read(&launch_path).unwrap()).unwrap();
    let old_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(30);
    stored["launched_at"] = serde_json::json!(old_secs);
    fs::write(&launch_path, serde_json::to_vec(&stored).unwrap()).unwrap();
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::FileTimesExt as _;
        let old_time = std::time::SystemTime::now() - Duration::from_secs(30);
        let times = fs::FileTimes::new()
            .set_created(old_time)
            .set_modified(old_time);
        fs::File::options()
            .write(true)
            .open(&launch_path)
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
            interrupt_busy: false,
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
            false,
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
    assert!(
        fs::read_dir(&to)
            .unwrap()
            .all(|entry| entry.unwrap().file_name() == "auth.json")
    );
    drop(reg);
    fs::remove_dir_all(root).unwrap();
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
                size: None,
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
        disposition: ReplacementDisposition::Committed,
        bot_grant_token: None,
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
                    interrupt_busy: false,
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

#[test]
fn unreadable_replacement_evidence_is_preserved_and_blocks_new_spawns() {
    let root = Path::new("/tmp").join(format!("kas-evidence-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();
    let reg = Registry::new(None, Some(&root));
    let path = replacement_journal(&root, SessionId(8));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    for bytes in [
        b"broken".to_vec(),
        vec![b'x'; 4097],
        serde_json::to_vec(&Prepared {
            from: SessionId(7),
            to: SessionId(7),
            account_id: "fixture".into(),
        })
        .unwrap(),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(reg.recover_prepared_account().blocks(SessionId(7)));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(
            reg.ensure_no_unresolved_account_replacement(SessionId(7))
                .is_err()
        );
    }
    fs::write(
        &path,
        serde_json::to_vec(&Prepared {
            from: SessionId(7),
            to: SessionId(8),
            account_id: "fixture".into(),
        })
        .unwrap(),
    )
    .unwrap();
    assert!(
        reg.ensure_no_unresolved_account_replacement(SessionId(7))
            .is_err()
    );
    assert!(
        reg.ensure_no_unresolved_account_replacement(SessionId(8))
            .is_err()
    );
    assert!(
        reg.ensure_no_unresolved_account_replacement(SessionId(9))
            .is_ok()
    );
    // Unknown Spawn outcomes are not evidence that a target process ended.
    drop(Replacement {
        registry: &reg,
        original: SessionId(7),
        session: SessionId(8),
        disposition: ReplacementDisposition::Unresolved,
        bot_grant_token: None,
    });
    assert!(path.exists());
    fs::remove_file(&path).unwrap();
    fs::remove_dir(path.parent().unwrap()).unwrap();
    fs::write(root.join("account-replacements"), b"not a directory").unwrap();
    assert!(reg.recover_prepared_account().blocks(SessionId(9)));
    assert!(
        reg.ensure_no_unresolved_account_replacement(SessionId(9))
            .is_err()
    );
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn replacement_cleanup_failure_or_done_without_exit_retains_journal_and_fences_retry() {
    for done in [false, true] {
        let root = Path::new("/tmp").join(format!("kas-cleanup-{}-{done}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
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
        let (holder, reader, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let mut processes = Vec::new();
        let kept = [SessionId(7), SessionId(8)]
            .map(|id| {
                let (master, process) = UnixStream::pair().unwrap();
                processes.push(process);
                Kept {
                    id,
                    pid: 800,
                    status: None,
                    size: None,
                    master: OwnedFd::from(master),
                }
            })
            .into();
        let reg = Arc::new(Registry::new(Some((holder, kept)), Some(&root)));
        for id in [SessionId(7), SessionId(8)] {
            reg.agents
                .remember_session(
                    id,
                    &AgentLaunch {
                        profile: "codex".into(),
                        permission: Permission::Yolo,
                    },
                    Some("conversation".into()),
                    Vec::new(),
                    Some(LaunchAccount {
                        id: "selected".into(),
                        env_key: "CODEX_HOME".into(),
                        dir: root.display().to_string(),
                    }),
                )
                .unwrap();
        }
        let path = replacement_journal(&root, SessionId(8));
        let bytes = serde_json::to_vec(&Prepared {
            from: SessionId(7),
            to: SessionId(8),
            account_id: "selected".into(),
        })
        .unwrap();
        accounts::write_atomic_0600(&path, &bytes).unwrap();
        let r = Arc::clone(&reg);
        let reader_worker =
            std::thread::spawn(move || r.holder.as_ref().unwrap().run(reader, |_, _| {}));
        let mock = std::thread::spawn(move || {
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Terminate { req, id } = ToHolder::decode(packet.frame()).unwrap() else {
                panic!("expected cleanup only")
            };
            assert_eq!(id, SessionId(8));
            let reply = if done {
                FromHolder::Done { req }
            } else {
                FromHolder::Failed {
                    req,
                    errno: libc::EPERM,
                }
            };
            holder_link.send(&reply.encode().unwrap(), &[]).unwrap();
        });
        let fence = reg.recover_prepared_account();
        assert!(fence.blocks(SessionId(7)));
        assert!(fence.blocks(SessionId(8)));
        assert!(!fence.blocks(SessionId(9)));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(
            !reg.account_original_ended(SessionId(8)),
            "Done must not be treated as Exited"
        );
        assert!(
            reg.ensure_no_unresolved_account_replacement(SessionId(7))
                .is_err()
        );
        assert!(
            reg.ensure_no_unresolved_account_replacement(SessionId(8))
                .is_err()
        );
        mock.join().unwrap();
        reader_worker.join().unwrap();
        drop(processes);
        drop(reg);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn failed_kept_engine_start_is_not_evidence_that_a_replacement_process_ended() {
    let root = Path::new("/tmp").join(format!("kas-kept-unknown-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let (reg, _process) = kept_fixture(&root, SessionId(7));
    lock(&reg.state).unregistered_kept.insert(SessionId(8));
    let path = replacement_journal(&root, SessionId(8));
    let raw = serde_json::to_vec(&Prepared {
        from: SessionId(7),
        to: SessionId(8),
        account_id: "selected".into(),
    })
    .unwrap();
    accounts::write_atomic_0600(&path, &raw).unwrap();
    assert!(!reg.account_original_ended(SessionId(8)));
    assert!(reg.recover_prepared_account().blocks(SessionId(7)));
    assert_eq!(fs::read(&path).unwrap(), raw);
    assert!(
        reg.ensure_no_unresolved_account_replacement(SessionId(7))
            .is_err()
    );
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

// A registry without its own state folder must not overwrite or obey the recovery records of the
// daemon that owns the process-default folder (`KRILL_STATE_DIR` is the operational one in a session).
#[test]
fn stateless_registry_leaves_the_default_folder_account_records_alone() {
    const ROLE: &str = "KRILL_TEST_STATELESS_ACCOUNT_RECORDS";
    if std::env::var_os(ROLE).is_some() {
        let reg = Registry::new(None, None);
        lock(&reg.account_handoffs).insert(
            SessionId(1),
            SessionHandoff {
                from: SessionId(1),
                to: None,
                account_id: "stateless".into(),
                error: None,
            },
        );
        reg.save_account_handoffs().unwrap();
        assert!(
            reg.ensure_no_unresolved_account_replacement(SessionId(8))
                .is_ok(),
            "another daemon's replacement journal blocked this registry"
        );
        return;
    }
    let root = std::env::temp_dir().join(format!("kas-stateless-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(root.join("account-replacements")).unwrap();
    let handoffs = br#"[{"from":40332,"to":40351,"account_id":"operational","error":null}]"#;
    fs::write(root.join(FILE), handoffs).unwrap();
    fs::write(
        root.join("account-replacements/8.json"),
        br#"{"from":7,"to":8,"account_id":"operational"}"#,
    )
    .unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "registry::account_switch::tests::stateless_registry_leaves_the_default_folder_account_records_alone",
            "--nocapture",
        ])
        .env(ROLE, "1")
        .env("KRILL_STATE_DIR", &root)
        .env("HOME", root.join("home"))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .status()
        .unwrap();
    let after = fs::read(root.join(FILE)).unwrap();
    fs::remove_dir_all(&root).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&after),
        String::from_utf8_lossy(handoffs),
        "the default folder's handoff records were overwritten"
    );
    assert!(status.success());
}

#[test]
fn dead_retained_replacement_handoff_is_hidden_and_pruned_preserving_session_id_floor() {
    let root = std::env::temp_dir().join(format!("kas-retained-dead-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let retained = SessionHandoff {
        from: SessionId(40_351),
        to: None,
        account_id: "codex-alpha".into(),
        error: Some(
            "Original session ended; unconfirmed replacement 40373 was retained. Select it explicitly to continue; held input was not sent"
                .into(),
        ),
    };
    accounts::write_atomic_0600(&root.join(FILE), &serde_json::to_vec(&[retained]).unwrap())
        .unwrap();
    assert!(!root.join("session-id-sequence.json").exists());

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
    let (holder, _reader, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let reg = Registry::new(Some((holder, Vec::new())), Some(&root));

    let mut snapshot = AccountsSnapshot::default();
    reg.account_switch_snapshot(&mut snapshot);
    assert!(
        snapshot.details.handoffs.is_empty(),
        "dead retained replacement handoff must be hidden: {:?}",
        snapshot.details.handoffs
    );

    reg.recover_account_work(&home);
    assert!(
        load(Some(&root)).is_empty(),
        "recover_account_work must prune dead retained handoff from account-handoffs.json"
    );

    let next = reg.reserve_session_id(None).unwrap();
    assert!(
        next >= SessionId(40_374),
        "expected session ID >= 40374 after reserving retained 40373, got {next:?}"
    );

    drop(reg);
    drop(holder_link);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_retained_replacement_clears_on_interactive_selection_replacement_close_or_manual_cancel() {
    use krill_proto::control::Reply;
    use krill_proto::ipc::{self, ClientRole, ServerHello};
    use krill_proto::roles::{self, Role};
    use krill_proto::screen::{Attach, Msg as _, Stream, kind as screen_kind};
    use krill_proto::usage::{UsageReply, UsageRequest};

    let attach_screen = |reg: &Arc<Registry>, role: Role, session: SessionId| {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let serving_reg = Arc::clone(reg);
        let serving = std::thread::spawn(move || {
            serving_reg.serve(
                server,
                &ServerHello {
                    proto_min: ipc::PROTO_MIN,
                    proto_max: ipc::PROTO_MAX,
                    features: ipc::FEATURES,
                    build: "test".into(),
                    instance_id: 1,
                },
            )
        });
        ipc::read_frame(&mut client).unwrap().unwrap();
        let hello = krill_proto::daemon::client_hello(ClientRole::App, "test");
        client.write_all(&hello.encode().unwrap()).unwrap();
        client
            .write_all(&roles::Request::Set(role).encode().unwrap())
            .unwrap();
        let (ack_kind, ack_body) = ipc::read_frame(&mut client).unwrap().unwrap();
        assert_eq!(
            roles::Ack::decode(ack_kind, &ack_body).unwrap(),
            roles::Ack::Applied
        );
        client
            .write_all(
                &Attach {
                    session,
                    stream: Stream::Screen { known: None },
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            ipc::read_frame(&mut client).unwrap().unwrap().0,
            screen_kind::ATTACHED
        );
        // OWE-530: macOS는 shutdown만으로는 상대의 막힌 read를 드물게 깨우지 못하므로
        // join 전에 소켓을 먼저 닫는다. 초기 keyframe 기록 중 닫히면 BrokenPipe가 날 수 있다.
        client.shutdown(std::net::Shutdown::Both).ok();
        drop(client);
        serving.join().unwrap().ok();
    };

    for mode in ["interactive_attach", "close_replacement", "manual_cancel"] {
        let root =
            std::env::temp_dir().join(format!("kas-retained-live-{}-{mode}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
        let retained = SessionHandoff {
            from: SessionId(7),
            to: None,
            account_id: "codex-alpha".into(),
            error: Some(
                "Original session ended; unconfirmed replacement 8 was retained. Select it explicitly to continue; held input was not sent"
                    .into(),
            ),
        };
        accounts::write_atomic_0600(&root.join(FILE), &serde_json::to_vec(&[retained]).unwrap())
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
        let pty = rustix_openpty::openpty(None, None).unwrap();
        let replacement_process = pty.user;
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let reg = Arc::new(Registry::new(
            Some((
                holder,
                vec![Kept {
                    id: SessionId(8),
                    pid: 800,
                    status: None,
                    size: None,
                    master: pty.controller,
                }],
            )),
            Some(&root),
        ));
        let (exited_tx, exited_rx) = std::sync::mpsc::channel();
        let reader = Arc::clone(&reg);
        let reader_worker = std::thread::spawn(move || {
            reader.holder.as_ref().unwrap().run(link, |id, status| {
                reader.on_exited(id, status);
                exited_tx.send(id).ok();
            });
        });

        let mut initial = AccountsSnapshot::default();
        reg.account_switch_snapshot(&mut initial);
        assert_eq!(initial.details.handoffs.len(), 1, "{mode}");
        let handoff = initial.details.handoffs.first().unwrap();
        assert_eq!(handoff.from, SessionId(7), "{mode}");
        assert_eq!(handoff.to, None, "{mode}");
        assert!(
            handoff
                .error
                .as_deref()
                .is_some_and(|e| e.contains("retained")),
            "{mode}"
        );

        attach_screen(&reg, Role::Observer, SessionId(8));
        let mut after_observer = AccountsSnapshot::default();
        reg.account_switch_snapshot(&mut after_observer);
        assert_eq!(
            after_observer.details.handoffs.len(),
            1,
            "observer attach must not clear retained handoff: {mode}"
        );

        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        match mode {
            "interactive_attach" => {
                attach_screen(&reg, Role::Interactive, SessionId(8));
            }
            "close_replacement" => {
                let mock = std::thread::spawn(move || {
                    let packet = holder_link.recv().unwrap().unwrap();
                    let ToHolder::Terminate { req, id } = ToHolder::decode(packet.frame()).unwrap()
                    else {
                        panic!("expected Terminate for replacement 8");
                    };
                    assert_eq!(id, SessionId(8));
                    holder_link
                        .send(&FromHolder::Done { req }.encode().unwrap(), &[])
                        .unwrap();
                    holder_link
                        .send(
                            &FromHolder::Exited {
                                id: SessionId(8),
                                status: 0,
                            }
                            .encode()
                            .unwrap(),
                            &[],
                        )
                        .unwrap();
                    let packet = holder_link.recv().unwrap().unwrap();
                    let ToHolder::Release { id, .. } = ToHolder::decode(packet.frame()).unwrap()
                    else {
                        panic!("expected Release for replacement 8");
                    };
                    assert_eq!(id, SessionId(8));
                    holder_link
                });
                assert_eq!(reg.close(&peer, SessionId(8)), Ok(Reply::Closed));
                holder_link = mock.join().unwrap();
                assert_eq!(
                    exited_rx.recv_timeout(Duration::from_secs(3)),
                    Ok(SessionId(8))
                );
            }
            "manual_cancel" => {
                let reply = reg.on_usage(
                    &peer,
                    Role::Interactive,
                    UsageRequest::CancelAccountSwitch {
                        session: SessionId(7),
                    },
                );
                assert!(
                    matches!(reply, UsageReply::Accounts(_)),
                    "expected UsageReply::Accounts, got {reply:?}"
                );
            }
            _ => unreachable!(),
        }

        let mut cleared = AccountsSnapshot::default();
        reg.account_switch_snapshot(&mut cleared);
        assert!(
            cleared.details.handoffs.is_empty(),
            "expected cleared handoffs in snapshot for {mode}, got {:?}",
            cleared.details.handoffs
        );
        assert!(
            load(Some(&root)).is_empty(),
            "expected cleared account-handoffs.json for {mode}"
        );

        drop(replacement_process);
        drop(holder_link);
        reader_worker.join().unwrap();
        drop(reg);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn retained_replacement_disposition_unlinks_journal_only_when_persisted_without_stopping_session() {
    let root = std::env::temp_dir().join(format!("kas-retained-disp-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let (reg, _process) = kept_fixture(&root, SessionId(8));
    let journal = replacement_journal(&root, SessionId(8));
    accounts::write_atomic_0600(&journal, b"fixture").unwrap();

    lock(&reg.account_replacements).insert(SessionId(8));
    drop(Replacement {
        registry: &reg,
        original: SessionId(7),
        session: SessionId(8),
        disposition: ReplacementDisposition::Retained { persisted: false },
        bot_grant_token: None,
    });
    assert!(journal.exists());
    assert!(lock(&reg.state).sessions.contains_key(&SessionId(8)));
    assert!(!lock(&reg.state).closing.contains(&SessionId(8)));

    lock(&reg.account_replacements).insert(SessionId(8));
    drop(Replacement {
        registry: &reg,
        original: SessionId(7),
        session: SessionId(8),
        disposition: ReplacementDisposition::Retained { persisted: true },
        bot_grant_token: None,
    });
    assert!(!journal.exists());
    assert!(lock(&reg.state).sessions.contains_key(&SessionId(8)));
    assert!(!lock(&reg.state).closing.contains(&SessionId(8)));

    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn chat_poll_discovers_claude_from_process_record_when_transcript_closed() {
    let root = std::env::temp_dir().join(format!("kas-chat-poll-claude-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let profile = root.join("claude-profile");
    let projects = profile.join("projects/work");
    let sessions_dir = profile.join("sessions");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&projects).unwrap();
    fs::create_dir_all(&sessions_dir).unwrap();

    let snap = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Claude Profile",
        None,
        profile.to_str().unwrap(),
    )
    .unwrap();
    let account_id = snap
        .accounts
        .iter()
        .find(|a| a.label == "Claude Profile")
        .unwrap()
        .id
        .clone();

    let sid = "3b71560c-6c71-4dce-86d0-5ce025eac068";
    let transcript = projects.join(format!("{sid}.jsonl"));
    fs::write(
        &transcript,
        format!("{{\"sessionId\":\"{sid}\",\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":\"Process record reply\"}}}}\n"),
    )
    .unwrap();

    // Spawn a real process named `claude` that does NOT hold `transcript` open (matching Windows
    // and closed-JSONL behavior between Claude writes).
    let bin = root.join("claude");
    fs::copy("/bin/sleep", &bin).unwrap();
    let mut child = std::process::Command::new(&bin).arg("30").spawn().unwrap();
    let pid = child.id();
    let ps_out = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    let birth = String::from_utf8_lossy(&ps_out.stdout).trim().to_owned();
    fs::write(
        sessions_dir.join(format!("{pid}.json")),
        serde_json::json!({
            "pid": pid,
            "procStart": birth,
            "sessionId": sid,
        })
        .to_string(),
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
    let (master, _process) = UnixStream::pair().unwrap();
    let (holder, _reader, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let reg = Arc::new(Registry::new(
        Some((
            holder,
            vec![Kept {
                id: SessionId(88),
                pid,
                status: None,
                size: None,
                master: OwnedFd::from(master),
            }],
        )),
        Some(&state),
    ));
    reg.agents
        .remember_session(
            SessionId(88),
            &AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Manual,
            },
            None,
            root.as_os_str().as_encoded_bytes().to_vec(),
            Some(LaunchAccount {
                id: account_id,
                env_key: "CLAUDE_CONFIG_DIR".into(),
                dir: profile.display().to_string(),
            }),
        )
        .unwrap();

    let poll = reg.chat_poll(SessionId(88), 0);
    child.kill().ok();
    child.wait().ok();
    drop(holder_link);
    drop(reg);
    fs::remove_dir_all(root).unwrap();

    assert!(
        poll.bound,
        "chat_poll must recover Claude session identity via process record when transcript is closed: {poll:?}"
    );
    assert_eq!(poll.session_id, sid);
    assert_eq!(
        poll.items
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Process record reply"]
    );
}

#[test]
fn multi_session_straggler_rate_limit_requeues_host_default_and_shared_claude_sessions() {
    use krill_proto::hook::{EventId, HookEvent, Input};
    use krill_proto::usage::{AccountStatus, AutoSwitchPolicy, AutoSwitchRule};

    let root = std::env::temp_dir().join(format!("kas-straggler-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let host_claude = home.join(".claude");
    let host_projects = host_claude.join("projects/repo");
    let healthy = root.join("claude-healthy");
    let old_isolated = root.join("claude-old");
    let disabled_isolated = root.join("claude-disabled");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&host_projects).unwrap();
    fs::create_dir_all(&healthy).unwrap();
    fs::create_dir_all(&old_isolated).unwrap();
    fs::create_dir_all(&disabled_isolated).unwrap();
    // Share `projects` between host default and the healthy target profile to verify that
    // `keep_active_when_shared` never misattributes or poisons the healthy target account.
    #[cfg(unix)]
    std::os::unix::fs::symlink(host_claude.join("projects"), healthy.join("projects")).unwrap();

    // Prevent background login CLI probes against any host binary.
    let mut profiles: Option<String> = None;
    for name in ["claude", "codex", "agy", "opencode"] {
        let profile = krill_proto::agent::Profile {
            name: name.into(),
            launch: vec![root.join("uninstalled").join(name).display().to_string()],
            yolo_args: Vec::new(),
            install: String::new(),
            install_windows: String::new(),
            env: std::collections::BTreeMap::new(),
        };
        profiles = Some(
            krill_proto::agent::set_user_profile(profiles.as_deref(), name, &profile).unwrap(),
        );
    }
    fs::write(state.join(krill_proto::agent::FILE_NAME), profiles.unwrap()).unwrap();

    fs::write(
        host_claude.join(crate::usage::CLAUDE_CACHE),
        r#"{"five_hour":{"utilization":50}}"#,
    )
    .unwrap();
    fs::write(
        healthy.join(crate::usage::CLAUDE_CACHE),
        r#"{"five_hour":{"utilization":10}}"#,
    )
    .unwrap();

    accounts::set_auto_switch(
        &home,
        Some(&state),
        &AutoSwitchPolicy {
            enabled: true,
            switch_on_rate_limit: true,
            ..AutoSwitchPolicy::default()
        },
    )
    .unwrap();
    let snap = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Healthy",
        None,
        healthy.to_str().unwrap(),
    )
    .unwrap();
    let healthy_id = snap
        .accounts
        .iter()
        .find(|a| a.label == "Healthy")
        .unwrap()
        .id
        .clone();
    assert_eq!(
        accounts::load_file(&home, Some(&state))
            .active
            .get("claude")
            .map(String::as_str),
        None
    );

    let (reg, _processes) = kept_sessions_fixture(
        &state,
        &[
            SessionId(101),
            SessionId(102),
            SessionId(103),
            SessionId(104),
            SessionId(105),
        ],
    );
    let sid_101 = "11111111-1111-4111-8111-111111111101";
    let sid_102 = "11111111-1111-4111-8111-111111111102";
    let tp_101 = host_projects.join(format!("{sid_101}.jsonl"));
    let tp_102 = host_projects.join(format!("{sid_102}.jsonl"));
    fs::write(&tp_101, format!("{{\"sessionId\":\"{sid_101}\"}}\n")).unwrap();
    fs::write(&tp_102, format!("{{\"sessionId\":\"{sid_102}\"}}\n")).unwrap();

    for (session, sid, tp) in [
        (SessionId(101), sid_101, &tp_101),
        (SessionId(102), sid_102, &tp_102),
    ] {
        let start_payload = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": sid,
            "transcript_path": tp,
        }))
        .unwrap();
        assert!(!reg.agents.record_hook_session_with_context(
            session,
            &start_payload,
            Some("claude"),
            None,
        ));
    }
    reg.agents
        .remember_session(
            SessionId(103),
            &AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Manual,
            },
            Some("sid-103".into()),
            Vec::new(),
            Some(LaunchAccount {
                id: "claude-old".into(),
                env_key: "CLAUDE_CONFIG_DIR".into(),
                dir: old_isolated.display().to_string(),
            }),
        )
        .unwrap();
    lock(&reg.account_switch_opt_out).insert(SessionId(103));
    lock(&reg.account_handoffs).insert(
        SessionId(103),
        SessionHandoff {
            from: SessionId(103),
            to: None,
            account_id: healthy_id.clone(),
            error: Some("stale failure before rate limit".into()),
        },
    );
    reg.save_account_handoffs().unwrap();
    lock(&reg.account_work.pending).insert(
        SwitchScope::Session(SessionId(103), ProviderKind::Claude),
        accounts::switch_work::SwitchTarget {
            account_id: healthy_id.clone(),
            prepared: true,
            not_before: Some(Instant::now() + Duration::from_secs(30)),
            interrupt_busy: false,
        },
    );

    reg.agents
        .remember_session(
            SessionId(104),
            &AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Manual,
            },
            Some("sid-104".into()),
            Vec::new(),
            Some(LaunchAccount {
                id: "claude-disabled".into(),
                env_key: "CLAUDE_CONFIG_DIR".into(),
                dir: disabled_isolated.display().to_string(),
            }),
        )
        .unwrap();
    accounts::upsert_auto_switch_rule(
        &home,
        Some(&state),
        AutoSwitchRule {
            provider: ProviderKind::Claude,
            from_account_id: "claude-disabled".into(),
            enabled: false,
            threshold_percent: 90,
            metric: krill_proto::usage::AutoSwitchMetric::Max,
            target_account_id: None,
        },
    )
    .unwrap();

    reg.agents
        .remember_session(
            SessionId(105),
            &AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Manual,
            },
            Some("sid-105".into()),
            Vec::new(),
            Some(LaunchAccount {
                id: "claude-old".into(),
                env_key: "CLAUDE_CONFIG_DIR".into(),
                dir: old_isolated.display().to_string(),
            }),
        )
        .unwrap();

    let rate_limit_event = |unix_ns| HookEvent {
        id: EventId { pid: 100, unix_ns },
        ppid: 100,
        agent: "claude".into(),
        input: Input::Complete,
        payload: br#"{"hook_event_name":"StopFailure","error":"rate_limit"}"#.to_vec(),
    };

    // 1. Session 101 (host default, `account: None`) triggers the initial switch `claude-host` -> `healthy_id`.
    let e1 = rate_limit_event(1);
    assert!(
        reg.apply_agent_hook_in(&home, SessionId(101), &e1, None, None)
            .unwrap()
    );
    reg.auto_switch_from_hook_in(&home, SessionId(101), &e1);
    assert_eq!(
        accounts::load_file(&home, Some(&state))
            .active
            .get("claude")
            .map(String::as_str),
        Some(healthy_id.as_str())
    );
    assert!(
        lock(&reg.account_work.pending).contains_key(&SwitchScope::Provider(ProviderKind::Claude))
    );
    // Simulate worker having already consumed the provider-wide notification.
    lock(&reg.account_work.pending).clear();

    // 2. Straggler Session 102 (`account: None`, shared `projects/`) hits rate limit afterward:
    // must record `claude-host` (never poisoning `healthy_id`) and queue Session 102 -> `healthy_id`.
    let e2 = rate_limit_event(2);
    assert!(
        reg.apply_agent_hook_in(&home, SessionId(102), &e2, None, None)
            .unwrap()
    );
    reg.auto_switch_from_hook_in(&home, SessionId(102), &e2);
    assert!(reg.agents.is_rate_limited(SessionId(102)));
    let after_102 = accounts::refresh_accounts_with(&home, Some(&state), false);
    assert_eq!(
        after_102.status_for("claude-host").map(|s| s.status),
        Some(AccountStatus::RateLimited)
    );
    assert_ne!(
        after_102.status_for(&healthy_id).map(|s| s.status),
        Some(AccountStatus::RateLimited),
        "shared history must never poison the healthy selected account"
    );
    let saved_102 = load(Some(&state));
    let handoff_102 = saved_102.get(&SessionId(102)).expect("queued session 102");
    assert_eq!(handoff_102.account_id, healthy_id);
    assert_eq!(handoff_102.to, None);
    assert_eq!(handoff_102.error, None);
    {
        let pending = lock(&reg.account_work.pending);
        let target = pending
            .get(&SwitchScope::Session(SessionId(102), ProviderKind::Claude))
            .expect("Session 102 queued in pending");
        assert_eq!(target.account_id, healthy_id);
        assert!(target.not_before.is_none());
    }

    // 3. Straggler Session 103 had opt-out, a stale failed handoff error, and a delayed `not_before`:
    // hitting StopFailure clears opt-out & error and queues immediately (`not_before: None`).
    let e3 = rate_limit_event(3);
    assert!(
        reg.apply_agent_hook_in(&home, SessionId(103), &e3, None, None)
            .unwrap()
    );
    reg.auto_switch_from_hook_in(&home, SessionId(103), &e3);
    assert!(!lock(&reg.account_switch_opt_out).contains(&SessionId(103)));
    let saved_103 = load(Some(&state));
    let handoff_103 = saved_103.get(&SessionId(103)).expect("queued session 103");
    assert_eq!(handoff_103.account_id, healthy_id);
    assert_eq!(handoff_103.error, None);
    assert!(
        lock(&reg.account_work.pending)
            .get(&SwitchScope::Session(SessionId(103), ProviderKind::Claude))
            .is_some_and(|t| t.not_before.is_none())
    );

    // 3b. A repeated StopFailure on Session 103 while its switch to `healthy_id` is already
    // pending without error must be a no-op (must not re-insert into `account_work.pending`).
    lock(&reg.account_work.pending)
        .remove(&SwitchScope::Session(SessionId(103), ProviderKind::Claude));
    let e3b = rate_limit_event(31);
    reg.auto_switch_from_hook_in(&home, SessionId(103), &e3b);
    assert!(
        !lock(&reg.account_work.pending)
            .contains_key(&SwitchScope::Session(SessionId(103), ProviderKind::Claude)),
        "active/unfailed pending handoff must not be re-queued on duplicate StopFailure"
    );

    // 3c. If a straggler re-queue fails (e.g. non-live session), opt-out and prior error stay intact.
    let failed_before = SessionHandoff {
        from: SessionId(999),
        to: None,
        account_id: "claude-old".into(),
        error: Some("preserve me".into()),
    };
    lock(&reg.account_switch_opt_out).insert(SessionId(999));
    lock(&reg.account_handoffs).insert(SessionId(999), failed_before);
    let e_missing = rate_limit_event(32);
    reg.auto_switch_from_hook_in(&home, SessionId(999), &e_missing);
    assert!(lock(&reg.account_switch_opt_out).contains(&SessionId(999)));
    assert_eq!(
        lock(&reg.account_handoffs)
            .get(&SessionId(999))
            .and_then(|h| h.error.as_deref()),
        Some("preserve me")
    );
    lock(&reg.account_switch_opt_out).remove(&SessionId(999));
    lock(&reg.account_handoffs).remove(&SessionId(999));

    // 4. Disabled per-account rule blocks straggler auto-queueing for Session 104.
    let e4 = rate_limit_event(4);
    assert!(
        reg.apply_agent_hook_in(&home, SessionId(104), &e4, None, None)
            .unwrap()
    );
    reg.auto_switch_from_hook_in(&home, SessionId(104), &e4);
    assert!(!load(Some(&state)).contains_key(&SessionId(104)));
    assert!(
        !lock(&reg.account_work.pending)
            .contains_key(&SwitchScope::Session(SessionId(104), ProviderKind::Claude))
    );

    // 5. If `healthy_id` is itself rate-limited, Session 105 must not queue a switch into it.
    accounts::record_rate_limited(ProviderKind::Claude, &healthy_id);
    let e5 = rate_limit_event(5);
    assert!(
        reg.apply_agent_hook_in(&home, SessionId(105), &e5, None, None)
            .unwrap()
    );
    reg.auto_switch_from_hook_in(&home, SessionId(105), &e5);
    assert!(!load(Some(&state)).contains_key(&SessionId(105)));

    // 6. `switch_on_rate_limit = false` also prevents straggler queueing even when selected is healthy.
    accounts::clear_rate_limited(ProviderKind::Claude, &healthy_id);
    accounts::set_auto_switch(
        &home,
        Some(&state),
        &AutoSwitchPolicy {
            enabled: true,
            switch_on_rate_limit: false,
            ..AutoSwitchPolicy::default()
        },
    )
    .unwrap();
    let e6 = rate_limit_event(6);
    reg.auto_switch_from_hook_in(&home, SessionId(105), &e6);
    assert!(!load(Some(&state)).contains_key(&SessionId(105)));

    accounts::clear_rate_limited(ProviderKind::Claude, "claude-host");
    accounts::clear_rate_limited(ProviderKind::Claude, "claude-old");
    accounts::clear_rate_limited(ProviderKind::Claude, "claude-disabled");
    accounts::clear_rate_limited(ProviderKind::Claude, &healthy_id);
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn deferred_session_not_before_is_not_dispatched_early_by_provider_scope() {
    let root = std::env::temp_dir().join(format!("kas-not-before-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let to = root.join("to");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&to).unwrap();
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

    let (reg, _processes) = kept_sessions_fixture(&state, &[SessionId(201)]);
    reg.agents
        .remember_session(
            SessionId(201),
            &AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Manual,
            },
            Some("sid-201".into()),
            Vec::new(),
            Some(LaunchAccount {
                id: "codex-old".into(),
                env_key: "CODEX_HOME".into(),
                dir: root.display().to_string(),
            }),
        )
        .unwrap();
    lock(&reg.account_handoffs).insert(
        SessionId(201),
        SessionHandoff {
            from: SessionId(201),
            to: None,
            account_id: target.clone(),
            error: None,
        },
    );
    reg.save_account_handoffs().unwrap();
    {
        let mut pending = lock(&reg.account_work.pending);
        pending.insert(
            SwitchScope::Session(SessionId(201), ProviderKind::Codex),
            accounts::switch_work::SwitchTarget {
                account_id: target.clone(),
                prepared: true,
                not_before: Some(Instant::now() + Duration::from_secs(60)),
                interrupt_busy: false,
            },
        );
        pending.insert(
            SwitchScope::Provider(ProviderKind::Codex),
            accounts::switch_work::SwitchTarget {
                account_id: target,
                prepared: true,
                not_before: None,
                interrupt_busy: false,
            },
        );
    }

    reg.process_account_work_in(&home);
    let saved = load(Some(&state));
    let handoff = saved.get(&SessionId(201)).unwrap();
    assert_eq!(handoff.to, None);
    assert_eq!(handoff.error, None);
    assert!(
        lock(&reg.account_work.pending)
            .contains_key(&SwitchScope::Session(SessionId(201), ProviderKind::Codex))
    );
    assert!(
        !lock(&reg.account_work.pending).contains_key(&SwitchScope::Provider(ProviderKind::Codex))
    );

    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn superseded_handoff_failure_does_not_overwrite_newer_pending_selection() {
    let root = std::env::temp_dir().join(format!("kas-superseded-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let newer_dir = root.join("newer");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&newer_dir).unwrap();
    let snap = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Codex,
        "Newer",
        None,
        newer_dir.to_str().unwrap(),
    )
    .unwrap();
    let newer_id = snap
        .accounts
        .iter()
        .find(|a| a.label == "Newer")
        .unwrap()
        .id
        .clone();

    let (reg, _processes) = kept_sessions_fixture(&state, &[SessionId(202)]);
    reg.agents
        .remember_session(
            SessionId(202),
            &AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Manual,
            },
            Some("sid-202".into()),
            Vec::new(),
            Some(LaunchAccount {
                id: "codex-host".into(),
                env_key: "CODEX_HOME".into(),
                dir: String::new(),
            }),
        )
        .unwrap();
    lock(&reg.account_handoffs).insert(
        SessionId(202),
        SessionHandoff {
            from: SessionId(202),
            to: None,
            account_id: "missing-old-account".into(),
            error: None,
        },
    );
    reg.save_account_handoffs().unwrap();
    lock(&reg.account_work.pending).insert(
        SwitchScope::Session(SessionId(202), ProviderKind::Codex),
        accounts::switch_work::SwitchTarget {
            account_id: newer_id.clone(),
            prepared: true,
            not_before: None,
            interrupt_busy: false,
        },
    );

    // An older attempt for `missing-old-account` failing with BadRequest must not stamp a
    // stale error onto `account_handoffs` once `account_work.pending` holds a newer selection.
    reg.process_account_handoff(
        SessionId(202),
        ProviderKind::Codex,
        "missing-old-account",
        false,
        &home,
    );
    let saved = load(Some(&state));
    let handoff = saved.get(&SessionId(202)).unwrap();
    assert_eq!(handoff.to, None);
    assert_eq!(handoff.error, None);
    assert_eq!(
        lock(&reg.account_work.pending)
            .get(&SwitchScope::Session(SessionId(202), ProviderKind::Codex))
            .map(|t| t.account_id.as_str()),
        Some(newer_id.as_str())
    );

    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn running_account_resolves_isolated_claude_session_id_without_transcript_path() {
    let root = std::env::temp_dir().join(format!("kas-iso-sid-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let host_projects = home.join(".claude/projects/repo");
    let isolated_dir = root.join("claude-isolated");
    let isolated_projects = isolated_dir.join("projects/repo");
    let shared_dir = root.join("claude-shared");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&host_projects).unwrap();
    fs::create_dir_all(&isolated_dir).unwrap();
    fs::create_dir_all(&shared_dir).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(home.join(".claude/projects"), shared_dir.join("projects")).unwrap();

    // Register accounts while `isolated_dir` is still empty so `add_account` finishes synchronously
    // without spawning a background history merge, then restore an isolated non-shared `projects` dir.
    let _ = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Isolated",
        None,
        isolated_dir.to_str().unwrap(),
    )
    .unwrap();
    let snap = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Claude,
        "Shared",
        None,
        shared_dir.to_str().unwrap(),
    )
    .unwrap();
    fs::remove_file(isolated_dir.join("projects"))
        .or_else(|_| fs::remove_dir_all(isolated_dir.join("projects")))
        .ok();
    fs::create_dir_all(&isolated_projects).unwrap();

    let sid_iso = "claude-iso-1";
    let sid_shared = "claude-shared-1";
    fs::write(
        isolated_projects.join(format!("{sid_iso}.jsonl")),
        format!("{{\"sessionId\":\"{sid_iso}\"}}\n"),
    )
    .unwrap();
    fs::write(
        host_projects.join(format!("{sid_shared}.jsonl")),
        format!("{{\"sessionId\":\"{sid_shared}\"}}\n"),
    )
    .unwrap();
    let iso_id = snap
        .accounts
        .iter()
        .find(|a| a.label == "Isolated")
        .unwrap()
        .id
        .clone();

    let (reg, _processes) = kept_sessions_fixture(&state, &[SessionId(203), SessionId(204)]);
    reg.agents
        .remember_session(
            SessionId(203),
            &AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Manual,
            },
            Some(sid_iso.into()),
            Vec::new(),
            None,
        )
        .unwrap();
    reg.agents
        .remember_session(
            SessionId(204),
            &AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Manual,
            },
            Some(sid_shared.into()),
            Vec::new(),
            None,
        )
        .unwrap();

    let stored_iso = reg.agents.stored(SessionId(203)).unwrap().unwrap();
    let stored_shared = reg.agents.stored(SessionId(204)).unwrap().unwrap();
    assert_eq!(
        reg.resolve_session_account_id(&home, ProviderKind::Claude, Some(&stored_iso)),
        iso_id
    );
    assert_eq!(
        reg.resolve_session_account_id(&home, ProviderKind::Claude, Some(&stored_shared)),
        "claude-host"
    );

    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn switch_control_inspects_switches_only_the_default_and_hands_off_selected_sessions() {
    use krill_proto::hook::{EventId, HookEvent, Input};
    use krill_proto::roles::Role;
    use krill_proto::usage::{
        CantMoveReason, FEAT_ACCOUNT_SWITCH_CONTROL, SessionSwitchGroup, UsageReply, UsageRequest,
    };
    let root = Path::new("/tmp").join(format!("kas-control-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let own = root.join("own");
    fs::create_dir_all(&own).unwrap();
    let mut ids = Vec::new();
    let mut snapshot = AccountsSnapshot::default();
    for label in ["A", "B", "C"] {
        let dir = root.join(label);
        fs::create_dir_all(&dir).unwrap();
        snapshot = accounts::add_account(
            &home,
            Some(&state),
            ProviderKind::Codex,
            label,
            None,
            dir.to_str().unwrap(),
        )
        .unwrap();
        let id = snapshot
            .accounts
            .iter()
            .find(|a| a.label == label)
            .unwrap()
            .id
            .clone();
        ids.push((id, dir));
    }
    let [(a, a_dir), (b, b_dir), (c, c_dir)] = ids.try_into().unwrap();
    let sessions: Vec<_> = (1..=9).map(SessionId).collect();
    let (reg, _processes) = kept_sessions_fixture(&state, &sessions);
    let launch = |session: u64, conversation: Option<&str>, id: &str, dir: &Path| {
        reg.agents
            .remember_session(
                SessionId(session),
                &AgentLaunch {
                    profile: "codex".into(),
                    permission: Permission::Yolo,
                },
                conversation.map(str::to_owned),
                format!("/w/{session}").into_bytes(),
                Some(LaunchAccount {
                    id: id.into(),
                    env_key: "CODEX_HOME".into(),
                    dir: dir.display().to_string(),
                }),
            )
            .unwrap();
    };
    launch(1, Some("conv-1"), &a, &a_dir);
    launch(2, Some("conv-2"), &b, &b_dir);
    launch(3, Some("conv-3"), &c, &c_dir);
    launch(4, Some("conv-4"), &a, &own);
    launch(5, Some("conv-5"), "ghost", Path::new(""));
    launch(6, Some("shared"), &a, &a_dir);
    launch(7, Some("shared"), &a, &a_dir);
    launch(8, None, &a, &a_dir);
    launch(9, Some("conv-9"), &a, &a_dir);
    lock(&reg.account_handoffs).insert(
        SessionId(9),
        SessionHandoff {
            from: SessionId(9),
            to: None,
            account_id: c.clone(),
            error: None,
        },
    );
    let working = HookEvent {
        id: EventId {
            pid: 723,
            unix_ns: 1,
        },
        ppid: 723,
        agent: "codex".into(),
        input: Input::Complete,
        payload: br#"{"hook_event_name":"UserPromptSubmit","session_id":"conv-3","prompt":"go"}"#
            .to_vec(),
    };
    reg.status.hook(&[(SessionId(3), &working)]).unwrap();
    assert!(reg.account_switch_busy(SessionId(3)));

    let plan = reg
        .inspect_session_switches_in(
            &home,
            ProviderKind::Codex,
            Some(a.clone()),
            b.clone(),
            &snapshot,
        )
        .unwrap();
    let group = |session: u64| {
        plan.candidates
            .iter()
            .find(|c| c.session == SessionId(session))
            .map(|c| c.group)
    };
    assert_eq!(group(1), Some(SessionSwitchGroup::OnPreviousAccount));
    assert_eq!(group(2), None, "already on the target account");
    assert_eq!(group(3), Some(SessionSwitchGroup::Elsewhere));
    let cant = |reason| Some(SessionSwitchGroup::CantMove(reason));
    assert_eq!(group(4), cant(CantMoveReason::OwnFolder));
    assert_eq!(group(5), cant(CantMoveReason::UnknownAccount));
    assert_eq!(group(6), cant(CantMoveReason::SharedSession));
    assert_eq!(group(7), cant(CantMoveReason::SharedSession));
    assert_eq!(group(8), cant(CantMoveReason::NoSession));
    assert_eq!(group(9), cant(CantMoveReason::PendingHandoff));
    let first = &plan.candidates[0];
    assert_eq!(
        (first.cwd.as_str(), first.agent.as_str(), first.busy),
        ("/w/1", "codex", false)
    );
    assert_eq!(first.current_account_label, "A");
    assert!(
        plan.candidates
            .iter()
            .any(|c| c.session == SessionId(3) && c.busy)
    );
    assert_eq!(
        plan.candidates
            .iter()
            .find(|c| c.session == SessionId(5))
            .unwrap()
            .current_account_label,
        "ghost"
    );
    assert_eq!(
        reg.inspect_session_switches_in(
            &home,
            ProviderKind::Codex,
            None,
            "missing".into(),
            &snapshot
        ),
        Err(Failure::BadRequest)
    );

    // The default changes while no running session is queued for a handoff.
    let peer = Peer {
        pid: None,
        scope: Scope::Full,
    };
    let only = UsageRequest::SwitchAccountOnly {
        provider: ProviderKind::Codex,
        account_id: b.clone(),
    };
    let features = krill_proto::ipc::FEATURES;
    assert_eq!(
        reg.on_usage_negotiated_in(
            &peer,
            Role::Interactive,
            only.clone(),
            features & !FEAT_ACCOUNT_SWITCH_CONTROL,
            &home
        ),
        UsageReply::Failed(Failure::BadRequest)
    );
    let handoffs_before = lock(&reg.account_handoffs).len();
    let UsageReply::Accounts(after) =
        reg.on_usage_negotiated_in(&peer, Role::Interactive, only, features, &home)
    else {
        panic!("expected accounts");
    };
    assert!(after.accounts.iter().any(|x| x.id == b && x.active));
    assert!(lock(&reg.account_work.pending).is_empty());
    assert_eq!(lock(&reg.account_handoffs).len(), handoffs_before);
    let config = fs::read(state.join("accounts.toml")).unwrap();

    // Only the chosen sessions are queued, carrying the busy override; the default is untouched.
    let UsageReply::Accounts(_) = reg.on_usage_negotiated_in(
        &peer,
        Role::Interactive,
        UsageRequest::SwitchSelectedSessions {
            provider: ProviderKind::Codex,
            account_id: c.clone(),
            sessions: vec![SessionId(1), SessionId(1), SessionId(3)],
            interrupt_busy: true,
        },
        features,
        &home,
    ) else {
        panic!("expected accounts");
    };
    assert_eq!(fs::read(state.join("accounts.toml")).unwrap(), config);
    let pending = lock(&reg.account_work.pending);
    let queued: Vec<_> = pending
        .iter()
        .map(|(scope, target)| (*scope, target.account_id.clone(), target.interrupt_busy))
        .collect();
    assert_eq!(
        queued,
        [
            (
                SwitchScope::Session(SessionId(1), ProviderKind::Codex),
                c.clone(),
                true
            ),
            (
                SwitchScope::Session(SessionId(3), ProviderKind::Codex),
                c.clone(),
                true
            ),
        ]
    );
    drop(pending);
    let saved = load(Some(&state));
    assert!(saved.contains_key(&SessionId(1)) && !saved.contains_key(&SessionId(6)));

    // A working session refuses without the override and passes the busy gate with it.
    lock(&reg.account_handoffs).insert(
        SessionId(3),
        SessionHandoff {
            from: SessionId(3),
            to: None,
            account_id: a.clone(),
            error: None,
        },
    );
    lock(&reg.account_work.pending).clear();
    for (interrupt_busy, expected) in [
        (false, Failure::Os(libc::EBUSY)),
        (true, Failure::BadRequest),
    ] {
        // Claude is the wrong provider, so a passed busy gate stops at the provider recheck.
        assert_eq!(
            reg.handoff_account(
                &peer,
                SessionId(3),
                ProviderKind::Claude,
                &a,
                interrupt_busy,
                &home
            ),
            Err(expected)
        );
    }
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn automatic_switch_does_not_treat_rewritten_old_unbound_session_as_recently_launched() {
    let root = std::env::temp_dir().join(format!(
        "kas-recently-launched-immutable-{}",
        std::process::id()
    ));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let to = root.join("codex-target");
    fs::create_dir_all(&home).unwrap();
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
    let session = SessionId(39375);
    let (reg, _original) = kept_fixture(&state, session);
    reg.agents
        .remember_session(
            session,
            &AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Manual,
            },
            None,
            Vec::new(),
            None,
        )
        .unwrap();
    let launch_path = state.join("agent-launches/39375.json");
    let mut raw: serde_json::Value =
        serde_json::from_slice(&fs::read(&launch_path).unwrap()).unwrap();
    let old_ts = raw["launched_at"].as_u64().unwrap().saturating_sub(60);
    raw["launched_at"] = serde_json::json!(old_ts);
    fs::write(&launch_path, serde_json::to_vec(&raw).unwrap()).unwrap();
    reg.agents.record_hook_session(
        session,
        br#"{"hook_event_name":"PreToolUse","tool_name":"Task"}"#,
    );
    assert_eq!(reg.agents.launched_at(session), Some(old_ts));

    reg.prepare_account_switch_in(ProviderKind::Codex, &target, &home, true);
    assert!(
        !lock(&reg.account_handoffs).contains_key(&session),
        "old unbound session must not be enqueued as recently_launched after metadata rewrite"
    );
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn bot_session_account_switch_rebinds_bot_runtime_and_preserves_instructions() {
    use krill_proto::bots::{BotRpcRequest, BotRpcResponse, CreateBotParams, MemorySnapshot};

    let root = Path::new("/tmp").join(format!("kas-bot-agy-rebind-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let from = root.join("from");
    let to = root.join("to");
    fs::create_dir_all(&state).unwrap();
    let sid = "9b230a24-4055-4bdb-8c26-bd8d72c64bc9";
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
    fs::write(&tp, "{\"role\":\"user\",\"content\":\"hello bot\"}\n").unwrap();
    fs::write(
        from_agy.join("conversations").join(format!("{sid}.db")),
        b"SQLite format 3\0fixture-bot-conversation",
    )
    .unwrap();
    fs::write(
        from_agy.join("annotations").join(format!("{sid}.pbtxt")),
        b"title: \"bot agy switch\"",
    )
    .unwrap();
    fs::write(
        to_agy.join("antigravity-oauth-token"),
        r#"{"access_token":"tok","expiry_date":4102444800000,"email":"craftowen@example.com"}"#,
    )
    .unwrap();

    let snap = accounts::add_account(
        &home,
        Some(&state),
        ProviderKind::Antigravity,
        "Antigravity #2",
        Some("craftowen@example.com".into()),
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
    let old_sid = SessionId(41);
    let reg = Arc::new(Registry::new(
        Some((
            holder,
            vec![Kept {
                id: old_sid,
                pid: 4100,
                status: None,
                size: None,
                master: OwnedFd::from(master),
            }],
        )),
        Some(&state),
    ));

    // Create a live Antigravity bot and bind `old_sid` to it (mirroring real Krill Bots).
    let bots = reg.bots().expect("init bots manager");
    let BotRpcResponse::Bot(bot) = bots.handle_rpc(
        &reg,
        BotRpcRequest::CreateBot(Box::new(CreateBotParams {
            name: "크릴".to_owned(),
            agent_id: "agy".to_owned(),
            description: "Main assistant".to_owned(),
            emoji: None,
            avatar: None,
            model: None,
            effort: None,
            permission_mode: Some(Permission::Yolo),
            workspace: None,
            template_id: None,
            initial_soul: Some("# 크릴\nAlways stay in place.".to_owned()),
        })),
    ) else {
        panic!("expected Bot from CreateBot");
    };
    let bot_home = state.join("bots").join(bot.slug.as_str());
    fs::create_dir_all(&bot_home).unwrap();
    let old_token = zeroize::Zeroizing::new("old-ephemeral-token".to_owned());
    lock(bots.runtime_for_test().sessions_for_test()).insert(
        old_sid,
        crate::bots::runtime::BotSessionBinding {
            bot_id: bot.id.clone(),
            link_id: None,
            memory_snapshot: MemorySnapshot::capture(""),
            chat_offset: 0,
            explicit_reply_posted: false,
            launch_token: Some(old_token.clone()),
            pid: 4100,
            label: "크릴".to_owned(),
            created_at_ms: 1_795_400_000_000,
            owner_turn_pending: false,
            last_prompt: None,
        },
    );
    reg.agents
        .remember_session(
            old_sid,
            &AgentLaunch {
                profile: "agy".into(),
                permission: Permission::Yolo,
            },
            None,
            bot_home.as_os_str().as_encoded_bytes().to_vec(),
            Some(LaunchAccount {
                id: "agy-old".into(),
                env_key: "ANTIGRAVITY_CLI_HOME".into(),
                dir: from.display().to_string(),
            }),
        )
        .unwrap();
    reg.agents.bind_discovered_session(old_sid, sid, Some(&tp));
    reg.agents
        .remember_bot_instructions(old_sid, "You are 크릴")
        .unwrap();

    let reader = Arc::clone(&reg);
    let reader_worker =
        std::thread::spawn(move || reader.holder.as_ref().unwrap().run(link, |_, _| {}));
    let hook_reg = Arc::clone(&reg);
    let expected_bot_id = bot.id.as_str().to_owned();
    let mock = std::thread::spawn(move || {
        let packet = holder_link.recv().unwrap().unwrap();
        let ToHolder::Spawn {
            req, id, argv, env, ..
        } = ToHolder::decode(packet.frame()).unwrap()
        else {
            panic!("expected replacement spawn");
        };
        let argv_str: Vec<String> = argv
            .iter()
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        let env_get = |key: &[u8]| -> Option<String> {
            env.iter()
                .rev()
                .find(|(k, _)| k.as_slice() == key)
                .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
        };
        assert!(argv_str.iter().any(|a| a == "--conversation"));
        assert!(argv_str.iter().any(|a| a == sid));
        let has_bot_agents_md = argv_str.windows(2).any(|w| {
            w[0] == "--add-dir"
                && fs::read_to_string(Path::new(&w[1]).join("AGENTS.md"))
                    .is_ok_and(|c| c.contains("크릴"))
        });
        assert!(
            has_bot_agents_md,
            "replacement agy bot must receive --add-dir containing bot AGENTS.md: {argv_str:?}"
        );
        assert_eq!(
            env_get(b"KRILL_BOT_ID").as_deref(),
            Some(expected_bot_id.as_str())
        );
        assert!(env_get(b"KRILL_BOT_HOME").is_some());
        assert!(env_get(b"KRILL_BOT_TOOLS_URL").is_some());
        let new_tok = env_get(b"KRILL_BOT_TOOLS_TOKEN").expect("fresh KRILL_BOT_TOOLS_TOKEN");
        assert_ne!(new_tok, "old-ephemeral-token");
        assert!(
            env_get(b"NO_PROXY")
                .unwrap_or_default()
                .contains("127.0.0.1")
        );

        let (master, mut process) = UnixStream::pair().unwrap();
        holder_link
            .send(
                &FromHolder::Spawned { req, id, pid: 4200 }.encode().unwrap(),
                &[master.as_fd()],
            )
            .unwrap();
        process.write_all(b"Antigravity bot resumed > ").unwrap();

        let packet = holder_link.recv().unwrap().unwrap();
        let ToHolder::Terminate { req, id: old } = ToHolder::decode(packet.frame()).unwrap() else {
            panic!("expected old bot retirement");
        };
        assert_eq!(old, old_sid);
        holder_link
            .send(&FromHolder::Done { req }.encode().unwrap(), &[])
            .unwrap();
        // Simulate real daemon exit path (including `on_session_removed` on `old_sid`)!
        hook_reg.remove(old);
        lock(&hook_reg.state).exited.insert(old, 0);
        (id, process)
    });

    reg.queue_account_switch(ProviderKind::Antigravity, &target);
    reg.process_account_work_in(&home);
    let saved = load(Some(&state));
    let result = saved.get(&old_sid).expect("handoff record");
    assert!(result.error.is_none(), "handoff failed: {:?}", result.error);
    let new_sid = result.to.expect("replacement session id");
    assert_ne!(new_sid, old_sid);

    // 1. Replacement persisted `bot_instructions` and target account on disk.
    let stored_new = reg.agents.stored(new_sid).unwrap().unwrap();
    assert_eq!(stored_new.account.unwrap().id, target);
    assert!(
        stored_new
            .bot_instructions
            .as_deref()
            .is_some_and(|text| text.contains("크릴")),
        "replacement agent-launch JSON must keep bot_instructions"
    );

    // 2. Bot runtime binding moved atomically from `old_sid` -> `new_sid` (surviving `remove(old_sid)`!).
    assert!(
        !bots.runtime_for_test().is_bot_session(old_sid),
        "old bot session must be unbound after retirement"
    );
    assert!(
        bots.runtime_for_test().is_bot_session(new_sid),
        "replacement session must own the bot runtime binding"
    );
    assert_eq!(
        bots.runtime_for_test().bot_for_session(new_sid),
        Some(bot.id.clone())
    );

    // 3. Durable `runtime-bindings.json` records `new_sid` (not `old_sid`) with original `created_at_ms`.
    let bindings_raw = fs::read_to_string(state.join("bots/runtime-bindings.json")).unwrap();
    let bindings_json: serde_json::Value = serde_json::from_str(&bindings_raw).unwrap();
    let arr = bindings_json.as_array().expect("bindings array");
    assert_eq!(arr.len(), 1, "expected 1 bound bot session: {bindings_raw}");
    assert_eq!(arr[0]["session_id"].as_u64(), Some(new_sid.0));
    assert_eq!(arr[0]["bot_id"].as_str(), Some(bot.id.as_str()));
    assert_eq!(arr[0]["created_at_ms"].as_u64(), Some(1_795_400_000_000));

    let (_id, _proc) = mock.join().unwrap();
    bots.shutdown();
    drop(reg);
    reader_worker.join().ok();
    fs::remove_dir_all(root).ok();
}
