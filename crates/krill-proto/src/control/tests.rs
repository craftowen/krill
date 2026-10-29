use super::*;

use crate::holder::STATUS_UNKNOWN;

use crate::ipc::read_frame;

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::new(), |mut s, x| {
        write!(s, "{x:02x}").unwrap();
        s
    })
}

fn roundtrip<T: Msg + PartialEq + fmt::Debug>(m: &T) -> String {
    let bytes = m.encode().unwrap();
    let (k, body) = read_frame(&mut &bytes[..]).unwrap().unwrap();
    assert_eq!(&T::decode(k, &body).unwrap(), m);
    hex(&bytes)
}

/// 고정 바이트: 이 표가 바뀌면 호환 위반이다(새 메시지는 줄을 추가만 한다).
#[test]
fn fixtures_v1() {
    let cases: Vec<(String, &str)> = vec![
        (
            roundtrip(&Request::Create(Create {
                account: None,
                program: Program::Login,
                cwd: b"/tmp".to_vec(),
                cols: 80,
                rows: 24,
            })),
            "0a000000300000042f746d705018",
        ),
        (
            roundtrip(&Request::Create(Create {
                account: None,
                program: Program::Custom {
                    program: b"/bin/sh".to_vec(),
                    args: vec![b"-c".to_vec(), b"x".to_vec()],
                },
                cwd: Vec::new(),
                cols: 200,
                rows: 50,
            })),
            "15000000300001072f62696e2f736802022d63017800c80132",
        ),
        (
            roundtrip(&Request::Create(Create {
                account: None,
                program: Program::Agent {
                    id: "claude".to_owned(),
                },
                cwd: Vec::new(),
                cols: 80,
                rows: 24,
            })),
            "0d00000030000206636c61756465005018",
        ),
        (roundtrip(&Request::List), "020000003001"),
        (
            roundtrip(&Request::Close {
                session: SessionId(300),
            }),
            "040000003002ac02",
        ),
        (
            roundtrip(&Reply::Created {
                session: SessionId(7),
                pid: 4242,
            }),
            "050000003100079221",
        ),
        (
            roundtrip(&Reply::Sessions(vec![
                Info {
                    session: SessionId(1),
                    pid: 10,
                    status: None,
                },
                Info {
                    session: SessionId(2),
                    pid: 11,
                    status: Some(STATUS_UNKNOWN),
                },
            ])),
            "0a000000310102010a00020b0101",
        ),
        (roundtrip(&Reply::Closed), "020000003102"),
        (
            roundtrip(&Reply::Failed(Failure::SessionLimit)),
            "03000000310301",
        ),
        (
            roundtrip(&Reply::Failed(Failure::Os(2))),
            "0400000031030604",
        ),
        (roundtrip(&Request::States), "020000003003"),
        (
            roundtrip(&Reply::States(vec![
                SessionStatus {
                    session: SessionId(1),
                    status: AgentStatus::UNKNOWN,
                },
                SessionStatus {
                    session: SessionId(2),
                    status: AgentStatus {
                        state: AgentState::Waiting,
                        source: Some(Source::Hook),
                        confidence: Confidence::High,
                        agent: "claude".to_owned(),
                        since_unix_ns: 1_760_000_000_000_000_000,
                        foreground: None,
                        title: None,
                        subagents: Vec::new(),
                        root_shell: None,
                    },
                },
            ])),
            "1e000000310402010500000000020101000206636c617564658080c0a5cdd5b1b618",
        ),
    ];
    for (got, want) in cases {
        assert_eq!(got, want);
    }
}

#[test]
fn eproclim_is_session_limit() {
    assert_eq!(
        Failure::from_holder_errno(SESSION_LIMIT_ERRNO),
        Failure::SessionLimit
    );
    assert_eq!(Failure::from_holder_errno(2), Failure::Os(2));
    assert!(Failure::SessionLimit.to_string().contains("256"));
}

/// 새 클라이언트가 보낸 모르는 variant는 오류로 읽힌다(krilld는 `BadRequest`로 답한다).
/// Request 10(Sidebar)은 OWE-411 지휘자 배정으로 유효하므로, `owen-run` 예약 범위(20~29)를 쓴다.
#[test]
fn unknown_variant_is_an_error() {
    assert!(Request::decode(kind::REQUEST, &[20]).is_err());
}

/// MK-042 확장도 기존 variant 번호를 보존하고 별도 feature로 협상한다.
#[test]
fn worktree_fixtures() {
    assert_eq!(
        roundtrip(&Request::NewWorktree(NewWorktree {
            repo: b"/r".to_vec(),
            name: "x".to_owned(),
            base: None,
            setup: Setup::Ask,
        })),
        "090000003004022f7201780000"
    );
    assert_eq!(
        roundtrip(&Reply::WorktreeCreated(Worktree {
            path: b"/w".to_vec(),
            branch: "x".to_owned(),
            base: "main".to_owned(),
            copied: Vec::new(),
            warnings: Vec::new(),
            setup: None,
        })),
        "0f0000003105022f770178046d61696e000000"
    );
    assert_eq!(
        roundtrip(&Reply::SetupUntrusted {
            script: "x".to_owned(),
            hash: vec![1, 2]
        }),
        "0700000031060178020102"
    );
    assert_eq!(
        roundtrip(&Reply::WorktreeFailed("x".to_owned())),
        "0400000031070178"
    );
    for setup in [Setup::Skip, Setup::Approve(vec![7; 32])] {
        roundtrip(&Request::NewWorktree(NewWorktree {
            repo: b"/r".to_vec(),
            name: "한글".to_owned(),
            base: Some("main".to_owned()),
            setup,
        }));
    }
}

#[test]
fn worktree_remove_fixtures() {
    assert_eq!(
        roundtrip(&Request::RemoveWorktree(RemoveWorktree {
            repo: b"/r".to_vec(),
            path: b"/w".to_vec(),
            force: false,
        })),
        "090000003005022f72022f7700"
    );
    assert_eq!(
        roundtrip(&Reply::WorktreeRemoved(RemovedWorktree {
            branch: Some("x".to_owned()),
            branch_deleted: true,
            warnings: vec![],
        })),
        "0700000031080101780100"
    );
    for branch in [None, Some("한글".to_owned())] {
        roundtrip(&Reply::WorktreeRemoved(RemovedWorktree {
            branch,
            branch_deleted: false,
            warnings: vec!["preserved".to_owned()],
        }));
    }
    roundtrip(&Request::RemoveWorktree(RemoveWorktree {
        repo: vec![0xff],
        path: vec![0xfe],
        force: true,
    }));
}

