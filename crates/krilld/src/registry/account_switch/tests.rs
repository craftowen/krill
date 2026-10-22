use super::*;
use std::fs;
use std::io::{Read as _, Write as _};
use std::sync::Arc;

use crate::agent::control::LaunchAccount;
use crate::holder::{Holder, Kept, link::Link};
use crate::os::{AsFd as _, OwnedFd};
use krill_proto::agent::Permission;
use krill_proto::holder::{FromHolder, ToHolder, VERSION};
use krill_proto::os::net::UnixStream;

#[test]
fn switch_resumes_same_conversation_with_selected_credentials_before_retiring_old() {
    for (fail, cancel, automatic, missing_transcript, missing_copy) in [
        (false, false, false, false, false),
        (true, false, false, false, false),
        (false, true, false, false, false),
        (false, false, true, false, false),
        (true, false, true, false, false),
        (true, false, true, true, false),
        (true, false, false, true, false),
        (false, false, false, false, true),
    ] {
        let root = Path::new("/tmp").join(format!(
            "kas-{}-{fail}-{cancel}-{automatic}-{missing_transcript}-{missing_copy}",
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
        if missing_transcript {
            fs::remove_file(&transcript).unwrap();
        }
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
                    permission: Permission::Manual,
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
        let mock = std::thread::spawn(move || {
            if missing_transcript {
                return None;
            }
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Spawn {
                req, id, argv, env, ..
            } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!("expected replacement spawn");
            };
            assert!(argv.iter().any(|arg| arg == b"resume"));
            assert!(argv.iter().any(|arg| arg == sid.as_bytes()));
            assert!(
                argv.iter()
                    .any(|arg| arg == b"--dangerously-bypass-approvals-and-sandbox")
            );
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
            if missing_copy {
                fs::remove_file(
                    expected_home.join(format!("sessions/2026/10/20/rollout-date-{sid}.jsonl")),
                )
                .unwrap();
            }
            process.write_all(b"resumed selected account\r\n").unwrap();
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Terminate { req, id: old } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!("expected old process retirement after new output");
            };
            assert_eq!(
                old,
                if cancel || missing_copy {
                    id
                } else {
                    SessionId(7)
                }
            );
            holder_link
                .send(&FromHolder::Done { req }.encode().unwrap(), &[])
                .unwrap();
            if !cancel && !missing_copy {
                process
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = vec![0; CONTINUE_PROMPT.len() + 1];
                process.read_exact(&mut bytes).unwrap();
                assert_eq!(bytes, format!("{CONTINUE_PROMPT}\r").as_bytes());
            }
            // Keep the new PTY open until the parent has committed its handoff.
            Some((id, process))
        });
        if automatic {
            let mut file = accounts::load_file(&home, Some(&state));
            file.direct_usage = false;
            file.auto_switch.enabled = true;
            file.active.insert("codex".into(), "codex-old".into());
            file.accounts.push(accounts::StoredAccount {
                id: "codex-old".into(),
                provider: "codex".into(),
                label: "Original".into(),
                email: None,
                profile_dir: from.display().to_string(),
            });
            accounts::save_file(&home, Some(&state), &file).unwrap();
            for (dir, percent) in [(&from, 95), (&to, 10)] {
                fs::write(
                    dir.join("usage_cache.json"),
                    format!("{{\"five_hour\":{{\"utilization\":{percent}}}}}"),
                )
                .unwrap();
            }
            let mut snap = super::super::usage_tick::tick(&home, Some(&state), &mut None)
                .expect("automatic evaluation");
            assert_eq!(snap.switched.len(), 1);
            assert_eq!(snap.switched[0].to_id, target);
            reg.account_switch_snapshot(&mut snap);
            assert!(
                snap.details.handoffs.iter().any(|h| {
                    h.from == SessionId(7) && h.account_id == target && h.to.is_none()
                })
            );
        } else {
            reg.queue_account_switch(ProviderKind::Codex, &target);
            reg.queue_account_switch(ProviderKind::Codex, "codex-old");
            assert!(
                lock(&reg.account_handoffs).is_empty(),
                "reselecting the running account cancels a queued handoff"
            );
            reg.queue_account_switch(ProviderKind::Codex, &target);
        }
        reg.process_account_work_in(&home);
        let saved = load(Some(&state));
        if cancel {
            assert!(
                saved.is_empty(),
                "in-flight cancellation must not rebind the original"
            );
            assert!(!lock(&reg.state).closing.contains(&SessionId(7)));
        } else if fail || missing_copy {
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
        assert_eq!(transcript.is_file(), !missing_transcript);
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
        assert!(error.contains("before retrying"), "{case}: {error}");
        assert!(!lock(&reg.state).closing.contains(&SessionId(9)));
        assert!(!lock(&reg.state).exited.contains_key(&SessionId(9)));
        assert_eq!(fs::read(&launch_path).unwrap(), original_launch, "{case}");
        assert!(!state.join(PREPARED_FILE).exists());
        assert_eq!(
            fs::read_dir(state.join(PREPARED_DIR)).map_or(0, Iterator::count),
            0
        );
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
fn recovery_finishes_each_committed_handoff_before_continuing_its_work() {
    let root = Path::new("/tmp").join(format!("kas-commit-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
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
    let mut kept = Vec::new();
    let mut processes = Vec::new();
    for id in 1..=4 {
        let (master, mut process) = UnixStream::pair().unwrap();
        process
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        process.write_all(b"restored session ready\r\n").unwrap();
        kept.push(Kept {
            id: SessionId(id),
            pid: u32::try_from(700 + id).unwrap(),
            status: None,
            master: OwnedFd::from(master),
        });
        processes.push(process);
    }
    let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let reg = Arc::new(Registry::new(Some((holder, kept)), Some(&state)));
    for id in 1..=4 {
        reg.agents
            .remember_session(
                SessionId(id),
                &AgentLaunch {
                    profile: "codex".into(),
                    permission: Permission::Yolo,
                },
                Some(format!("fixture-session-{id}")),
                root.as_os_str().as_encoded_bytes().to_vec(),
                Some(LaunchAccount {
                    id: "selected".into(),
                    env_key: "CODEX_HOME".into(),
                    dir: String::new(),
                }),
            )
            .unwrap();
    }
    for (from, to) in [(1, 2), (3, 4)] {
        // Same conversation identity is required when adopting a staged replacement.
        let source = reg.agents.stored(SessionId(from)).unwrap().unwrap();
        reg.agents
            .remember_session(
                SessionId(to),
                &AgentLaunch {
                    profile: "codex".into(),
                    permission: Permission::Yolo,
                },
                source.session_id,
                source.cwd,
                source.account,
            )
            .unwrap();
        lock(&reg.account_handoffs).insert(
            SessionId(from),
            SessionHandoff {
                from: SessionId(from),
                to: Some(SessionId(to)),
                account_id: "selected".into(),
                error: None,
            },
        );
        let prepared = Prepared {
            from: SessionId(from),
            to: SessionId(to),
            account_id: "selected".into(),
            continue_work: true,
        };
        accounts::write_atomic_0600(
            &prepared_path(&state, prepared.to),
            &serde_json::to_vec(&prepared).unwrap(),
        )
        .unwrap();
    }
    reg.save_account_handoffs().unwrap();
    let reader = Arc::clone(&reg);
    let reader_worker =
        std::thread::spawn(move || reader.holder.as_ref().unwrap().run(link, |_, _| {}));
    let mock = std::thread::spawn(move || {
        for expected in [SessionId(1), SessionId(3)] {
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Terminate { req, id } = ToHolder::decode(packet.frame()).unwrap() else {
                panic!("expected original retirement")
            };
            assert_eq!(id, expected);
            holder_link
                .send(&FromHolder::Done { req }.encode().unwrap(), &[])
                .unwrap();
        }
        for index in [1, 3] {
            let mut bytes = vec![0; CONTINUE_PROMPT.len() + 1];
            processes
                .get_mut(index)
                .unwrap()
                .read_exact(&mut bytes)
                .unwrap();
            assert_eq!(bytes, format!("{CONTINUE_PROMPT}\r").as_bytes());
        }
        processes
    });
    reg.recover_account_work(&root.join("home"));
    assert_eq!(fs::read_dir(state.join(PREPARED_DIR)).unwrap().count(), 0);
    reg.recover_account_work(&root.join("home"));
    drop(mock.join().unwrap());
    reader_worker.join().unwrap();
    drop(reg);
    fs::remove_dir_all(root).unwrap();
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
