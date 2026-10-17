use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Stdio;

use krill_proto::SessionId;
use krill_proto::control::{AgentState, Confidence, Source};
use krill_proto::hook::{self, EventId, HookEvent, Input};

use super::*;
use crate::status::Statuses;

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("mk040-'settings-{}", uuid().unwrap()));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }

    fn script(&self, name: &str, text: &str) -> PathBuf {
        let p = self.0.join(name);
        fs::write(&p, text).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        p
    }

    fn login(&self) -> Launch {
        Launch {
            argv: vec![b"/bin/zsh".to_vec(), b"-l".to_vec()],
            env: vec![
                (b"HOME".to_vec(), self.0.as_os_str().as_bytes().to_vec()),
                (b"ZDOTDIR".to_vec(), self.0.as_os_str().as_bytes().to_vec()),
                (b"PATH".to_vec(), b"/usr/bin:/bin".to_vec()),
            ],
        }
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn profile(program: &Path) -> Profile {
    Profile {
        name: "Claude fixture".into(),
        launch: vec![program.to_str().unwrap().into()],
        yolo_args: vec!["--dangerously-skip-permissions".into()],
        install: String::new(),
        env: std::collections::BTreeMap::new(),
    }
}

fn execute(launch: &Launch, stdin: &[u8]) -> std::process::Output {
    let (program, args) = launch.argv.split_first().unwrap();
    let mut child = Command::new(OsStr::from_bytes(program))
        .args(args.iter().map(|a| OsStr::from_bytes(a)))
        .env_clear()
        .envs(
            launch
                .env
                .iter()
                .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn session_settings_are_private_unique_and_do_not_touch_user_settings() {
    let tmp = Temp::new();
    let user_dir = tmp.0.join(".claude");
    fs::create_dir(&user_dir).unwrap();
    let user = user_dir.join("settings.json");
    let original = b"{\n  \"hooks\": {}, \"permissions\": {\"defaultMode\": \"manual\"}\n}\n";
    fs::write(&user, original).unwrap();
    let krill = tmp.script(
        "krill",
        "#!/bin/sh\nprintf '{}\\n'\ncat > \"$KRILL_STATE_DIR/received\"\n",
    );
    let a = Settings::create(&tmp.0, &krill, SessionId(7), Some("2.1.299")).unwrap();
    let b = Settings::create(&tmp.0, &krill, SessionId(8), Some("2.1.100")).unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(a.id.len(), 36);
    assert_eq!(a.id.chars().nth(14), Some('4'));
    assert!(matches!(a.id.chars().nth(19), Some('8' | '9' | 'a' | 'b')));
    assert_eq!(
        fs::metadata(&a.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(a.path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let settings: serde_json::Value = serde_json::from_slice(&fs::read(&a.path).unwrap()).unwrap();
    // OWE-701: hooks + statusLine 두 키다. statusLine 명령은 `krill statusline` 베어 형태다.
    assert_eq!(settings.as_object().unwrap().len(), 2);
    let status_line = settings["statusLine"]["command"].as_str().unwrap();
    // `create`가 정식화한 경로로 기록하므로 판정도 정식 경로로 한다.
    let canonical = krill.canonicalize().unwrap();
    assert_eq!(
        krill_proto::statusline::classify_command(status_line, canonical.to_str().unwrap()),
        krill_proto::statusline::OwnCommand::Bare
    );
    let statuses = Statuses::open(None);
    for (ns, event, state) in [
        (1, "PermissionRequest", AgentState::Waiting),
        (2, "Stop", AgentState::Done),
    ] {
        let payload =
            serde_json::to_vec(&serde_json::json!({"hook_event_name": event, "session_id": a.id}))
                .unwrap();
        let command = settings["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let launch = Launch {
            argv: vec![
                b"/bin/sh".to_vec(),
                b"-c".to_vec(),
                command.as_bytes().to_vec(),
            ],
            env: tmp.login().env,
        };
        let out = execute(&launch, &payload);
        assert!(out.status.success(), "{out:?}");
        assert_eq!(out.stdout, b"{}\n");
        let event = HookEvent {
            id: EventId {
                pid: 1,
                unix_ns: ns,
            },
            agent: "claude".into(),
            ppid: 1,
            input: Input::Complete,
            payload: fs::read(tmp.0.join("received")).unwrap(),
        };
        statuses.hook(&[(SessionId(1), &event)]).unwrap();
        let status = statuses.get(SessionId(1));
        assert_eq!(status.state, state);
        assert_eq!(status.source, Some(Source::Hook));
        assert_eq!(status.confidence, Confidence::High);
    }
    let id = a.id.clone();
    let path = a.path.clone();
    let launch = a
        .launch(&profile(Path::new("claude")), Permission::Yolo, tmp.login())
        .unwrap();
    assert_eq!(launch.argv.last().unwrap(), id.as_bytes());
    assert!(launch.argv.contains(&path.as_os_str().as_bytes().to_vec()));
    assert!(
        launch
            .argv
            .contains(&b"--dangerously-skip-permissions".to_vec())
    );
    assert_eq!(launch.env, tmp.login().env);
    assert!(path.exists()); // launch 뒤에도 세션 파일이 남는다.
    assert_eq!(fs::read(user).unwrap(), original);
}

#[test]
fn profile_conflicts_are_errors_and_existing_settings_are_not_overwritten() {
    let tmp = Temp::new();
    for flag in [
        "--settings",
        "--session-id=x",
        "--resume",
        "--continue",
        "-r",
        "-c",
    ] {
        let session =
            Settings::create(&tmp.0, Path::new("/usr/bin/true"), SessionId(9), None).unwrap();
        let path = session.path.clone();
        let mut profile = profile(Path::new("claude"));
        profile.launch.push(flag.into());
        assert_eq!(
            session
                .launch(&profile, Permission::Manual, tmp.login())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!path.exists());
    }
    let dir = tmp.0.join("claude-settings");
    fs::remove_dir_all(&dir).unwrap();
    fs::write(&dir, "existing").unwrap();
    assert!(Settings::create(&tmp.0, Path::new("/usr/bin/true"), SessionId(9), None).is_err());
    assert_eq!(fs::read(dir).unwrap(), b"existing");
}

#[test]
fn discard_removes_only_that_session_and_sweep_keeps_live() {
    let tmp = Temp::new();
    let krill = tmp.script("krill", "#!/bin/sh\nexit 0\n");
    let live = Settings::create(&tmp.0, &krill, SessionId(3), Some("2.1.299")).unwrap();
    let dead = Settings::create(&tmp.0, &krill, SessionId(4), Some("2.1.299")).unwrap();
    let live_path = live.path.clone();
    let dead_path = dead.path.clone();
    assert!(
        live_path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("3-")
    );
    assert!(
        dead_path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("4-")
    );
    discard(&tmp.0, SessionId(4));
    assert!(!dead_path.exists());
    assert!(live_path.exists());
    discard(&tmp.0, SessionId(4)); // 없어도 성공
    let other = Settings::create(&tmp.0, &krill, SessionId(5), Some("2.1.299")).unwrap();
    let other_path = other.path.clone();
    let legacy = tmp
        .0
        .join("claude-settings")
        .join(format!("{}.json", other.id));
    fs::write(&legacy, b"{}").unwrap();
    let stray = tmp.0.join("claude-settings").join("README");
    fs::write(&stray, b"x").unwrap();
    assert_eq!(sweep(&tmp.0, &[SessionId(3), SessionId(5)]), 0);
    assert!(live_path.exists() && other_path.exists());
    assert_eq!(sweep(&tmp.0, &[SessionId(3)]), 1);
    assert!(!other_path.exists());
    assert!(live_path.exists());
    assert!(legacy.exists()); // 구버전 `<uuid>.json`은 건드리지 않는다.
    assert!(stray.exists());
    assert_eq!(sweep(&tmp.0.join("none"), &[]), 0); // 디렉터리 없음도 0(공용 TMPDIR은 건드리지 않는다).
}

/// 구버전 `<uuid>.json`의 첫 덩어리가 전부 숫자여도 세션 파일로 보지 않는다(OWE-529).
#[test]
fn legacy_uuid_with_numeric_head_is_not_a_session_file() {
    let tmp = Temp::new();
    let dir = tmp.0.join("claude-settings");
    fs::create_dir_all(&dir).unwrap();
    let legacy = dir.join("12345678-1234-4123-8123-123456789abc.json");
    let bad = dir.join("7-not-a-uuid.json");
    fs::write(&legacy, b"{}").unwrap();
    fs::write(&bad, b"{}").unwrap();
    discard(&tmp.0, SessionId(12_345_678));
    discard(&tmp.0, SessionId(7));
    assert_eq!(sweep(&tmp.0, &[]), 0);
    assert!(legacy.exists() && bad.exists());
}

#[test]
fn probe_uses_login_environment_and_version_args_only() {
    let tmp = Temp::new();
    let exe = tmp.script("claude", "#!/bin/sh\n[ \"$#\" = 1 ] && [ \"$1\" = --version ] || exit 9\nprintf '2.1.299 (Claude Code)\\n'\n");
    let mut profile = profile(&exe);
    profile
        .launch
        .extend(["--settings".into(), "missing".into()]);
    assert_eq!(
        probe(&profile, &tmp.login()).as_deref(),
        Some("2.1.299 (Claude Code)")
    );
}

#[test]
fn resume_and_session_id_roundtrip() {
    let tmp = Temp::new();
    let krill = tmp.script("krill", "#!/bin/sh\nexit 0\n");
    let created = Settings::create(&tmp.0, &krill, SessionId(5), Some("2.1.299")).unwrap();
    let sid = created.id.clone();
    assert_eq!(
        session_id(&tmp.0, SessionId(5)).as_deref(),
        Some(sid.as_str())
    );
    let next = Settings::create(&tmp.0, &krill, SessionId(6), Some("2.1.299")).unwrap();
    let next_path = next.path.clone();
    let launch = next
        .resume(
            &profile(Path::new("claude")),
            Permission::Yolo,
            tmp.login(),
            Some(&sid),
        )
        .unwrap();
    let argv: Vec<_> = launch
        .argv
        .iter()
        .map(|a| String::from_utf8_lossy(a))
        .collect();
    assert!(
        argv.iter().any(|a| a.as_ref() == format!("--resume={sid}")),
        "{argv:?}"
    );
    assert!(!argv.iter().any(|a| a == "--session-id"), "{argv:?}");
    assert!(
        launch
            .argv
            .contains(&next_path.as_os_str().as_bytes().to_vec())
    );
}

#[test]
fn fork_settings_do_not_expose_uninjected_session_id_but_clean_up_on_discard_and_sweep() {
    let tmp = Temp::new();
    let krill = tmp.script("krill", "#!/bin/sh\nexit 0\n");
    let forked = Settings::create_for_fork(&tmp.0, &krill, SessionId(15), Some("2.1.299")).unwrap();
    let fork_path = forked.path.clone();
    assert!(
        fork_path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("15-fork-")
    );
    assert_eq!(session_id(&tmp.0, SessionId(15)), None);
    let launch = forked
        .fork(
            &profile(Path::new("claude")),
            Permission::Manual,
            tmp.login(),
            Some("11111111-2222-4333-8444-555555555555"),
        )
        .unwrap();
    assert!(
        launch
            .argv
            .contains(&fork_path.as_os_str().as_bytes().to_vec())
    );
    assert_eq!(session_id(&tmp.0, SessionId(15)), None);

    // `Settings::create`로 만든 뒤 `fork()`를 호출해도 파일명이 `-fork-`로 바뀌어 미주입 UUID가 노출되지 않는다.
    let converted = Settings::create(&tmp.0, &krill, SessionId(16), Some("2.1.299")).unwrap();
    let _ = converted
        .fork(
            &profile(Path::new("claude")),
            Permission::Manual,
            tmp.login(),
            None,
        )
        .unwrap();
    assert_eq!(session_id(&tmp.0, SessionId(16)), None);

    // discard와 sweep은 `-fork-` 설정 파일도 해당 세션 소유로 정리한다.
    discard(&tmp.0, SessionId(15));
    assert!(!fork_path.exists());
    assert_eq!(sweep(&tmp.0, &[]), 1);
}

/// OWE-701: statusLine은 버전 게이트를 탄다. 1.0.64 미만에는 주입하지 않고,
/// 미확인 버전에는 주입한다(미확인 ≠ 구버전).
#[test]
fn statusline_is_injected_only_on_supported_versions() {
    let tmp = Temp::new();
    let krill = tmp.script("krill", "#!/bin/sh\nexit 0\n");
    let read = |session: &Settings| {
        serde_json::from_slice::<serde_json::Value>(&fs::read(&session.path).unwrap()).unwrap()
    };
    let modern =
        Settings::create(&tmp.0, &krill, SessionId(21), Some("2.1.301 (Claude Code)")).unwrap();
    assert!(read(&modern).get("statusLine").is_some());
    let floor = Settings::create(&tmp.0, &krill, SessionId(22), Some("1.0.64")).unwrap();
    assert!(read(&floor).get("statusLine").is_some());
    let unknown = Settings::create(&tmp.0, &krill, SessionId(23), None).unwrap();
    assert!(read(&unknown).get("statusLine").is_some());
    let old = Settings::create(&tmp.0, &krill, SessionId(24), Some("1.0.63")).unwrap();
    let old_value = read(&old);
    assert!(old_value.get("statusLine").is_none());
    assert!(old_value.get("hooks").is_some());
}

/// 실제 krill hook을 쓰는 오프라인 검증. 모델 호출 없음. 실행 전 cargo build -p krill --locked.
#[test]
#[ignore = "requires MK040_TEST_MK pointing to a built krill binary"]
fn real_mk_hook_spools_waiting_and_done_without_changing_user_settings() {
    let tmp = Temp::new();
    fs::create_dir(tmp.0.join(".claude")).unwrap();
    let user = tmp.0.join(".claude/settings.json");
    let bytes = b"{\"hooks\":{},\"env\":{\"USER_SETTING\":\"preserve\"}}\n";
    fs::write(&user, bytes).unwrap();
    let krill = PathBuf::from(std::env::var_os("MK040_TEST_MK").unwrap());
    let session = Settings::create(&tmp.0, &krill, SessionId(11), Some("2.1.299")).unwrap();
    let id = session.id.clone();
    let fixture = tmp.script(
        "claude",
        r"#!/usr/bin/python3
import json, subprocess, sys
args = sys.argv[1:]
with open(args[args.index('--settings') + 1]) as file:
    settings = json.load(file)
payload = sys.stdin.buffer.read()
event = json.loads(payload)
assert event['session_id'] == args[args.index('--session-id') + 1]
command = settings['hooks'][event['hook_event_name']][0]['hooks'][0]['command']
result = subprocess.run(command, shell=True, input=payload, stdout=subprocess.PIPE)
sys.stdout.buffer.write(result.stdout)
sys.exit(result.returncode)
",
    );
    let launch = session
        .launch(&profile(&fixture), Permission::Manual, tmp.login())
        .unwrap();
    let statuses = Statuses::open(None);
    for (event, want) in [
        ("PermissionRequest", AgentState::Waiting),
        ("Stop", AgentState::Done),
    ] {
        let payload =
            serde_json::to_vec(&serde_json::json!({"hook_event_name": event, "session_id": id}))
                .unwrap();
        let out = execute(&launch, &payload);
        assert!(out.status.success(), "{out:?}");
        assert_eq!(out.stdout, b"{}\n");
        let events = hook::read_spool(&fs::read(tmp.0.join(hook::SPOOL_FILE)).unwrap());
        assert_eq!(events.last().unwrap().payload, payload);
        statuses
            .hook(&events.iter().map(|e| (SessionId(1), e)).collect::<Vec<_>>())
            .unwrap();
        assert_eq!(statuses.get(SessionId(1)).state, want);
        assert_eq!(fs::read(&user).unwrap(), bytes);
    }
}
