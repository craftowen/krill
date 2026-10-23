//! 수동 가져오기 → 각 폴더에서 대화 재개 → 실패 재시도·중복 기동 방지의 CLI/IPC 경계.
#![cfg(unix)]

use std::fs;
use std::io::Write as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use krill_proto::SessionId;
use krill_proto::agent_history::{Action, FEAT_AGENT_HISTORY, FEAT_AI_VAULT, Response};
use krill_proto::control::{FEAT_CONTROL, Failure, Reply, Request};
use krill_proto::ipc::{self, ClientHello, ServerHello};
use krill_proto::screen::Msg as _;
use serde_json::{Value, json};

#[test]
fn cli_manual_import_resumes_current_panes_retries_failure_and_skips_duplicates() {
    let root = std::env::temp_dir().join(format!("orca-resume-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let orca = root.join("orca");
    fs::create_dir_all(state.join("run")).unwrap();
    fs::create_dir_all(&orca).unwrap();
    fs::create_dir_all(root.join("workspace")).unwrap();
    let cwd = fs::canonicalize(root.join("workspace"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let key = format!("repo::{cwd}::workspace:2de7222e-4069-4b91-bb07-3e05a9808a9f");
    let source = json!({"workspaceSession": {
        "tabsByWorktree": {(key.as_str()): [{"id":"t1", "worktreeId":key,
            "aiVaultTitle":{"agent":"codex", "sessionId":"stale"}}]},
        "sleepingAgentSessionsByPaneKey": {
            "t1:a":{"tabId":"t1", "worktreeId":key, "agent":"claude", "providerSession":{"id":"claude-current"}},
            "t1:b":{"tabId":"t1", "worktreeId":key, "agent":"codex", "providerSession":{"id":"codex-current"}}
        }
    }}).to_string();
    fs::write(orca.join("orca-data.json"), &source).unwrap();

    let import = |dry: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_krill"));
        cmd.args(["import", "orca", "--json", "--only", "sessions", "--from"])
            .arg(&orca)
            .env("KRILL_STATE_DIR", &state)
            .env("KRILL_TEST_OFFLINE", "1");
        if dry {
            cmd.arg("--dry-run");
        }
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };

    // 미리보기는 데몬에 연결하거나 재개 원장을 쓰지 않는다.
    let preview = import(true);
    assert_eq!(preview["sessions"]["items"].as_array().unwrap().len(), 2);
    assert!(!state.join("orca-sessions-imported").exists());

    let listener = UnixListener::bind(state.join("run/krilld.sock")).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_server = Arc::clone(&stop);
    let server = std::thread::spawn(move || {
        let mut targets = Vec::new();
        let mut fail_codex = true;
        while !stop_server.load(Ordering::Relaxed) {
            let (mut stream, _) = listener.accept().unwrap();
            if stop_server.load(Ordering::Relaxed) {
                break;
            }
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .write_all(
                    &ServerHello {
                        proto_min: ipc::PROTO_MIN,
                        proto_max: ipc::PROTO_MAX,
                        features: ipc::FEATURES | FEAT_CONTROL | FEAT_AGENT_HISTORY | FEAT_AI_VAULT,
                        build: krill_proto::BUILD.into(),
                        instance_id: 1,
                    }
                    .encode()
                    .unwrap(),
                )
                .unwrap();
            let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
            ClientHello::decode(kind, &body).unwrap();
            while let Some((kind, body)) = ipc::read_frame(&mut stream).unwrap() {
                let Request::AgentHistory(Action::ResumeVault(target)) =
                    Request::decode(kind, &body).unwrap()
                else {
                    unreachable!("import sends only ResumeVault");
                };
                let reply = if target.id == "codex-current" && fail_codex {
                    fail_codex = false;
                    Reply::Failed(Failure::UnknownAgent)
                } else {
                    Reply::AgentHistory(Response::Resumed {
                        session: SessionId(10),
                    })
                };
                targets.push(target);
                stream.write_all(&reply.encode().unwrap()).unwrap();
            }
        }
        targets
    });

    let first = import(false);
    let second = import(false);
    let third = import(false);
    stop.store(true, Ordering::Relaxed);
    UnixStream::connect(state.join("run/krilld.sock")).unwrap();
    let targets = server.join().unwrap();
    assert_eq!(first["sessions"]["resumed"], 1);
    assert_eq!(first["warnings"].as_array().unwrap().len(), 1);
    assert_eq!(second["sessions"]["resumed"], 1);
    assert_eq!(second["sessions"]["already_resumed"], 1);
    assert_eq!(third["sessions"]["resumed"], 0);
    assert_eq!(third["sessions"]["already_resumed"], 2);
    assert!(
        third["sessions"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["resumed"] == true)
    );
    assert_eq!(
        targets.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["claude-current", "codex-current", "codex-current"]
    );
    assert!(targets.iter().all(|target| target.cwd == cwd));
    assert_eq!(
        fs::read_to_string(state.join("orca-sessions-imported"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(
        fs::read_to_string(orca.join("orca-data.json")).unwrap(),
        source
    );
    fs::remove_dir_all(root).unwrap();
}
