//! MK-037: YOLO는 토글 시만, 자식 env에 내부 값 없음, preflight 안내와 실제 실행.

use krill_proto::os::ffi::OsStrExt;
use std::ffi::OsStr;
use std::process::{Command, Output, Stdio};

use krill_proto::agent::Agents;

use super::*;
use crate::sys::User;

fn kv(k: &str, v: &str) -> (Vec<u8>, Vec<u8>) {
    (k.as_bytes().to_vec(), v.as_bytes().to_vec())
}

fn user() -> User {
    User {
        name: b"owen".to_vec(),
        home: b"/Users/owen".to_vec(),
        shell: b"/bin/zsh".to_vec(),
    }
}

/// Orca·Claude Code·krill 개발 셸 안에서 krilld가 떴을 때 섞여 있을 법한 내부 값.
fn polluted() -> Vec<(Vec<u8>, Vec<u8>)> {
    vec![
        kv("KRILL_STATE_DIR", "/tmp/krill"),
        kv("MK_PANE_ID", "7"),
        kv("ORCA_TERMINAL_HANDLE", "term_x"),
        kv("ORCA_AGENT_HOOK_TOKEN", "secret"),
        kv("CLAUDECODE", "1"),
        kv("CLAUDE_CODE_ENTRYPOINT", "cli"),
        kv("ANTHROPIC_API_KEY", "sk-ant-x"),
        kv("PATH", "/polluted/bin:/usr/bin"),
        kv("TMPDIR", "/var/folders/x/T/"),
    ]
}

fn claude(permission: Permission) -> Launch {
    let agents = Agents::load(None).unwrap();
    let login = Shell::Login.launch_with(Some(&user()), polluted());
    wrap(&agents.profiles["claude"], permission, login)
}

#[test]
fn argv_goes_through_login_interactive_shell() {
    let l = claude(Permission::Manual);
    let argv: Vec<_> = l.argv.iter().map(|a| String::from_utf8_lossy(a)).collect();
    assert_eq!(argv[..4], ["/bin/zsh", "-l", "-i", "-c"]);
    assert_eq!(argv[5], "mk-agent");
    assert!(argv[6].contains("claude.ai/install.sh"));
    assert_eq!(argv[7..], ["claude"]);
}

#[test]
fn yolo_only_when_toggled() {
    let flag = b"--dangerously-skip-permissions".as_slice();
    assert!(!claude(Permission::Manual).argv.iter().any(|a| a == flag));
    assert_eq!(claude(Permission::Yolo).argv.last().unwrap(), flag);
}

#[test]
fn child_env_has_no_internal_values() {
    let l = claude(Permission::Yolo);
    let keys: Vec<_> = l
        .env
        .iter()
        .map(|(k, _)| String::from_utf8_lossy(k))
        .collect();
    for k in &keys {
        assert!(
            !["MK_", "ORCA_", "CLAUDE", "ANTHROPIC_"]
                .iter()
                .any(|p| k.starts_with(p)),
            "내부 값 누출: {k}"
        );
    }
    // 셸 실행과 같은 환경이다(에이전트용으로 더하는 값 없음).
    assert_eq!(
        l.env,
        Shell::Login.launch_with(Some(&user()), polluted()).env
    );
}

/// 만들어진 argv를 실제 zsh로 돌린다. 사용자 rc 대신 빈 ZDOTDIR을 쓴다.
fn run(profile: &Profile) -> Output {
    let dir = std::env::temp_dir().join(format!("mk037-zdot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut l = wrap(
        profile,
        Permission::Manual,
        Shell::Login.launch_with(Some(&user()), polluted()),
    );
    l.env
        .push((b"ZDOTDIR".to_vec(), dir.as_os_str().as_bytes().to_vec()));
    let (prog, args) = l.argv.split_first().unwrap();
    Command::new(OsStr::from_bytes(prog))
        .args(args.iter().map(|a| OsStr::from_bytes(a)))
        .env_clear()
        .envs(
            l.env
                .iter()
                .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
        )
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn profile(launch: &[&str]) -> Profile {
    Profile {
        name: "test".into(),
        launch: launch.iter().map(|s| (*s).to_owned()).collect(),
        yolo_args: vec![],
        install: "brew install mk-nope".into(),
        install_windows: String::new(),
        env: std::collections::BTreeMap::new(),
    }
}

#[test]
fn preflight_prints_install_hint_when_missing() {
    let out = run(&profile(&["mk-037-no-such-agent", "--x"]));
    assert_eq!(out.status.code(), Some(127));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("mk-037-no-such-agent: not installed. install: brew install mk-nope"),
        "{err}"
    );
}

#[test]
fn installed_agent_is_execed_with_args() {
    let out = run(&profile(&["/bin/echo", "mk-ok", "it's \"quoted\""]));
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("mk-ok it's \"quoted\""),
        "{out:?}"
    );
}

