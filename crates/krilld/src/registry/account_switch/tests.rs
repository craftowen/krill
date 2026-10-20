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
    for (fail, cancel) in [(false, false), (true, false), (false, true)] {
        let root = Path::new("/tmp").join(format!("kas-{}-{fail}-{cancel}", std::process::id()));
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
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Terminate { req, id: old } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!("expected old process retirement after new output");
            };
            assert_eq!(old, if cancel { id } else { SessionId(7) });
            holder_link
                .send(&FromHolder::Done { req }.encode().unwrap(), &[])
                .unwrap();
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
        } else if fail {
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
fn switch_without_transcript_launches_selected_account_and_login_shell_gets_active_env() {
    let root = Path::new("/tmp").join(format!("kas-fresh-{}", std::process::id()));
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
    accounts::switch_account(&home, Some(&state), ProviderKind::Codex, &target).unwrap();

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
                id: SessionId(9),
                pid: 900,
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
            None,
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
    let mock = std::thread::spawn(move || {
        let packet = holder_link.recv().unwrap().unwrap();
        let ToHolder::Spawn {
            req, id, argv, env, ..
        } = ToHolder::decode(packet.frame()).unwrap()
        else {
            panic!("expected replacement spawn");
        };
        assert!(!argv.iter().any(|arg| arg == b"resume"));
        assert!(env.iter().any(|(key, value)| key == b"CODEX_HOME"
            && value == expected_home.as_os_str().as_encoded_bytes()));
        let (master, mut process) = UnixStream::pair().unwrap();
        holder_link
            .send(
                &FromHolder::Spawned { req, id, pid: 901 }.encode().unwrap(),
                &[master.as_fd()],
            )
            .unwrap();
        process.write_all(b"fresh selected account\r\n").unwrap();
        let packet = holder_link.recv().unwrap().unwrap();
        let ToHolder::Terminate { req, id: old } = ToHolder::decode(packet.frame()).unwrap() else {
            panic!("expected old process retirement");
        };
        assert_eq!(old, SessionId(9));
        holder_link
            .send(&FromHolder::Done { req }.encode().unwrap(), &[])
            .unwrap();
        Some((id, process))
    });
    reg.queue_account_switch(ProviderKind::Codex, &target);
    reg.process_account_work_in(&home);
    let saved = load(Some(&state));
    let result = saved.get(&SessionId(9)).unwrap();
    let new = result.to.expect("durable replacement without transcript");
    assert_eq!(
        reg.agents.stored(new).unwrap().unwrap().account.unwrap().id,
        target
    );
    drop(mock.join().unwrap());
    reader_worker.join().unwrap();
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}