#[test]
fn git_changes_fixtures() {
    use crate::git::{ChangedFile, Changes, Numstat};
    assert_eq!(
        roundtrip(&Request::GitChanges {
            repo: b"/r".to_vec()
        }),
        "050000003006022f72"
    );
    assert_eq!(
        roundtrip(&Reply::GitChanges(Changes {
            files: vec![],
            limited: true
        })),
        "0400000031090001"
    );
    assert_eq!(
        roundtrip(&Reply::GitChangesFailed("x".to_owned())),
        "04000000310a0178"
    );
    assert_eq!(
        roundtrip(&Reply::GitChanges(Changes {
            files: vec![ChangedFile {
                path: b"p".to_vec(),
                old_path: None,
                status: *b".M",
                staged: None,
                unstaged: Some(Numstat {
                    added: Some(2),
                    deleted: Some(1)
                })
            }],
            limited: false,
        })),
        "0f0000003109010170002e4d00010102010100"
    );
}

#[test]
fn git_changes_reply_rejects_oversized_collections_and_paths() {
    use crate::git::{ChangedFile, Changes, MAX_FILES, MAX_PATH_BYTES};
    let file = ChangedFile {
        path: b"p".to_vec(),
        old_path: None,
        status: *b"??",
        staged: None,
        unstaged: None,
    };
    for changes in [
        Changes {
            files: vec![file.clone(); MAX_FILES + 1],
            limited: false,
        },
        Changes {
            files: vec![ChangedFile {
                path: vec![b'p'; MAX_PATH_BYTES + 1],
                ..file.clone()
            }],
            limited: false,
        },
    ] {
        let bytes = Reply::GitChanges(changes).encode().unwrap();
        let (kind, body) = read_frame(&mut &bytes[..]).unwrap().unwrap();
        assert!(Reply::decode(kind, &body).is_err());
    }
    let changes = Changes {
        files: vec![file; MAX_FILES],
        limited: true,
    };
    roundtrip(&Reply::GitChanges(changes));
}

#[test]
fn cli_fixtures() {
    assert_eq!(
        roundtrip(&Request::Capture {
            session: SessionId(7),
            format: CaptureFormat::Plain
        }),
        "0400000030070700"
    );
    assert_eq!(
        roundtrip(&Request::Capture {
            session: SessionId(7),
            format: CaptureFormat::Ansi
        }),
        "0400000030070701"
    );
    assert_eq!(
        roundtrip(&Request::Resize {
            session: SessionId(7),
            cols: 80,
            rows: 24
        }),
        "050000003008075018"
    );
    assert_eq!(
        roundtrip(&Reply::Captured {
            seq: crate::screen::Seq(7),
            bytes: b"ok".to_vec()
        }),
        "06000000310b07026f6b"
    );
    assert_eq!(roundtrip(&Reply::Resized), "02000000310c");
}

proptest::proptest! {
    #[test]
    fn arbitrary_control_frames_do_not_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..2048)) {
        Request::decode(kind::REQUEST, &bytes).ok();
        Reply::decode(kind::REPLY, &bytes).ok();
    }
}

#[test]
fn oversized_capture_reply_is_rejected() {
    let frame = Reply::Captured {
        seq: crate::screen::Seq(0),
        bytes: vec![0; MAX_CAPTURE + 1],
    }
    .encode()
    .unwrap();
    let (kind, body) = read_frame(&mut &frame[..]).unwrap().unwrap();
    assert!(Reply::decode(kind, &body).is_err());
}

#[test]
fn wait_status_fixtures() {
    assert_eq!(
        roundtrip(&Request::WaitStatus {
            session: SessionId(7)
        }),
        "03000000300907"
    );
    assert_eq!(
        roundtrip(&Reply::WaitStatus {
            state: AgentState::Done,
            quiet_ms: 300
        }),
        "05000000310d03ac02"
    );
}

#[test]
fn sidebar_fixtures_and_shared_badge() {
    assert_eq!(roundtrip(&Request::Sidebar), "02000000300a");
    assert_eq!(
        roundtrip(&Reply::Sidebar(SidebarData {
            sessions: vec![],
            repositories: vec![],
            branches: vec![],
            prompt_histories: Vec::new(),
        })),
        "04000000310e0000"
    );
    assert_eq!(
        roundtrip(&Reply::Sidebar(SidebarData {
            sessions: vec![SessionDirectory {
                session: SessionId(1),
                cwd: b"/a".to_vec()
            }],
            repositories: vec![RepositoryTrees {
                repo: b"/a".to_vec(),
                worktrees: vec![b"/a".to_vec()]
            }],
            branches: vec![],
            prompt_histories: Vec::new(),
        })),
        "0f000000310e0101022f6101022f6101022f61"
    );
    let with_branches = Reply::Sidebar(SidebarData {
        sessions: vec![],
        repositories: vec![RepositoryTrees {
            repo: b"/a".to_vec(),
            worktrees: vec![b"/a".to_vec()],
        }],
        branches: vec![(b"/a".to_vec(), "main".into())],
        prompt_histories: Vec::new(),
    });
    let _ = roundtrip(&with_branches);
    let mut status = AgentStatus::UNKNOWN;
    for (source, badge) in [
        (Source::Hook, None),
        (Source::Process, None),
        (Source::Title, Some("추정")),
        (Source::Screen, Some("추정")),
    ] {
        status.source = Some(source);
        assert_eq!(status.badge(), badge);
    }
}

#[test]
fn sidebar_metadata_rejects_oversized_paths_and_collections() {
    let cases = [
        SidebarData {
            sessions: vec![],
            repositories: vec![],
            branches: vec![(vec![b'x'; 4097], "main".into())],
            prompt_histories: Vec::new(),
        },
        SidebarData {
            sessions: vec![],
            repositories: vec![],
            branches: vec![(b"/a".to_vec(), "x".repeat(MAX_TITLE_TEXT + 1))],
            prompt_histories: Vec::new(),
        },
        SidebarData {
            sessions: vec![],
            repositories: vec![],
            branches: vec![(b"/a".to_vec(), "main".into()); 513],
            prompt_histories: Vec::new(),
        },
        SidebarData {
            sessions: vec![SessionDirectory {
                session: SessionId(1),
                cwd: vec![b'x'; 4097],
            }],
            repositories: vec![],
            branches: vec![],
            prompt_histories: Vec::new(),
        },
        SidebarData {
            sessions: vec![
                SessionDirectory {
                    session: SessionId(1),
                    cwd: vec![]
                };
                257
            ],
            repositories: vec![],
            branches: vec![],
            prompt_histories: Vec::new(),
        },
        SidebarData {
            sessions: vec![],
            repositories: vec![RepositoryTrees {
                repo: vec![],
                worktrees: vec![vec![]; 513],
            }],
            branches: vec![],
            prompt_histories: Vec::new(),
        },
    ];
    for data in cases {
        let bytes = Reply::Sidebar(data).encode().unwrap();
        let (kind, body) = read_frame(&mut &bytes[..]).unwrap().unwrap();
        assert!(Reply::decode(kind, &body).is_err());
    }
}

