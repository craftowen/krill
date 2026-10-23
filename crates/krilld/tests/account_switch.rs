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
    let reply = usage_reply(socket, request);
    let UsageReply::Accounts(snapshot) = reply else {
        panic!("account request failed: {reply:?}");
    };
    snapshot
}

fn usage_reply(socket: &Path, request: &UsageRequest) -> UsageReply {
    let (mut stream, _) = connect(socket).unwrap();
    stream.write_all(&request.encode().unwrap()).unwrap();
    let (kind, raw) = ipc::read_frame(&mut stream).unwrap().unwrap();
    UsageReply::decode(kind, &raw).unwrap()
}

#[test]
fn running_account_switch_rebinds_credentials_and_survives_daemon_sigkill() {
    switch_survives_daemon_sigkill(false, ProviderKind::Codex, 1, None);
}

#[test]
fn automatic_account_switch_rebinds_running_session_and_survives_daemon_sigkill() {
    switch_survives_daemon_sigkill(true, ProviderKind::Codex, 1, None);
}

#[test]
fn claude_switch_skips_subagent_workflows_and_survives_daemon_sigkill() {
    switch_survives_daemon_sigkill(false, ProviderKind::Claude, 1, None);
}

#[test]
fn parallel_switch_bounds_restarts_and_recovers_every_inflight_session() {
    switch_survives_daemon_sigkill(false, ProviderKind::Codex, 3, None);
}

#[test]
fn unverifiable_handoff_preserves_live_process_conversation_and_history() {
    for failure in ["unknown", "missing"] {
        switch_survives_daemon_sigkill(false, ProviderKind::Codex, 1, Some(failure));
    }
}

