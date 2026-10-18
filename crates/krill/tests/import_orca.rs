//! `krill import orca` CLI 통합 테스트 (MK-172 / OWE-726).
//!
//! - `--dry-run --json` 출력 스키마(OWE-727 GUI 위저드 연동 계약) 검증
//! - 데몬 미기동 시 `accounts` 도메인 `'데몬 필요'` 건너뛰기 + 로컬 `accounts.toml` 미접촉 + 나머지 도메인 정상 이관 검증
//! - 데몬(`run/krilld.sock`) 연결 시 `ListAccounts` -> `AddAccount` -> `SwitchAccount` IPC 호출 및 재실행 멱등성 검증

#![cfg_attr(unix, expect(clippy::unwrap_used, reason = "integration test helpers"))]
#![cfg(unix)]

use std::fs;
use std::io::Write as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use krill_proto::ipc::{self, ClientHello, ServerHello};
use krill_proto::orchestration::WorktreeStore;
use krill_proto::schedule::Schedules;
use krill_proto::screen::Msg as _;
use krill_proto::usage::{
    AccountInfo, AccountsSnapshot, FEAT_ACCOUNT_CONNECTION, FEAT_ACCOUNT_POLICY, FEAT_USAGE,
    FEAT_USAGE_AGY, FEAT_USAGE_MANAGE, FEAT_USAGE_RATELIMIT, FEAT_USAGE_RULES, FEAT_USAGE_STATUS,
    Snapshot, UsageReply, UsageRequest,
};

fn create_sqlite_db(db_path: &Path, rows: &[(&str, &str)]) {
    use std::fmt::Write as _;
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut sql = String::from(
        "CREATE TABLE profile_state_documents (domain TEXT PRIMARY KEY, payload TEXT NOT NULL, updated_at INTEGER NOT NULL);\n",
    );
    for (domain, payload) in rows {
        let escaped = payload.replace('\'', "''");
        writeln!(
            sql,
            "INSERT INTO profile_state_documents (domain, payload, updated_at) VALUES ('{domain}', '{escaped}', 1700000000);"
        )
        .unwrap();
    }
    let status = Command::new("/usr/bin/sqlite3")
        .arg(db_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(sql.as_bytes())?;
            }
            child.wait()
        })
        .unwrap();
    assert!(status.success());
}

fn spawn_fake_krilld(
    state_dir: &Path,
) -> (Arc<AtomicBool>, std::thread::JoinHandle<Vec<UsageRequest>>) {
    let run_dir = state_dir.join("run");
    fs::create_dir_all(&run_dir).unwrap();
    let sock_path = run_dir.join("krilld.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);

    let handle = std::thread::spawn(move || {
        let mut accounts: Vec<AccountInfo> = Vec::new();
        let mut recorded: Vec<UsageRequest> = Vec::new();
        while !stop_clone.load(Ordering::Relaxed) {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            if stop_clone.load(Ordering::Relaxed) {
                break;
            }
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let hello = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: ipc::FEATURES
                    | FEAT_USAGE
                    | FEAT_USAGE_RATELIMIT
                    | FEAT_USAGE_AGY
                    | FEAT_USAGE_RULES
                    | FEAT_USAGE_STATUS
                    | FEAT_USAGE_MANAGE
                    | FEAT_ACCOUNT_POLICY
                    | FEAT_ACCOUNT_CONNECTION,
                build: krill_proto::BUILD.to_owned(),
                instance_id: 1,
            };
            if stream.write_all(&hello.encode().unwrap()).is_err() {
                continue;
            }
            let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) else {
                continue;
            };
            if ClientHello::decode(kind, &body).is_err() {
                continue;
            }
            let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) else {
                continue;
            };
            let Ok(req) = UsageRequest::decode(kind, &body) else {
                continue;
            };
            recorded.push(req.clone());
            match req {
                UsageRequest::AddAccount {
                    provider,
                    label,
                    email,
                    profile_dir,
                } => {
                    let id = format!(
                        "{}-{}",
                        provider.as_str(),
                        accounts
                            .iter()
                            .filter(|a| a.provider == provider)
                            .count()
                            .saturating_add(1)
                    );
                    if !accounts
                        .iter()
                        .any(|a| a.provider == provider && a.profile_dir == profile_dir)
                    {
                        let first = !accounts.iter().any(|a| a.provider == provider);
                        accounts.push(AccountInfo {
                            id,
                            provider,
                            label,
                            email,
                            profile_dir,
                            active: first,
                            host_default: false,
                            stale: false,
                            usage: Snapshot::default(),
                        });
                    }
                }
                UsageRequest::SwitchAccount {
                    provider,
                    account_id,
                } => {
                    for acc in accounts.iter_mut().filter(|a| a.provider == provider) {
                        acc.active = acc.id == account_id;
                    }
                }
                _ => {}
            }
            let reply = UsageReply::Accounts(AccountsSnapshot {
                accounts: accounts.clone(),
                ..AccountsSnapshot::default()
            });
            stream.write_all(&reply.encode().unwrap()).ok();
        }
        recorded
    });

    (stop, handle)
}

