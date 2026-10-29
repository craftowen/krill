use super::*;

use std::fs;

use krill_proto::agent::Permission;

#[test]
fn sidebar_uses_launch_and_persisted_hook_cwd_without_shell_reports() {
    use krill_proto::os::ffi::OsStrExt as _;

    let dir = std::env::temp_dir().join(format!("sidebar-cwd-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let work = dir.join("project");
    fs::create_dir_all(&work).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(&work)
            .status()
            .unwrap()
            .success()
    );
    let work = krill_proto::os::canonicalize(&work).unwrap();
    let id = SessionId(41);
    let peer = Peer {
        pid: None,
        scope: Scope::Session(id),
    };
    // A reader with no usable process cwd and no OSC report reproduces Windows on macOS too.
    let insert = |reg: &Registry, cwd: PathBuf| {
        let (master, writer) = UnixStream::pair().unwrap();
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
                pid: 0,
                cwd,
            },
        );
        writer
    };
    let check = |reg: &Registry, expected: &[u8]| {
        let Reply::Sidebar(data) = reg.handle(&peer, Request::Sidebar, &mut HashMap::new()) else {
            panic!("expected sidebar reply");
        };
        assert_eq!(data.sessions.len(), 1);
        assert_eq!(data.sessions[0].session, id);
        assert_eq!(data.sessions[0].cwd, expected);
        if !expected.is_empty() {
            assert_eq!(data.repositories.len(), 1);
            assert_eq!(data.repositories[0].worktrees, [expected]);
        }
    };
    let reg = Registry::new(None, Some(&dir));
    let writer = insert(&reg, work.clone());
    check(&reg, work.as_os_str().as_bytes());
    // Login-shell agents persist the hook workspace, even if the initial shell directory differs.
    assert!(
        !reg.agents.record_hook_session_with_context(
            id,
            &serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "SessionStart",
                "session_id": "sidebar-cwd-conversation",
                "cwd": work,
            }))
            .unwrap(),
            Some("claude"),
            None,
        )
    );
    drop(writer);
    drop(reg);

    // Daemon replacement starts with empty Entry::cwd and an empty terminal cwd parser.
    let reg = Registry::new(None, Some(&dir));
    let writer = insert(&reg, PathBuf::new());
    check(&reg, work.as_os_str().as_bytes());
    assert_eq!(reg.session_cwd(id), Some(work.clone()));
    lock(&reg.state).sessions.get_mut(&id).unwrap().cwd = dir.clone();
    assert_eq!(reg.session_cwd(id), Some(work.clone()));
    // A later observed directory must survive loss of the live cwd, ahead of stale launch data.
    let latest = work.join("latest");
    fs::create_dir_all(&latest).unwrap();
    sidebar::snapshot(&reg.sidebar, &[(id, Some(latest.clone()))]).unwrap();
    assert_eq!(reg.session_cwd(id), Some(latest));
    drop(writer);
    drop(reg);

    // With no cwd evidence, leave the session unclassified rather than selecting another project.
    let reg = Registry::new(None, None);
    let writer = insert(&reg, PathBuf::new());
    check(&reg, b"");
    drop(writer);
    drop(reg);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn agent_settings_and_metadata_obey_peer_scope() {
    let reg = Registry::new(None, None);
    let mut clients = HashMap::new();
    for scope in [Scope::Restricted, Scope::Session(SessionId(7))] {
        let peer = Peer { pid: None, scope };
        for permission in [None, Some(Permission::Yolo)] {
            assert_eq!(
                reg.handle(&peer, Request::AgentSettings { permission }, &mut clients),
                Reply::Failed(Failure::Denied)
            );
        }
        assert_eq!(
            reg.handle(
                &peer,
                Request::AgentLaunch {
                    session: SessionId(9)
                },
                &mut clients
            ),
            Reply::Failed(Failure::Denied)
        );
    }
    let full = Peer {
        pid: None,
        scope: Scope::Full,
    };
    assert!(matches!(
        reg.handle(
            &full,
            Request::AgentSettings { permission: None },
            &mut clients
        ),
        Reply::AgentSettings(control::AgentSettings {
            permission: Permission::Yolo,
            ..
        })
    ));
    assert_eq!(
        reg.handle(
            &full,
            Request::AgentLaunch {
                session: SessionId(9)
            },
            &mut clients
        ),
        Reply::Failed(Failure::NoSession)
    );
    assert!(read_only(&Request::AgentSettings { permission: None }));
    assert!(!read_only(&Request::AgentSettings {
        permission: Some(Permission::Yolo)
    }));
}

#[test]
fn capture_since_hides_out_of_scope_sessions_and_remains_read_only() {
    let reg = Registry::new(None, None);
    let mut clients = HashMap::new();
    for scope in [Scope::Restricted, Scope::Session(SessionId(7))] {
        let peer = Peer { pid: None, scope };
        let request = Request::CaptureSince {
            session: SessionId(9),
            since: Some(control::CaptureStamp {
                session: SessionId(9),
                revision: screen::Revision(1),
                seq: screen::Seq(2),
            }),
        };
        assert!(read_only(&request));
        assert_eq!(
            reg.handle(&peer, request, &mut clients),
            Reply::Failed(Failure::NoSession)
        );
    }
}

#[test]
fn unnegotiated_agent_requests_and_observer_mutations_are_rejected() {
    for (features, observer, expected) in [
        (FEAT_CONTROL, false, Failure::BadRequest),
        (
            FEAT_CONTROL | control::FEAT_AGENTS | control::FEAT_GIT_EDIT | FEAT_ROLES,
            true,
            Failure::Denied,
        ),
    ] {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let thread = std::thread::spawn(move || {
            let hello = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            };
            Registry::new(None, None).serve(server, &hello).unwrap();
        });
        let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        ServerHello::decode(kind, &body).unwrap();
        let mut hello = krill_proto::daemon::client_hello(ipc::ClientRole::App, "test");
        hello.features = features;
        client.write_all(&hello.encode().unwrap()).unwrap();
        if observer {
            client
                .write_all(&roles::Request::Set(Role::Observer).encode().unwrap())
                .unwrap();
            let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
            assert_eq!(Ack::decode(kind, &body).unwrap(), Ack::Applied);
        }
        let requests = if observer {
            vec![
                Request::AgentSettings {
                    permission: Some(Permission::Yolo),
                },
                Request::GitEdit {
                    repo: b"/repo".into(),
                    edit: krill_proto::git::Edit::Commit {
                        message: "m".into(),
                    },
                },
            ]
        } else {
            vec![
                Request::CaptureSince {
                    session: SessionId(9),
                    since: None,
                },
                Request::AgentSettings { permission: None },
                Request::AgentLaunch {
                    session: SessionId(9),
                },
                Request::GitEdit {
                    repo: b"/repo".into(),
                    edit: krill_proto::git::Edit::Commit {
                        message: "m".into(),
                    },
                },
                Request::Create(Create {
                    account: None,
                    program: Program::Agent {
                        id: "fork:9".into(),
                    },
                    cwd: Vec::new(),
                    cols: 80,
                    rows: 24,
                }),
            ]
        };
        for request in requests {
            client.write_all(&request.encode().unwrap()).unwrap();
            let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
            assert_eq!(Reply::decode(kind, &body).unwrap(), Reply::Failed(expected));
        }
        drop(client);
        thread.join().unwrap();
    }

    // OWE-738: FEAT_GIT_EDIT만 협상하고 FEAT_GIT_SYNC를 협상하지 않은 피어가 동기화 변형을 보내면 거부한다.
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let thread = std::thread::spawn(move || {
        let hello = ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features: ipc::FEATURES,
            build: "test".into(),
            instance_id: 1,
        };
        Registry::new(None, None).serve(server, &hello).unwrap();
    });
    let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
    ServerHello::decode(kind, &body).unwrap();
    let mut hello = krill_proto::daemon::client_hello(ipc::ClientRole::App, "test");
    hello.features = FEAT_CONTROL | control::FEAT_GIT_EDIT;
    client.write_all(&hello.encode().unwrap()).unwrap();
    for edit in [
        krill_proto::git::Edit::Push,
        krill_proto::git::Edit::Pull,
        krill_proto::git::Edit::Sync,
        krill_proto::git::Edit::CommitAndPush {
            message: "m".into(),
        },
    ] {
        let req = Request::GitEdit {
            repo: b"/repo".into(),
            edit,
        };
        client.write_all(&req.encode().unwrap()).unwrap();
        let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        assert_eq!(
            Reply::decode(kind, &body).unwrap(),
            Reply::Failed(Failure::BadRequest)
        );
    }
    drop(client);
    thread.join().unwrap();
}

/// OWE-502: 닫은 세션이 목록에서 사라졌으면 holder에 `Release`가 이미 가 있어야 한다. 그렇지 않으면 그 틈에
/// krilld가 죽을 때 holder가 끝난 세션을 다음 krilld에 다시 넘긴다. holder 쪽 버퍼를 가득 채워 `Release` 전송을
/// 붙잡아 두고, 그동안 세션이 목록에 남아 있는지 본다.
#[test]
fn closed_session_leaves_list_only_after_release_reaches_holder() {
    use krill_proto::os::net::UnixStream;
    use std::io::Read;
    use std::time::Duration;

    use krill_proto::holder::{FromHolder, VERSION};

    let (krilld_end, mut holder_end) = UnixStream::pair().unwrap();
    for m in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
            inbox: false,
        },
        FromHolder::Ready,
    ] {
        holder_end.write_all(&m.encode().unwrap()).unwrap();
    }
    let filler = krilld_end.try_clone().unwrap();
    let (holder, _link, _, _) = Holder::connect(OwnedFd::from(krilld_end)).unwrap();
    let (master, _pty) = UnixStream::pair().unwrap();
    let id = SessionId(3);
    let kept = Kept {
        id,
        pid: 1,
        status: None,
        master: OwnedFd::from(master),
    };
    let reg = Registry::new(Some((holder, vec![kept])), None);
    lock(&reg.state).closing.insert(id);

    // krilld → holder 방향을 가득 채운다: 다음 전송(`Release`)은 holder가 읽을 때까지 막힌다.
    filler.set_nonblocking(true).unwrap();
    let mut filled = 0;
    while let Ok(n) = (&filler).write(&[0; 4096]) {
        filled += n;
    }
    filler.set_nonblocking(false).unwrap();

    let listed = || lock(&reg.state).sessions.contains_key(&id);
    std::thread::scope(|s| {
        let exited = s.spawn(|| reg.on_exited(id, 0));
        std::thread::sleep(Duration::from_millis(200));
        let held = listed();
        let mut rest = vec![0; filled];
        holder_end.read_exact(&mut rest).unwrap();
        exited.join().unwrap();
        assert!(
            held,
            "closed session left the list before Release reached the holder"
        );
    });
    assert!(!listed());
    let release = ToHolder::Release { req: 1, id }.encode().unwrap();
    let mut got = vec![0; release.len()];
    holder_end.read_exact(&mut got).unwrap();
    assert_eq!(got, release);
}