#[test]
fn selected_credentials_win_over_login_rc_and_custom_profile_environment() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = std::env::temp_dir().join(format!("krill-account-rc-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    let exe = root.join("codex");
    std::fs::write(&exe, "#!/bin/sh\nprintf '%s\\n' \"$CODEX_HOME\" \"${OPENAI_API_KEY-unset}\" \"${KRILL_ACCOUNT_DIR-unset}\"\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        root.join(".zshrc"),
        "export CODEX_HOME=/wrong/from-rc OPENAI_API_KEY=wrong-from-rc\n",
    )
    .unwrap();
    let mut profile = profile(&[exe.to_str().unwrap()]);
    profile
        .env
        .insert("CODEX_HOME".into(), "/wrong/from-profile".into());
    profile
        .env
        .insert("OPENAI_API_KEY".into(), "wrong-from-profile".into());
    for dir in [root.join("selected").display().to_string(), String::new()] {
        let pinned = LaunchAccount {
            id: "codex-selected".into(),
            env_key: "CODEX_HOME".into(),
            dir,
        };
        let mut login = Shell::Login.launch_with(Some(&user()), polluted());
        login
            .env
            .push((b"ZDOTDIR".to_vec(), root.as_os_str().as_bytes().to_vec()));
        let applied = apply_account_env(&mut login, &profile, Some(&root), Some(&pinned));
        let launch = wrap_account(&profile, Permission::Manual, login, applied.as_ref());
        let (program, args) = launch.argv.split_first().unwrap();
        let out = Command::new(OsStr::from_bytes(program))
            .args(args.iter().map(|a| OsStr::from_bytes(a)))
            .env_clear()
            .envs(
                launch
                    .env
                    .iter()
                    .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
            )
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            format!("{}\nunset\nunset\n", pinned.dir)
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// OWE-503: 설치가 끝난 receipt의 고정 명령만 `-c` hook으로, 프로그램 바로 뒤에 붙인다.
#[test]
fn codex_hooks_follow_complete_receipt_only() {
    let dir = std::env::temp_dir().join(format!("mk503-codex-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("hooks")).unwrap();
    let agents = Agents::load(None).unwrap();
    let codex = &agents.profiles["codex"];
    let receipt = |complete: bool| {
        let r = serde_json::json!({"agent": "codex", "complete": complete, "changes": [],
            "codex": {"path": "/x/config.toml", "command": "fixed 'cmd'", "added": {}}});
        std::fs::write(hooks::receipt(&dir, Agent::Codex), r.to_string()).unwrap();
    };
    assert!(codex_hooks(codex, Some(&dir)).is_none(), "no receipt");
    receipt(false);
    assert!(
        codex_hooks(codex, Some(&dir)).is_none(),
        "interrupted install"
    );
    receipt(true);
    let hooked = codex_hooks(codex, Some(&dir)).unwrap();
    let mut want = vec!["codex".to_owned()];
    want.extend(hooks::codex_args("fixed 'cmd'"));
    want.extend(codex.launch[1..].iter().cloned());
    assert_eq!(hooked.launch, want);
    assert!(codex_hooks(&agents.profiles["claude"], Some(&dir)).is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pane_env_has_pane_id() {
    let env = pane_env(krill_proto::SessionId(42));
    assert_eq!(env[0], kv("MK_PANE_ID", "42"));
}

#[test]
fn pane_env_passes_state_dir() {
    let env = pane_env_in(krill_proto::SessionId(7), Some(Path::new("/s")));
    assert!(env.contains(&kv("MK_PANE_ID", "7")));
    assert!(env.contains(&kv("KRILL_STATE_DIR", "/s")));
    let (_, executable) = env.iter().find(|(key, _)| key == b"KRILL_CLI").unwrap();
    assert!(Path::new(OsStr::from_bytes(executable)).is_absolute());
}

#[test]
fn resume_in_builds_claude_and_codex_resume_commands() {
    let dir = std::env::temp_dir().join(format!("mk052-resume-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("hooks")).unwrap();
    let agents = Agents::load(None).unwrap();
    let claude_exe = dir.join("claude");
    std::fs::write(&claude_exe, "#!/bin/sh\nprintf '2.1.299\\n'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&claude_exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    let r = serde_json::json!({"agent": "codex", "complete": true, "changes": [],
        "codex": {"path": "/x/config.toml", "command": "fixed 'cmd'", "added": {}}});
    std::fs::write(hooks::receipt(&dir, Agent::Codex), r.to_string()).unwrap();

    let (codex_with_id, sid, _) = resume_in(
        &agents.profiles["codex"],
        Permission::Yolo,
        krill_proto::SessionId(9),
        Some(&dir),
        Some("codex-sid-1"),
        None,
    )
    .unwrap();
    assert_eq!(sid.as_deref(), Some("codex-sid-1"));
    let argv: Vec<_> = codex_with_id
        .argv
        .iter()
        .map(|a| String::from_utf8_lossy(a))
        .collect();
    assert_eq!(
        &argv[argv.len() - 3..],
        [
            "--dangerously-bypass-approvals-and-sandbox",
            "resume",
            "codex-sid-1"
        ]
    );

    let (codex_plain, sid_none, _) = resume_in(
        &agents.profiles["codex"],
        Permission::Manual,
        krill_proto::SessionId(10),
        Some(&dir),
        None,
        None,
    )
    .unwrap();
    assert_eq!(sid_none, None);
    assert_eq!(codex_plain.argv.last().unwrap().as_slice(), b"resume");

    let mut claude_prof = agents.profiles["claude"].clone();
    claude_prof.launch = vec![claude_exe.to_str().unwrap().into()];
    let (claude_resumed, claude_sid, _) = resume_in(
        &claude_prof,
        Permission::Yolo,
        krill_proto::SessionId(11),
        Some(&dir),
        Some("11111111-2222-4333-8444-555555555555"),
        None,
    )
    .unwrap();
    assert_eq!(
        claude_sid.as_deref(),
        Some("11111111-2222-4333-8444-555555555555")
    );
    let claude_argv: Vec<_> = claude_resumed
        .argv
        .iter()
        .map(|a| String::from_utf8_lossy(a))
        .collect();
    assert_eq!(
        &claude_argv[claude_argv.len() - 1..],
        ["--resume=11111111-2222-4333-8444-555555555555"]
    );

    let (agy_with_id, agy_sid, _) = resume_in(
        &agents.profiles["agy"],
        Permission::Yolo,
        krill_proto::SessionId(12),
        Some(&dir),
        Some("conv-123"),
        None,
    )
    .unwrap();
    assert_eq!(agy_sid.as_deref(), Some("conv-123"));
    let agy_argv: Vec<_> = agy_with_id
        .argv
        .iter()
        .map(|a| String::from_utf8_lossy(a))
        .collect();
    assert_eq!(
        &agy_argv[agy_argv.len() - 3..],
        [
            "--dangerously-skip-permissions",
            "--conversation",
            "conv-123"
        ]
    );

    let (agy_plain, agy_sid_none, _) = resume_in(
        &agents.profiles["agy"],
        Permission::Manual,
        krill_proto::SessionId(13),
        Some(&dir),
        None,
        None,
    )
    .unwrap();
    assert_eq!(agy_sid_none, None);
    assert_eq!(agy_plain.argv.last().unwrap().as_slice(), b"-c");

    let home = dir.join("home");
    let ws = dir.join("workspace");
    std::fs::create_dir_all(&ws).unwrap();
    super::pre_trust_workspace_in(&home, &ws).unwrap();
    let settings_text =
        std::fs::read_to_string(home.join(".gemini/antigravity-cli/settings.json")).unwrap();
    let settings: serde_json::Value = serde_json::from_str(&settings_text).unwrap();
    let trusted = settings["trustedWorkspaces"].as_array().unwrap();
    assert!(trusted.contains(&serde_json::Value::String(
        ws.to_string_lossy().into_owned()
    )));

    std::fs::remove_dir_all(&dir).ok();
}

/// K-09: 병렬 워커 기동 때 trust 추가분이 유실되지 않는다.
#[test]
fn concurrent_trust_updates_keep_all_workspaces() {
    use std::sync::{Arc, Barrier};

    let dir = std::env::temp_dir().join(format!("mk-trust-race-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let home = dir.join("home");
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|n| {
            let (dir, home, barrier) = (dir.clone(), home.clone(), Arc::clone(&barrier));
            std::thread::spawn(move || {
                let ws = dir.join(format!("ws-{n}"));
                std::fs::create_dir_all(&ws).unwrap();
                barrier.wait();
                super::pre_trust_workspace_in(&home, &ws)
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".gemini/antigravity-cli/settings.json")).unwrap(),
    )
    .unwrap();
    let trusted = settings["trustedWorkspaces"].as_array().unwrap();
    // macOS `/tmp`(`TMPDIR`)는 `/private` 심링크라 원문·정규 경로 두 형태가 들어간다. 어느 쪽이든
    // 8개 워크스페이스가 하나도 유실되지 않아야 한다(잠금 전에는 읽기-수정-쓰기 경합으로 빠졌다).
    assert!(trusted.len() >= 8, "{trusted:?}");
    for n in 0..8 {
        let ws = dir.join(format!("ws-{n}"));
        let raw = ws.to_string_lossy().into_owned();
        let canonicalized = std::fs::canonicalize(&ws)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            trusted.contains(&serde_json::Value::String(raw.clone()))
                || trusted.contains(&serde_json::Value::String(canonicalized.clone())),
            "ws-{n} lost: {trusted:?}"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn resume_rejects_invalid_ids_from_older_metadata() {
    let agents = Agents::load(None).unwrap();
    for name in ["claude", "codex", "agy"] {
        for id in ["--help", "bad/path", "two words", ""] {
            assert!(
                resume_in(
                    &agents.profiles[name],
                    Permission::Manual,
                    krill_proto::SessionId(1),
                    None,
                    Some(id),
                    None
                )
                .is_err(),
                "{name}: {id:?}"
            );
        }
    }
}

#[test]
fn profile_env_and_args_reach_launch_and_resume() {
    let mut p = profile(&["mk-657-agent", "--extra", "x y"]);
    p.env.insert("PATH".into(), "/mk657/bin".into());
    p.env.insert("MK657_KEY".into(), "v=1".into());
    let session = krill_proto::SessionId(657);
    let (launched, _, _) = launch_in(&p, Permission::Manual, session, None).unwrap();
    let (resumed, _, _) = resume_in(&p, Permission::Manual, session, None, None, None).unwrap();
    for l in [launched, resumed] {
        assert_eq!(
            l.argv[l.argv.len() - 3..],
            [
                b"mk-657-agent".to_vec(),
                b"--extra".to_vec(),
                b"x y".to_vec()
            ]
        );
        let path: Vec<_> = l.env.iter().filter(|(k, _)| k == b"PATH").collect();
        assert_eq!(path, [&kv("PATH", "/mk657/bin")], "기존 PATH 위에 덮어쓴다");
        assert!(l.env.contains(&kv("MK657_KEY", "v=1")));
    }
}

/// OWE-706/721: Codex는 선택 `CODEX_HOME`, agy는 선택 프로필 `.gemini` 폴더로 재개한다.
#[test]
fn codex_and_agy_resume_use_selected_profile() {
    let state = std::env::temp_dir().join(format!("owe721-agent-{}", std::process::id()));
    std::fs::create_dir_all(&state).unwrap();
    for (program, key, expected_dir) in [
        ("codex", "CODEX_HOME", Some("/fixture/account-b")),
        ("agy", "ANTIGRAVITY_CLI_HOME", Some("/fixture/account-b")),
        (
            "antigravity",
            "ANTIGRAVITY_CLI_HOME",
            Some("/fixture/account-b"),
        ),
    ] {
        let p = profile(&[program]);
        let provider = ProviderKind::parse(program).unwrap();
        let selected = LaunchAccount {
            id: "account-b".into(),
            env_key: key.into(),
            dir: "/fixture/account-b".into(),
        };
        let (launch, _, recorded) = resume_in(
            &p,
            Permission::Manual,
            krill_proto::SessionId(721),
            Some(&state),
            Some("0199f6c0-1234-7000-8000-000000000158"),
            Some(&selected),
        )
        .unwrap();
        let env: Vec<_> = launch
            .env
            .iter()
            .filter(|(k, _)| k == key.as_bytes())
            .collect();
        if let Some(dir) = expected_dir {
            if provider == ProviderKind::Antigravity {
                assert!(env.is_empty());
                assert!(
                    launch
                        .argv
                        .iter()
                        .any(|arg| arg == b"--gemini_dir=/fixture/account-b/.gemini")
                );
                assert!(!launch.env.iter().any(|(k, _)| k == b"SSH_CONNECTION"));
            } else {
                assert_eq!(env, [&kv(key, dir)]);
            }
            assert_eq!(recorded, Some(selected));
            let resume_arg = if provider == ProviderKind::Antigravity {
                b"--conversation".as_slice()
            } else {
                b"resume".as_slice()
            };
            assert!(launch.argv.iter().any(|arg| arg == resume_arg));
        } else {
            assert!(env.is_empty());
        }
    }
    // Explicit account selection wins over stale profile and inherited credentials.
    let mut p = profile(&["codex"]);
    p.env
        .insert("CODEX_HOME".into(), "/fixture/override".into());
    let mut login =
        Shell::Login.launch_with(Some(&user()), vec![kv("CODEX_HOME", "/fixture/inherited")]);
    apply_account_env(
        &mut login,
        &p,
        Some(&state),
        Some(&LaunchAccount {
            id: "b".into(),
            env_key: "CODEX_HOME".into(),
            dir: "/fixture/account-b".into(),
        }),
    );
    assert_eq!(
        login
            .env
            .iter()
            .filter(|(k, _)| k == b"CODEX_HOME")
            .collect::<Vec<_>>(),
        [&kv("CODEX_HOME", "/fixture/account-b")]
    );
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn opencode_account_uses_xdg_data_and_state_profile_directories() {
    let profile = profile(&["opencode"]);
    let selected = LaunchAccount {
        id: "opencode-one".into(),
        env_key: "OPENCODE_CONFIG_DIR".into(),
        dir: "/fixture/opencode-one".into(),
    };
    let mut login = Launch {
        argv: Vec::new(),
        env: vec![kv("OPENCODE_CONFIG_DIR", "/stale")],
    };
    apply_account_env(&mut login, &profile, None, Some(&selected));
    assert!(
        login
            .env
            .contains(&kv("XDG_DATA_HOME", "/fixture/opencode-one/data"))
    );
    assert!(
        login
            .env
            .contains(&kv("XDG_STATE_HOME", "/fixture/opencode-one/state"))
    );
    assert!(login.env.contains(&kv("OPENCODE_DB", "opencode.db")));
    assert!(
        !login
            .env
            .iter()
            .any(|(key, _)| key == b"OPENCODE_CONFIG_DIR")
    );
}

#[test]
fn agy_workspace_trust_is_written_inside_selected_profile() {
    let root = std::env::temp_dir().join(format!("owe706-agy-trust-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let profile_dir = root.join("profile");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let account = LaunchAccount {
        id: "agy-one".into(),
        env_key: "ANTIGRAVITY_CLI_HOME".into(),
        dir: profile_dir.display().to_string(),
    };
    let agents = Agents::load(None).unwrap();
    super::pre_trust_workspace(
        &agents.profiles["agy"],
        workspace.as_os_str().as_encoded_bytes(),
        Some(&account),
    );
    let path = profile_dir.join(".gemini/antigravity-cli/settings.json");
    let settings: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(
        settings["trustedWorkspaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == workspace.to_str().unwrap())
    );
    assert!(!root.join(".gemini/antigravity-cli/settings.json").exists());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn agy_hidden_flag_guard_uses_only_temporary_home_and_profile() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = std::env::temp_dir().join(format!("owe706-agy-guard-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    let exe = root.join("agy");
    std::fs::write(
        &exe,
        "#!/bin/sh\ntest -d \"$HOME\" && test \"$SSH_CONNECTION\" = krill && case \"$1\" in --gemini_dir=*) test \"$2 $3\" = 'mcp list' ;; *) exit 1 ;; esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(super::agy_gemini_dir_supported_with(&exe, &root));
    assert!(!root.exists());
}

/// OWE-901: 가드 결과는 (실행 경로, mtime, 버전) 키로 캐시돼 같은 바이너리는 다시 실행하지 않는다.
#[test]
fn agy_guard_result_is_cached_per_binary_and_version() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = std::env::temp_dir().join(format!("owe901-agy-guard-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    super::clear_agy_guard_cache_for_tests();
    let before = super::agy_guard_cache_len_for_tests();
    let exe = root.join("agy");
    let counter = root.join("calls");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\necho x >> '{}'\nif [ \"$1\" = '--version' ]; then echo 'agy 1.2.0'; exit 0; fi\ntest -d \"$HOME\" && test \"$SSH_CONNECTION\" = krill && case \"$1\" in --gemini_dir=*) test \"$2 $3\" = 'mcp list' ;; *) exit 1 ;; esac\n",
            counter.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    // 1회차: `--version` + `mcp list` 2회 실행.
    assert!(super::agy_gemini_dir_supported_cached(&exe));
    assert_eq!(super::agy_guard_cache_len_for_tests(), before + 1);
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap().lines().count(),
        2
    );
    // 2회차: 캐시 적중이라 추가 실행 없음.
    assert!(super::agy_gemini_dir_supported_cached(&exe));
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap().lines().count(),
        2
    );
    super::clear_agy_guard_cache_for_tests();
    std::fs::remove_dir_all(root).unwrap();
}

/// OWE-698: 실행 뒤 계정을 바꿔도 재개는 실행 당시 계정 프로필로 이어 간다.
/// 기록 계정 폴더가 없어지면 대화가 든 프로필, 호스트 기본 기록이면 env를 덮어쓰지 않는다.
#[test]
fn resume_keeps_launch_account_after_switch() {
    use krill_proto::usage::ProviderKind;
    let dir = std::env::temp_dir().join(format!("owe698-resume-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    // 실제 홈을 쓰므로 프로비저닝(기본 켜짐)이 ~/.claude를 링크하지 않게 끈다.
    std::fs::write(dir.join("accounts.toml"), "provisioning = false\n").unwrap();
    let home = accounts::default_home();
    let id_of = |label: &str, dir_path: &Path| {
        accounts::add_account(
            &home,
            Some(&dir),
            ProviderKind::Claude,
            label,
            None,
            dir_path.to_str().unwrap(),
        )
        .unwrap()
        .accounts
        .into_iter()
        .find(|a| a.label == label)
        .unwrap()
        .id
    };
    let (a_dir, b_dir) = (dir.join("profile-a"), dir.join("profile-b"));
    std::fs::create_dir_all(&a_dir).unwrap();
    std::fs::create_dir_all(&b_dir).unwrap();
    let (a, b) = (id_of("A", &a_dir), id_of("B", &b_dir));
    accounts::switch_account(&home, Some(&dir), ProviderKind::Claude, &a).unwrap();

    let claude_exe = dir.join("claude");
    std::fs::write(&claude_exe, "#!/bin/sh\nprintf '2.1.299\\n'\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&claude_exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut claude = Agents::load(None).unwrap().profiles["claude"].clone();
    claude.launch = vec![claude_exe.to_str().unwrap().into()];
    let config = |l: &Launch| {
        l.env
            .iter()
            .filter(|(k, _)| k == b"CLAUDE_CONFIG_DIR")
            .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
            .collect::<Vec<_>>()
    };
    let a_path = a_dir.to_str().unwrap().to_owned();
    let b_path = b_dir.to_str().unwrap().to_owned();

    let (launched, sid, recorded) = launch_in(
        &claude,
        Permission::Manual,
        krill_proto::SessionId(1),
        Some(&dir),
    )
    .unwrap();
    assert_eq!(config(&launched), [a_path.as_str()]);
    let recorded = recorded.unwrap();
    assert_eq!(
        recorded,
        LaunchAccount {
            id: a.clone(),
            env_key: "CLAUDE_CONFIG_DIR".into(),
            dir: a_path.clone(),
        }
    );

    accounts::switch_account(&home, Some(&dir), ProviderKind::Claude, &b).unwrap();
    let resume = |pin: Option<&LaunchAccount>| {
        resume_in(
            &claude,
            Permission::Manual,
            krill_proto::SessionId(2),
            Some(&dir),
            sid.as_deref(),
            pin,
        )
        .unwrap()
    };
    // 수정 전 경로(기록 없이 재개)는 지금 활성 계정 B로 간다 — 이 결함을 막는 것이 OWE-698이다.
    assert_eq!(config(&resume(None).0), [b_path.as_str()]);
    let pin = resume_account(&claude, Some(&dir), Some(&recorded), sid.as_deref());
    assert_eq!(pin.as_ref(), Some(&recorded));
    let (resumed, _, again) = resume(pin.as_ref());
    assert_eq!(config(&resumed), [a_path.as_str()]);
    assert_eq!(again.as_ref(), Some(&recorded), "다음 재개에도 같은 계정");

    // 기록 계정 폴더가 사라지면 대화가 실제로 든 프로필(B)로 대체한다.
    std::fs::remove_dir_all(&a_dir).unwrap();
    let project = b_dir.join("projects").join("-tmp-work");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join(format!("{}.jsonl", sid.as_deref().unwrap())),
        "{}\n",
    )
    .unwrap();
    let moved = resume_account(&claude, Some(&dir), Some(&recorded), sid.as_deref()).unwrap();
    assert_eq!(
        (moved.id.as_str(), moved.dir.as_str()),
        (b.as_str(), b_path.as_str())
    );
    // 대화도 못 찾으면 `None` → 지금 활성 계정.
    assert_eq!(
        resume_account(&claude, Some(&dir), Some(&recorded), Some("not-found-id")),
        None
    );

    // 호스트 기본 기록: 계정을 바꿔도 env를 덮어쓰지 않는다.
    let host = LaunchAccount {
        id: accounts::host_default_id(ProviderKind::Claude),
        env_key: "CLAUDE_CONFIG_DIR".into(),
        dir: String::new(),
    };
    let pin = resume_account(&claude, Some(&dir), Some(&host), sid.as_deref());
    assert!(config(&resume(pin.as_ref()).0).is_empty());

    // 프로필이 다른 프로바이더로 바뀌었으면 기록을 쓰지 않는다.
    let mut codex = claude.clone();
    codex.launch = vec!["codex".into()];
    assert_eq!(
        resume_account(&codex, Some(&dir), Some(&recorded), None),
        None
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn selected_account_is_per_session_and_does_not_change_global_selection() {
    use krill_proto::usage::ProviderKind;
    let root = std::env::temp_dir().join(format!("owe712-selected-{}", std::process::id()));
    let home = accounts::default_home();
    let state = root.join("state");
    let dir = root.join("codex");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let mut file = accounts::AccountsFile::default();
    file.auto_switch.enabled = false;
    file.accounts.push(accounts::StoredAccount {
        id: "codex-selected".into(),
        provider: "codex".into(),
        label: "Selected".into(),
        email: None,
        profile_dir: dir.display().to_string(),
    });
    accounts::save_file(&home, Some(&state), &file).unwrap();
    let p = profile(&["codex"]);
    let (launch, _, account) = launch_selected_in(
        &p,
        Permission::Manual,
        krill_proto::SessionId(712),
        Some(&state),
        Some("codex-selected"),
    )
    .unwrap();
    assert!(
        launch
            .env
            .contains(&kv(ProviderKind::Codex.env_var(), dir.to_str().unwrap()))
    );
    assert_eq!(account.as_ref().unwrap().id, "codex-selected");
    assert!(accounts::load_file(&home, Some(&state)).active.is_empty());
    assert!(selected_account(&profile(&["claude"]), Some(&state), "codex-selected").is_err());
    assert!(selected_account(&p, Some(&state), "missing").is_err());
    std::fs::remove_dir_all(root).ok();
}

/// OWE-699: 재개 요청이 계정을 지정하면(다른 계정으로 이어가기) 기록 계정 대신 그 계정 프로필로 이어 간다.
/// 지정이 없으면 기존(OWE-698)처럼 기록 계정, 모르는 계정은 거부한다.
#[test]
fn resume_target_honors_requested_account() {
    use krill_proto::usage::ProviderKind;
    let dir = std::env::temp_dir().join(format!("owe699-target-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    // 실제 HOME(`~/.claude`·Orca 캐시)을 읽지 않게 임시 홈을 쓴다.
    let home = dir.join("home");
    let (a_dir, b_dir) = (dir.join("profile-a"), dir.join("profile-b"));
    let add = |label: &str, path: &Path| {
        std::fs::create_dir_all(path).unwrap();
        accounts::add_account(
            &home,
            Some(&dir),
            ProviderKind::Claude,
            label,
            None,
            path.to_str().unwrap(),
        )
        .unwrap()
        .accounts
        .into_iter()
        .find(|a| a.label == label)
        .unwrap()
        .id
    };
    let (a, b) = (add("A", &a_dir), add("B", &b_dir));
    let mut claude = Agents::load(None).unwrap().profiles["claude"].clone();
    claude.launch = vec!["claude".into()];
    let recorded = LaunchAccount {
        id: a.clone(),
        env_key: "CLAUDE_CONFIG_DIR".into(),
        dir: a_dir.to_str().unwrap().into(),
    };
    let target = |req: Option<&str>| resume_target(&claude, Some(&dir), req, Some(&recorded), None);

    assert_eq!(target(None).unwrap(), Some(recorded.clone()));
    let other = target(Some(&b)).unwrap().unwrap();
    assert_eq!(other.id, b);
    assert_eq!(other.dir, b_dir.to_str().unwrap());
    assert_eq!(
        target(Some("claude-nope")).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// OWE-736: Claude는 `--resume=<id> --fork-session`(또는 `--continue --fork-session`),
/// Codex는 `fork <id>`(또는 `fork --last`) 인자로 포크 실행 명령을 만든다.
#[test]
fn fork_in_builds_claude_and_codex_fork_argv() {
    let dir = std::env::temp_dir().join(format!("owe736-fork-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();

    let claude = profile(&["claude"]);
    let (launch_with_id, sid, _) = fork_in(
        &claude,
        Permission::Manual,
        krill_proto::SessionId(10),
        Some(&dir),
        Some("11111111-2222-4333-8444-555555555555"),
        None,
    )
    .unwrap();
    assert_eq!(sid, None);
    let args: Vec<_> = launch_with_id
        .argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    assert!(args.iter().any(|a| a == "--settings"));
    assert!(
        args.iter()
            .any(|a| a == "--resume=11111111-2222-4333-8444-555555555555")
    );
    assert_eq!(args.last().map(String::as_str), Some("--fork-session"));

    let (launch_no_id, _, _) = fork_in(
        &claude,
        Permission::Manual,
        krill_proto::SessionId(11),
        Some(&dir),
        None,
        None,
    )
    .unwrap();
    let args_no_id: Vec<_> = launch_no_id
        .argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    assert!(args_no_id.iter().any(|a| a == "--continue"));
    assert_eq!(
        args_no_id.last().map(String::as_str),
        Some("--fork-session")
    );

    let codex = profile(&["codex"]);
    let (codex_with_id, _, _) = fork_in(
        &codex,
        Permission::Yolo,
        krill_proto::SessionId(12),
        Some(&dir),
        Some("0199f6c0-1234-7000-8000-000000000158"),
        None,
    )
    .unwrap();
    let codex_args: Vec<_> = codex_with_id
        .argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    assert_eq!(
        &codex_args[codex_args.len() - 2..],
        ["fork", "0199f6c0-1234-7000-8000-000000000158"]
    );

    let (codex_last, _, _) = fork_in(
        &codex,
        Permission::Manual,
        krill_proto::SessionId(13),
        Some(&dir),
        None,
        None,
    )
    .unwrap();
    let codex_last_args: Vec<_> = codex_last
        .argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    assert_eq!(
        &codex_last_args[codex_last_args.len() - 2..],
        ["fork", "--last"]
    );

    // OWE-752: Claude 포크 세션은 `--session-id`를 주입하지 않으므로 hook 수신 전 `claude::session_id`가
    // 미주입 UUID를 반환하지 않아야 하며, ID 없는 `--resume` 후에도 마찬가지여야 한다.
    assert_eq!(claude::session_id(&dir, krill_proto::SessionId(10)), None);
    assert_eq!(claude::session_id(&dir, krill_proto::SessionId(11)), None);
    let (resumed_without_id, resumed_sid, _) = resume_in(
        &claude,
        Permission::Manual,
        krill_proto::SessionId(14),
        Some(&dir),
        None,
        None,
    )
    .unwrap();
    assert_eq!(resumed_sid, None);
    assert_eq!(
        resumed_without_id.argv.last().map(Vec::as_slice),
        Some(b"--resume".as_slice())
    );
    assert_eq!(claude::session_id(&dir, krill_proto::SessionId(14)), None);

    // OWE-752: 격리 계정이 지정된 Antigravity(`agy`·`antigravity`) 세션을 포크할 때도
    // `--gemini_dir=<account.dir>/.gemini` 플래그가 보존되어야 한다.
    for prog in ["agy", "antigravity"] {
        let agy_prof = profile(&[prog]);
        let agy_acc = LaunchAccount {
            id: "agy-b".into(),
            env_key: "ANTIGRAVITY_CLI_HOME".into(),
            dir: "/fixture/agy-b".into(),
        };
        let (agy_fork, agy_fork_sid, agy_recorded) = fork_in(
            &agy_prof,
            Permission::Yolo,
            krill_proto::SessionId(15),
            Some(&dir),
            Some("33333333-2222-4333-8444-555555555555"),
            Some(&agy_acc),
        )
        .unwrap();
        assert_eq!(agy_fork_sid, None);
        assert_eq!(agy_recorded, Some(agy_acc));
        assert!(
            agy_fork
                .argv
                .iter()
                .any(|arg| arg == b"--gemini_dir=/fixture/agy-b/.gemini"),
            "{prog} fork argv={:?}",
            agy_fork.argv
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// OWE-753: Antigravity(`agy`)·`OpenCode` 볼트 재개 시 `accounts.toml` 및 `<state_dir>/profiles/*`의
/// 격리 프로필 디렉터리(`.gemini/antigravity-cli/brain/<id>` 등)를 찾아 실행 계정을 결정한다.
#[test]
fn resume_account_resolves_agy_and_opencode_vault_profiles() {
    let dir = std::env::temp_dir().join(format!("owe753-resume-acc-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let state = dir.join("state");
    let home = accounts::default_home();
    let agy_acc_dir = dir.join("agy-account-a");
    let agy_state_profile = state.join("profiles/agy/work-slot");
    let oc_state_profile = state.join("profiles/opencode/oc-slot");
    let conv_a = "aaaaaaaa-1111-4222-8333-444444444444";
    let conv_b = "bbbbbbbb-1111-4222-8333-444444444444";
    let oc_conv = "ses_owe753_opencode";

    std::fs::create_dir_all(
        agy_acc_dir
            .join(".gemini/antigravity-cli/brain")
            .join(conv_a),
    )
    .unwrap();
    std::fs::create_dir_all(
        agy_state_profile
            .join(".gemini/antigravity/brain")
            .join(conv_b),
    )
    .unwrap();
    std::fs::create_dir_all(oc_state_profile.join("data/opencode/storage/session/global")).unwrap();
    std::fs::write(
        oc_state_profile
            .join("data/opencode/storage/session/global")
            .join(format!("{oc_conv}.json")),
        "{}\n",
    )
    .unwrap();

    let mut file = accounts::AccountsFile::default();
    file.accounts.push(accounts::StoredAccount {
        id: "agy-a".into(),
        provider: "antigravity".into(),
        label: "Agy A".into(),
        email: None,
        profile_dir: agy_acc_dir.display().to_string(),
    });
    accounts::save_file(&home, Some(&state), &file).unwrap();

    let agy_prof = profile(&["agy"]);
    let resolved_a = resume_account(&agy_prof, Some(&state), None, Some(conv_a)).unwrap();
    assert_eq!(resolved_a.id, "agy-a");
    assert_eq!(resolved_a.env_key, "ANTIGRAVITY_CLI_HOME");
    assert_eq!(resolved_a.dir, agy_acc_dir.display().to_string());

    let resolved_b = resume_account(&agy_prof, Some(&state), None, Some(conv_b)).unwrap();
    assert_eq!(resolved_b.env_key, "ANTIGRAVITY_CLI_HOME");
    assert_eq!(resolved_b.dir, agy_state_profile.display().to_string());

    let oc_prof = profile(&["opencode"]);
    let resolved_oc = resume_account(&oc_prof, Some(&state), None, Some(oc_conv)).unwrap();
    assert_eq!(resolved_oc.env_key, "OPENCODE_CONFIG_DIR");
    assert_eq!(resolved_oc.dir, oc_state_profile.display().to_string());

    std::fs::remove_dir_all(&dir).ok();
}

/// OWE-737: 새 워크스페이스 trust 프리셋 주입(Claude·Codex·Cursor·Antigravity)과 안전 가드 검증.
#[test]
fn pre_trust_all_agents_preserves_existing_configs_and_skips_broad_paths() {
    let dir = std::env::temp_dir().join(format!("owe737-trust-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let home = dir.join("home");
    let ws = dir.join("worktrees/repo/feat-login");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&ws).unwrap();
    let ws_canon = std::fs::canonicalize(&ws).unwrap();
    let ws_canon_str = ws_canon.to_string_lossy().into_owned();

    // 1. 루트(/)와 홈 디렉터리 자체는 거부해야 한다.
    assert!(super::is_too_broad_to_pre_trust(&home, Path::new("/")));
    assert!(super::is_too_broad_to_pre_trust(&home, &home));
    assert!(!super::pre_trust_claude_in(&home, None, &home).unwrap());
    assert!(!super::pre_trust_codex_in(&home, None, Path::new("/")).unwrap());

    // 2. Claude: 기존 설정 필드와 다른 프로젝트 항목을 보존하면서 hasTrustDialogAccepted = true 주입.
    let claude_json = home.join(".claude.json");
    std::fs::write(
        &claude_json,
        serde_json::json!({
            "numStartups": 42,
            "projects": {
                "/existing/proj": {
                    "hasTrustDialogAccepted": true,
                    "allowedTools": ["Bash"]
                },
                ws_canon_str.clone(): {
                    "allowedTools": ["Read"]
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    assert!(super::pre_trust_claude_in(&home, None, &ws).unwrap());
    // 멱등성: 이미 신뢰된 경로는 다시 쓰지 않고 false.
    assert!(!super::pre_trust_claude_in(&home, None, &ws).unwrap());
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    assert_eq!(parsed["numStartups"], 42);
    assert_eq!(
        parsed["projects"]["/existing/proj"]["allowedTools"],
        serde_json::json!(["Bash"])
    );
    assert_eq!(
        parsed["projects"][&ws_canon_str]["hasTrustDialogAccepted"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        parsed["projects"][&ws_canon_str]["allowedTools"],
        serde_json::json!(["Read"])
    );

    // 손상된 .claude.json은 덮어쓰지 않고 보존해야 한다.
    let custom_claude_dir = dir.join("custom-claude");
    std::fs::create_dir_all(&custom_claude_dir).unwrap();
    let broken_path = custom_claude_dir.join(".claude.json");
    std::fs::write(&broken_path, "{broken json").unwrap();
    assert!(!super::pre_trust_claude_in(&home, Some(&custom_claude_dir), &ws).unwrap());
    assert_eq!(
        std::fs::read_to_string(&broken_path).unwrap(),
        "{broken json"
    );

    // 3. Codex: 기존 config.toml 주석·설정을 유지하면서 [projects."<realpath>"] trust_level = "trusted" 추가.
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let codex_toml = codex_dir.join("config.toml");
    std::fs::write(
        &codex_toml,
        "# user comment\nmodel = \"gpt-5-codex\"\n\n[projects.\"/other\"]\ntrust_level = \"untrusted\"\n",
    )
    .unwrap();
    assert!(super::pre_trust_codex_in(&home, None, &ws).unwrap());
    assert!(!super::pre_trust_codex_in(&home, None, &ws).unwrap());
    let toml_text = std::fs::read_to_string(&codex_toml).unwrap();
    assert!(toml_text.contains("# user comment"));
    assert!(toml_text.contains("model = \"gpt-5-codex\""));
    assert!(toml_text.contains(&format!(
        "[projects.\"{ws_canon_str}\"]\ntrust_level = \"trusted\""
    )));

    // 기존 섹션의 untrusted 값을 trusted로 갱신하는지 확인.
    let (updated, changed) = super::upsert_codex_project_trust(&toml_text, "/other");
    assert!(changed);
    assert!(updated.contains("[projects.\"/other\"]\ntrust_level = \"trusted\""));

    // 4. Cursor & Antigravity 통합 호출(pre_trust_all_agents_in) 검증.
    let ws2 = dir.join("worktrees/repo/feat-two");
    std::fs::create_dir_all(&ws2).unwrap();
    let ws2_canon = std::fs::canonicalize(&ws2)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    super::pre_trust_all_agents_in(&home, None, None, &ws2);
    let slug = super::cursor_project_slug(&ws2_canon);
    let marker = home
        .join(".cursor/projects")
        .join(slug)
        .join(".workspace-trusted");
    let marker_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
    assert_eq!(marker_json["workspacePath"], ws2_canon);

    let agy_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".gemini/antigravity-cli/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(
        agy_json["trustedWorkspaces"]
            .as_array()
            .unwrap()
            .contains(&serde_json::Value::String(ws2_canon))
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// OWE-737 후속: (1) 기본 꺼짐(동의 플래그 필수), (2) JSON 키 순서·TOML 주석 보존 및 예상 밖 스키마 거부,
/// (3) 동시 수정 감지(CAS 실패 시 덮어쓰지 않고 건너뜀) 검증.
#[test]
fn trust_preset_default_off_preserves_key_order_and_aborts_on_concurrent_modification() {
    let dir = std::env::temp_dir().join(format!("owe737-safety-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let state_dir = dir.join("state");
    let home = dir.join("home");
    let ws = dir.join("worktrees/repo/feat-safe");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&ws).unwrap();
    let ws_canon_str = std::fs::canonicalize(&ws)
        .unwrap()
        .to_string_lossy()
        .into_owned();

    // (1) 기본 꺼짐: 상태 디렉터리에 workspace-trust-preset.enabled 파일이 없으면 아무 파일도 쓰지 않는다.
    assert!(!super::is_trust_preset_enabled_in(Some(&state_dir)));
    assert!(!super::pre_trust_all_agents_if_enabled(
        Some(&state_dir),
        &home,
        None,
        None,
        &ws,
    ));
    assert!(!home.join(".claude.json").exists());
    assert!(!home.join(".codex/config.toml").exists());
    assert!(!home.join(".gemini/antigravity-cli/settings.json").exists());

    // 동의 플래그 파일을 생성하면 활성화된다.
    std::fs::write(
        state_dir.join(krill_proto::git::WORKSPACE_TRUST_PRESET_ENABLED_FLAG),
        b"1\n",
    )
    .unwrap();
    assert!(super::is_trust_preset_enabled_in(Some(&state_dir)));

    // (2) JSON 키 순서 보존(.claude.json & settings.json)과 예상 밖 스키마 거부.
    let claude_json = home.join(".claude.json");
    let original_claude = format!(
        "{{\n  \"zetaRoot\": 1,\n  \"alphaRoot\": \"keep\",\n  \"projects\": {{\n    \"/z-first\": {{\n      \"zTool\": true,\n      \"aTool\": false\n    }},\n    \"{ws_canon_str}\": {{\n      \"customZ\": [3, 2, 1],\n      \"hasTrustDialogAccepted\": false,\n      \"customA\": \"tail\"\n    }}\n  }},\n  \"middleRoot\": {{\n    \"nestedZ\": 9,\n    \"nestedA\": 1\n  }}\n}}\n"
    );
    std::fs::write(&claude_json, &original_claude).unwrap();
    assert!(super::pre_trust_claude_in(&home, None, &ws).unwrap());
    let updated_claude = std::fs::read_to_string(&claude_json).unwrap();
    let z_root = updated_claude.find("\"zetaRoot\"").unwrap();
    let a_root = updated_claude.find("\"alphaRoot\"").unwrap();
    let p_root = updated_claude.find("\"projects\"").unwrap();
    let m_root = updated_claude.find("\"middleRoot\"").unwrap();
    assert!(
        z_root < a_root && a_root < p_root && p_root < m_root,
        "최상위 키 순서가 보존되어야 한다: {updated_claude}"
    );
    let z_tool = updated_claude.find("\"zTool\"").unwrap();
    let a_tool = updated_claude.find("\"aTool\"").unwrap();
    let custom_z = updated_claude.find("\"customZ\"").unwrap();
    let trust_key = updated_claude
        .find("\"hasTrustDialogAccepted\": true")
        .unwrap();
    let custom_a = updated_claude.find("\"customA\"").unwrap();
    let nested_z = updated_claude.find("\"nestedZ\"").unwrap();
    let nested_a = updated_claude.find("\"nestedA\"").unwrap();
    assert!(z_tool < a_tool);
    assert!(custom_z < trust_key && trust_key < custom_a);
    assert!(nested_z < nested_a);

    // projects가 객체가 아닌 경우(null 또는 배열) 건드리지 않고 그대로 두어야 한다.
    let bad_schema_dir = dir.join("bad-claude");
    std::fs::create_dir_all(&bad_schema_dir).unwrap();
    let bad_claude_path = bad_schema_dir.join(".claude.json");
    let null_projects = "{\n  \"zeta\": 1,\n  \"projects\": null\n}\n";
    std::fs::write(&bad_claude_path, null_projects).unwrap();
    assert!(!super::pre_trust_claude_in(&home, Some(&bad_schema_dir), &ws).unwrap());
    assert_eq!(
        std::fs::read_to_string(&bad_claude_path).unwrap(),
        null_projects
    );

    // Antigravity settings.json 키 순서 보존 검증.
    let agy_settings = home.join(".gemini/antigravity-cli/settings.json");
    std::fs::create_dir_all(agy_settings.parent().unwrap()).unwrap();
    std::fs::write(
        &agy_settings,
        "{\n  \"zetaFlag\": true,\n  \"trustedWorkspaces\": [\n    \"/first/ws\"\n  ],\n  \"alphaFlag\": 7\n}\n",
    )
    .unwrap();
    super::pre_trust_workspace_in(&home, &ws).unwrap();
    let updated_agy = std::fs::read_to_string(&agy_settings).unwrap();
    assert!(
        updated_agy.find("\"zetaFlag\"").unwrap()
            < updated_agy.find("\"trustedWorkspaces\"").unwrap()
            && updated_agy.find("\"trustedWorkspaces\"").unwrap()
                < updated_agy.find("\"alphaFlag\"").unwrap(),
        "settings.json 키 순서가 보존되어야 한다: {updated_agy}"
    );

    // Codex config.toml: 손상된 TOML이나 projects가 테이블이 아닌 경우 건드리지 않는다.
    let bad_codex_dir = dir.join("bad-codex");
    std::fs::create_dir_all(&bad_codex_dir).unwrap();
    let bad_codex_toml = bad_codex_dir.join("config.toml");
    let broken_toml = "# comment\nprojects = \"not-a-table\"\n";
    std::fs::write(&bad_codex_toml, broken_toml).unwrap();
    assert!(!super::pre_trust_codex_in(&home, Some(&bad_codex_dir), &ws).unwrap());
    assert_eq!(
        std::fs::read_to_string(&bad_codex_toml).unwrap(),
        broken_toml
    );

    // (3) 동시 수정 감지: 읽기 이후 rename 직전에 다른 프로세스가 원본을 수정하면 덮어쓰지 않고 건너뛴다.
    let race_claude_dir = dir.join("race-claude");
    std::fs::create_dir_all(&race_claude_dir).unwrap();
    let race_claude_json = race_claude_dir.join(".claude.json");
    std::fs::write(&race_claude_json, "{\n  \"initial\": 1\n}\n").unwrap();
    let concurrent_content = "{\n  \"initial\": 1,\n  \"concurrentFromClaudeCli\": true\n}\n";
    let wrote = super::pre_trust_claude_in_with_hook(
        &home,
        Some(&race_claude_dir),
        &ws,
        Some(&|target| {
            std::fs::write(target, concurrent_content).unwrap();
        }),
    )
    .unwrap();
    assert!(
        !wrote,
        "동시 수정이 감지되면 false를 반환하고 쓰지 않아야 한다"
    );
    assert_eq!(
        std::fs::read_to_string(&race_claude_json).unwrap(),
        concurrent_content,
        "외부 프로세스가 기록한 변경분이 보존되어야 한다"
    );

    let race_codex_dir = dir.join("race-codex");
    std::fs::create_dir_all(&race_codex_dir).unwrap();
    let race_codex_toml = race_codex_dir.join("config.toml");
    std::fs::write(&race_codex_toml, "model = \"o3\"\n").unwrap();
    let concurrent_toml = "model = \"o4-mini\" # updated by codex\n";
    let codex_wrote = super::pre_trust_codex_in_with_hook(
        &home,
        Some(&race_codex_dir),
        &ws,
        Some(&|target| {
            std::fs::write(target, concurrent_toml).unwrap();
        }),
    )
    .unwrap();
    assert!(
        !codex_wrote,
        "Codex 설정 동시 수정 감지 시 쓰지 않아야 한다"
    );
    assert_eq!(
        std::fs::read_to_string(&race_codex_toml).unwrap(),
        concurrent_toml
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn sync_agy_profile_hooks_copies_krill_bundle_while_preserving_existing_hooks() {
    let dir = std::env::temp_dir().join(format!("krill-agy-hook-sync-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let host_home = dir.join("host");
    let account_dir = dir.join("account");
    let host_hooks = host_home.join(".gemini/config/hooks.json");
    let account_hooks = account_dir.join(".gemini/config/hooks.json");
    std::fs::create_dir_all(host_hooks.parent().unwrap()).unwrap();
    std::fs::create_dir_all(account_hooks.parent().unwrap()).unwrap();

    std::fs::write(
        &host_hooks,
        "{\n  \"krill\": {\n    \"hooks\": {\n      \"PreInvocation\": [\n        {\n          \"hooks\": [\n            {\n              \"command\": \"'/usr/local/bin/krill' hook agy user-prompt-submit\",\n              \"type\": \"command\"\n            }\n          ]\n        }\n      ]\n    }\n  }\n}\n",
    )
    .unwrap();
    std::fs::write(
        &account_hooks,
        "{\n  \"userCustom\": {\n    \"enabled\": true\n  }\n}\n",
    )
    .unwrap();

    assert!(super::sync_agy_profile_hooks_in(&host_home, &account_dir).unwrap());
    // 두 번째 호출은 멱등(변경 없음 -> false)이어야 한다.
    assert!(!super::sync_agy_profile_hooks_in(&host_home, &account_dir).unwrap());

    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&account_hooks).unwrap()).unwrap();
    assert_eq!(
        parsed
            .pointer("/userCustom/enabled")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        parsed
            .pointer("/krill/hooks/PreInvocation/0/hooks/0/command")
            .and_then(serde_json::Value::as_str),
        Some("'/usr/local/bin/krill' hook agy user-prompt-submit")
    );

    std::fs::remove_dir_all(&dir).ok();
}