/// OWE-488: 기존 Create fixture는 그대로, 새 enum은 왕복하고 내부 DTO는 고정 바이트로 검사한다.
#[test]
fn agent_settings_and_launch_wire_contract() {
    use crate::agent::Permission;
    let settings = AgentSettings {
        permission: Permission::Yolo,
        profiles: vec![AgentProfile {
            id: "x".into(),
            name: "X".into(),
        }],
    };
    let launch = AgentLaunch {
        profile: "x".into(),
        permission: Permission::Yolo,
    };
    for req in [
        Request::AgentSettings { permission: None },
        Request::AgentSettings {
            permission: Some(Permission::Manual),
        },
        Request::AgentSettings {
            permission: Some(Permission::Yolo),
        },
        Request::AgentLaunch {
            session: SessionId(300),
        },
    ] {
        roundtrip(&req);
    }
    for reply in [
        Reply::AgentSettings(settings.clone()),
        Reply::AgentLaunch(Some(launch.clone())),
        Reply::AgentLaunch(None),
    ] {
        roundtrip(&reply);
    }
    let settings_fixture = [1, 1, 1, b'x', 1, b'X'];
    assert_eq!(postcard::to_stdvec(&settings).unwrap(), settings_fixture);
    assert_eq!(
        postcard::from_bytes::<AgentSettings>(&settings_fixture).unwrap(),
        settings
    );
    let launch_fixture = [1, b'x', 1];
    assert_eq!(postcard::to_stdvec(&launch).unwrap(), launch_fixture);
    assert_eq!(
        postcard::from_bytes::<AgentLaunch>(&launch_fixture).unwrap(),
        launch
    );
}

#[test]
fn agent_metadata_limits_reject_oversized_collections_and_text() {
    for reply in [
        Reply::AgentSettings(AgentSettings {
            permission: crate::agent::Permission::Manual,
            profiles: vec![
                AgentProfile {
                    id: String::new(),
                    name: String::new()
                };
                MAX_AGENT_PROFILES + 1
            ],
        }),
        Reply::AgentSettings(AgentSettings {
            permission: crate::agent::Permission::Manual,
            profiles: vec![AgentProfile {
                id: "x".into(),
                name: "x".repeat(MAX_AGENT_TEXT + 1),
            }],
        }),
        Reply::AgentLaunch(Some(AgentLaunch {
            profile: "x".repeat(MAX_AGENT_TEXT + 1),
            permission: crate::agent::Permission::Yolo,
        })),
    ] {
        let bytes = reply.encode().unwrap();
        let (kind, body) = read_frame(&mut &bytes[..]).unwrap().unwrap();
        assert!(Reply::decode(kind, &body).is_err());
    }
}

#[test]
fn upgrade_wire_contract_and_build_order() {
    let up = Upgrade {
        build: "0.2.0".into(),
        holder: b"/h".to_vec(),
        krilld: b"/k".to_vec(),
    };
    roundtrip(&Request::Upgrade(up.clone()));
    roundtrip(&Reply::Upgrade(Upgraded::Started));
    roundtrip(&Reply::Upgrade(Upgraded::NotNewer));
    let fixture = [
        5, b'0', b'.', b'2', b'.', b'0', 2, b'/', b'h', 2, b'/', b'k',
    ];
    assert_eq!(postcard::to_stdvec(&up).unwrap(), fixture);
    assert_eq!(postcard::from_bytes::<Upgrade>(&fixture).unwrap(), up);
    assert_eq!(postcard::to_stdvec(&Upgraded::NotNewer).unwrap(), [1]);
    let long = Upgrade {
        krilld: vec![b'k'; 4097],
        ..up
    };
    assert!(postcard::from_bytes::<Upgrade>(&postcard::to_stdvec(&long).unwrap()).is_err());

    assert_eq!(is_newer("0.2.0", "0.1.9"), Some(true));
    assert_eq!(is_newer("0.10.0", "0.9.0"), Some(true));
    assert_eq!(is_newer("1.0.0+abc", "0.99.99"), Some(true));
    assert_eq!(is_newer("0.1.0", "0.1.0"), Some(false));
    assert_eq!(is_newer("0.1.0", "0.2.0"), Some(false)); // 다운그레이드
    assert_eq!(is_newer("0.2", "0.1.0"), None);
    assert_eq!(is_newer("0.2.0.1", "0.1.0"), None);
    assert_eq!(is_newer("x", "0.1.0"), None);
}

/// AS-11: 프리릴리스 순서를 무시하면 같은 코어 베타 간 업데이트가 전달되지 않는다.
#[test]
fn version_prerelease_ordering_follows_semver() {
    assert_eq!(is_newer("0.3.0-beta.2", "0.3.0-beta.1"), Some(true));
    assert_eq!(is_newer("0.3.0-beta.1", "0.3.0-beta.2"), Some(false));
    assert_eq!(is_newer("0.3.0-beta.10", "0.3.0-beta.9"), Some(true));
    assert_eq!(is_newer("0.3.0", "0.3.0-beta.9"), Some(true));
    assert_eq!(is_newer("0.3.0-beta.1", "0.3.0"), Some(false));
    assert_eq!(is_newer("0.3.0-alpha", "0.3.0-beta"), Some(false));
    assert_eq!(is_newer("0.3.0-beta.1", "0.3.0-beta.1"), Some(false));
    assert_eq!(is_newer("0.3.0-beta.1+dev.aa", "0.3.0-beta.1"), Some(true));
    assert_eq!(is_newer("0.2.0", "0.1.9"), Some(true));
}

