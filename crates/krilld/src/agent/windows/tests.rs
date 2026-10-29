use super::*;

fn env() -> Vec<(Vec<u8>, Vec<u8>)> {
    std::env::vars_os()
        .map(|(k, v)| (k.as_encoded_bytes().to_vec(), v.as_encoded_bytes().to_vec()))
        .collect()
}

fn prepared(provider: &str, dir: &str) -> Command {
    let mut env = env();
    env.retain(|(k, _)| !k.starts_with(b"KRILL_ACCOUNT_"));
    env.extend([
        (
            b"KRILL_ACCOUNT_PROVIDER".to_vec(),
            provider.as_bytes().to_vec(),
        ),
        (b"KRILL_ACCOUNT_DIR".to_vec(), dir.as_bytes().to_vec()),
        (b"openai_api_key".to_vec(), b"wrong-key".to_vec()),
        (b"codex_home".to_vec(), b"wrong-account".to_vec()),
        (b"anthropic_api_key".to_vec(), b"wrong-key".to_vec()),
        (b"claudecode".to_vec(), b"1".to_vec()),
        (b"claude_config_dir".to_vec(), b"wrong-account".to_vec()),
    ]);
    command(std::env::current_exe().unwrap().as_os_str(), env).unwrap()
}

fn value(command: &Command, name: &str) -> Option<OsString> {
    command
        .get_envs()
        .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case(name))
        .and_then(|(_, value)| value.map(OsStr::to_owned))
}

#[test]
fn custom_batch_commands_use_the_trampoline_without_changing_native_programs() {
    for program in [r"C:\Users\한글 O'wen\claude.cmd", r"C:\tools\agent.BAT"] {
        let login = Launch {
            argv: [program, "has space", "percent% ^ ! & |", "", "trailing\\"]
                .map(|arg| arg.as_bytes().to_vec())
                .to_vec(),
            env: env(),
        };
        let launch = wrap_shell(login.clone());
        assert_eq!(launch.argv[1], b"--agent-exec");
        assert_eq!(launch.argv[3..], login.argv);
        assert_eq!(launch.env, login.env);
    }
    for program in [r"C:\tools\node.exe", "powershell.exe"] {
        let login = Launch {
            argv: vec![program.as_bytes().to_vec()],
            env: env(),
        };
        assert_eq!(wrap_shell(login.clone()), login);
    }
}

#[test]
fn windows_wrapper_keeps_argv_literal_and_accepts_later_session_flags() {
    let args = vec![
        "codex".into(),
        "has space 한글".into(),
        "it's \"quoted\"; $x & end\\".into(),
        String::new(),
    ];
    let login = Launch {
        argv: vec![b"powershell.exe".to_vec()],
        env: env(),
    };
    let mut launch = wrap(login.clone(), "install hint", args.clone(), true);
    launch
        .argv
        .extend([b"resume".to_vec(), b"conversation-id".to_vec()]);
    assert_eq!(launch.argv[1], AGENT_THEN_SHELL);
    assert_eq!(launch.argv[2], b"install hint");
    assert_eq!(
        launch.argv[3..7],
        args.iter()
            .map(|arg| arg.as_bytes().to_vec())
            .collect::<Vec<_>>()
    );
    assert_eq!(launch.argv[7..], [b"resume".as_slice(), b"conversation-id"]);
    assert_eq!(launch.env, login.env);
}

#[test]
fn selected_and_default_accounts_clear_credentials_and_markers_case_insensitively() {
    for (provider, key) in [("codex", "CODEX_HOME"), ("claude", "CLAUDE_CONFIG_DIR")] {
        for dir in ["C:\\Users\\한글 O'wen\\selected", ""] {
            let command = prepared(provider, dir);
            assert_eq!(
                value(&command, key),
                (!dir.is_empty()).then(|| OsString::from(dir))
            );
            let credential = if provider == "codex" {
                "OPENAI_API_KEY"
            } else {
                "ANTHROPIC_API_KEY"
            };
            assert_eq!(value(&command, credential), None);
            if provider == "claude" {
                assert_eq!(value(&command, "CLAUDECODE"), None);
            }
            assert_eq!(value(&command, "KRILL_ACCOUNT_PROVIDER"), None);
            assert_eq!(value(&command, "KRILL_ACCOUNT_DIR"), None);
        }
    }
}

#[test]
fn opencode_and_agy_keep_their_account_isolation() {
    let dir = "selected 한글 O'wen";
    let command = prepared("opencode", dir);
    assert_eq!(value(&command, "OPENCODE_CONFIG_DIR"), None);
    assert_eq!(value(&command, "OPENAI_API_KEY"), None);
    assert_eq!(value(&command, "OPENCODE_DB"), Some("opencode.db".into()));
    for (key, suffix) in [("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state")] {
        assert_eq!(
            value(&command, key),
            Some(PathBuf::from(dir).join(suffix).into_os_string())
        );
    }
    let command = prepared("agy", dir);
    assert_eq!(value(&command, "SSH_CONNECTION"), Some("krill".into()));
    assert_eq!(value(&command, "ANTIGRAVITY_CLI_HOME"), None);
}

#[test]
fn missing_agent_is_not_resolved_to_a_shell() {
    assert!(command(OsStr::new("krill-no-such-agent-20261023"), env()).is_none());
}

#[test]
fn codex_trust_preset_uses_the_native_project_path() {
    let root = std::env::temp_dir().join(format!("krill trust 한글 {}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let profile = root.join("account");
    let workspace = root.join("한국어 프로젝트");
    for dir in [&home, &profile, &workspace] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let config = profile.join("config.toml");
    std::fs::write(&config, "# user settings\nmodel = 'mine'\n").unwrap();
    assert!(crate::agent::pre_trust_codex_in(&home, Some(&profile), &workspace).unwrap());
    assert!(!crate::agent::pre_trust_codex_in(&home, Some(&profile), &workspace).unwrap());
    let text = std::fs::read_to_string(&config).unwrap();
    let value: toml::Value = toml::from_str(&text).unwrap();
    let native = krill_proto::os::canonicalize(&workspace).unwrap();
    assert_eq!(
        value["projects"][native.to_str().unwrap()]["trust_level"].as_str(),
        Some("trusted")
    );
    assert_eq!(value["projects"].as_table().unwrap().len(), 1);
    assert_eq!(value["model"].as_str(), Some("mine"));
    assert!(text.contains("# user settings"));
    std::fs::remove_dir_all(root).unwrap();
}