#[test]
fn restart_daemon_needs_full_scope_and_holder() {
    let reg = Registry::new(None, None);
    let mut clients = HashMap::new();
    let restart = Request::Resources(Action::RestartDaemon);
    for scope in [Scope::Restricted, Scope::Session(SessionId(7))] {
        let peer = Peer { pid: None, scope };
        assert_eq!(
            reg.handle(&peer, restart.clone(), &mut clients),
            Reply::Failed(Failure::Denied)
        );
    }
    let full = Peer {
        pid: None,
        scope: Scope::Full,
    };
    assert_eq!(
        reg.handle(&full, restart.clone(), &mut clients),
        Reply::Failed(Failure::NoHolder)
    );
    assert!(matches!(
        reg.handle(&full, Request::Resources(Action::Query), &mut clients),
        Reply::Resources(r) if r.daemon > 0 && r.sessions.is_empty()
    ));
    assert!(read_only(&Request::Resources(Action::Query)));
    assert!(!read_only(&restart));
}

#[test]
fn worktree_requests_obey_peer_scope_and_write_audit() {
    let dir = std::env::temp_dir().join(format!(
        "mk-reg-wt-scope-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let reg = Registry::new(None, Some(&dir));
    let mut clients = HashMap::new();
    for scope in [Scope::Restricted, Scope::Session(SessionId(7))] {
        let peer = Peer {
            pid: Some(42),
            scope,
        };
        assert_eq!(
            reg.handle(
                &peer,
                Request::NewWorktree(control::NewWorktree {
                    repo: dir.as_os_str().as_encoded_bytes().to_vec(),
                    name: "feat".into(),
                    base: None,
                    setup: control::Setup::Skip,
                }),
                &mut clients
            ),
            Reply::Failed(Failure::Denied)
        );
        assert_eq!(
            reg.handle(
                &peer,
                Request::PrepareWorktree(control::PrepareWorktree {
                    token: 1,
                    repo: dir.as_os_str().as_encoded_bytes().to_vec(),
                    base: None,
                    cancel: false,
                }),
                &mut clients
            ),
            Reply::Failed(Failure::Denied)
        );
        assert_eq!(
            reg.handle(
                &peer,
                Request::RemoveWorktree(control::RemoveWorktree {
                    repo: dir.as_os_str().as_encoded_bytes().to_vec(),
                    path: dir.join("wt").as_os_str().as_encoded_bytes().to_vec(),
                    force: false,
                }),
                &mut clients
            ),
            Reply::Failed(Failure::Denied)
        );
    }
    let audit = std::fs::read_to_string(dir.join("audit.log")).unwrap();
    assert!(audit.contains("deny op=worktree-new target=0 peer_pid=42 scope=restricted\n"));
    assert!(audit.contains("deny op=worktree-new target=0 peer_pid=42 scope=session:7\n"));
    assert!(audit.contains("deny op=worktree-rm target=0 peer_pid=42 scope=restricted\n"));
    assert!(audit.contains("deny op=worktree-rm target=0 peer_pid=42 scope=session:7\n"));
    std::fs::remove_dir_all(&dir).ok();
}

/// OWE-612: 기동 시 `live`에 없는 자식 ID는 `session-creators`에서 정리된다.
#[test]
fn new_prunes_creators_of_dead_sessions() {
    let dir = std::env::temp_dir().join(format!("mk612-reg-prune-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let stale = vec![(SessionId(2), SessionId(1)), (SessionId(3), SessionId(2))];
    std::fs::write(
        dir.join("session-creators"),
        serde_json::to_vec(&stale).unwrap(),
    )
    .unwrap();
    let _reg = Registry::new(None, Some(&dir));
    let rows: Vec<(SessionId, SessionId)> =
        serde_json::from_slice(&std::fs::read(dir.join("session-creators")).unwrap()).unwrap();
    assert!(
        rows.is_empty(),
        "dead creator entries must be pruned: {rows:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// OWE-590: 실제 IPC 핸드셰이크로 테마를 두 번 푸시하고, 연결을 닫은 뒤에도 질의·디스크 값이 같다.
#[test]
fn theme_ipc_updates_queries_and_survives_reconnect_and_restart() {
    use crate::term::{Engine, Feed};
    use krill_proto::theme::{CellSize, DEFAULT, FEAT_THEME, Push, Rgb};

    let dir = std::env::temp_dir().join(format!("mk590-theme-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let reg = Arc::new(Registry::new(None, Some(&dir)));
    // 세션 작업을 실제 리더에 넣어 IPC 경로가 모드 2031 알림도 보내는지 검사한다.
    let (master, mut pty) = UnixStream::pair().unwrap();
    pty.set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let session = Session::start(
        SessionId(1),
        OwnedFd::from(master),
        WinSize { cols: 80, rows: 24 },
        Arc::clone(&reg.look),
        None,
    )
    .unwrap();
    pty.write_all(b"\x1b[?2031hREADY").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !session.screen_text().contains("READY") {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    lock(&reg.state).sessions.insert(
        SessionId(1),
        Entry {
            session,
            pid: 0,
            cwd: dir.clone(),
        },
    );
    let mut light = DEFAULT;
    light.background = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    light.foreground = Rgb { r: 0, g: 0, b: 0 };
    let push = Push {
        theme: light,
        cell: Some(CellSize {
            width: 9,
            height: 18,
        }),
    };
    for reconnect in 0..2 {
        let (mut client, server) = UnixStream::pair().unwrap();
        let r = Arc::clone(&reg);
        let serving = std::thread::spawn(move || {
            r.serve(
                server,
                &ServerHello {
                    proto_min: ipc::PROTO_MIN,
                    proto_max: ipc::PROTO_MAX,
                    features: ipc::FEATURES,
                    build: "test".into(),
                    instance_id: 1,
                },
            )
            .unwrap();
        });
        ipc::read_frame(&mut client).unwrap().unwrap();
        let mut hello = krill_proto::daemon::client_hello(ipc::ClientRole::App, "test");
        hello.features = FEAT_THEME;
        client.write_all(&hello.encode().unwrap()).unwrap();
        for theme in [DEFAULT, light] {
            client
                .write_all(&Push { theme, ..push }.encode().unwrap())
                .unwrap();
        }
        client.shutdown(std::net::Shutdown::Write).unwrap();
        serving.join().unwrap();
        let expected = if reconnect == 0 {
            "\x1b[?997;2n"
        } else {
            "\x1b[?997;1n\x1b[?997;2n"
        };
        let mut notice = vec![0; expected.len()];
        std::io::Read::read_exact(&mut pty, &mut notice).unwrap();
        assert_eq!(notice, expected.as_bytes());
        let mut engine = Engine::new(WinSize { cols: 80, rows: 24 });
        assert_eq!(
            engine.feed(b"\x1b]10;?\x07\x1b]11;?\x07\x1b[14t", Feed::Live, &reg.look),
            "\x1b]10;rgb:0000/0000/0000\x07\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[4;432;720t"
        );
    }
    drop(pty);
    drop(reg);
    let restarted = Registry::new(None, Some(&dir));
    assert_eq!(restarted.look.theme(), light);
    assert_eq!(restarted.look.cell_size(), None);
    let mut engine = Engine::new(WinSize { cols: 80, rows: 24 });
    assert_eq!(
        engine.feed(b"\x1b]11;?\x07", Feed::Live, &restarted.look),
        "\x1b]11;rgb:ffff/ffff/ffff\x07"
    );
    drop(restarted);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn theme_push_requires_negotiation_and_interactive_app() {
    use krill_proto::theme::{DEFAULT, FEAT_THEME, Push};
    for (features, role, observer) in [
        (FEAT_CONTROL, ipc::ClientRole::App, false),
        (FEAT_THEME, ipc::ClientRole::Cli, false),
        (FEAT_THEME | FEAT_ROLES, ipc::ClientRole::App, true),
    ] {
        let reg = Arc::new(Registry::new(None, None));
        let (mut client, server) = UnixStream::pair().unwrap();
        let r = Arc::clone(&reg);
        let serving = std::thread::spawn(move || {
            r.serve(
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
        let mut hello = krill_proto::daemon::client_hello(role, "test");
        hello.features = features;
        client.write_all(&hello.encode().unwrap()).unwrap();
        if observer {
            client
                .write_all(&roles::Request::Set(Role::Observer).encode().unwrap())
                .unwrap();
            ipc::read_frame(&mut client).unwrap().unwrap();
        }
        let mut theme = DEFAULT;
        theme.background.r = 255;
        client
            .write_all(&Push { theme, cell: None }.encode().unwrap())
            .unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        serving.join().unwrap().unwrap();
        assert_eq!(reg.look.theme(), DEFAULT);
    }
}

/// OWE-605: 기록 저장소를 열지 못하면 조용히 넘기지 않고 상태 디렉터리에 경고를 남긴다.
#[test]
fn store_open_failure_leaves_warning() {
    let dir = std::env::temp_dir().join(format!(
        "mk-reg-store-warn-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    // 기록 설정을 깨뜨려 `Store::open`을 실패시킨다.
    std::fs::write(dir.join("history.toml"), "recording = \"yes\"\n").unwrap();
    let _reg = Registry::new(None, Some(&dir));
    let warnings =
        std::fs::read_to_string(dir.join(krill_proto::diag::RECORD_WARNINGS_FILE)).unwrap();
    assert!(
        warnings.contains("store open failed"),
        "기록 실패 경고가 없음: {warnings:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn preparation_requires_feature_and_rejects_observer() {
    for (features, expected) in [
        (FEAT_CONTROL, Failure::BadRequest),
        (
            FEAT_CONTROL | control::FEAT_WORKTREE_PREPARE | FEAT_ROLES,
            Failure::Denied,
        ),
    ] {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let server = std::thread::spawn(move || {
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
        let mut hello = krill_proto::daemon::client_hello(ipc::ClientRole::App, "test");
        hello.features = features;
        client.write_all(&hello.encode().unwrap()).unwrap();
        if features & FEAT_ROLES != 0 {
            client
                .write_all(&roles::Request::Set(Role::Observer).encode().unwrap())
                .unwrap();
            ipc::read_frame(&mut client).unwrap().unwrap();
        }
        client
            .write_all(
                &Request::PrepareWorktree(control::PrepareWorktree {
                    token: 1,
                    repo: b"/not-a-repo".to_vec(),
                    base: None,
                    cancel: false,
                })
                .encode()
                .unwrap(),
            )
            .unwrap();
        let (kind, bytes) = ipc::read_frame(&mut client).unwrap().unwrap();
        assert_eq!(
            Reply::decode(kind, &bytes).unwrap(),
            Reply::Failed(expected)
        );
        drop(client);
        server.join().unwrap();
    }
}

/// MK-096: `serve()`는 핸드셰이크 전에 서명 정책을 강제한다. 테스트 바이너리 자신(ad-hoc 서명)의 식별자와
/// 같으면 이어지고, 다르면 `ServerHello`도 보내지 않고 닫는다. 정책 파일이 없으면 기존처럼 이어진다.
#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "macOS 전용: 코드서명(LOCAL_PEERTOKEN)"
)]
fn serve_enforces_peer_signing_policy() {
    let dir = std::env::temp_dir().join(format!("mksign-serve-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    // 이 테스트 프로세스 자신의 식별자를 읽는다(socketpair 피어 = 자기).
    let me = {
        let (probe_client, probe_server) = UnixStream::pair().unwrap();
        let token = crate::sys::peer_token(probe_server.as_fd()).unwrap();
        let sign = crate::sys::verify_audit_token(&token);
        assert!(sign.valid, "test binary must be ad-hoc signed");
        drop(probe_client);
        drop(probe_server);
        sign.identifier.unwrap()
    };
    for (tag, policy, expect_open) in [
        (
            "match",
            Some(format!("require_identifier = \"{me}\"\n")),
            true,
        ),
        (
            "mismatch",
            Some("require_identifier = \"com.example.definitely-not-me\"\n".to_owned()),
            false,
        ),
        ("off", None, true),
    ] {
        let path = dir.join(scope::SIGNING_POLICY_FILE);
        match policy {
            Some(text) => std::fs::write(&path, text).unwrap(),
            None => {
                std::fs::remove_file(&path).ok();
            }
        }
        let reg = Registry::new(None, Some(&dir));
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            // Security.framework 검증이 ServerHello보다 먼저 실행되며 로컬 실측에서 2초를 넘긴다.
            // IPC 왕복 상한과 코드서명 IO 시간을 혼동하지 않는다.
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .unwrap();
        let thread = std::thread::spawn(move || {
            let hello = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            };
            reg.serve(server, &hello).unwrap();
        });
        let first = ipc::read_frame(&mut client).unwrap();
        if expect_open {
            let (kind, body) = first.unwrap();
            ServerHello::decode(kind, &body).unwrap();
            let mut hello = krill_proto::daemon::client_hello(ipc::ClientRole::App, "test");
            hello.features = FEAT_CONTROL;
            client.write_all(&hello.encode().unwrap()).unwrap();
            client.write_all(&Request::List.encode().unwrap()).unwrap();
            let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
            assert!(
                matches!(Reply::decode(kind, &body).unwrap(), Reply::Sessions(_)),
                "{tag}"
            );
        } else {
            assert!(
                first.is_none(),
                "{tag}: unsigned peer must be closed before hello"
            );
        }
        drop(client);
        thread.join().unwrap();
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn telemetry_publisher_defaults_on_and_respects_opt_out_and_environment_vetoes() {
    use krill_proto::telemetry::{self, Env, Event};
    let dir = std::env::temp_dir().join(format!("mk089-publish-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let registry = Registry::new(None, Some(&dir));
    let event = Event::AppOpened(telemetry::AppOpened {});
    let mut transport = telemetry::Recorder::default();
    assert!(registry.emit_telemetry_to(&Env::default(), event.clone(), &mut transport));
    assert_eq!(transport.sent.len(), 1);
    transport.sent.clear();
    telemetry::disable(&dir).unwrap();
    assert!(!registry.emit_telemetry_to(&Env::default(), event.clone(), &mut transport));
    assert!(transport.sent.is_empty());
    telemetry::enable(&dir).unwrap();
    for key in [telemetry::DO_NOT_TRACK, telemetry::KRILL_DISABLED, "CI"] {
        assert!(!registry.emit_telemetry_to(
            &Env::stub(&[(key, "1")]),
            event.clone(),
            &mut transport,
        ));
        assert!(transport.sent.is_empty());
    }
    assert!(registry.emit_telemetry_to(&Env::default(), event.clone(), &mut transport));
    assert_eq!(transport.sent.len(), 1);
    let envelope: telemetry::Envelope = serde_json::from_str(&transport.sent[0]).unwrap();
    assert_eq!(envelope.event, event);
    assert!(!transport.sent[0].contains(&dir.display().to_string()));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn daemon_startup_collects_expired_orphan_launch_metadata() {
    let dir = std::env::temp_dir().join(format!("startup-launch-gc-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let agents = crate::agent::control::Control::new(Some(&dir));
    let id = SessionId(42);
    agents
        .remember(
            id,
            &control::AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Manual,
            },
        )
        .unwrap();
    let old = SystemTime::now() - Duration::from_hours(8 * 24);
    fs::File::options()
        .write(true)
        .open(dir.join("agent-launches/42.json"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(old))
        .unwrap();
    let reg = Registry::new(None, Some(&dir));
    assert!(!reg.agents.has(id));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn monitor_tick_collects_history_on_interval_without_restart() {
    let dir = std::env::temp_dir().join(format!("mk587-gc-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let reg = Registry::new(None, Some(&dir));
    let store = reg.store.as_ref().unwrap();
    let mut rec = store.recorder(SessionId(42), FALLBACK_SIZE).unwrap();
    rec.append(b"expired");
    rec.seal();
    let last = *lock(&reg.last_gc);
    let later = SystemTime::now() + Duration::from_hours(8 * 24);
    reg.gc_due(last + GC_INTERVAL / 2, later);
    assert!(store.session_dir(SessionId(42)).exists());
    reg.gc_due(last + GC_INTERVAL, later);
    assert!(!store.session_dir(SessionId(42)).exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn git_mutations_reject_scoped_peers_and_are_never_read_only() {
    let reg = Registry::new(None, None);
    for scope in [Scope::Restricted, Scope::Session(SessionId(7))] {
        let peer = Peer { pid: None, scope };
        for edit in [
            krill_proto::git::Edit::Stage {
                path: b"file".into(),
                old_path: None,
            },
            krill_proto::git::Edit::Unstage {
                path: b"file".into(),
                old_path: None,
            },
            krill_proto::git::Edit::Commit {
                message: "message".into(),
            },
        ] {
            let request = Request::GitEdit {
                repo: b"/repo".into(),
                edit,
            };
            assert!(!read_only(&request));
            assert_eq!(
                reg.handle(&peer, request, &mut HashMap::new()),
                Reply::Failed(Failure::Denied)
            );
        }
    }
}

/// K-25: 같은 세션의 동시 `Resume`는 먼저 잡은 쪽만 진행한다.
/// 두 번째 표시는 기다리지 않고 거부되고, 첫 표시를 놓으면 다시 잡힌다.
#[test]
fn concurrent_resume_claims_serialize() {
    use krill_proto::SessionId;
    use std::sync::Arc;
    let reg = Arc::new(Registry::new(None, None));
    let first = reg.claim_resume(SessionId(7));
    assert!(first.is_some());
    // 진행 중인 표시는 다른 스레드에서 기다리지 않고 바로 거부된다.
    let (tx, rx) = std::sync::mpsc::channel();
    let busy = Arc::clone(&reg);
    std::thread::spawn(move || {
        tx.send(busy.claim_resume(SessionId(7)).is_none()).unwrap();
    });
    assert!(
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
        "second claim must fail fast instead of blocking"
    );
    drop(first);
    assert!(reg.claim_resume(SessionId(7)).is_some());
}

#[test]
fn cross_review_resume_claims_are_per_session() {
    let reg = Registry::new(None, None);
    let first = reg.claim_resume(SessionId(7)).unwrap();
    let other = reg.claim_resume(SessionId(8));
    assert!(
        other.is_some(),
        "a slow resume must not reject unrelated sessions"
    );
    assert!(reg.claim_resume(SessionId(7)).is_none());
    drop(first);
    assert!(reg.claim_resume(SessionId(7)).is_some());
    assert!(reg.claim_resume(SessionId(8)).is_none());
}

/// OWE-707: `SessionAccounts`는 실행 당시 계정만 담는다(실패 우선).
/// 셸·구 기록·라벨 없는 세션은 빠져 배지를 숨기고, 원 세션 PTY는 절대 닫지 않는다.
#[test]
fn session_accounts_returns_launch_account_with_label() {
    use krill_proto::os::net::UnixStream;
    use std::io::Write as _;

    use krill_proto::control::SessionAccount;
    use krill_proto::holder::{FromHolder, VERSION};

    let dir = std::env::temp_dir().join(format!("owe707-accounts-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    // `session_accounts`가 읽는 `<state_dir>/accounts.toml`에 표시용 라벨을 둔다.
    fs::write(
        dir.join("accounts.toml"),
        "[[accounts]]\nid = \"claude-work\"\nprovider = \"claude\"\nlabel = \"Work Max\"\nprofile_dir = \"/tmp/owe707-work\"\n",
    )
    .unwrap();

    // holder 핸드셰이크(Hello·Ready)를 먼저 보내야 `Holder::connect`가 돌아온다.
    let (krilld_end, mut holder_end) = UnixStream::pair().unwrap();
    for m in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
            inbox: false,
        },
        FromHolder::Ready,
    ] {
        holder_end.write_all(&m.encode().unwrap()).unwrap();
    }
    let (master_a, _pty_a) = UnixStream::pair().unwrap();
    let (master_b, _pty_b) = UnixStream::pair().unwrap();
    let (holder, _link, _, _) = Holder::connect(OwnedFd::from(krilld_end)).unwrap();
    let reg = Registry::new(
        Some((
            holder,
            vec![
                Kept {
                    id: SessionId(7),
                    pid: 1,
                    status: None,
                    master: OwnedFd::from(master_a),
                },
                Kept {
                    id: SessionId(8),
                    pid: 2,
                    status: None,
                    master: OwnedFd::from(master_b),
                },
            ],
        )),
        Some(&dir),
    );
    // 세션 7만 실행 당시 계정을 기록한다. 세션 8(셸·구 기록)은 빠져야 한다.
    reg.agents
        .remember_session(
            SessionId(7),
            &control::AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Manual,
            },
            None,
            vec![],
            Some(crate::agent::control::LaunchAccount {
                id: "claude-work".into(),
                env_key: "CLAUDE_CONFIG_DIR".into(),
                dir: "/tmp/owe707-work".into(),
            }),
        )
        .unwrap();

    let full = Peer {
        pid: None,
        scope: Scope::Full,
    };
    assert!(read_only(&Request::SessionAccounts));
    assert_eq!(
        reg.handle(&full, Request::SessionAccounts, &mut HashMap::new()),
        Reply::SessionAccounts(vec![SessionAccount {
            session: SessionId(7),
            account_id: "claude-work".into(),
            label: "Work Max".into(),
        }])
    );
    // 세션 범위 피어는 자기 것만 본다.
    let peer7 = Peer {
        pid: None,
        scope: Scope::Session(SessionId(7)),
    };
    assert_eq!(
        reg.handle(&peer7, Request::SessionAccounts, &mut HashMap::new()),
        Reply::SessionAccounts(vec![SessionAccount {
            session: SessionId(7),
            account_id: "claude-work".into(),
            label: "Work Max".into(),
        }])
    );
    let peer8 = Peer {
        pid: None,
        scope: Scope::Session(SessionId(8)),
    };
    assert_eq!(
        reg.handle(&peer8, Request::SessionAccounts, &mut HashMap::new()),
        Reply::SessionAccounts(vec![])
    );
    fs::remove_dir_all(&dir).ok();
}

/// OWE-722: 실제 usage 분기에서 저장·조회·권한·협상 거부를 확인한다.
#[test]
fn provisioning_usage_dispatch_and_access_control() {
    use krill_proto::usage::{UsageReply, UsageRequest};
    let root = std::env::temp_dir().join(format!("owe722-dispatch-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let reg = Registry::new(None, Some(&root));
    let full = Peer {
        pid: None,
        scope: Scope::Full,
    };
    assert_eq!(
        reg.on_usage(&full, Role::Interactive, UsageRequest::GetProvisioning),
        UsageReply::Provisioning(true),
        "설정 파일이 없으면 기본 켜짐"
    );
    for enabled in [false, true] {
        assert_eq!(
            reg.on_usage(
                &full,
                Role::Interactive,
                UsageRequest::SetProvisioning(enabled)
            ),
            UsageReply::Provisioning(enabled)
        );
        assert_eq!(
            reg.on_usage(&full, Role::Observer, UsageRequest::GetProvisioning),
            UsageReply::Provisioning(enabled)
        );
    }
    assert_eq!(
        reg.on_usage(&full, Role::Observer, UsageRequest::SetProvisioning(true)),
        UsageReply::Failed(Failure::Denied)
    );
    for scope in [Scope::Restricted, Scope::Session(SessionId(1))] {
        let peer = Peer { pid: None, scope };
        for request in [
            UsageRequest::GetProvisioning,
            UsageRequest::SetProvisioning(true),
        ] {
            assert_eq!(
                reg.on_usage(&peer, Role::Interactive, request),
                UsageReply::Failed(Failure::Denied)
            );
        }
    }
    let raw = "provisioning = [broken";
    fs::write(root.join("accounts.toml"), raw).unwrap();
    for request in [
        UsageRequest::GetProvisioning,
        UsageRequest::SetProvisioning(true),
    ] {
        assert!(matches!(
            reg.on_usage(&full, Role::Interactive, request),
            UsageReply::Failed(Failure::Os(_))
        ));
    }
    assert_eq!(fs::read_to_string(root.join("accounts.toml")).unwrap(), raw);
    drop(reg);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn provisioning_requests_without_management_bit_are_rejected_by_daemon() {
    use krill_proto::usage::{FEAT_USAGE, UsageReply, UsageRequest};
    for request in [
        UsageRequest::GetProvisioning,
        UsageRequest::SetProvisioning(true),
    ] {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            let hello = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            };
            Registry::new(None, None).serve(server, &hello).unwrap();
        });
        let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        ServerHello::decode(kind, &body).unwrap();
        let mut hello =
            krill_proto::daemon::client_hello(ipc::ClientRole::Cli, "provisioning-test");
        hello.features = FEAT_USAGE;
        client.write_all(&hello.encode().unwrap()).unwrap();
        client.write_all(&request.encode().unwrap()).unwrap();
        let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        assert_eq!(
            UsageReply::decode(kind, &body).unwrap(),
            UsageReply::Failed(Failure::BadRequest)
        );
        drop(client);
        worker.join().unwrap();
    }
}

/// OWE-736: `Program::Agent { id: "fork:<sid>" }`는 원 세션을 닫지 않고 같은 cwd·프로필·권한으로
/// 포크된 새 에이전트 세션을 띄운다.
#[test]
fn fork_creates_new_session_and_keeps_original_session_alive() {
    use crate::os::AsFd as _;
    use krill_proto::os::net::UnixStream;

    use crate::holder::link::Link;
    use krill_proto::holder::{FromHolder, ToHolder, VERSION};

    let dir = std::env::temp_dir().join(format!("owe736-reg-fork-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();

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
    let (master_orig, _pty_orig) = UnixStream::pair().unwrap();
    let (holder, link, _, _) = Holder::connect(OwnedFd::from(krilld_end)).unwrap();
    let reg = Arc::new(Registry::new(
        Some((
            holder,
            vec![Kept {
                id: SessionId(7),
                pid: 700,
                status: None,
                master: OwnedFd::from(master_orig),
            }],
        )),
        Some(&dir),
    ));
    let reg_reader = Arc::clone(&reg);
    let reader_worker = std::thread::spawn(move || {
        reg_reader.holder.as_ref().unwrap().run(link, |_, _| {});
    });
    reg.agents
        .remember_session(
            SessionId(7),
            &control::AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Yolo,
            },
            Some("11111111-2222-4333-8444-555555555555".into()),
            b"/tmp/owe736-cwd".to_vec(),
            None,
        )
        .unwrap();

    // 가짜 holder 스레드: Spawn 요청을 받으면 argv를 검증하고 Spawned 응답 + 새 PTY fd를 돌려준다.
    let holder_worker = std::thread::spawn(move || {
        let pkt = holder_link.recv().unwrap().unwrap();
        let msg = ToHolder::decode(pkt.frame()).unwrap();
        let ToHolder::Spawn {
            req, id, argv, cwd, ..
        } = msg
        else {
            panic!("expected Spawn, got {msg:?}");
        };
        let args: Vec<_> = argv
            .iter()
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        assert!(
            args.iter()
                .any(|a| a == "--resume=11111111-2222-4333-8444-555555555555"),
            "fork args={args:?}"
        );
        assert_eq!(args.last().map(String::as_str), Some("--fork-session"));
        assert_eq!(cwd, b"/tmp/owe736-cwd");
        let (master_new, _pty_new) = UnixStream::pair().unwrap();
        holder_link
            .send(
                &FromHolder::Spawned { req, id, pid: 800 }.encode().unwrap(),
                &[master_new.as_fd()],
            )
            .unwrap();
        // 2번째 Spawn: 포크된 세션(spawned_id)을 hook 수신 전에 다시 포크할 때 미주입 UUID가 아니라 `--continue --fork-session`이 넘어간다(OWE-752).
        let pkt2 = holder_link.recv().unwrap().unwrap();
        let ToHolder::Spawn {
            req: req2,
            id: id2,
            argv: argv2,
            ..
        } = ToHolder::decode(pkt2.frame()).unwrap()
        else {
            panic!("expected second Spawn");
        };
        let args2: Vec<_> = argv2
            .iter()
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        assert!(
            args2.iter().any(|a| a == "--continue"),
            "re-fork before hook must use --continue, got {args2:?}"
        );
        assert!(
            !args2.iter().any(|a| a.starts_with("--resume=")),
            "re-fork before hook must not pass uninjected UUID: {args2:?}"
        );
        assert_eq!(args2.last().map(String::as_str), Some("--fork-session"));
        let (master2, _pty2) = UnixStream::pair().unwrap();
        holder_link
            .send(
                &FromHolder::Spawned {
                    req: req2,
                    id: id2,
                    pid: 801,
                }
                .encode()
                .unwrap(),
                &[master2.as_fd()],
            )
            .unwrap();

        // 3번째 Spawn: 포크된 세션(spawned_id)이 hook 수신 전에 종료된 뒤 Resume할 때 `--resume`만 넘어간다(OWE-752).
        let pkt3 = holder_link.recv().unwrap().unwrap();
        let ToHolder::Spawn {
            req: req3,
            id: id3,
            argv: argv3,
            ..
        } = ToHolder::decode(pkt3.frame()).unwrap()
        else {
            panic!("expected third Spawn");
        };
        let args3: Vec<_> = argv3
            .iter()
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        assert_eq!(
            args3.last().map(String::as_str),
            Some("--resume"),
            "resume before hook must use bare --resume, got {args3:?}"
        );
        assert!(
            !args3.iter().any(|a| a.starts_with("--resume=")),
            "resume before hook must not pass uninjected UUID: {args3:?}"
        );
        let (master3, _pty3) = UnixStream::pair().unwrap();
        holder_link
            .send(
                &FromHolder::Spawned {
                    req: req3,
                    id: id3,
                    pid: 802,
                }
                .encode()
                .unwrap(),
                &[master3.as_fd()],
            )
            .unwrap();
        (id, id2, id3)
    });

    let full = Peer {
        pid: None,
        scope: Scope::Full,
    };
    let reply = reg.handle(
        &full,
        Request::Create(Create {
            account: None,
            program: Program::Agent {
                id: "fork:7".into(),
            },
            cwd: Vec::new(),
            cols: 80,
            rows: 24,
        }),
        &mut HashMap::new(),
    );
    let spawned_id = match reply {
        Reply::Created { session, pid: 800 } => session,
        other => panic!("unexpected reply: {other:?}"),
    };
    // OWE-752: 포크된 세션의 settings 파일에서 미주입 UUID가 폴백으로 읽히지 않아야 한다.
    assert_eq!(crate::agent::claude::session_id(&dir, spawned_id), None);
    let refork_reply = reg.handle(
        &full,
        Request::Create(Create {
            account: None,
            program: Program::Agent {
                id: format!("fork:{}", spawned_id.0),
            },
            cwd: Vec::new(),
            cols: 80,
            rows: 24,
        }),
        &mut HashMap::new(),
    );
    let refork_id = match refork_reply {
        Reply::Created { session, pid: 801 } => session,
        other => panic!("unexpected refork reply: {other:?}"),
    };

    // 포크된 세션(refork_id)이 hook 수신 전에 콜드 상태가 된 뒤 Resume해도 미주입 UUID가 넘어가지 않는다.
    lock(&reg.state).sessions.remove(&refork_id);
    let resume_reply = reg.handle(
        &full,
        Request::Resume {
            session: refork_id,
            create: Create {
                account: None,
                program: Program::Login,
                cwd: Vec::new(),
                cols: 80,
                rows: 24,
            },
        },
        &mut HashMap::new(),
    );
    assert!(matches!(resume_reply, Reply::Resumed { pid: 802, .. }));

    let (joined_id, _, _) = holder_worker.join().unwrap();
    reader_worker.join().unwrap();
    assert_eq!(joined_id, spawned_id);
    // 원 세션(7)과 포크된 세션(spawned_id) 모두 살아 있어야 한다(세션 불멸).
    let st = lock(&reg.state);
    assert!(st.sessions.contains_key(&SessionId(7)));
    assert!(st.sessions.contains_key(&spawned_id));
    drop(st);
    let stored_fork = reg.agents.stored(spawned_id).unwrap().unwrap();
    assert_eq!(stored_fork.profile, "claude");
    assert_eq!(stored_fork.permission, Permission::Yolo);
    assert_eq!(stored_fork.session_id, None);
    assert_eq!(stored_fork.cwd, b"/tmp/owe736-cwd");

    fs::remove_dir_all(&dir).ok();
}

/// OWE-755: 여러 연결 스레드·spool 수거 스레드가 동시에 `maybe_rename_from_hook`을 호출해도
/// `session-titles.json` 임시 파일 충돌이나 RMW 유실 없이 모든 세션 제목이 기록된다.
#[test]
fn owe755_concurrent_hook_auto_rename_preserves_all_session_titles() {
    use std::sync::Barrier;

    let dir = std::env::temp_dir().join(format!("owe755-reg-hook-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();

    let reg = Arc::new(Registry::new(None, Some(&dir)));
    let n = 10_u64;
    let barrier = Arc::new(Barrier::new(usize::try_from(n).unwrap()));
    let handles: Vec<_> = (1..=n)
        .map(|i| {
            let r = Arc::clone(&reg);
            let b = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let payload = format!(
                    r#"{{"hook_event_name":"UserPromptSubmit","prompt":"Fix hook bug {i}"}}"#
                );
                b.wait();
                r.maybe_rename_from_hook(SessionId(i), payload.as_bytes());
                // 같은 세션에 두 번째 프롬프트 훅이 와도 첫 제목이 유지되어야 한다.
                let second =
                    r#"{"hook_event_name":"UserPromptSubmit","prompt":"Second prompt ignored"}"#;
                r.maybe_rename_from_hook(SessionId(i), second.as_bytes());
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    let titles = krill_proto::git::load_session_auto_titles(&dir);
    assert_eq!(titles.len(), usize::try_from(n).unwrap());
    for i in 1..=n {
        let expected = format!("fix-hook-bug-{i}");
        assert_eq!(
            titles.get(&SessionId(i)).map(String::as_str),
            Some(expected.as_str())
        );
    }

    drop(reg);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn orchestration_ipc_wakes_check_messages_and_enforces_worker_depth() {
    use krill_proto::orchestration::{MessageType, OrchAction, OrchResponse, SendParams};

    let dir = std::env::temp_dir().join(format!("owe922-reg-orch-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();

    let reg = Arc::new(Registry::new(None, Some(&dir)));
    let peer = Peer {
        pid: None,
        scope: Scope::Full,
    };

    let Reply::Orchestration(OrchResponse::Run(run)) = reg.handle(
        &peer,
        Request::Orchestration(OrchAction::CreateRun {
            title: "ipc-run".into(),
            coordinator_session_id: Some(1),
        }),
        &mut HashMap::new(),
    ) else {
        panic!("expected Run response");
    };

    // StartWorker with depth 1 <= max_depth 2 succeeds and persists depth = 1
    let Reply::Orchestration(OrchResponse::Dispatch(disp)) = reg.handle(
        &peer,
        Request::Orchestration(OrchAction::StartWorker {
            run_id: run.id.clone(),
            task_id: None,
            worker_name: "w1".into(),
            agent: "claude".into(),
            session_id: Some(42),
            cwd: "/tmp".into(),
            prompt: "work".into(),
            depth: 1,
            max_depth: 2,
        }),
        &mut HashMap::new(),
    ) else {
        panic!("expected Dispatch response");
    };
    assert_eq!(disp.depth, 1);

    // StartWorker with depth 3 > max_depth 2 fails
    let over_depth = reg.handle(
        &peer,
        Request::Orchestration(OrchAction::StartWorker {
            run_id: run.id.clone(),
            task_id: None,
            worker_name: "w2".into(),
            agent: "claude".into(),
            session_id: Some(43),
            cwd: "/tmp".into(),
            prompt: "work".into(),
            depth: 3,
            max_depth: 2,
        }),
        &mut HashMap::new(),
    );
    assert!(matches!(
        over_depth,
        Reply::Orchestration(OrchResponse::Error(_))
    ));

    // Blocking CheckMessages with 5s timeout wakes up immediately when SendMessage fires
    let reg_waiter = Arc::clone(&reg);
    let waiter = std::thread::spawn(move || {
        let start = Instant::now();
        let reply = reg_waiter.handle(
            &Peer {
                pid: None,
                scope: Scope::Full,
            },
            Request::Orchestration(OrchAction::CheckMessages {
                recipient: Some("coordinator".into()),
                types: vec![MessageType::WorkerDone],
                unread_only: true,
                ack: true,
                wait_ms: 5000,
                run_id: None,
                delivery_id: None,
            }),
            &mut HashMap::new(),
        );
        (start.elapsed(), reply)
    });

    std::thread::sleep(Duration::from_millis(60));
    let send_reply = reg.handle(
        &peer,
        Request::Orchestration(OrchAction::SendMessage(SendParams {
            run_id: Some(run.id),
            task_id: None,
            dispatch_id: Some(disp.id),
            from: "w1".into(),
            to: "coordinator".into(),
            msg_type: MessageType::WorkerDone,
            summary: "done".into(),
            body: "completed".into(),
            outcome: Some("succeeded".into()),
            reply_to: None,
        })),
        &mut HashMap::new(),
    );
    assert!(matches!(
        send_reply,
        Reply::Orchestration(OrchResponse::Messages(_))
    ));

    let (elapsed, wait_reply) = waiter.join().unwrap();
    assert!(
        elapsed < Duration::from_secs(2),
        "condvar wakeup took too long: {elapsed:?}"
    );
    let Reply::Orchestration(OrchResponse::Messages(msgs)) = wait_reply else {
        panic!("expected Messages response");
    };
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].summary, "done");

    drop(reg);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn states_resolve_launched_pi_without_hooks_and_preserve_hook_identity() {
    let dir = std::env::temp_dir().join(format!("krill-pi-identity-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let reg = Registry::new(None, Some(&dir));
    let id = SessionId(1);
    let (master, _output) = UnixStream::pair().unwrap();
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
            pid: 0,
            cwd: dir.clone(),
        },
    );
    reg.agents
        .remember(
            id,
            &control::AgentLaunch {
                profile: "pi".into(),
                permission: Permission::Manual,
            },
        )
        .unwrap();
    let full = Peer {
        pid: None,
        scope: Scope::Full,
    };
    let Reply::States(states) = reg.states(&full) else {
        panic!("states reply")
    };
    let status = &states.first().unwrap().status;
    assert_eq!(
        status.agent, "pi",
        "a Pi launch must have runtime identity without a title or hook"
    );
    assert_eq!(status.state, control::AgentState::Unknown);
    assert_eq!(
        status.source, None,
        "identity must not invent evidence of working/waiting state"
    );
    let restricted = Peer {
        pid: None,
        scope: Scope::Restricted,
    };
    assert_eq!(reg.states(&restricted), Reply::States(Vec::new()));
    let hook = HookEvent {
        id: krill_proto::hook::EventId {
            pid: 123,
            unix_ns: 1,
        },
        agent: "claude".into(),
        ppid: 1,
        input: krill_proto::hook::Input::Complete,
        payload: br#"{"hook_event_name":"SessionStart","source":"startup"}"#.to_vec(),
    };
    reg.status.hook(&[(id, &hook)]).unwrap();
    let Reply::States(states) = reg.states(&full) else {
        panic!("states reply")
    };
    assert_eq!(
        states.first().unwrap().status.agent,
        "claude",
        "hook identity takes precedence over a launch hint"
    );

    let agy_id = SessionId(2);
    let (agy_master, _agy_output) = UnixStream::pair().unwrap();
    let agy_session = Session::start(
        agy_id,
        OwnedFd::from(agy_master),
        WinSize { cols: 80, rows: 24 },
        Arc::clone(&reg.look),
        None,
    )
    .unwrap();
    lock(&reg.state).sessions.insert(
        agy_id,
        Entry {
            session: agy_session,
            pid: 0,
            cwd: dir.clone(),
        },
    );
    reg.agents
        .remember_session(
            agy_id,
            &control::AgentLaunch {
                profile: "agy".into(),
                permission: Permission::Manual,
            },
            Some("c93bbb42-1866-4949-bc9a-493495f6cc94".into()),
            Vec::new(),
            None,
        )
        .unwrap();
    let Reply::States(states) = reg.states(&full) else {
        panic!("states reply")
    };
    let agy_status = &states.iter().find(|s| s.session == agy_id).unwrap().status;
    assert_eq!(agy_status.agent, "agy");
    assert_eq!(agy_status.state, control::AgentState::Done);

    drop(reg);
    fs::remove_dir_all(dir).unwrap();
}

/// A hook sent before the holder's Spawned reply must wait for root/session registration,
/// both on the socket and when the CLI has already fallen back to its spool.
#[test]
fn first_hook_waits_for_spawn_registration_on_socket_and_spool() {
    use crate::holder::link::Link;
    use crate::os::AsFd as _;
    use krill_proto::holder::{FromHolder, ToHolder, VERSION};
    use krill_proto::hook::{EventId, Input};
    use std::sync::mpsc;
    use std::time::Duration;

    for spooled in [false, true] {
        let dir =
            std::env::temp_dir().join(format!("krill-first-hook-{}-{spooled}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
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
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let reg = Arc::new(Registry::new(Some((holder, vec![])), Some(&dir)));
        let reader_reg = Arc::clone(&reg);
        let reader = std::thread::spawn(move || reader_reg.run_holder(link));
        let id = SessionId(1);
        let spawn_reg = Arc::clone(&reg);
        let spawning = std::thread::spawn(move || {
            spawn_reg
                .spawn_holder(
                    &Peer {
                        pid: None,
                        scope: Scope::Full,
                    },
                    spawn_reg.holder.as_ref().unwrap(),
                    id,
                    vec![b"/bin/sh".to_vec()],
                    vec![],
                    b"/tmp".to_vec(),
                    80,
                    24,
                )
                .unwrap()
        });
        let packet = holder_link.recv().unwrap().unwrap();
        let ToHolder::Spawn { req, .. } = ToHolder::decode(packet.frame()).unwrap() else {
            panic!("expected Spawn");
        };
        // The fake holder deliberately withholds Spawned, leaving the child unregistered.
        let event = HookEvent {
            id: EventId {
                pid: std::process::id(),
                unix_ns: 1,
            },
            agent: "claude".into(),
            ppid: std::process::id(),
            input: Input::Complete,
            payload: br#"{"hook_event_name":"UserPromptSubmit"}"#.to_vec(),
        };
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let hook_reg = Arc::clone(&reg);
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        if spooled {
            hook::append_spool(
                &dir.join(hook::SPOOL_FILE),
                &event.encode().unwrap(),
                Instant::now() + Duration::from_secs(2),
            )
            .unwrap();
        } else {
            let hello = krill_proto::daemon::client_hello(ipc::ClientRole::App, "test");
            client.write_all(&hello.encode().unwrap()).unwrap();
            client.write_all(&event.encode().unwrap()).unwrap();
        }
        let applying = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            if spooled {
                hook_reg.drain_spool();
            } else {
                let hello = ServerHello {
                    proto_min: ipc::PROTO_MIN,
                    proto_max: ipc::PROTO_MAX,
                    features: ipc::FEATURES,
                    build: "test".into(),
                    instance_id: 1,
                };
                hook_reg.serve(server, &hello).unwrap();
            }
            done_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(
            matches!(
                done_rx.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "unregistered hook was consumed"
        );
        if spooled {
            // A blocked drain must leave the spool unlocked so another CLI can append.
            let mut next = event.clone();
            next.id.unix_ns = 2;
            hook::append_spool(
                &dir.join(hook::SPOOL_FILE),
                &next.encode().unwrap(),
                Instant::now() + Duration::from_millis(50),
            )
            .unwrap();
        }
        let (master, _pty) = UnixStream::pair().unwrap();
        holder_link
            .send(
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
        assert_eq!(spawning.join().unwrap(), (id, std::process::id()));
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        applying.join().unwrap();
        assert_eq!(reg.status.get(id).state, control::AgentState::Working);
        if !spooled {
            let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
            ServerHello::decode(kind, &body).unwrap();
            let (kind, _) = ipc::read_frame(&mut client).unwrap().unwrap();
            assert_eq!(kind, hook::kind::ACK);
        }
        drop(holder_link);
        reader.join().unwrap();
        drop(reg);
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn late_hook_cannot_resurrect_a_deleted_session_or_launch_metadata() {
    let dir = std::env::temp_dir().join(format!("hook-delete-commit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let reg = Registry::new(None, Some(&dir));
    let id = SessionId(991);
    let (master, user) = UnixStream::pair().unwrap();
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
            pid: 0,
            cwd: dir.clone(),
        },
    );
    reg.agents
        .remember(
            id,
            &control::AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Manual,
            },
        )
        .unwrap();
    reg.remove(id);
    let event = krill_proto::hook::HookEvent {
        id: krill_proto::hook::EventId {
            pid: 1,
            unix_ns: 10,
        },
        agent: "codex".into(),
        ppid: 0,
        input: krill_proto::hook::Input::Complete,
        payload: br#"{"hook_event_name":"UserPromptSubmit","session_id":"late-conversation"}"#
            .to_vec(),
    };
    let peer = Peer {
        pid: None,
        scope: Scope::Session(id),
    };
    assert_eq!(reg.on_hook(&peer, &event).unwrap(), None);
    assert_eq!(reg.status.get(id), control::AgentStatus::UNKNOWN);
    assert!(!reg.agents.has(id));
    drop(user);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unnegotiated_remote_fs_requests_are_rejected_without_filesystem_access() {
    use krill_proto::remote_fs::RemoteFsRequest;
    assert_ne!(ipc::FEATURES & control::FEAT_REMOTE_FS, 0);
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let thread = std::thread::spawn(move || {
        let hello = ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features: ipc::FEATURES,
            build: "test".into(),
            instance_id: 1,
        };
        Registry::new(None, None).serve(server, &hello).unwrap();
    });
    let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
    assert_ne!(
        ServerHello::decode(kind, &body).unwrap().features & control::FEAT_REMOTE_FS,
        0
    );
    let mut hello = krill_proto::daemon::client_hello(ipc::ClientRole::App, "test");
    hello.features = FEAT_CONTROL;
    client.write_all(&hello.encode().unwrap()).unwrap();
    for request in [
        RemoteFsRequest::Stat {
            path: b"/reserved".to_vec(),
        },
        RemoteFsRequest::WriteAtomic {
            path: b"/reserved".to_vec(),
            bytes: b"blocked".to_vec(),
            expected: None,
            stage_id: 0,
            offset: 0,
            commit: true,
        },
        RemoteFsRequest::PortForward { port: 8080 },
    ] {
        client
            .write_all(&Request::RemoteFs(request).encode().unwrap())
            .unwrap();
        let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        assert_eq!(
            Reply::decode(kind, &body).unwrap(),
            Reply::Failed(Failure::BadRequest)
        );
    }
    drop(client);
    thread.join().unwrap();
}

#[test]
fn negotiated_remote_fs_enforces_scope_and_observer_permissions() {
    use krill_proto::os::ffi::OsStrExt as _;
    use krill_proto::remote_fs::{RemoteFsReply, RemoteFsRequest, WireWriteOutcome};
    let dir = std::env::temp_dir().join(format!("registry-remote-fs-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("document.txt");
    fs::write(&path, b"original").unwrap();
    let raw = path.as_os_str().as_bytes().to_vec();
    let stat = RemoteFsRequest::Stat { path: raw.clone() };
    let write = RemoteFsRequest::WriteAtomic {
        path: raw.clone(),
        bytes: b"updated".to_vec(),
        expected: None,
        stage_id: 0,
        offset: 0,
        commit: true,
    };
    let reg = Registry::new(None, None);
    for scope in [Scope::Restricted, Scope::Session(SessionId(7))] {
        for request in [stat.clone(), write.clone()] {
            assert_eq!(
                reg.handle(
                    &Peer { pid: None, scope },
                    Request::RemoteFs(request),
                    &mut HashMap::new()
                ),
                Reply::Failed(Failure::Denied)
            );
        }
    }
    for observer in [true, false] {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let thread = std::thread::spawn(move || {
            let hello = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            };
            Registry::new(None, None)
                .serve_mobile(server, &hello)
                .unwrap();
        });
        let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        ServerHello::decode(kind, &body).unwrap();
        let mut hello = krill_proto::daemon::client_hello(ipc::ClientRole::App, "test");
        hello.features = FEAT_CONTROL | control::FEAT_REMOTE_FS | FEAT_ROLES;
        client.write_all(&hello.encode().unwrap()).unwrap();
        if observer {
            client
                .write_all(&roles::Request::Set(Role::Observer).encode().unwrap())
                .unwrap();
            let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
            assert_eq!(Ack::decode(kind, &body).unwrap(), Ack::Applied);
        }
        client
            .write_all(&Request::RemoteFs(stat.clone()).encode().unwrap())
            .unwrap();
        let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        assert!(
            matches!(Reply::decode(kind, &body).unwrap(), Reply::RemoteFs(RemoteFsReply::Stat(Ok(s))) if s.is_file)
        );
        client
            .write_all(&Request::RemoteFs(write.clone()).encode().unwrap())
            .unwrap();
        let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        let reply = Reply::decode(kind, &body).unwrap();
        if observer {
            assert_eq!(reply, Reply::Failed(Failure::Denied));
            assert_eq!(fs::read(&path).unwrap(), b"original");
            client
                .write_all(
                    &Request::RemoteFs(RemoteFsRequest::PortForward { port: 8080 })
                        .encode()
                        .unwrap(),
                )
                .unwrap();
            let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
            assert_eq!(
                Reply::decode(kind, &body).unwrap(),
                Reply::Failed(Failure::Denied)
            );
        } else {
            assert!(matches!(
                reply,
                Reply::RemoteFs(RemoteFsReply::Written(WireWriteOutcome::Saved(_)))
            ));
            assert_eq!(fs::read(&path).unwrap(), b"updated");
        }
        drop(client);
        thread.join().unwrap();
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn explicit_close_marks_session_closed_and_blocks_cold_or_resume_resurrection() {
    use crate::holder::link::Link;
    use krill_proto::holder::{FromHolder, ToHolder, VERSION};
    use krill_proto::os::net::UnixStream;
    use std::io::Write as _;

    let dir = std::env::temp_dir().join(format!("close-blocks-resume-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();

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
    let (holder, link, kept, _) =
        crate::holder::Holder::connect(OwnedFd::from(krilld_end)).unwrap();

    let reg = Arc::new(Registry::new(Some((holder, kept)), Some(&dir)));
    let reg_for_reader = Arc::clone(&reg);
    let reader_worker = std::thread::spawn(move || {
        reg_for_reader.holder.as_ref().unwrap().run(link, |_, _| {});
    });
    let holder_worker = std::thread::spawn(move || {
        let pkt = holder_link.recv().unwrap().unwrap();
        match ToHolder::decode(pkt.frame()).unwrap() {
            ToHolder::Release { req, .. } | ToHolder::Terminate { req, .. } => {
                holder_link
                    .send(&FromHolder::Done { req }.encode().unwrap(), &[])
                    .unwrap();
            }
            other => panic!("unexpected holder request across Close/Resume: {other:?}"),
        }
    });

    let id = SessionId(42);
    let (master, mut pty_child) = UnixStream::pair().unwrap();
    let session = reg
        .start(id, OwnedFd::from(master), WinSize { cols: 80, rows: 24 })
        .unwrap();
    pty_child.write_all(b"closed session output\r\n").unwrap();
    drop(pty_child);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !session.is_closed() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    {
        let mut st = lock(&reg.state);
        st.sessions.insert(
            id,
            Entry {
                session,
                pid: 1234,
                cwd: dir.clone(),
            },
        );
        st.exited.insert(id, 0);
    }

    reg.agents
        .remember(
            id,
            &control::AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Manual,
            },
        )
        .unwrap();
    reg.on_exited(id, 0);
    assert!(
        reg.agents.has(id),
        "natural exit must preserve resume metadata"
    );
    let full = Peer {
        pid: None,
        scope: Scope::Full,
    };
    assert_eq!(
        reg.handle(&full, Request::Close { session: id }, &mut HashMap::new()),
        Reply::Closed
    );
    assert!(dir.join("sessions/42.noindex/closed").is_file());
    assert!(
        !reg.agents.has(id),
        "explicit close must forget launch metadata"
    );
    assert_eq!(
        reg.handle(&full, Request::Cold { session: id }, &mut HashMap::new()),
        Reply::Failed(Failure::NoSession)
    );
    assert_eq!(
        reg.handle(
            &full,
            Request::Resume {
                session: id,
                create: Create {
                    account: None,
                    program: Program::Login,
                    cwd: Vec::new(),
                    cols: 80,
                    rows: 24,
                },
            },
            &mut HashMap::new()
        ),
        Reply::Failed(Failure::NoSession)
    );

    drop(reg);
    holder_worker.join().unwrap();
    reader_worker.join().unwrap();

    // 데몬 재기동 뒤에도 디스크의 closed 표식이 남아 Cold/Resume 부활을 막는다.
    let reloaded = Registry::new(None, Some(&dir));
    assert_eq!(
        reloaded.handle(&full, Request::Cold { session: id }, &mut HashMap::new()),
        Reply::Failed(Failure::NoSession)
    );
    assert_eq!(
        reloaded.handle(
            &full,
            Request::Resume {
                session: id,
                create: Create {
                    account: None,
                    program: Program::Login,
                    cwd: Vec::new(),
                    cols: 80,
                    rows: 24,
                },
            },
            &mut HashMap::new()
        ),
        Reply::Failed(Failure::NoSession)
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn spawn_strips_workspace_suffix_and_falls_back_when_cwd_is_missing() {
    use crate::holder::link::Link;
    use krill_proto::holder::{FromHolder, ToHolder, VERSION};
    use krill_proto::os::net::UnixStream;
    use std::os::fd::AsFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    // The fake holder has no OS process. Low fake PIDs can name real CI processes,
    // whose live cwd would override the initial fallback being tested here.
    const FIRST_FAKE_PID: u32 = u32::MAX - 1;
    const SECOND_FAKE_PID: u32 = u32::MAX;

    let dir = std::env::temp_dir().join(format!("mk-spawn-fallback-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let parent_dir = dir.join("workspace-parent");
    std::fs::create_dir_all(&parent_dir).unwrap();
    let missing_child = parent_dir.join("deleted-worktree");
    let polluted_cwd = format!(
        "{}::workspace:2de7222e-4069-4b91-bb07-3e05a9808a9f",
        parent_dir.display()
    );

    let (daemon_end, holder_end) = UnixStream::pair().unwrap();
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
    let (holder, link_for_reader, kept, _) =
        crate::holder::Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let reg = std::sync::Arc::new(Registry::new(Some((holder, kept)), Some(&dir)));
    let reg_reader = std::sync::Arc::clone(&reg);
    let reader_worker = std::thread::spawn(move || {
        reg_reader
            .holder
            .as_ref()
            .unwrap()
            .run(link_for_reader, |_, _| {});
    });

    let expected_parent = parent_dir.as_os_str().as_bytes().to_vec();
    let expected_missing = missing_child.as_os_str().as_bytes().to_vec();
    let holder_worker = std::thread::spawn(move || {
        // 1) ::workspace:<uuid> 오염 경로는 첫 Spawn 전에 정규 폴더로 벗겨진다.
        let pkt1 = holder_link.recv().unwrap().unwrap();
        let ToHolder::Spawn {
            req: req1,
            id: id1,
            cwd: cwd1,
            ..
        } = ToHolder::decode(pkt1.frame()).unwrap()
        else {
            panic!("expected first Spawn");
        };
        assert_eq!(cwd1, expected_parent);
        let (m1, _p1) = UnixStream::pair().unwrap();
        holder_link
            .send(
                &FromHolder::Spawned {
                    req: req1,
                    id: id1,
                    pid: FIRST_FAKE_PID,
                }
                .encode()
                .unwrap(),
                &[m1.as_fd()],
            )
            .unwrap();

        // 2) 삭제된 하위 폴더가 ENOENT를 내면 존재하는 상위 폴더(parent_dir)로 즉시 재시도한다.
        let pkt2 = holder_link.recv().unwrap().unwrap();
        let ToHolder::Spawn {
            req: req2,
            cwd: cwd2,
            ..
        } = ToHolder::decode(pkt2.frame()).unwrap()
        else {
            panic!("expected second Spawn");
        };
        assert_eq!(cwd2, expected_missing);
        holder_link
            .send(
                &FromHolder::Failed {
                    req: req2,
                    errno: libc::ENOENT,
                }
                .encode()
                .unwrap(),
                &[],
            )
            .unwrap();

        let pkt3 = holder_link.recv().unwrap().unwrap();
        let ToHolder::Spawn {
            req: req3,
            id: id3,
            cwd: cwd3,
            ..
        } = ToHolder::decode(pkt3.frame()).unwrap()
        else {
            panic!("expected retry Spawn");
        };
        assert_eq!(cwd3, expected_parent);
        let (m3, _p3) = UnixStream::pair().unwrap();
        holder_link
            .send(
                &FromHolder::Spawned {
                    req: req3,
                    id: id3,
                    pid: SECOND_FAKE_PID,
                }
                .encode()
                .unwrap(),
                &[m3.as_fd()],
            )
            .unwrap();
    });

    let full = Peer {
        pid: None,
        scope: Scope::Full,
    };
    let r1 = reg.handle(
        &full,
        Request::Create(Create {
            account: None,
            program: Program::Login,
            cwd: polluted_cwd.into_bytes(),
            cols: 80,
            rows: 24,
        }),
        &mut HashMap::new(),
    );
    assert!(
        matches!(
            r1,
            Reply::Created {
                pid: FIRST_FAKE_PID,
                ..
            }
        ),
        "{r1:?}"
    );

    let r2 = reg.handle(
        &full,
        Request::Create(Create {
            account: None,
            program: Program::Login,
            cwd: missing_child.as_os_str().as_bytes().to_vec(),
            cols: 80,
            rows: 24,
        }),
        &mut HashMap::new(),
    );
    let Reply::Created {
        session: s2,
        pid: SECOND_FAKE_PID,
    } = r2
    else {
        panic!("expected Created on fallback retry, got {r2:?}");
    };
    assert_eq!(
        reg.session_cwd(s2)
            .and_then(|p| std::fs::canonicalize(p).ok()),
        std::fs::canonicalize(&parent_dir).ok()
    );

    drop(reg);
    holder_worker.join().unwrap();
    reader_worker.join().unwrap();
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn close_succeeds_when_exited_races_with_failed_terminate_and_preserves_inflight_closing() {
    use krill_holder::link::Link;
    use krill_proto::holder::{FromHolder, VERSION};
    use krill_proto::os::net::UnixStream;

    let (daemon_end, holder_end) = UnixStream::pair().unwrap();
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
    let (holder, link_for_reader, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let (m1, _p1) = UnixStream::pair().unwrap();
    let (m2, _p2) = UnixStream::pair().unwrap();
    let s1 = SessionId(11);
    let s2 = SessionId(12);
    let reg = Arc::new(Registry::new(
        Some((
            holder,
            vec![
                Kept {
                    id: s1,
                    pid: 101,
                    status: None,
                    master: OwnedFd::from(m1),
                },
                Kept {
                    id: s2,
                    pid: 102,
                    status: None,
                    master: OwnedFd::from(m2),
                },
            ],
        )),
        None,
    ));
    let reg_reader = Arc::clone(&reg);
    let reader_worker = std::thread::spawn(move || {
        reg_reader
            .holder
            .as_ref()
            .unwrap()
            .run(link_for_reader, |id, status| {
                reg_reader.on_exited(id, status);
            });
    });

    let full = Peer {
        pid: None,
        scope: Scope::Full,
    };

    // 1) Terminate 왕복 도중 세션이 이미 종료되어 holder가 오류(예: Windows conhost 종료 뒤 errno=5)를 돌려줘도,
    //    그사이 exited가 기록됐으면 Release·정리 후 Closed로 끝난다.
    let reg_exited = Arc::clone(&reg);
    let holder_worker = std::thread::spawn(move || {
        let pkt1 = holder_link.recv().unwrap().unwrap();
        let ToHolder::Terminate { req: req1, id: id1 } = ToHolder::decode(pkt1.frame()).unwrap()
        else {
            panic!("expected first Terminate");
        };
        assert_eq!(id1, s1);
        reg_exited.on_exited(s1, 0);
        // on_exited가 notify로 보낸 Release를 읽는다.
        let rel1 = holder_link.recv().unwrap().unwrap();
        let ToHolder::Release {
            req: rel_req1,
            id: rel_id1,
        } = ToHolder::decode(rel1.frame()).unwrap()
        else {
            panic!("expected Release from on_exited");
        };
        assert_eq!(rel_id1, s1);
        holder_link
            .send(&FromHolder::Done { req: rel_req1 }.encode().unwrap(), &[])
            .unwrap();
        holder_link
            .send(
                &FromHolder::Failed {
                    req: req1,
                    errno: 5,
                }
                .encode()
                .unwrap(),
                &[],
            )
            .unwrap();

        // 2) 이미 closing 중인 세션(s2)에 중복 Close가 들어와 실패하더라도 기존 closing 표시는 지우지 않는다.
        let pkt2 = holder_link.recv().unwrap().unwrap();
        let ToHolder::Terminate { req: req2, id: id2 } = ToHolder::decode(pkt2.frame()).unwrap()
        else {
            panic!("expected second Terminate");
        };
        assert_eq!(id2, s2);
        holder_link
            .send(
                &FromHolder::Failed {
                    req: req2,
                    errno: 5,
                }
                .encode()
                .unwrap(),
                &[],
            )
            .unwrap();
        // 뒤이어 s2가 실제로 종료되면 on_exited가 Release를 보낸다.
        let rel3 = holder_link.recv().unwrap().unwrap();
        let ToHolder::Release {
            req: rel_req3,
            id: rel_id3,
        } = ToHolder::decode(rel3.frame()).unwrap()
        else {
            panic!("expected Release for s2");
        };
        assert_eq!(rel_id3, s2);
        holder_link
            .send(&FromHolder::Done { req: rel_req3 }.encode().unwrap(), &[])
            .unwrap();
    });

    assert_eq!(
        reg.handle(&full, Request::Close { session: s1 }, &mut HashMap::new()),
        Reply::Closed
    );
    assert!(!lock(&reg.state).sessions.contains_key(&s1));

    lock(&reg.state).closing.insert(s2);
    assert_eq!(
        reg.handle(&full, Request::Close { session: s2 }, &mut HashMap::new()),
        Reply::Failed(Failure::Os(5))
    );
    assert!(
        lock(&reg.state).closing.contains(&s2),
        "duplicate failed close must not clear existing closing mark"
    );
    reg.on_exited(s2, 0);
    assert!(!lock(&reg.state).sessions.contains_key(&s2));

    drop(reg);
    holder_worker.join().unwrap();
    reader_worker.join().unwrap();
}

#[test]
fn maybe_rename_from_hook_records_every_prompt_turn_while_preserving_first_title() {
    let dir = std::env::temp_dir().join(format!("hook-prompts-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let reg = Registry::new(None, Some(&dir));
    let id = SessionId(77);

    reg.maybe_rename_from_hook(
        id,
        br#"{"hook_event_name":"UserPromptSubmit","prompt":"Fix auth token expiry bug"}"#,
    );
    reg.maybe_rename_from_hook(
        id,
        br#"{"hook_event_name":"UserPromptSubmit","prompt":"Add regression test for expired cookie"}"#,
    );
    reg.maybe_rename_from_hook(
        id,
        br#"{"hook_event_name":"UserPromptSubmit","prompt":"ok proceed"}"#,
    );

    let titles = krill_proto::git::load_session_auto_titles(&dir);
    assert_eq!(
        titles.get(&id).map(String::as_str),
        Some("fix-auth-token-expiry-bug"),
        "first prompt auto-title must remain intact across subsequent turns"
    );

    let histories = krill_proto::git::load_session_prompt_histories(&dir);
    let hist = histories.get(&id).expect("prompt history must be recorded");
    assert_eq!(hist.total_turns, 3);
    assert_eq!(hist.turns.len(), 3);
    assert_eq!(
        hist.display_last_prompt().as_deref(),
        Some("Add regression test for expired cookie (→ ok proceed)")
    );
    assert_eq!(
        hist.previous_turn().map(|t| (t.turn, t.text.as_str())),
        Some((1, "Fix auth token expiry bug"))
    );

    fs::remove_dir_all(&dir).ok();
}

/// foundation-02: 1MiB 프레임을 넘는 스냅샷(활성 task 12개 × 100KB)은 EOF가 아니라 명시 오류로 답하고
/// 연결은 살아 있으며, 페이지 스냅샷은 같은 연결에서 디스크와 같은 저장소를 무손실로 돌려준다. 기능을
/// 협상하지 않은(N-1) 피어의 페이지 요청은 `BadRequest`다.
#[test]
fn oversized_orchestration_snapshot_answers_explicitly_and_pages_losslessly() {
    use krill_proto::orchestration::{
        FEAT_ORCH_SNAPSHOT_PAGE, FRAME_LIMIT_ERROR, OrchAction, OrchResponse, OrchestrationStore,
    };
    let dir = std::env::temp_dir().join(format!("krill-orch-paged-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let mut store = OrchestrationStore::default();
    let run = store
        .create_run("verify_core independent synthetic snapshot", None)
        .unwrap();
    for i in 0..12_u8 {
        let description = String::from_utf8(vec![b'A' + i; 100_000]).unwrap();
        store
            .create_task(&run.id, &format!("fixture-{i}"), &description, vec![])
            .unwrap();
    }
    store.save(&dir).unwrap();
    assert!(fs::metadata(dir.join("orchestration.json")).unwrap().len() > 1 << 20);
    let serve = |features: u64| {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let dir = dir.clone();
        let thread = std::thread::spawn(move || {
            let hello = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            };
            Registry::new(None, Some(&dir))
                .serve(server, &hello)
                .unwrap();
        });
        let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        ServerHello::decode(kind, &body).unwrap();
        let mut hello = krill_proto::daemon::client_hello(ipc::ClientRole::Cli, "test");
        hello.features = features;
        client.write_all(&hello.encode().unwrap()).unwrap();
        (client, thread)
    };
    let request = |client: &mut UnixStream, action: OrchAction| -> Reply {
        client
            .write_all(&Request::Orchestration(action).encode().unwrap())
            .unwrap();
        let (kind, body) = ipc::read_frame(client)
            .unwrap()
            .expect("daemon closed the stream instead of replying");
        Reply::decode(kind, &body).unwrap()
    };

    let (mut client, thread) = serve(ipc::FEATURES);
    // 구 CLI 경로: 단일 프레임 SnapshotV2는 EOF 대신 명시 오류이고 연결은 살아 있다.
    match request(&mut client, OrchAction::SnapshotV2) {
        Reply::Orchestration(OrchResponse::Error(msg)) => {
            assert!(msg.contains(FRAME_LIMIT_ERROR), "{msg}");
        }
        other => panic!("{other:?}"),
    }
    // 새 CLI 경로: 같은 연결에서 페이지로 읽어 디스크와 같다.
    let fetched = OrchestrationStore::fetch_paged(|generation, offset| {
        match request(&mut client, OrchAction::SnapshotPage { generation, offset }) {
            Reply::Orchestration(response) => Ok(response),
            other => Err(io::Error::other(format!("{other:?}"))),
        }
    })
    .unwrap();
    assert_eq!(fetched, OrchestrationStore::load(&dir).unwrap());
    assert_eq!(fetched.tasks.len(), 12);
    assert!(fetched.tasks.iter().all(|t| t.description.len() == 100_000));
    let check = OrchAction::CheckMessages {
        recipient: Some("nobody".into()),
        types: vec![],
        unread_only: true,
        ack: false,
        wait_ms: 0,
        run_id: None,
        delivery_id: None,
    };
    assert!(matches!(
        request(&mut client, check),
        Reply::Orchestration(OrchResponse::Delivery { delivery: None, messages }) if messages.is_empty()
    ));
    drop(client);
    thread.join().unwrap();

    let (mut old, thread) = serve(ipc::FEATURES & !FEAT_ORCH_SNAPSHOT_PAGE);
    let page = OrchAction::SnapshotPage {
        generation: None,
        offset: 0,
    };
    assert_eq!(request(&mut old, page), Reply::Failed(Failure::BadRequest));
    drop(old);
    thread.join().unwrap();
    fs::remove_dir_all(dir).ok();
}

#[cfg(windows)]
#[test]
fn spawn_cwd_uses_shell_compatible_paths_without_requiring_them_to_exist() {
    for (input, expected) in [
        (r"\\?\C:\Users\한글 QA\project", r"C:\Users\한글 QA\project"),
        (r"\\?\UNC\server\share\project", r"\\server\share\project"),
        (r"C:\Users\한글 QA\project", r"C:\Users\한글 QA\project"),
        (r"\\?\C:\special.\project", r"\\?\C:\special.\project"),
        (r"\\?\C:\special \project", r"\\?\C:\special \project"),
        (r"\\?\C:\CON\project", r"\\?\C:\CON\project"),
        (r"\\.\pipe\krill", r"\\.\pipe\krill"),
        (
            r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\",
            r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\",
        ),
    ] {
        let mut cwd = input.as_bytes().to_vec();
        sanitize_spawn_cwd(&mut cwd);
        assert_eq!(cwd, expected.as_bytes(), "{input}");
    }
}

#[cfg(windows)]
#[test]
fn spawn_cwd_preserves_long_verbatim_paths() {
    let path = format!(r"\\?\C:\{}", "long-directory\\".repeat(30));
    let mut cwd = path.as_bytes().to_vec();
    sanitize_spawn_cwd(&mut cwd);
    assert_eq!(cwd, path.as_bytes());
}