/// MK-032. 변형 번호(첫 body 바이트)는 병합 순서로 정해지므로 머리(길이·kind)와 그 뒤 바이트만 고정한다.
#[test]
fn cold_fixtures() {
    use crate::screen::{Cell, Op};
    fn parts<T: Msg + PartialEq + fmt::Debug>(m: &T) -> (String, String) {
        roundtrip(m);
        let bytes = m.encode().unwrap();
        let (head, rest) = bytes.split_at(5);
        (hex(head), hex(rest.split_at(1).1))
    }
    assert_eq!(
        parts(&Request::Cold {
            session: SessionId(7),
        }),
        ("0300000030".to_owned(), "07".to_owned())
    );
    let cell = Cell {
        ch: 'a',
        ..Cell::default()
    };
    assert_eq!(
        parts(&Reply::Cold(Cold {
            screen: vec![Op::Reset { cols: 2, lines: 1 }],
            history: vec![vec![cell]],
        })),
        (
            "0f00000031".to_owned(),
            "01000201010101610000000000".to_owned()
        )
    );
}

/// 화면은 그대로, 스크롤백은 프레임에 들어갈 만큼 최신 행만 남는다.
#[test]
fn cold_fit_drops_oldest_history() {
    use crate::ipc::MAX_FRAME;
    use crate::screen::{Cell, Op};
    let row = |ch| {
        vec![
            Cell {
                ch,
                ..Cell::default()
            };
            1_000
        ]
    };
    let mut cold = Cold {
        screen: vec![Op::Reset {
            cols: 80,
            lines: 24,
        }],
        history: (0..400)
            .map(|i| row(if i == 399 { 'z' } else { 'a' }))
            .collect(),
    };
    assert!(cold.fit());
    assert!(cold.history.len() < 400 && !cold.history.is_empty());
    assert_eq!(cold.history.last().unwrap()[0].ch, 'z');
    assert!(Reply::Cold(cold).encode().unwrap().len() <= MAX_FRAME + 4);

    let huge = Op::Line {
        line: 0,
        cells: row('a'),
    };
    let mut too_big = Cold {
        screen: vec![huge; 200],
        history: Vec::new(),
    };
    assert!(!too_big.fit());
}

/// MK-052. 변형 번호(첫 body 바이트)는 병합 순서로 정해지므로 머리(길이·kind)와 그 뒤 바이트만 고정한다.
#[test]
fn resume_fixtures() {
    fn parts<T: Msg + PartialEq + fmt::Debug>(m: &T) -> (String, String) {
        roundtrip(m);
        let bytes = m.encode().unwrap();
        let (head, rest) = bytes.split_at(5);
        (hex(head), hex(rest.split_at(1).1))
    }
    assert_eq!(
        parts(&Request::Resume {
            session: SessionId(7),
            create: Create {
                account: None,
                program: Program::Agent {
                    id: "claude".into(),
                },
                cwd: b"/w".to_vec(),
                cols: 80,
                rows: 24,
            },
        }),
        (
            "1000000030".to_owned(),
            "070206636c61756465022f775018".to_owned()
        )
    );
    assert_eq!(
        parts(&Reply::Resumed {
            session: SessionId(8),
            pid: 300,
        }),
        ("0500000031".to_owned(), "08ac02".to_owned())
    );
}

#[test]
fn git_watch_fixtures() {
    use crate::git::{Changes, GitWatchReply, GitWatchRequest};
    fn parts<T: Msg + PartialEq + fmt::Debug>(m: &T) -> (String, String) {
        roundtrip(m);
        let bytes = m.encode().unwrap();
        let (head, rest) = bytes.split_at(5);
        (hex(head), hex(rest.split_at(1).1))
    }
    assert_eq!(
        parts(&Request::GitWatch(GitWatchRequest {
            repo: b"/r".to_vec(),
            visible: true,
            since: 7,
            force: false,
        })),
        ("0800000030".to_owned(), "022f72010700".to_owned())
    );
    assert_eq!(
        parts(&Reply::GitWatch(GitWatchReply::Hidden)),
        ("0300000031".to_owned(), "00".to_owned())
    );
    assert_eq!(
        parts(&Reply::GitWatch(GitWatchReply::Unchanged { pending: true })),
        ("0400000031".to_owned(), "0101".to_owned())
    );
    assert_eq!(
        parts(&Reply::GitWatch(GitWatchReply::Changed {
            generation: 3,
            changes: Changes {
                files: vec![],
                limited: false,
            },
        })),
        ("0600000031".to_owned(), "02030000".to_owned())
    );
    assert_eq!(
        parts(&Reply::GitWatch(GitWatchReply::Failed {
            generation: 4,
            error: "e".to_owned(),
        })),
        ("0600000031".to_owned(), "03040165".to_owned())
    );
}

/// MK-081은 기존 enum 뒤에 추가한다. 머리·payload fixture는 병합 순서와 무관하게 고정한다.
#[test]
fn hibernation_fixtures() {
    fn parts<T: Msg + PartialEq + fmt::Debug>(m: &T) -> (String, String) {
        roundtrip(m);
        let bytes = m.encode().unwrap();
        let (head, body) = bytes.split_at(5);
        (hex(head), hex(body.split_at(1).1))
    }
    assert_eq!(
        parts(&Request::Hibernation {
            session: SessionId(7),
            create: Create {
                account: None,
                program: Program::Login,
                cwd: vec![],
                cols: 80,
                rows: 24
            }
        }),
        ("0700000030".into(), "0700005018".into())
    );
    assert_eq!(
        parts(&Reply::Hibernation { session: None }),
        ("0300000031".into(), "00".into())
    );
    assert_eq!(
        parts(&Reply::Hibernation {
            session: Some(SessionId(9))
        }),
        ("0400000031".into(), "0109".into())
    );
}

#[test]
fn worktree_preparation_payload_fixtures() {
    // The coordinator may append other variants before merging. Pin payload bytes without
    // inventing an enum index; all previous variants keep their existing full-frame fixtures.
    fn payload<T: Msg + PartialEq + fmt::Debug>(msg: &T) -> String {
        roundtrip(msg);
        let encoded = msg.encode().unwrap();
        hex(encoded.split_at(6).1)
    }
    assert_eq!(
        payload(&Request::PrepareWorktree(PrepareWorktree {
            token: 7,
            repo: b"/r".to_vec(),
            base: Some("main".into()),
            cancel: false,
        })),
        "07022f7201046d61696e00"
    );
    assert_eq!(
        payload(&Request::PrepareWorktree(PrepareWorktree {
            token: 7,
            repo: b"/r".to_vec(),
            base: None,
            cancel: true,
        })),
        "07022f720001"
    );
    for (value, bytes) in [
        (WorktreePreparation::Preparing, "00"),
        (WorktreePreparation::Ready, "01"),
        (WorktreePreparation::Busy, "02"),
        (WorktreePreparation::Cancelled, "03"),
        (WorktreePreparation::Skipped, "04"),
        (WorktreePreparation::Failed("e".into()), "050165"),
    ] {
        assert_eq!(payload(&Reply::WorktreePreparation(value)), bytes);
    }
}

