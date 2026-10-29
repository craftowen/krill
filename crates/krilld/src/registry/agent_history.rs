//! MK-082의 외부 transcript → MK-052 재개 경계.
//! 잠정 결정 의존: OWE-417의 `agent::resume_in`. 거부 시 이 모듈의 재개 분기만 제거한다.
use super::{Failure, Launch, Path, Peer, Registry, Reply, control, os};
use krill_proto::agent_history::{Action, Response, VaultAgent, VaultTarget};

impl Registry {
    pub(super) fn agent_history(&self, peer: &Peer, action: Action) -> Result<Reply, Failure> {
        if !peer.scope.full() {
            peer.deny("agent-history", &self.audit);
            return Err(Failure::Denied);
        }
        let target = match action {
            Action::Scan => {
                return Ok(Reply::AgentHistory(Response::Scanned(
                    crate::agent_history::scan(self.state_dir().as_deref()),
                )));
            }
            Action::Search(query) => {
                return Ok(Reply::AgentHistory(Response::Searched(
                    crate::agent_history::vault::search(self.state_dir().as_deref(), &query),
                )));
            }
            Action::ChatPoll { session, offset } => {
                return Ok(Reply::AgentHistory(Response::ChatPolled(
                    self.chat_poll(session, offset),
                )));
            }
            Action::Resume(entry) => {
                if !crate::agent_history::valid(&entry) || !Path::new(&entry.cwd).is_dir() {
                    return Err(Failure::BadRequest);
                }
                VaultTarget::from(&entry)
            }
            Action::ResumeVault(target) => {
                if !crate::agent_history::vault::valid_target(&target)
                    || !Path::new(&target.cwd).is_dir()
                {
                    return Err(Failure::BadRequest);
                }
                target
            }
        };
        let holder = self.holder.as_ref().ok_or(Failure::NoHolder)?;
        let agents = self.agents.agents().map_err(|_| Failure::UnknownAgent)?;
        let id = target.agent.profile();
        let profile = agents.profiles.get(id).ok_or(Failure::UnknownAgent)?;
        // 기본 프로필이 다른 프로그램이면 그 프로그램에 대화 ID를 넘기지 않는다.
        if profile
            .program()
            .is_none_or(|p| krill_proto::agent::executable_id(p) != id)
        {
            return Err(Failure::UnknownAgent);
        }
        let session = self.next_id();
        let state = self.state_dir();
        let cwd_bytes = target.cwd.clone().into_bytes();
        // K-13·OWE-698: 비활성 계정 대화를 활성 계정 프로필에서 `--resume`하면 "No conversation found"가 난다.
        // transcript가 실제로 든 프로필(스캔한 데몬 env 저장소 포함)로 띄우고 그 계정을 실행 기록에 남긴다.
        // 못 찾으면 스캔한 저장소(데몬 env)와 맞추고, 그것도 없으면 지금 활성 계정이다.
        let config_var = match target.agent {
            VaultAgent::Claude => Some("CLAUDE_CONFIG_DIR"),
            VaultAgent::Codex => Some("CODEX_HOME"),
            VaultAgent::Agy => None,
            VaultAgent::OpenCode => Some("OPENCODE_CONFIG_DIR"),
        };
        let account =
            crate::agent::resume_account(profile, state.as_deref(), None, Some(&target.id))
                .or_else(|| {
                    let home = crate::usage::accounts::default_home();
                    let file = crate::usage::accounts::load_file(&home, state.as_deref());
                    let dir = crate::agent_history::vault::profile_dir_for_vault_target_in(
                        &file,
                        &target,
                        None,
                        state.as_deref(),
                    )?;
                    let env_key = config_var.unwrap_or("ANTIGRAVITY_CLI_HOME");
                    Some(crate::agent::control::LaunchAccount {
                        id: String::new(),
                        env_key: env_key.to_owned(),
                        dir: dir.into_os_string().into_string().ok()?,
                    })
                })
                .or_else(|| {
                    let var = config_var?;
                    let dir = std::env::var(var).ok().filter(|s| !s.is_empty())?;
                    Some(crate::agent::control::LaunchAccount {
                        id: String::new(),
                        env_key: var.to_owned(),
                        dir,
                    })
                });
        crate::agent::pre_trust_workspace(profile, &cwd_bytes, account.as_ref());
        let (Launch { argv, env }, session_id, account) = crate::agent::resume_in(
            profile,
            agents.permission,
            session,
            state.as_deref(),
            Some(&target.id),
            account.as_ref(),
        )
        .map_err(|e| os(&e))?;
        let metadata = control::AgentLaunch {
            profile: id.to_owned(),
            permission: agents.permission,
        };
        self.agents
            .remember_session(session, &metadata, session_id, cwd_bytes.clone(), account)
            .map_err(|e| {
                self.discard_settings(session);
                os(&e)
            })?;
        let (session, _) = self.spawn_holder(holder, session, argv, env, cwd_bytes, 80, 24)?;
        Ok(Reply::AgentHistory(Response::Resumed { session }))
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::scope::Scope;
    use krill_proto::agent_history::{Agent, Entry, SearchQuery};
    use krill_proto::control::{FEAT_CONTROL, Request};
    use krill_proto::os::net::UnixStream;
    use krill_proto::screen::Msg as _;
    use krill_proto::{
        SessionId,
        ipc::{self, ServerHello},
        roles::{self, Ack, Role},
    };
    use std::io::Write as _;

    fn entry() -> Entry {
        Entry {
            agent: Agent::Codex,
            id: "11111111-2222-4333-8444-555555555555".into(),
            cwd: "/tmp".into(),
            modified: 0,
        }
    }

    #[test]
    fn history_denies_unverified_peers_and_scan_is_the_only_read_only_action() {
        let reg = Registry::new(None, None);
        {
            let peer = Peer {
                pid: None,
                scope: Scope::Restricted,
            };
            for action in [
                Action::Scan,
                Action::Search(SearchQuery::default()),
                Action::Resume(entry()),
                Action::ResumeVault(VaultTarget::from(&entry())),
                Action::ChatPoll {
                    session: SessionId(1),
                    offset: 0,
                },
            ] {
                assert_eq!(reg.agent_history(&peer, action), Err(Failure::Denied));
            }
        }
        assert!(super::super::read_only(&Request::AgentHistory(
            Action::Scan
        )));
        assert!(super::super::read_only(&Request::AgentHistory(
            Action::Search(SearchQuery::default())
        )));
        assert!(super::super::read_only(&Request::AgentHistory(
            Action::ChatPoll {
                session: SessionId(1),
                offset: 0,
            }
        )));
        assert!(!super::super::read_only(&Request::AgentHistory(
            Action::Resume(entry())
        )));
        assert!(!super::super::read_only(&Request::AgentHistory(
            Action::ResumeVault(VaultTarget::from(&entry()))
        )));
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        let mut invalid = entry();
        invalid.id = "--help".into();
        assert_eq!(
            reg.agent_history(&peer, Action::Resume(invalid.clone())),
            Err(Failure::BadRequest)
        );
        assert_eq!(
            reg.agent_history(&peer, Action::ResumeVault(VaultTarget::from(&invalid))),
            Err(Failure::BadRequest)
        );
        assert_eq!(
            reg.agent_history(&peer, Action::Resume(entry())),
            Err(Failure::NoHolder)
        );
        assert_eq!(
            reg.agent_history(
                &peer,
                Action::ResumeVault(VaultTarget {
                    agent: VaultAgent::OpenCode,
                    id: "ses_12345abc".into(),
                    cwd: "/tmp".into(),
                    modified: 0,
                })
            ),
            Err(Failure::NoHolder)
        );
    }

    #[test]
    fn unnegotiated_scan_and_observer_resume_are_rejected_on_wire() {
        use std::time::Duration;
        for observer in [false, true] {
            let (mut client, server) = UnixStream::pair().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let server_thread = std::thread::spawn(move || {
                let hello = ServerHello {
                    proto_min: ipc::PROTO_MIN,
                    proto_max: ipc::PROTO_MAX,
                    features: ipc::FEATURES,
                    build: "test".into(),
                    instance_id: 1,
                };
                Registry::new(None, None).serve(server, &hello).unwrap();
            });
            ipc::read_frame(&mut client).unwrap().unwrap();
            let mut hello = krill_proto::daemon::client_hello(ipc::ClientRole::App, "history-test");
            hello.features = if observer {
                ipc::FEATURES
            } else {
                FEAT_CONTROL
            };
            client.write_all(&hello.encode().unwrap()).unwrap();
            if observer {
                client
                    .write_all(&roles::Request::Set(Role::Observer).encode().unwrap())
                    .unwrap();
                let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
                assert_eq!(Ack::decode(kind, &body).unwrap(), Ack::Applied);
            }
            let action = if observer {
                Action::Resume(entry())
            } else {
                Action::Scan
            };
            client
                .write_all(&Request::AgentHistory(action).encode().unwrap())
                .unwrap();
            let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
            assert_eq!(
                Reply::decode(kind, &body).unwrap(),
                Reply::Failed(if observer {
                    Failure::Denied
                } else {
                    Failure::BadRequest
                })
            );
            drop(client);
            server_thread.join().unwrap();
        }
    }

    #[test]
    fn resume_vault_agy_preserves_isolated_account_profile_and_records_launch_account() {
        use crate::os::{AsFd as _, OwnedFd};
        use std::sync::Arc;

        use crate::holder::Holder;
        use crate::holder::link::Link;
        use krill_proto::holder::{FromHolder, ToHolder, VERSION};

        let dir = std::env::temp_dir().join(format!("owe753-reg-vault-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let ws = dir.join("workspace");
        let agy_profile = dir.join("profiles/agy/work-slot");
        let conv_id = "55555555-2222-4333-8444-555555555555";
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(
            agy_profile
                .join(".gemini/antigravity-cli/brain")
                .join(conv_id),
        )
        .unwrap();

        let (krilld_end, holder_end) = UnixStream::pair().unwrap();
        let mut holder_link = Link::new(OwnedFd::from(holder_end));
        for m in [
            FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            },
            FromHolder::Ready,
        ] {
            holder_link.send(&m.encode().unwrap(), &[]).unwrap();
        }
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(krilld_end)).unwrap();
        let reg = Arc::new(Registry::new(Some((holder, Vec::new())), Some(&dir)));
        let reg_reader = Arc::clone(&reg);
        let reader_worker = std::thread::spawn(move || {
            reg_reader.holder.as_ref().unwrap().run(link, |_, _| {});
        });

        let expected_gemini_flag =
            format!("--gemini_dir={}", agy_profile.join(".gemini").display());
        let holder_worker = std::thread::spawn(move || {
            let pkt = holder_link.recv().unwrap().unwrap();
            let ToHolder::Spawn { req, id, argv, .. } = ToHolder::decode(pkt.frame()).unwrap()
            else {
                panic!("expected Spawn");
            };
            let args: Vec<_> = argv
                .iter()
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            assert!(
                args.iter().any(|a| a == &expected_gemini_flag),
                "expected {expected_gemini_flag} in {args:?}"
            );
            assert_eq!(
                &args[args.len() - 2..],
                ["--conversation", conv_id],
                "args={args:?}"
            );
            let (master, _pty) = UnixStream::pair().unwrap();
            holder_link
                .send(
                    &FromHolder::Spawned { req, id, pid: 900 }.encode().unwrap(),
                    &[master.as_fd()],
                )
                .unwrap();
            id
        });

        let full = Peer {
            pid: None,
            scope: Scope::Full,
        };
        let reply = reg
            .agent_history(
                &full,
                Action::ResumeVault(VaultTarget {
                    agent: VaultAgent::Agy,
                    id: conv_id.into(),
                    cwd: ws.display().to_string(),
                    modified: 1,
                }),
            )
            .unwrap();
        let spawned_id = holder_worker.join().unwrap();
        reader_worker.join().unwrap();
        assert_eq!(
            reply,
            Reply::AgentHistory(Response::Resumed {
                session: spawned_id
            })
        );
        let stored = reg.agents.stored(spawned_id).unwrap().unwrap();
        assert_eq!(stored.profile, "agy");
        assert_eq!(stored.session_id.as_deref(), Some(conv_id));
        let recorded_acc = stored.account.unwrap();
        assert_eq!(recorded_acc.env_key, "ANTIGRAVITY_CLI_HOME");
        assert_eq!(recorded_acc.dir, agy_profile.display().to_string());

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(all(test, target_os = "macos"))]
mod chat_process_tests {
    use super::super::*;
    use krill_proto::agent::Permission;
    use krill_proto::agent_history::ChatItemKind;
    use std::fs::{self, File};
    use std::process::{Child, Command, Stdio};

    struct HeldTranscript(Child);
    impl Drop for HeldTranscript {
        fn drop(&mut self) {
            self.0.kill().ok();
            self.0.wait().ok();
        }
    }

    #[test]
    fn chat_binds_process_owned_transcripts_outside_daemon_account_roots() {
        for profile in ["codex", "claude"] {
            for known_id in [false, true] {
                let dir = std::env::temp_dir().join(format!(
                    "desktop-chat-process-{}-{profile}-{known_id}",
                    std::process::id()
                ));
                fs::remove_dir_all(&dir).ok();
                let path = dir.join("external-account").join(if profile == "codex" {
                    "sessions/2026/10/24/rollout-owned.jsonl"
                } else {
                    "projects/project/owned.jsonl"
                });
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                let sid = "11111111-2222-4333-8444-555555555555";
                let data = if profile == "codex" {
                    format!(
                        "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{sid}\",\"cwd\":\"/same/project\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"Process owned reply\"}}]}}}}\n"
                    )
                } else {
                    format!(
                        "{{\"sessionId\":\"{sid}\",\"cwd\":\"/same/project\",\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"Process owned reply\"}}]}}}}\n"
                    )
                };
                fs::write(&path, data).unwrap();
                let path = fs::canonicalize(path).unwrap();
                let child = HeldTranscript(
                    Command::new("/bin/sleep")
                        .arg("30")
                        .stdin(Stdio::from(File::open(&path).unwrap()))
                        .spawn()
                        .unwrap(),
                );
                let state = dir.join("state");
                let reg = Registry::new(None, Some(&state));
                let id = SessionId(993);
                reg.agents
                    .remember_session(
                        id,
                        &control::AgentLaunch {
                            profile: profile.into(),
                            permission: Permission::Manual,
                        },
                        known_id.then(|| sid.into()),
                        b"/same/project".to_vec(),
                        None,
                    )
                    .unwrap();
                let (master, _writer) = UnixStream::pair().unwrap();
                let session = Session::start(
                    id,
                    OwnedFd::from(master),
                    WinSize { cols: 80, rows: 24 },
                    Arc::clone(&reg.look),
                    None,
                )
                .unwrap();
                lock(&reg.state).sessions.insert(
                    id,
                    Entry {
                        session,
                        pid: child.0.id(),
                        cwd: "/same/project".into(),
                    },
                );
                let snap = reg.chat_poll(id, 0);
                assert!(snap.bound, "{profile} known_id={known_id}: {snap:?}");
                assert_eq!(snap.session_id, sid);
                assert_eq!(Path::new(&snap.transcript_path), path);
                assert_eq!(snap.items.len(), 1);
                assert_eq!(snap.items[0].kind, ChatItemKind::Assistant);
                assert_eq!(snap.items[0].text, "Process owned reply");
                let stored = reg.agents.stored(id).unwrap().unwrap();
                assert_eq!(stored.session_id.as_deref(), Some(sid));
                assert_eq!(
                    stored.transcript_path.as_deref().map(Path::new),
                    Some(path.as_path())
                );
                assert!(reg.chat_poll(id, snap.next_offset).items.is_empty());
                // An existing hook identity cannot be overwritten by a different open transcript.
                reg.agents
                    .remember_session(
                        id,
                        &control::AgentLaunch {
                            profile: profile.into(),
                            permission: Permission::Manual,
                        },
                        Some("different-conversation".into()),
                        b"/same/project".to_vec(),
                        None,
                    )
                    .unwrap();
                let mismatch = reg.chat_poll(id, snap.next_offset);
                assert!(!mismatch.bound);
                assert!(mismatch.reset);
                assert!(mismatch.items.is_empty());
                assert_eq!(
                    reg.agents
                        .stored(id)
                        .unwrap()
                        .unwrap()
                        .session_id
                        .as_deref(),
                    Some("different-conversation")
                );
                drop(child);
                drop(reg);
                fs::remove_dir_all(dir).unwrap();
            }
        }
    }
}