#[test]
fn cli_import_orca_dry_run_daemon_skip_and_daemon_ipc_apply() {
    let root = std::env::temp_dir().join(format!("owe726-cli-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state_dir = root.join("state");
    let orca_dir = root.join("orca");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&orca_dir).unwrap();

    fs::write(
        orca_dir.join("orca-profile-index.json"),
        r#"{"version":1,"activeProfileId":"default","profiles":[{"id":"default","name":"Default"}]}"#,
    )
    .unwrap();

    let claude_id = "c1111111-2222-3333-4444-555555555555";
    let codex_id = "x1111111-2222-3333-4444-555555555555";
    let claude_home = orca_dir
        .join("claude-profiles")
        .join(claude_id)
        .join("home");
    let codex_home = orca_dir.join("codex-accounts").join(codex_id).join("home");
    fs::create_dir_all(&claude_home).unwrap();
    fs::create_dir_all(&codex_home).unwrap();

    let profile_dir = orca_dir.join("profiles").join("default");
    let settings_json = serde_json::json!({
        "defaultTuiAgent": "claude",
        "terminalFontSize": 14,
        "terminalFontFamily": "Menlo",
        "terminalCursorStyle": "underline",
        "terminalCursorBlink": false,
        "claudeManagedAccounts": [
            {
                "id": claude_id,
                "email": "dev@example.com",
                "organizationName": "Dev Org",
                "managedAuthPath": claude_home.display().to_string()
            }
        ],
        "activeClaudeManagedAccountId": claude_id,
        "codexManagedAccounts": [
            {
                "id": codex_id,
                "email": "codex-dev@example.com",
                "workspaceLabel": "Dev Team",
                "managedHomePath": codex_home.display().to_string()
            }
        ],
        "activeCodexManagedAccountId": codex_id,
        "terminalQuickCommands": [
            {"id": "q1", "label": "Test All", "command": "cargo nextest run"}
        ],
        "agentDefaultArgs": {"claude": ["--verbose"]},
        "agentDefaultEnv": {"claude": {"CLAUDE_CODE_EXPERIMENTAL": "1"}}
    })
    .to_string();

    let ssh_json = serde_json::json!([
        {
            "id": "prod-srv",
            "label": "Prod Server",
            "host": "prod.example.com",
            "username": "deploy",
            "port": 2200,
            "identityFile": "~/.ssh/id_ed25519",
            "source": "manual"
        }
    ])
    .to_string();

    let repos_json = serde_json::json!([
        {
            "id": "r1",
            "path": "/Users/dev/project-alpha",
            "displayName": "Project Alpha",
            "badgeColor": "#3b82f6",
            "kind": "git",
            "hookSettings": {
                "mode": "override",
                "scripts": {"setup": "npm ci"}
            }
        }
    ])
    .to_string();

    let wt_meta_json = serde_json::json!({
        "r1::/Users/dev/project-alpha-wt": {
            "displayName": "Alpha WT",
            "comment": "Fixing login flow",
            "linkedPR": 42,
            "linkedLinearIssue": "OWE-726",
            "isUnread": true,
            "isPinned": true,
            "sortOrder": 2,
            "workspaceStatus": "in-review"
        }
    })
    .to_string();

    let automations_json = serde_json::json!([
        {
            "id": "auto-daily",
            "name": "Daily Report",
            "prompt": "Summarize open PRs",
            "agentId": "claude",
            "rrule": "FREQ=DAILY;BYHOUR=8;BYMINUTE=15",
            "enabled": true,
            "projectId": "r1"
        }
    ])
    .to_string();

    create_sqlite_db(
        &profile_dir.join("profile-state.db"),
        &[
            ("settings", &settings_json),
            ("sshTargets", &ssh_json),
            ("repos", &repos_json),
            ("worktreeMeta", &wt_meta_json),
            ("automations", &automations_json),
        ],
    );

    let bin = env!("CARGO_BIN_EXE_krill");

    // 1. `--dry-run --json`: works even without krilld and writes no files
    let dry_out = Command::new(bin)
        .args(["import", "orca", "--dry-run", "--json", "--from"])
        .arg(&orca_dir)
        .env("HOME", &home)
        .env("KRILL_STATE_DIR", &state_dir)
        .env("KRILL_STATE_DIR", &state_dir)
        .env("KRILL_TEST_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        dry_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&dry_out.stderr)
    );
    let dry_report: serde_json::Value = serde_json::from_slice(&dry_out.stdout).unwrap();
    assert_eq!(dry_report["dry_run"], true);
    assert_eq!(dry_report["accounts"]["skipped"], false);
    assert_eq!(dry_report["accounts"]["claude_added"], 1);
    assert_eq!(dry_report["accounts"]["codex_added"], 1);
    assert_eq!(dry_report["ssh"]["hosts_added"], 1);
    assert_eq!(dry_report["projects"]["projects_added"], 1);
    assert_eq!(dry_report["projects"]["worktrees_migrated"], 1);
    assert_eq!(dry_report["settings"]["settings_updated"], true);
    assert!(!state_dir.join("accounts.toml").exists());

    // 2. Without running krilld: `accounts` domain is skipped with `'데몬 필요'`, local `accounts.toml`
    // is never touched, no daemon is spawned, and other selected domains (`ssh`) succeed.
    let sentinel_accounts = "# existing\nprovisioning = true\n";
    fs::write(state_dir.join("accounts.toml"), sentinel_accounts).unwrap();

    let no_daemon_out = Command::new(bin)
        .args([
            "import",
            "orca",
            "--only",
            "accounts,ssh",
            "--json",
            "--from",
        ])
        .arg(&orca_dir)
        .env("HOME", &home)
        .env("KRILL_STATE_DIR", &state_dir)
        .env("KRILL_STATE_DIR", &state_dir)
        .env("KRILL_TEST_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(no_daemon_out.status.success());
    let no_daemon_report: serde_json::Value =
        serde_json::from_slice(&no_daemon_out.stdout).unwrap();
    assert_eq!(no_daemon_report["dry_run"], false);
    assert_eq!(no_daemon_report["accounts"]["skipped"], true);
    assert!(
        no_daemon_report["accounts"]["skipped_reason"]
            .as_str()
            .is_some_and(|s| s.starts_with("데몬 필요"))
    );
    assert_eq!(no_daemon_report["accounts"]["claude_added"], 0);
    assert_eq!(no_daemon_report["accounts"]["codex_added"], 0);
    assert_eq!(no_daemon_report["ssh"]["skipped"], false);
    assert_eq!(no_daemon_report["ssh"]["hosts_added"], 1);
    assert_eq!(
        fs::read_to_string(state_dir.join("accounts.toml")).unwrap(),
        sentinel_accounts,
        "orca_import must never directly write accounts.toml"
    );
    assert!(
        !state_dir.join("run/krilld.sock.launch").exists(),
        "orca_import must not spawn krilld when absent"
    );
    assert!(state_dir.join("hosts.toml").exists());
    assert!(!state_dir.join("projects.toml").exists());

    // 3. Start fake krilld on `run/krilld.sock` and run full import (human output)
    let (stop, server_handle) = spawn_fake_krilld(&state_dir);

    let full_out = Command::new(bin)
        .args(["import", "orca", "--from"])
        .arg(&orca_dir)
        .env("HOME", &home)
        .env("KRILL_STATE_DIR", &state_dir)
        .env("KRILL_STATE_DIR", &state_dir)
        .env("KRILL_TEST_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(full_out.status.success());
    let stdout_str = String::from_utf8_lossy(&full_out.stdout);
    assert!(stdout_str.contains("Orca import [APPLIED]"));
    assert!(stdout_str.contains("accounts: claude +1 (~0), codex +1 (~0)"));

    // Verify files created in `state_dir` for projects, worktrees, and schedules
    let proj_raw = fs::read_to_string(state_dir.join("projects.toml")).unwrap();
    let proj_toml: toml::Value = toml::from_str(&proj_raw).unwrap();
    let projects = proj_toml["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["display_name"].as_str(), Some("Project Alpha"));
    assert_eq!(projects[0]["setup_script"].as_str(), Some("npm ci"));

    let wt_store = WorktreeStore::load(&state_dir).unwrap();
    let cp = wt_store.checkpoint("/Users/dev/project-alpha-wt");
    assert_eq!(cp.comment.as_deref(), Some("Fixing login flow"));
    assert_eq!(cp.workspace_status.as_deref(), Some("in-review"));
    assert_eq!(cp.pr, Some(42));
    assert_eq!(cp.issue.as_deref(), Some("OWE-726"));
    assert!(cp.unread);

    let schedules = Schedules::load(&state_dir).unwrap();
    assert_eq!(schedules.schedule.len(), 1);
    assert_eq!(schedules.schedule[0].cron, "15 8 * * *");

    // 4. Re-run full import with `--json` to verify 0 duplicate additions
    let rerun_out = Command::new(bin)
        .args(["import", "orca", "--json", "--from"])
        .arg(&orca_dir)
        .env("HOME", &home)
        .env("KRILL_STATE_DIR", &state_dir)
        .env("KRILL_STATE_DIR", &state_dir)
        .env("KRILL_TEST_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(rerun_out.status.success());
    let rerun_report: serde_json::Value = serde_json::from_slice(&rerun_out.stdout).unwrap();
    assert_eq!(rerun_report["accounts"]["skipped"], false);
    assert_eq!(rerun_report["accounts"]["claude_added"], 0);
    assert_eq!(rerun_report["accounts"]["codex_added"], 0);
    assert_eq!(rerun_report["ssh"]["hosts_added"], 0);
    assert_eq!(rerun_report["projects"]["projects_added"], 0);
    assert_eq!(rerun_report["settings"]["settings_updated"], false);
    assert_eq!(rerun_report["settings"]["schedules_merged"], 0);

    stop.store(true, Ordering::Relaxed);
    UnixStream::connect(state_dir.join("run/krilld.sock")).ok();
    let requests = server_handle.join().unwrap();
    assert!(
        requests
            .iter()
            .any(|r| matches!(r, UsageRequest::ListAccounts))
    );
    assert!(
        requests
            .iter()
            .any(|r| matches!(r, UsageRequest::AddAccount { .. }))
    );

    fs::remove_dir_all(&root).ok();
}