#[test]
fn git_edit_roundtrip_and_fixed_payload_without_control_variant_indices() {
    use crate::git::Edit;
    let cases = [
        (
            Edit::Stage {
                path: b"a".into(),
                old_path: None,
            },
            "00016100",
        ),
        (
            Edit::Unstage {
                path: b"b".into(),
                old_path: Some(b"a".into()),
            },
            "010162010161",
        ),
        (
            Edit::Commit {
                message: "m".into(),
            },
            "02016d",
        ),
        (Edit::Push, "03"),
        (Edit::Pull, "04"),
        (Edit::Sync, "05"),
        (
            Edit::CommitAndPush {
                message: "m".into(),
            },
            "06016d",
        ),
    ];
    for (edit, fixture) in cases {
        let bytes = postcard::to_stdvec(&edit).unwrap();
        assert_eq!(hex(&bytes), fixture);
        assert_eq!(postcard::from_bytes::<Edit>(&bytes).unwrap(), edit);
        let expected_features = match edit {
            Edit::Stage { .. } | Edit::Unstage { .. } | Edit::Commit { .. } => {
                FEAT_CONTROL | FEAT_GIT_EDIT
            }
            Edit::Push | Edit::Pull | Edit::Sync | Edit::CommitAndPush { .. } => {
                FEAT_CONTROL | FEAT_GIT_EDIT | crate::ipc::FEAT_GIT_SYNC
            }
        };
        assert_eq!(edit.required_features(), expected_features);
        roundtrip(&Request::GitEdit {
            repo: b"/r".into(),
            edit,
        });
    }
    roundtrip(&Reply::GitEdited(Ok(())));
    roundtrip(&Reply::GitEdited(Err("hook rejected".into())));
    assert_eq!(FEAT_GIT_EDIT, 1 << 48);
    assert_eq!(crate::ipc::FEAT_GIT_SYNC, 1 << 41);
    assert_eq!(crate::ipc::FEATURES & FEAT_GIT_EDIT, FEAT_GIT_EDIT);
    assert_eq!(
        crate::ipc::FEATURES & crate::ipc::FEAT_GIT_SYNC,
        crate::ipc::FEAT_GIT_SYNC
    );
}

#[test]
fn dev_build_hash_change_requests_replacement() {
    assert_eq!(is_newer("0.1.0+dev.2222", "0.1.0+dev.1111"), Some(true));
    assert_eq!(is_newer("0.1.0+dev.2222", "0.1.0"), Some(true));
    assert_eq!(is_newer("0.1.0+dev.2222", "0.1.0+dev.2222"), Some(false));
    assert_eq!(is_newer("0.1.0+dev.2222", "0.2.0+dev.1111"), Some(false));
    assert_eq!(is_newer("0.1.0", "0.1.0+dev.1111"), Some(false));
}

