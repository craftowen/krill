//! Running account changes use the selected credentials and survive a daemon crash.
//! All agents and credentials below are local fixtures; no provider or model is contacted.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "integration-test helpers outside #[test]"
)]

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use krill_proto::control::{CaptureFormat, Create, Program, Reply, Request};
use krill_proto::daemon::client_hello;
use krill_proto::ipc::{self, ClientRole, ServerHello};
use krill_proto::screen::Msg as _;
use krill_proto::usage::{AccountsSnapshot, ProviderKind, UsageReply, UsageRequest};
use rustix::process::{Pid, Signal, kill_process};

const KRILLD: &str = env!("CARGO_BIN_EXE_krilld");
const WAIT: Duration = Duration::from_secs(30);

struct Daemon {
    holder: Child,
    generations: Vec<u32>,
}
impl Drop for Daemon {
    fn drop(&mut self) {
        self.holder.kill().ok();
        self.holder.wait().ok();
        for &generation in &self.generations {
            if let Some(pid) = i32::try_from(generation).ok().and_then(Pid::from_raw) {
                kill_process(pid, Signal::KILL).ok();
            }
        }
    }
}

fn connect(socket: &Path) -> Option<(UnixStream, u32)> {
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(WAIT)).ok()?;
    stream.set_write_timeout(Some(WAIT)).ok()?;
    let (kind, raw) = ipc::read_frame(&mut stream).ok()??;
    let server = ServerHello::decode(kind, &raw).ok()?;
    stream
        .write_all(
            &client_hello(ClientRole::Cli, "account-switch-test")
                .encode()
                .ok()?,
        )
        .ok()?;
    Some((stream, u32::try_from(server.instance_id).ok()?))
}