#[expect(clippy::panic, reason = "integration-test fixture")]
fn switch_survives_daemon_sigkill(
    automatic: bool,
    provider: ProviderKind,
    count: usize,
    failure: Option<&str>,
) {
    let name = if provider == ProviderKind::Claude {
        "claude"
    } else {
        "codex"
    };
    let root = PathBuf::from(format!(
        "/tmp/kas-it-{}-{automatic}-{name}-{count}-{}",
        std::process::id(),
        failure.unwrap_or("resume")
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
    for (dir, percent) in [(&source, 5), (&target, 10)] {
        fs::write(
            dir.join("usage_cache.json"),
            format!("{{\"five_hour\":{{\"utilization\":{percent}}}}}"),
        )
        .unwrap();
    }
    let wait_ready = target.join("wait-before-ready");
    fs::write(&wait_ready, "").unwrap();
    // Same-cwd sessions each own a distinct conversation; none may resume another's.
    let conversations: Vec<String> = (0..count)
        .map(|i| format!("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeee{i}"))
        .collect();
    let conversation = conversations[0].as_str();
    let transcript_dir = if provider == ProviderKind::Claude {
        for project in ["a-deep", "m-target", "z-deep"] {
            fs::create_dir_all(
                source.join(format!("projects/{project}/sid/subagents/workflows/wf_1")),
            )
            .unwrap();
        }
        // The old depth-first search visits a decoy before this first directory,
        // reproducing the production failure independently of filesystem ordering.
        let project = fs::read_dir(source.join("projects"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name();
        format!("projects/{}/", project.to_str().unwrap())
    } else {
        "sessions/2026/10/20/rollout-date-".into()
    };
    let transcript_of = |id: &str| format!("{transcript_dir}{id}.jsonl");
    for id in &conversations {
        let identity = if provider == ProviderKind::Claude {
            format!("{{\"sessionId\":\"{id}\"}}\n")
        } else {
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\"}}}}\n")
        };
        fs::write(source.join(transcript_of(id)), identity).unwrap();
    }
    let transcript = transcript_of(conversation);
    let exe = root.join(name);
    let script = r#"#!/bin/sh
if [ "$1" = --version ]; then printf '2.1.0 (Claude Code)\n'; exit 0; fi
if [ "$1" = auth ] && [ "$2" = status ]; then printf '{"loggedIn":true}\n'; exit 0; fi
if [ "$1" = login ] && [ "$2" = status ]; then printf 'Logged in using ChatGPT\n' >&2; exit 0; fi
printf '%s' "$CODEX_HOME" > "$CODEX_HOME/observed-profile"
printf '%s\n' "$$" >> "$CODEX_HOME/observed-starts"
printf '%s\n' "$@" >> "$CODEX_HOME/observed-args"
while [ -f "$CODEX_HOME/wait-before-ready" ]; do sleep 0.05; done
printf 'selected account ready: %s\n' "$CODEX_HOME"
while IFS= read -r line; do printf '%s\n' "$line" >> "$CODEX_HOME/observed-input"; printf 'fixture: %s\n' "$line"; done
"#;
    fs::write(&exe, script.replace("CODEX_HOME", provider.env_var())).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
    let profile = krill_proto::agent::Profile {
        name: format!("Fixture {name}"),
        launch: vec![exe.display().to_string()],
        yolo_args: vec![if provider == ProviderKind::Claude {
            "--dangerously-skip-permissions".into()
        } else {
            "--dangerously-bypass-approvals-and-sandbox".into()
        }],
        install: String::new(),
        install_windows: String::new(),
        env: std::collections::BTreeMap::new(),
    };
    let mut profiles = krill_proto::agent::set_user_profile(None, name, &profile).unwrap();
    // Release authentication probes must use only fixture executables.
    for other in ["claude", "codex", "agy", "opencode"]
        .into_iter()
        .filter(|p| *p != name)
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
        auto_switch: krilld::usage::accounts::StoredAutoSwitch {
            enabled: automatic,
            ..Default::default()
        },
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
    let mut originals = Vec::new();
    for conversation in &conversations {
        let created = control(
            &mut stream,
            &Request::Create(Create {
                account: Some(a.id.clone()),
                program: Program::Agent { id: name.into() },
                cwd: root.as_os_str().as_encoded_bytes().to_vec(),
                cols: 80,
                rows: 24,
            }),
        );
        let Reply::Created { session: old, .. } = created else {
            panic!("fixture create failed: {created:?}");
        };
        // A real provider normally reports this through its session-start hook.
        let record = state.join("agent-launches").join(format!("{}.json", old.0));
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        if failure != Some("unknown") {
            metadata["session_id"] = conversation.as_str().into();
        }
        metadata["permission"] = "manual".into();
        fs::write(&record, serde_json::to_vec(&metadata).unwrap()).unwrap();
        originals.push(old);
    }
    let old = originals[0];
    wait_for(|| {
        fs::read_to_string(source.join("observed-starts"))
            .ok()
            .filter(|s| s.lines().count() == count)
    });
    if failure == Some("missing") {
        fs::remove_file(source.join(&transcript)).unwrap();
    }
    let record = state.join("agent-launches").join(format!("{}.json", old.0));
    let original_launch = fs::read(&record).unwrap();
    let Reply::Sessions(before) = control(&mut stream, &Request::List) else {
        panic!("session list failed");
    };
    let old_pid = before.iter().find(|s| s.session == old).unwrap().pid;
    if automatic {
        fs::write(
            source.join("usage_cache.json"),
            r#"{"five_hour":{"utilization":95}}"#,
        )
        .unwrap();
        // Even the legacy usage response discards the switch event. The common account
        // evaluator must still wake the worker and resume the existing conversation.
        assert!(matches!(
            usage_reply(&socket, &UsageRequest::Get),
            UsageReply::Current(_)
        ));
    } else {
        usage(
            &socket,
            &UsageRequest::SwitchAccount {
                provider,
                account_id: b.id.clone(),
            },
        );
    }
    if let Some(failure) = failure {
        let error = wait_for(|| {
            let snapshot = usage(&socket, &UsageRequest::ListAccounts);
            let handoff = snapshot.details.handoffs.iter().find(|h| h.from == old)?;
            assert!(handoff.to.is_none());
            handoff.error.clone()
        });
        assert!(
            error.contains("original session preserved"),
            "{failure}: {error}"
        );
        assert!(error.contains("before retrying"), "{failure}: {error}");
        assert_eq!(fs::read(&record).unwrap(), original_launch);
        assert!(!target.join("observed-profile").exists());
        assert!(!state.join("account-replacement.json").exists());
        assert_eq!(
            fs::read_dir(state.join("account-replacements")).map_or(0, Iterator::count),
            0
        );
        let Reply::Sessions(after) = control(&mut stream, &Request::List) else {
            panic!("session list failed");
        };
        assert!(
            after
                .iter()
                .any(|s| s.session == old && s.pid == old_pid && s.status.is_none())
        );
        assert_eq!(after.len(), before.len());
        assert_eq!(
            control(
                &mut stream,
                &Request::SendInput {
                    session: old,
                    bytes: b"original-still-responsive".to_vec(),
                    submit: true,
                    wait_submit: false
                }
            ),
            Reply::InputSent
        );
        wait_for(|| {
            let Reply::Captured { bytes, .. } = control(
                &mut stream,
                &Request::Capture {
                    session: old,
                    format: CaptureFormat::Plain,
                },
            ) else {
                return None;
            };
            let text = String::from_utf8_lossy(&bytes);
            (text.contains("selected account ready")
                && text.contains("fixture: original-still-responsive"))
            .then_some(())
        });
        assert_eq!(
            control(&mut stream, &Request::Close { session: old }),
            Reply::Closed
        );
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
        return;
    }
    // Crash after the new process starts but before it becomes ready or commits its alias.
    wait_for(|| fs::read_to_string(target.join("observed-profile")).ok());
    let inflight = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(2)
        .min(count);
    wait_for(|| {
        fs::read_to_string(target.join("observed-starts"))
            .ok()
            .filter(|s| s.lines().count() == inflight)
    });
    if count > 1 {
        std::thread::sleep(Duration::from_millis(900));
        assert_eq!(
            fs::read_to_string(target.join("observed-starts"))
                .unwrap()
                .lines()
                .count(),
            inflight,
            "blocked restarts must never exceed the worker limit"
        );
    }
    let abandoned: Vec<_> = fs::read_dir(state.join("account-replacements"))
        .unwrap()
        .map(|entry| {
            let prepared: serde_json::Value =
                serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap();
            prepared["to"].as_u64().unwrap()
        })
        .collect();
    assert_eq!(
        abandoned.len(),
        inflight,
        "each inflight session needs a separate recovery journal"
    );
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
    assert!(
        !abandoned.contains(&new.0),
        "retry must retire the uncommitted process"
    );
    let replacements = wait_for(|| {
        let snapshot = usage(&socket, &UsageRequest::ListAccounts);
        let mut replacements = Vec::new();
        for original in &originals {
            let h = snapshot
                .details
                .handoffs
                .iter()
                .find(|h| h.from == *original)?;
            assert!(h.error.is_none(), "{:?}", h.error);
            replacements.push(h.to?);
        }
        Some(replacements)
    });
    assert!(replacements.iter().all(|s| !abandoned.contains(&s.0)));
    assert_eq!(
        fs::read_to_string(target.join("observed-profile")).unwrap(),
        target.to_str().unwrap()
    );
    let observed_args = fs::read_to_string(target.join("observed-args")).unwrap();
    // `replacements` follows `originals`, which follows `conversations`.
    for (conversation, replacement) in conversations.iter().zip(&replacements) {
        let record = state
            .join("agent-launches")
            .join(format!("{}.json", replacement.0));
        let metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(record).unwrap()).unwrap();
        assert_eq!(metadata["session_id"].as_str(), Some(conversation.as_str()));
        let resume_arg = if provider == ProviderKind::Claude {
            format!("--resume={conversation}")
        } else {
            conversation.clone()
        };
        assert!(observed_args.lines().any(|arg| arg == resume_arg));
        assert_eq!(
            fs::read(source.join(transcript_of(conversation))).unwrap(),
            fs::read(target.join(transcript_of(conversation))).unwrap()
        );
    }
    assert_eq!(
        fs::read_to_string(source.join("auth.json")).unwrap(),
        "source-fixture-only"
    );
    assert_eq!(
        fs::read_to_string(target.join("auth.json")).unwrap(),
        "target-fixture-only"
    );
    let continued = wait_for(|| {
        fs::read_to_string(target.join("observed-input"))
            .ok()
            .filter(|s| s.lines().count() == count)
    });
    assert!(continued.contains("Continue the existing task from where you left off"));
    assert_eq!(continued.lines().count(), count);
    let args = fs::read_to_string(target.join("observed-args")).unwrap();
    assert!(args.contains(if provider == ProviderKind::Claude {
        "--dangerously-skip-permissions"
    } else {
        "--dangerously-bypass-approvals-and-sandbox"
    }));
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
        fs::read_to_string(target.join("observed-input")).unwrap(),
        continued,
        "a later daemon restart must not submit the continuation twice"
    );
    for session in replacements {
        assert_eq!(
            control(&mut stream, &Request::Close { session }),
            Reply::Closed
        );
    }
    drop(daemon);
    fs::remove_dir_all(root).unwrap();
}