#[test]
fn dev_build_identifier_contains_the_source_hash() {
    let (version, hash) = crate::BUILD.split_once("+dev.").unwrap_or(("", ""));
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
    assert_eq!(hash.len(), 16);
    assert!(hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

/// OWE-707: 세션별 사용 계정은 새 variant(끝에 추가)로 오간다.
/// 기존 variant 번호·고정 바이트는 그대로라 N-1이다. 구버전 피어는
/// [`FEAT_SESSION_ACCOUNT`] 없이 `SessionAccounts`를 보내지도 받지도 않는다.
#[test]
fn session_accounts_wire_contract() {
    // 요청·응답 왕복.
    roundtrip(&Request::SessionAccounts);
    roundtrip(&Reply::SessionAccounts(vec![]));
    roundtrip(&Reply::SessionAccounts(vec![
        SessionAccount {
            session: SessionId(1),
            account_id: "claude-work".into(),
            label: "Work Max".into(),
        },
        SessionAccount {
            session: SessionId(2),
            account_id: String::new(),
            label: String::new(),
        },
    ]));
    // 라벨 길이 상한(기존 `agent_text`와 같은 4096).
    let long = SessionAccount {
        session: SessionId(1),
        account_id: "a".into(),
        label: "x".repeat(MAX_AGENT_TEXT + 1),
    };
    let bytes = Reply::SessionAccounts(vec![long]).encode().unwrap();
    let (kind, body) = crate::ipc::read_frame(&mut &bytes[..]).unwrap().unwrap();
    assert!(Reply::decode(kind, &body).is_err());
    // JSON(구 문서)에서도 꼬리 기본값이 든다.
    let acc: SessionAccount = serde_json::from_str(r#"{"session":1,"account_id":"a"}"#).unwrap();
    assert_eq!(acc.label, "");
    let acc: SessionAccount = serde_json::from_str(r#"{"session":1}"#).unwrap();
    assert_eq!(acc.account_id, "");
    assert_eq!(acc.label, "");
    assert_eq!(FEAT_SESSION_ACCOUNT, 1 << 58);
    assert_ne!(crate::ipc::FEATURES & FEAT_SESSION_ACCOUNT, 0);
}

#[test]
fn clear_scrollback_wire_contract() {
    assert_eq!(
        roundtrip(&Request::ClearScrollback {
            session: SessionId(7),
        }),
        "03000000301707"
    );
    assert_eq!(roundtrip(&Reply::ScrollbackCleared), "02000000311b");
    assert_eq!(FEAT_CLEAR_SCROLLBACK, 1 << 47);
    assert_ne!(crate::ipc::FEATURES & FEAT_CLEAR_SCROLLBACK, 0);
}

#[test]
fn create_account_tail_preserves_n_minus_one_prefix_in_both_directions() {
    let legacy = (
        0_u8,
        (
            Program::Agent { id: "codex".into() },
            b"/tmp".to_vec(),
            80_u16,
            24_u16,
        ),
    );
    let old_body = postcard::to_stdvec(&legacy).unwrap();
    let Request::Create(mut create) = Request::decode(kind::REQUEST, &old_body).unwrap() else {
        panic!("create")
    };
    assert_eq!(create.account, None);
    create.account = Some("codex-work".into());
    let encoded = Request::Create(create.clone()).encode().unwrap();
    let (kind, body) = read_frame(&mut encoded.as_slice()).unwrap().unwrap();
    let (old, tail) =
        postcard::take_from_bytes::<(u8, (Program, Vec<u8>, u16, u16))>(&body).unwrap();
    assert_eq!(old, legacy);
    assert!(!tail.is_empty());
    assert_eq!(
        Request::decode(kind, &body).unwrap(),
        Request::Create(create)
    );
}

#[test]
fn resume_and_hibernation_account_tails_decode_old_bytes() {
    for tag in [16_u8, 18] {
        let old = postcard::to_stdvec(&(
            tag,
            SessionId(712),
            (Program::Login, Vec::<u8>::new(), 80_u16, 24_u16),
        ))
        .unwrap();
        let decoded = Request::decode(kind::REQUEST, &old).unwrap();
        let (Request::Resume { create, .. } | Request::Hibernation { create, .. }) = decoded else {
            panic!("create request")
        };
        assert_eq!(create.account, None);
        let mut selected = create;
        selected.account = Some("claude-host".into());
        let request = if tag == 16 {
            Request::Resume {
                session: SessionId(712),
                create: selected,
            }
        } else {
            Request::Hibernation {
                session: SessionId(712),
                create: selected,
            }
        };
        let encoded = request.encode().unwrap();
        let (_, body) = read_frame(&mut encoded.as_slice()).unwrap().unwrap();
        assert!(body.starts_with(&old));
        assert_eq!(Request::decode(kind::REQUEST, &body).unwrap(), request);
    }
}

/// `Failure::Os` 와이어: 같은 OS끼리는 늘 원래 errno로 돌아오고, Linux 번호는 표식으로 구분돼 macOS가 아는 것만 옮긴다.
#[test]
fn os_failure_errno_travels_in_macos_numbering() {
    use super::wire_errno::{from_wire, to_wire};
    for errno in [1, 2, 11, 13, 34, 35, 38, 40, 62, 63, 78, 110, 4000] {
        assert_eq!(
            from_wire(to_wire(errno)),
            errno,
            "same-OS roundtrip {errno}"
        );
    }
    #[cfg(target_os = "linux")]
    {
        assert_eq!(to_wire(2), 2);
        assert_eq!(to_wire(38), 10_038);
        assert_eq!(from_wire(78), 38, "원격 Mac의 ENOSYS");
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert_eq!(to_wire(78), 78);
        assert_eq!(from_wire(10_038), 78, "Linux ENOSYS → macOS ENOSYS");
        assert_eq!(
            from_wire(10_062),
            10_062,
            "모르는 Linux 번호는 틀린 문구 대신 그대로"
        );
        assert_eq!(
            roundtrip(&Reply::Failed(Failure::Os(78))),
            "050000003103069c01",
            "macOS 와이어 바이트 불변"
        );
    }
}

/// OWE-872: `Reply::States` 전경 프로세스 이름 꼬리는 구버전 프레임과 양방향 호환한다.
#[test]
fn states_foreground_tail_preserves_n_minus_one_in_both_directions() {
    let base = vec![
        SessionStatus {
            session: SessionId(1),
            status: AgentStatus::UNKNOWN,
        },
        SessionStatus {
            session: SessionId(2),
            status: AgentStatus {
                state: AgentState::Working,
                source: Some(Source::Hook),
                confidence: Confidence::High,
                agent: "claude".into(),
                since_unix_ns: 42,
                foreground: None,
                title: None,
                subagents: Vec::new(),
                root_shell: None,
            },
        },
    ];
    let old_frame = Reply::States(base.clone()).encode().unwrap();
    let (kind, old_body) = read_frame(&mut old_frame.as_slice()).unwrap().unwrap();
    assert_eq!(
        Reply::decode(kind, &old_body).unwrap(),
        Reply::States(base.clone())
    );

    let mut with_fg = base.clone();
    with_fg[0].status.foreground = Some("npm".into());
    let new_reply = Reply::States(with_fg);
    let new_frame = new_reply.encode().unwrap();
    let (kind, new_body) = read_frame(&mut new_frame.as_slice()).unwrap().unwrap();
    assert!(new_body.starts_with(&old_body));
    assert_eq!(Reply::decode(kind, &new_body).unwrap(), new_reply);
    // 구 디코더(postcard::from_bytes)는 꼬리를 무시하고 기존 상태만 읽는다.
    assert_eq!(
        postcard::from_bytes::<Reply>(&new_body).unwrap(),
        Reply::States(base)
    );

    for bad in [String::new(), "x".repeat(MAX_AGENT_TEXT + 1)] {
        let invalid = Reply::States(vec![SessionStatus {
            session: SessionId(1),
            status: AgentStatus {
                foreground: Some(bad),
                ..AgentStatus::UNKNOWN
            },
        }])
        .encode()
        .unwrap();
        let (kind, body) = read_frame(&mut invalid.as_slice()).unwrap().unwrap();
        assert!(Reply::decode(kind, &body).is_err());
    }
}

/// 세션 제목 꼬리는 전경 프로세스 꼬리 뒤에 붙어 구 디코더가 무시하고, 새 디코더는 둘 다 복원한다.
#[test]
fn states_title_tail_is_backward_compatible() {
    let base = vec![
        SessionStatus {
            session: SessionId(1),
            status: AgentStatus::UNKNOWN,
        },
        SessionStatus {
            session: SessionId(2),
            status: AgentStatus::UNKNOWN,
        },
    ];
    let mut titled = base.clone();
    titled[1].status.title = Some("krill 프로젝트 진행 상황".into());
    let reply = Reply::States(titled);
    let frame = reply.encode().unwrap();
    let (kind, body) = read_frame(&mut frame.as_slice()).unwrap().unwrap();
    assert_eq!(Reply::decode(kind, &body).unwrap(), reply);
    assert_eq!(
        postcard::from_bytes::<Reply>(&body).unwrap(),
        Reply::States(base)
    );

    let invalid = Reply::States(vec![SessionStatus {
        session: SessionId(1),
        status: AgentStatus {
            title: Some("x".repeat(MAX_TITLE_TEXT + 1)),
            ..AgentStatus::UNKNOWN
        },
    }])
    .encode()
    .unwrap();
    let (kind, body) = read_frame(&mut invalid.as_slice()).unwrap().unwrap();
    assert!(Reply::decode(kind, &body).is_err());
}

/// 서브에이전트 꼬리는 제목 꼬리 뒤에 붙어 구 디코더가 무시하고, 새 디코더는 복원하며 상한을 넘으면 거부한다.
#[test]
fn states_subagent_tail_is_backward_compatible() {
    let agent = Subagent {
        kind: "general-purpose".into(),
        description: "딥리서치 각도1".into(),
        since_unix_ns: 7,
    };
    let mut rows = vec![
        SessionStatus {
            session: SessionId(1),
            status: AgentStatus::UNKNOWN,
        },
        SessionStatus {
            session: SessionId(2),
            status: AgentStatus::UNKNOWN,
        },
    ];
    let base = rows.clone();
    rows[1].status.subagents = vec![agent.clone()];
    let reply = Reply::States(rows);
    let frame = reply.encode().unwrap();
    let (kind, body) = read_frame(&mut frame.as_slice()).unwrap().unwrap();
    assert_eq!(Reply::decode(kind, &body).unwrap(), reply);
    assert_eq!(
        postcard::from_bytes::<Reply>(&body).unwrap(),
        Reply::States(base)
    );

    let too_many = Reply::States(vec![SessionStatus {
        session: SessionId(1),
        status: AgentStatus {
            subagents: vec![agent; MAX_SUBAGENTS + 1],
            ..AgentStatus::UNKNOWN
        },
    }])
    .encode()
    .unwrap();
    let (kind, body) = read_frame(&mut too_many.as_slice()).unwrap().unwrap();
    assert!(Reply::decode(kind, &body).is_err());
}

#[test]
fn display_title_strips_agent_status_glyphs() {
    assert_eq!(
        display_title("✳ krill 프로젝트 진행 상황").as_deref(),
        Some("krill 프로젝트 진행 상황")
    );
    assert_eq!(
        display_title("⠂ PWA QR 코드 스캔 UX").as_deref(),
        Some("PWA QR 코드 스캔 UX")
    );
    assert_eq!(display_title("✦  Working").as_deref(), Some("Working"));
    assert_eq!(display_title("~/krill").as_deref(), Some("~/krill"));
    assert_eq!(display_title(" ✳ \u{7}"), None);
    assert!(display_title(&"가".repeat(200)).is_some_and(|t| t.len() <= MAX_TITLE_TEXT));
}

/// postcard는 필드를 순서대로만 읽으므로 `skip_serializing_if`로 빠진 필드가 있으면 디코드가 밀린다.
/// 빈 오케스트레이션 스냅샷(`current_run_id: None`)이 CLI에서 `bad body`로 깨지던 회귀.
#[test]
fn orchestration_snapshot_reply_roundtrips_without_current_run() {
    use crate::orchestration::{OrchResponse, OrchestrationStore};
    let empty = Reply::Orchestration(OrchResponse::Snapshot(OrchestrationStore::default()));
    roundtrip(&empty);
    let current = Reply::Orchestration(OrchResponse::Snapshot(OrchestrationStore {
        current_run_id: Some("run-1".into()),
        ..OrchestrationStore::default()
    }));
    roundtrip(&current);
}

#[test]
fn capture_since_is_appended_and_distinguishes_empty_from_unchanged() {
    let stamp = CaptureStamp {
        session: SessionId(7),
        revision: crate::screen::Revision(3),
        seq: crate::screen::Seq(9),
    };
    let request = Request::CaptureSince {
        session: SessionId(7),
        since: Some(stamp),
    };
    let bytes = postcard::to_stdvec(&request).unwrap();
    assert_eq!(bytes[0], 26, "new request must remain appended");
    assert_eq!(postcard::from_bytes::<Request>(&bytes).unwrap(), request);
    assert!(!roundtrip(&Reply::CaptureSince { stamp, bytes: None }).is_empty());
    assert_ne!(
        roundtrip(&Reply::CaptureSince { stamp, bytes: None }),
        roundtrip(&Reply::CaptureSince {
            stamp,
            bytes: Some(Vec::new())
        })
    );
    let too_large = Reply::CaptureSince {
        stamp,
        bytes: Some(vec![0; MAX_CAPTURE + 1]),
    };
    let encoded = postcard::to_stdvec(&too_large).unwrap();
    assert!(postcard::from_bytes::<Reply>(&encoded).is_err());
}

#[test]
fn states_root_shell_tail_is_bounded_and_preserves_old_frames() {
    let base = Reply::States(vec![SessionStatus {
        session: SessionId(1),
        status: AgentStatus::UNKNOWN,
    }]);
    let old = base.encode().unwrap();
    let old_body = &old[5..];
    assert_eq!(Reply::decode(kind::REPLY, old_body).unwrap(), base);
    let Reply::States(mut rows) = base.clone() else {
        unreachable!()
    };
    rows[0].status.root_shell = Some(Box::new(RootShell {
        os: ShellOs::Windows,
        program: "powershell".into(),
    }));
    let reply = Reply::States(rows.clone());
    let frame = reply.encode().unwrap();
    assert_eq!(Reply::decode(kind::REPLY, &frame[5..]).unwrap(), reply);
    // The N-1 decoder consumes foreground/title/subagent extensions, then ignores
    // the root shell extension. Its base postcard representation stays identical.
    let (old_reply, mut tail) = postcard::take_from_bytes::<Reply>(&frame[5..]).unwrap();
    assert_eq!(old_reply, base);
    for _ in 0..2 {
        tail = postcard::take_from_bytes::<Option<String>>(tail).unwrap().1;
    }
    tail = postcard::take_from_bytes::<Vec<Subagent>>(tail).unwrap().1;
    assert_eq!(
        postcard::from_bytes::<Option<Box<RootShell>>>(tail).unwrap(),
        rows[0].status.root_shell
    );
    for invalid in [String::new(), "a".repeat(257), "pwsh\ncmd".into()] {
        rows[0].status.root_shell.as_mut().unwrap().program = invalid;
        let frame = Reply::States(rows.clone()).encode().unwrap();
        assert_eq!(Reply::decode(kind::REPLY, &frame[5..]).unwrap(), base);
        // Still reject invalid metadata arriving from an unvalidated peer.
        let mut body = postcard::to_stdvec(&base).unwrap();
        body.extend(postcard::to_stdvec(&Option::<String>::None).unwrap());
        body.extend(postcard::to_stdvec(&Option::<String>::None).unwrap());
        body.extend(postcard::to_stdvec(&Vec::<Subagent>::new()).unwrap());
        body.extend(postcard::to_stdvec(&rows[0].status.root_shell).unwrap());
        assert!(Reply::decode(kind::REPLY, &body).is_err());
    }
}

#[test]
fn invalid_observed_root_metadata_does_not_poison_other_rows_or_status() {
    let valid = [
        "a".repeat(256),
        "한".repeat(85),
        "🦀".repeat(64),
        "sh name'quoted".into(),
    ];
    let invalid = [
        String::new(),
        "a".repeat(257),
        "한".repeat(86),
        "🦀".repeat(65),
        "sh\ncmd".into(),
        "sh\rcmd".into(),
        "sh\0cmd".into(),
        "sh\u{85}cmd".into(),
    ];
    for name in valid.iter().chain(&invalid) {
        let accepted = valid.contains(name);
        assert_eq!(
            RootShell::new(ShellOs::Unix, name.clone()).is_some(),
            accepted
        );
        let mut rows = vec![
            SessionStatus {
                session: SessionId(1),
                status: AgentStatus::UNKNOWN,
            },
            SessionStatus {
                session: SessionId(2),
                status: AgentStatus::UNKNOWN,
            },
        ];
        rows[0].status.root_shell = RootShell::new(ShellOs::Windows, "pwsh".into()).map(Box::new);
        rows[1].status.agent = "claude".into();
        rows[1].status.title = Some("working".into());
        // Public struct literals are possible: the encoder also omits only invalid metadata.
        rows[1].status.root_shell = Some(Box::new(RootShell {
            os: ShellOs::Unix,
            program: name.clone(),
        }));
        let frame = Reply::States(rows.clone()).encode().unwrap();
        if !accepted {
            rows[1].status.root_shell = None;
        }
        assert_eq!(
            Reply::decode(kind::REPLY, &frame[5..]).unwrap(),
            Reply::States(rows.clone())
        );
        let (Reply::States(old_rows), _) = postcard::take_from_bytes::<Reply>(&frame[5..]).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(old_rows.len(), 2);
        assert_eq!(old_rows[1].status.agent, "claude");
        assert!(old_rows.iter().all(|row| row.status.root_shell.is_none()));
    }
}

#[test]
fn sidebar_prompt_tail_roundtrip_and_legacy_compatibility() {
    use crate::git::{SessionPromptHistory, SessionPromptTurn};
    let history = SessionPromptHistory {
        total_turns: 1,
        turns: vec![SessionPromptTurn {
            text: "hi".into(),
            unix_ns: 1,
            turn: 1,
        }],
    };
    let reply = Reply::Sidebar(SidebarData {
        prompt_histories: vec![(SessionId(7), history)],
        ..SidebarData::default()
    });
    assert_eq!(roundtrip(&reply), "0e000000310e000000010701010268690101");
    let bytes = reply.encode().unwrap();
    let (_, body) = read_frame(&mut &bytes[..]).unwrap().unwrap();
    // N-1 decoder consumes the original sidebar and optional branches, ignoring newer tails.
    let (Reply::Sidebar(legacy), rest) = postcard::take_from_bytes::<Reply>(&body).unwrap() else {
        panic!("expected sidebar");
    };
    assert!(legacy.prompt_histories.is_empty());
    let (Branches(branches), _) = postcard::take_from_bytes::<Branches>(rest).unwrap();
    assert!(branches.is_empty());
    let Reply::Sidebar(old) = Reply::decode(kind::REPLY, &[14, 0, 0]).unwrap() else {
        panic!("expected legacy sidebar");
    };
    assert!(old.prompt_histories.is_empty());
}

#[test]
fn sidebar_prompt_tail_rejects_unbounded_peer_data() {
    use crate::git::{SessionPromptHistory, SessionPromptTurn};
    let turn = SessionPromptTurn {
        text: "hello".into(),
        unix_ns: 1,
        turn: 1,
    };
    for histories in [
        vec![(SessionId(1), SessionPromptHistory::default()); MAX_SESSIONS + 1],
        vec![(
            SessionId(1),
            SessionPromptHistory {
                total_turns: 6,
                turns: vec![turn.clone(); crate::git::MAX_PROMPT_HISTORY_TURNS + 1],
            },
        )],
        vec![(
            SessionId(1),
            SessionPromptHistory {
                total_turns: 1,
                turns: vec![SessionPromptTurn {
                    text: "x".repeat(crate::git::MAX_PROMPT_DISPLAY_CHARS + 2),
                    ..turn.clone()
                }],
            },
        )],
    ] {
        let bytes = Reply::Sidebar(SidebarData {
            prompt_histories: histories,
            ..SidebarData::default()
        })
        .encode()
        .unwrap();
        let (kind, body) = read_frame(&mut &bytes[..]).unwrap().unwrap();
        assert!(Reply::decode(kind, &body).is_err());
    }
}

#[test]
fn worktree_refs_roundtrip_append_only_fixtures() {
    assert_ne!(crate::ipc::FEATURES & FEAT_WORKTREE_REFS, 0);
    assert_eq!(
        roundtrip(&Request::WorktreeRefs {
            repo: b"/r".to_vec()
        }),
        "05000000301c022f72"
    );
    assert_eq!(
        roundtrip(&Reply::WorktreeRefs(vec!["main".into()])),
        "08000000312001046d61696e"
    );
    assert_eq!(roundtrip(&Reply::WorktreeRefs(vec![])), "03000000312000");
    // N-1 opcodes and payloads remain unchanged in both directions.
    assert_eq!(roundtrip(&Request::List), "020000003001");
    assert_eq!(roundtrip(&Reply::Closed), "020000003102");
    assert_eq!(
        Reply::decode(kind::REPLY, &[3, 5]).unwrap(),
        Reply::Failed(Failure::BadRequest)
    );
}

#[test]
fn worktree_refs_decode_rejects_oversized_and_invalid_payloads() {
    for refs in [
        vec!["main".to_owned(); MAX_WORKTREE_REFS + 1],
        vec!["x".repeat(MAX_WORKTREE_REF_BYTES + 1)],
        vec![String::new()],
        vec!["bad name".to_owned()],
        vec!["bad\nname".to_owned()],
    ] {
        let bytes = postcard::to_stdvec(&Reply::WorktreeRefs(refs)).unwrap();
        assert!(Reply::decode(kind::REPLY, &bytes).is_err());
    }
    let bytes = postcard::to_stdvec(&Request::WorktreeRefs {
        repo: vec![b'x'; 4097],
    })
    .unwrap();
    assert!(Request::decode(kind::REQUEST, &bytes).is_err());
    roundtrip(&Reply::WorktreeRefs(vec![
        "x".repeat(MAX_WORKTREE_REF_BYTES);
        MAX_WORKTREE_REFS
    ]));
}

proptest::proptest! {
    #[test]
    fn worktree_refs_decoder_does_not_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
        let mut body = vec![32];
        body.extend(bytes);
        Reply::decode(kind::REPLY, &body).ok();
    }
}
