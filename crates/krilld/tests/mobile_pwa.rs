//! 비보안 HTTP를 가정한 독립 JS 구현과 실제 데몬 간 모바일 워크플로 E2E.
// Windows는 모바일 게이트웨이가 아직 없다(sys::ipv4_interfaces 미구현).
#![cfg(unix)]

#[test]
fn non_secure_pwa_crypto_and_workflows() {
    let result = std::process::Command::new("node")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mobile_pwa.cjs"))
        .arg(env!("CARGO_BIN_EXE_krilld"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// 실제 holder·daemon의 Workflow JSON bridge로 생성·명시 종료·실패를 확인한다.
#[test]
fn workflow_terminal_lifecycle_and_snapshot_metadata() -> Result<(), Box<dyn std::error::Error>> {
    use krill_proto::control::{Create, Program, Reply, Request};
    use krill_proto::daemon::client_hello;
    use krill_proto::ipc::{self, ClientRole, ServerHello};
    use krill_proto::mobile::{MobileReply, MobileRequest};
    use krill_proto::screen::Msg;
    use serde_json::{Value, json};
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    struct Fixture {
        holder: std::process::Child,
        daemon_pid: Option<rustix::process::Pid>,
        dir: std::path::PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.holder.kill().ok();
            self.holder.wait().ok();
            if let Some(pid) = self.daemon_pid {
                rustix::process::kill_process(pid, rustix::process::Signal::KILL).ok();
            }
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    let daemon_bin = Path::new(env!("CARGO_BIN_EXE_krilld"));
    let dir = std::env::temp_dir().join(format!("mk-mobile-rpc-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let socket = dir.join("run/krilld.sock");
    let holder = Command::new(daemon_bin.with_file_name("krill-holder"))
        .arg(daemon_bin)
        .arg(&socket)
        .env("KRILL_STATE_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut fixture = Fixture {
        holder,
        daemon_pid: None,
        dir: dir.clone(),
    };
    (|| -> Result<(), Box<dyn std::error::Error>> {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut connection = loop {
            if let Ok(connection) = UnixStream::connect(&socket) {
                break connection;
            }
            assert!(Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        };
        connection.set_read_timeout(Some(Duration::from_secs(20)))?;
        let (kind, body) = ipc::read_frame(&mut connection)?.ok_or("missing hello")?;
        let hello = ServerHello::decode(kind, &body)?;
        fixture.daemon_pid = rustix::process::Pid::from_raw(i32::try_from(hello.instance_id)?);
        connection.write_all(&client_hello(ClientRole::Cli, "mobile-rpc-test").encode()?)?;
        let mut control = |request: Request| -> Result<Reply, Box<dyn std::error::Error>> {
            connection.write_all(&request.encode()?)?;
            let (kind, body) = ipc::read_frame(&mut connection)?.ok_or("missing reply")?;
            Ok(Reply::decode(kind, &body)?)
        };
        let Reply::Created {
            session: source, ..
        } = control(Request::Create(Create {
            program: Program::Custom {
                program: b"/bin/sh".to_vec(),
                args: vec![
                    b"-c".to_vec(),
                    "printf '\\033]0;모바일 세션 제목\\007'; exec sleep 60"
                        .as_bytes()
                        .to_vec(),
                ],
            },
            cwd: dir.as_os_str().as_encoded_bytes().to_vec(),
            cols: 80,
            rows: 24,
            account: None,
        }))?
        else {
            return Err("source creation failed".into());
        };
        krill_proto::git::record_session_auto_title(&dir, source, "fallback-task-title")?;
        let mut connection = UnixStream::connect(&socket)?;
        connection.set_read_timeout(Some(Duration::from_secs(20)))?;
        ipc::read_frame(&mut connection)?.ok_or("missing workflow hello")?;
        connection.write_all(&client_hello(ClientRole::Cli, "mobile-workflow-test").encode()?)?;
        let mut rpc = |request: Value| -> Result<Value, Box<dyn std::error::Error>> {
            connection.write_all(&MobileRequest::Workflow(request.to_string()).encode()?)?;
            let (kind, body) = ipc::read_frame(&mut connection)?.ok_or("missing workflow reply")?;
            let MobileReply::Workflow(response) = MobileReply::decode(kind, &body)? else {
                return Err("wrong workflow reply".into());
            };
            Ok(serde_json::from_str(&response)?)
        };
        let snapshot = loop {
            let snapshot = rpc(json!({"action":"snapshot"}))?;
            if snapshot["data"]["sessions"]
                .as_array()
                .is_some_and(|cards| {
                    cards.iter().any(|card| {
                        card["id"] == source.0
                            && card["cwd"].as_str().is_some_and(|cwd| !cwd.is_empty())
                            && card["title"] == "모바일 세션 제목"
                    })
                })
            {
                break snapshot;
            }
            assert!(Instant::now() < deadline, "source cwd unavailable");
            std::thread::sleep(Duration::from_millis(50));
        };
        let card = &snapshot["data"]["sessions"][0];
        assert!(snapshot["data"]["repositories"].is_array());
        assert_eq!(snapshot["data"]["truncated"], false);
        for capability in ["snapshot_groups", "session_create", "session_close"] {
            assert_eq!(snapshot["data"]["capabilities"][capability], true);
        }
        assert!(snapshot["data"]["host"]["display_name"].is_string());
        assert_eq!(snapshot["data"]["host"]["platform"], std::env::consts::OS);
        assert_eq!(card["state"], "Unknown");
        assert_eq!(card["state_source"], Value::Null);
        assert_eq!(card["state_confidence"], "Low");
        assert_eq!(card["state_since_unix_ns"], "0");
        assert_eq!(card["title"], "모바일 세션 제목");
        assert!(card.get("foreground").is_some());
        assert!(card["worktree_key"].is_string());
        // 저장소 밖 cwd는 git을 부르지 않고 요약을 비운다.
        assert_eq!(card["repo_key"], Value::Null);
        for field in ["branch", "added", "deleted"] {
            assert_eq!(card[field], Value::Null, "{field}");
        }
        assert!(card.get("state_source").is_some());
        assert!(card.get("state_confidence").is_some());
        let source_cwd = card["cwd"].clone();
        // 등록 목록은 세션 유무와 독립적이며 데스크톱에서 바꾼 이름을 매 폴링에 반영한다.
        let empty = dir.join("empty-workspace");
        std::fs::create_dir_all(&empty)?;
        std::fs::write(
            dir.join("projects.toml"),
            format!(
                "[[projects]]\nid = \"empty\"\npath = {:?}\ndisplay_name = \"이관된 이름\"\npinned = true\n",
                empty.to_string_lossy()
            ),
        )?;
        let mut prefs = std::collections::BTreeMap::new();
        prefs.insert(
            std::fs::canonicalize(&empty)?,
            (false, "PC에서 바꾼 이름".to_owned()),
        );
        let mut sidebar = vec![1];
        sidebar.extend(postcard::to_allocvec(&prefs)?);
        std::fs::write(dir.join("sidebar.bin"), sidebar)?;
        let snapshot = rpc(json!({"action":"snapshot"}))?;
        let workspace = snapshot["data"]["workspaces"]
            .as_array()
            .ok_or("missing workspaces")?
            .iter()
            .find(|w| w["label"] == "PC에서 바꾼 이름")
            .ok_or("empty workspace omitted")?;
        assert_eq!(workspace["pinned"], false);
        let key = workspace["key"].as_str().ok_or("missing workspace key")?;
        assert_eq!(rpc(json!({"action":"create_terminal"}))?["ok"], false);
        assert_eq!(
            rpc(json!({"action":"create_terminal","workspace":"not-registered"}))?["ok"],
            false
        );
        assert_eq!(
            rpc(json!({"action":"create_terminal","workspace":key,"cwd_session":source.0}))?["ok"],
            false
        );
        assert_eq!(
            rpc(json!({"action":"create_terminal","workspace":key,"cwd":"/"}))?["ok"],
            false
        );
        let created_empty = rpc(json!({"action":"create_terminal","workspace":key}))?;
        assert_eq!(created_empty["ok"], true, "{created_empty}");
        let empty_session = created_empty["data"]["session"]
            .as_u64()
            .ok_or("missing empty workspace session")?;
        loop {
            let snapshot = rpc(json!({"action":"snapshot"}))?;
            if snapshot["data"]["sessions"]
                .as_array()
                .is_some_and(|cards| {
                    cards
                        .iter()
                        .any(|c| c["id"] == empty_session && c["worktree_key"] == key)
                })
            {
                break;
            }
            assert!(Instant::now() < deadline, "workspace session cwd differs");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            rpc(json!({"action":"close_terminal","session":empty_session}))?["ok"],
            true
        );

        let created = rpc(json!({"action":"create_terminal","cwd_session":source.0}))?;
        assert_eq!(created["ok"], true, "{created}");
        let session = created["data"]["session"]
            .as_u64()
            .ok_or("missing new session")?;
        assert_ne!(session, source.0);
        krill_proto::git::record_session_auto_title(
            &dir,
            krill_proto::SessionId(session),
            "new-shell-task",
        )?;
        let snapshot = loop {
            let snapshot = rpc(json!({"action":"snapshot"}))?;
            if snapshot["data"]["sessions"]
                .as_array()
                .is_some_and(|cards| {
                    cards.iter().any(|card| {
                        card["id"] == session
                            && card["cwd"] == source_cwd
                            && card["title"] == "new-shell-task"
                    })
                })
            {
                break snapshot;
            }
            assert!(Instant::now() < deadline, "new login shell cwd differs");
            std::thread::sleep(Duration::from_millis(50));
        };
        std::fs::create_dir_all("/tmp/mk-mobile")?;
        std::fs::write(
            "/tmp/mk-mobile/snapshot-sample.json",
            serde_json::to_vec_pretty(&snapshot["data"])?,
        )?;
        assert_eq!(
            rpc(json!({"action":"create_terminal","cwd_session":u64::MAX}))?["ok"],
            false
        );
        assert_eq!(
            rpc(json!({"action":"create_terminal","cwd_session":source.0,"cwd":"/"}))?["ok"],
            false
        );
        let closed = rpc(json!({"action":"close_terminal","session":session}))?;
        assert_eq!(closed, json!({"ok":true,"data":{"closed":true}}));
        assert_eq!(
            rpc(json!({"action":"close_terminal","session":u64::MAX}))?["ok"],
            false
        );
        loop {
            let snapshot = rpc(json!({"action":"snapshot"}))?;
            let cards = snapshot["data"]["sessions"]
                .as_array()
                .ok_or("missing sessions")?;
            assert!(
                cards
                    .iter()
                    .any(|card| card["id"] == source.0 && card["exited"] == false)
            );
            if cards.iter().all(|card| card["id"] != session) {
                break;
            }
            assert!(Instant::now() < deadline, "closed session remains listed");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            rpc(json!({"action":"close_terminal","session":session}))?["ok"],
            false
        );
        assert_eq!(
            rpc(json!({"action":"close_terminal","session":source.0}))?["ok"],
            true
        );
        Ok(())
    })()
}

#[test]
fn session_screen_regressions() {
    let result = std::process::Command::new("node")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/mobile_session.cjs"
        ))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn terminal_adapter_regressions() {
    let result = std::process::Command::new("node")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/mobile_terminal.cjs"
        ))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// 모바일 화면·채팅·전송 실패 경로를 표준 Cargo 검사에도 포함한다.
#[test]
fn mobile_frontend_regressions() {
    for name in [
        "mobile_screens.cjs",
        "mobile_chat.cjs",
        "mobile_transport.cjs",
        "mobile_feed.cjs",
        "mobile_updates.cjs",
    ] {
        let result = std::process::Command::new("node")
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests")
                    .join(name),
            )
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{name}: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
