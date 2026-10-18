//! OWE-722: 실제 CLI와 가짜 IPC 데몬으로 프로비저닝 제어를 검증한다.
#![expect(clippy::unwrap_used, reason = "통합 테스트 헬퍼")]

use krill_proto::os::net::UnixListener;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use krill_proto::ipc::{self, ClientHello, ServerHello};
use krill_proto::screen::Msg as _;
use krill_proto::usage::{FEAT_USAGE, FEAT_USAGE_MANAGE, UsageReply, UsageRequest};

const KRILL: &str = env!("CARGO_BIN_EXE_krill");

fn root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("owe722-{}-{tag}", std::process::id()));
    fs::create_dir_all(root.join("run")).unwrap();
    root
}

fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(KRILL)
        .args(["account", "provisioning"])
        .args(args)
        .env("HOME", root.join("home"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude"))
        .env("CODEX_HOME", root.join("codex"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("xdg-state"))
        .env("KRILL_STATE_DIR", root)
        .output()
        .unwrap()
}

fn fake(
    root: &Path,
    features: u64,
    expected: Option<UsageRequest>,
    reply: UsageReply,
) -> std::thread::JoinHandle<()> {
    let listener = UnixListener::bind(root.join("run/krilld.sock")).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(
                &ServerHello {
                    proto_min: ipc::PROTO_MIN,
                    proto_max: ipc::PROTO_MAX,
                    features,
                    build: "fake".into(),
                    instance_id: 1,
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
        ClientHello::decode(kind, &body).unwrap();
        let frame = ipc::read_frame(&mut stream).unwrap();
        if let Some(expected) = expected {
            let (kind, body) = frame.unwrap();
            assert_eq!(UsageRequest::decode(kind, &body).unwrap(), expected);
            stream.write_all(&reply.encode().unwrap()).unwrap();
        } else {
            assert!(frame.is_none(), "미지원 피어에 요청을 보내면 안 된다");
        }
    })
}

#[test]
fn provisioning_on_off_status_and_json_use_daemon() {
    for (tag, args, request, enabled) in [
        ("on", vec!["on"], UsageRequest::SetProvisioning(true), true),
        (
            "off",
            vec!["off"],
            UsageRequest::SetProvisioning(false),
            false,
        ),
        (
            "status",
            vec!["status"],
            UsageRequest::GetProvisioning,
            false,
        ),
        (
            "on-json",
            vec!["on", "--json"],
            UsageRequest::SetProvisioning(true),
            true,
        ),
        (
            "off-json",
            vec!["off", "--json"],
            UsageRequest::SetProvisioning(false),
            false,
        ),
        (
            "status-json",
            vec!["status", "--json"],
            UsageRequest::GetProvisioning,
            true,
        ),
    ] {
        let root = root(tag);
        let server = fake(
            &root,
            FEAT_USAGE | FEAT_USAGE_MANAGE,
            Some(request),
            UsageReply::Provisioning(enabled),
        );
        let output = cli(&root, &args);
        server.join().unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        if args.contains(&"--json") {
            let json: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(json["provisioning"], enabled);
        } else {
            assert_eq!(
                text.trim(),
                if enabled {
                    "provisioning: on"
                } else {
                    "provisioning: off"
                }
            );
        }
        assert!(!root.join("accounts.toml").exists());
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn provisioning_refuses_old_peer_without_sending_request() {
    for action in ["on", "off", "status"] {
        let root = root(&format!("legacy-{action}"));
        let server = fake(&root, FEAT_USAGE, None, UsageReply::Provisioning(false));
        let output = cli(&root, &[action, "--json"]);
        server.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("does not support"));
        assert!(!root.join("accounts.toml").exists());
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn provisioning_without_daemon_errors_and_never_changes_local_file() {
    for action in ["on", "off", "status"] {
        let root = root(&format!("absent-{action}"));
        let path = root.join("accounts.toml");
        let raw = "# preserved\nprovisioning = true\nfuture = 'keep'\n";
        fs::write(&path, raw).unwrap();
        let output = cli(&root, &[action]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(fs::read_to_string(&path).unwrap(), raw);
        assert!(
            !root.join("run/krilld.sock.launch").exists(),
            "없는 데몬을 기동하면 안 된다"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn provisioning_reports_daemon_failure_and_rejects_bad_arguments() {
    let root = root("failure");
    let server = fake(
        &root,
        FEAT_USAGE | FEAT_USAGE_MANAGE,
        Some(UsageRequest::SetProvisioning(true)),
        UsageReply::Failed(krill_proto::control::Failure::Denied),
    );
    let output = cli(&root, &["on", "--json"]);
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty());
    for args in [
        vec![],
        vec!["maybe"],
        vec!["status", "extra"],
        vec!["on", "--json", "extra"],
    ] {
        assert_eq!(cli(&root, &args).status.code(), Some(2));
    }
    fs::remove_dir_all(root).unwrap();
}