#[track_caller]
fn wait_for<T>(mut read: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = read() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "account-switch fixture timed out"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn control(stream: &mut UnixStream, request: &Request) -> Reply {
    stream.write_all(&request.encode().unwrap()).unwrap();
    let (kind, raw) = ipc::read_frame(stream).unwrap().unwrap();
    Reply::decode(kind, &raw).unwrap()
}

#[expect(clippy::panic, reason = "integration-test fixture")]
fn usage(socket: &Path, request: &UsageRequest) -> AccountsSnapshot {
    let (mut stream, _) = connect(socket).unwrap();
    stream.write_all(&request.encode().unwrap()).unwrap();
    let (kind, raw) = ipc::read_frame(&mut stream).unwrap().unwrap();
    let reply = UsageReply::decode(kind, &raw).unwrap();
    let UsageReply::Accounts(snapshot) = reply else {
        panic!("account request failed: {reply:?}");
    };
    snapshot
}

#[test]
fn running_account_switch_rebinds_credentials_and_survives_daemon_sigkill() {
    switch_survives_daemon_sigkill(ProviderKind::Codex, false);
}

#[test]
fn claude_switch_skips_deep_subagent_trees_and_survives_daemon_sigkill() {
    switch_survives_daemon_sigkill(ProviderKind::Claude, false);
}

#[test]
fn codex_without_hooks_recovers_open_transcript_and_survives_daemon_sigkill() {
    switch_survives_daemon_sigkill(ProviderKind::Codex, true);
}

#[expect(clippy::panic, reason = "integration-test fixture")]
fn switch_survives_daemon_sigkill(provider: ProviderKind, without_hooks: bool) {
    let name = if provider == ProviderKind::Claude {
        "claude"
    } else {
        "codex"
    };
    let root = PathBuf::from(format!(
        "/tmp/kas-it-{}-{name}-{without_hooks}",
        std::process::id()
    ));
    fs::remove_dir_all(&root).ok();
    let state = root.join("state");
    let home = root.join("home");
    let source = root.join("a");
    let target = root.join("b");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(source.join("sessions/2026/10/20")).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::write(source.join("auth.json"), "source-fixture-only").unwrap();
    fs::write(target.join("auth.json"), "target-fixture-only").unwrap();
    let wait_ready = target.join("wait-before-ready");
    fs::write(&wait_ready, "").unwrap();
    let conversation = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let (transcript, identity) = if provider == ProviderKind::Claude {
        for project in ["a-deep", "m-target", "z-deep"] {
            fs::create_dir_all(
                source.join(format!("projects/{project}/sid/subagents/workflows/wf_1")),
            )
            .unwrap();
        }
        (
            format!("projects/m-target/{conversation}.jsonl"),
            format!("{{\"sessionId\":\"{conversation}\"}}\n"),
        )
    } else {
        (
            format!("sessions/2026/10/20/rollout-date-{conversation}.jsonl"),
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{conversation}\"}}}}\n"),
        )
    };
    fs::write(source.join(&transcript), identity).unwrap();
    let exe = root.join(name);
    fs::write(&exe, "#!/bin/sh\nif [ \"$1\" = login ] && [ \"$2\" = status ]; then printf 'Logged in using ChatGPT\\n' >&2; exit 0; fi\nprintf '%s' \"$CODEX_HOME\" > \"$CODEX_HOME/observed-profile\"\nprintf '%s\\n' \"$@\" > \"$CODEX_HOME/observed-args\"\nwhile [ -f \"$CODEX_HOME/wait-before-ready\" ]; do sleep 0.05; done\nprintf 'selected account ready: %s\\n' \"$CODEX_HOME\"\nwhile IFS= read -r line; do printf 'fixture: %s\\n' \"$line\"; done\n").unwrap();
    let cli = Path::new(KRILLD).parent().unwrap().join("krill");
    assert!(
        cli.is_file(),
        "build krill before account-switch integration tests"
    );
    let script = fs::read_to_string(&exe)
        .unwrap()
        .replace("CODEX_HOME", provider.env_var());
    let script = script.replacen("#!/bin/sh\n", "#!/bin/sh\nif [ \"$1\" = --version ]; then printf '2.1.0 (Claude Code)\\n'; exit 0; fi\nif [ \"$1\" = auth ] && [ \"$2\" = status ]; then printf '{\"loggedIn\":true}\\n'; exit 0; fi\n", 1);
    let signal = if without_hooks {
        String::new()
    } else {
        format!(
            "printf '%s' '{{\"hook_event_name\":\"SessionStart\",\"session_id\":\"{conversation}\"}}' | {} hook {name} >/dev/null\n",
            krill_proto::agent::hooks::shell_quote(cli.to_str().unwrap())
        )
    };
    let script = script.replace(
        "while IFS= read -r line;",
        &format!("{signal}while IFS= read -r line;"),
    );
    let script = if without_hooks {
        script.replace(
            "printf 'selected account ready:",
            &format!(
                "exec 3<\"${}/{}\"\nprintf 'selected account ready:",
                provider.env_var(),
                transcript
            ),
        )
    } else {
        script
    };
    fs::write(&exe, script).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
    let profile = krill_proto::agent::Profile {
        name: format!("Fixture {name}"),
        launch: vec![exe.display().to_string()],
        yolo_args: Vec::new(),
        install: String::new(),
        install_windows: String::new(),
        env: std::collections::BTreeMap::new(),
    };
    let mut profiles = krill_proto::agent::set_user_profile(None, name, &profile).unwrap();
    // Release authentication probes must use only fixture executables.
    for other in ["claude", "codex", "agy", "opencode"]
        .into_iter()
        .filter(|other| *other != name)
    {
        let mut unavailable = profile.clone();
        unavailable.launch = vec![root.join("uninstalled").join(other).display().to_string()];
        profiles =
            krill_proto::agent::set_user_profile(Some(&profiles), other, &unavailable).unwrap();
    }
    fs::write(state.join(krill_proto::agent::FILE_NAME), profiles).unwrap();
    let account = |id: &str, label: &str, dir: &Path| krilld::usage::accounts::StoredAccount {
        id: id.into(),
        provider: name.into(),
        label: label.into(),
        email: None,
        profile_dir: dir.display().to_string(),
    };
    let a = account(&format!("{name}-source"), "Source", &source);
    let b = account(&format!("{name}-target"), "Target", &target);
    let file = krilld::usage::accounts::AccountsFile {
        active: std::collections::BTreeMap::from([(name.into(), a.id.clone())]),
        accounts: vec![a.clone(), b.clone()],
        direct_usage: false,
        ..Default::default()
    };
    fs::write(state.join("accounts.toml"), toml::to_string(&file).unwrap()).unwrap();
    let socket = state.join("run/test.sock");
    let holder_exe = Path::new(KRILLD).parent().unwrap().join("krill-holder");
    let holder = Command::new(holder_exe)
        .arg(KRILLD)
        .arg(&socket)
        .env("HOME", &home)
        .env("KRILL_TEST_OFFLINE", "1")
        .env("KRILL_STATE_DIR", &state)
        .env("KRILL_LOG_DIR", root.join("logs"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = Daemon {
        holder,
        generations: Vec::new(),
    };
    let (mut stream, first_generation) = wait_for(|| connect(&socket));
    daemon.generations.push(first_generation);
    let created = control(
        &mut stream,
        &Request::Create(Create {
            account: Some(a.id),
            program: Program::Agent { id: name.into() },
            cwd: root.as_os_str().as_encoded_bytes().to_vec(),
            cols: 80,
            rows: 24,
        }),
    );
    let Reply::Created { session: old, .. } = created else {
        panic!("fixture create failed: {created:?}");
    };
    wait_for(|| {
        fs::read_to_string(source.join("observed-profile"))
            .ok()
            .filter(|s| s == source.to_str().unwrap())
    });
    // A real provider normally reports this through its session-start hook.
    let record = state.join("agent-launches").join(format!("{}.json", old.0));
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    if without_hooks {
        assert!(metadata.get("session_id").is_none());
    } else {
        metadata["session_id"] = conversation.into();
        fs::write(&record, serde_json::to_vec(&metadata).unwrap()).unwrap();
    }
    usage(
        &socket,
        &UsageRequest::SwitchAccount {
            provider,
            account_id: b.id.clone(),
        },
    );
    // Crash after the new process starts but before it becomes ready or commits its alias.
    wait_for(|| fs::read_to_string(target.join("observed-profile")).ok());
    let journal = fs::read_dir(state.join("account-replacements"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let prepared: serde_json::Value = serde_json::from_slice(&fs::read(journal).unwrap()).unwrap();
    let abandoned = prepared
        .get("to")
        .and_then(serde_json::Value::as_u64)
        .unwrap();
    let daemon_pid = Pid::from_raw(i32::try_from(first_generation).unwrap()).unwrap();
    kill_process(daemon_pid, Signal::KILL).unwrap();
    let (mut stream, resumed_generation) =
        wait_for(|| connect(&socket).filter(|(_, pid)| *pid != first_generation));
    daemon.generations.push(resumed_generation);
    fs::remove_file(wait_ready).unwrap();
    let new = wait_for(|| {
        let snapshot = usage(&socket, &UsageRequest::ListAccounts);
        let handoff = snapshot.details.handoffs.iter().find(|h| h.from == old)?;
        assert!(handoff.error.is_none(), "{:?}", handoff.error);
        handoff.to
    });
    assert_ne!(new, old);
    assert_ne!(
        new.0, abandoned,
        "retry must retire the uncommitted process"
    );
    assert_eq!(
        fs::read_to_string(target.join("observed-profile")).unwrap(),
        target.to_str().unwrap()
    );
    assert!(
        fs::read_to_string(target.join("observed-args"))
            .unwrap()
            .lines()
            .any(|arg| arg == conversation || arg == format!("--resume={conversation}"))
    );
    assert_eq!(
        fs::read(source.join(&transcript)).unwrap(),
        fs::read(target.join(&transcript)).unwrap()
    );
    assert_eq!(
        fs::read_to_string(source.join("auth.json")).unwrap(),
        "source-fixture-only"
    );
    let snapshot = control(&mut stream, &Request::List);
    let Reply::Sessions(sessions) = snapshot else {
        panic!("session list failed");
    };
    let new_pid = sessions.iter().find(|s| s.session == new).unwrap().pid;
    let daemon_pid = Pid::from_raw(i32::try_from(resumed_generation).unwrap()).unwrap();
    kill_process(daemon_pid, Signal::KILL).unwrap();
    let (mut stream, next_generation) =
        wait_for(|| connect(&socket).filter(|(_, pid)| *pid != resumed_generation));
    daemon.generations.push(next_generation);
    let snapshot = usage(&socket, &UsageRequest::ListAccounts);
    assert_eq!(
        snapshot
            .details
            .handoffs
            .iter()
            .find(|h| h.from == old)
            .and_then(|h| h.to),
        Some(new)
    );
    let Reply::Sessions(sessions) = control(&mut stream, &Request::List) else {
        panic!("session list failed");
    };
    assert!(
        sessions
            .iter()
            .any(|s| s.session == new && s.pid == new_pid && s.status.is_none())
    );
    let Reply::Captured { bytes, .. } = control(
        &mut stream,
        &Request::Capture {
            session: new,
            format: CaptureFormat::Plain,
        },
    ) else {
        panic!("replacement capture failed");
    };
    assert!(String::from_utf8_lossy(&bytes).contains("selected account ready"));
    assert_eq!(
        control(&mut stream, &Request::Close { session: new }),
        Reply::Closed
    );
    drop(daemon);
    fs::remove_dir_all(root).unwrap();
}
