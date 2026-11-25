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
    assert_eq!(
        value(&command, "AGY_CLI_FORCE_FILE_TOKEN_STORAGE"),
        Some("1".into())
    );
    assert_eq!(value(&command, "ANTIGRAVITY_CLI_HOME"), None);

    // Host default (empty KRILL_ACCOUNT_DIR) must strip both SSH_CONNECTION and
    // AGY_CLI_FORCE_FILE_TOKEN_STORAGE so host keychain login is never forced into file storage.
    let mut host_env = env();
    host_env.retain(|(k, _)| {
        !k.starts_with(b"KRILL_ACCOUNT_")
            && !k.eq_ignore_ascii_case(b"SSH_CONNECTION")
            && !k.eq_ignore_ascii_case(b"AGY_CLI_FORCE_FILE_TOKEN_STORAGE")
    });
    host_env.extend([
        (b"KRILL_ACCOUNT_PROVIDER".to_vec(), b"agy".to_vec()),
        (b"KRILL_ACCOUNT_DIR".to_vec(), Vec::new()),
        (b"SSH_CONNECTION".to_vec(), b"krill".to_vec()),
        (b"AGY_CLI_FORCE_FILE_TOKEN_STORAGE".to_vec(), b"1".to_vec()),
    ]);
    let host_cmd = super::command(std::env::current_exe().unwrap().as_os_str(), host_env).unwrap();
    assert_eq!(value(&host_cmd, "SSH_CONNECTION"), None);
    assert_eq!(value(&host_cmd, "AGY_CLI_FORCE_FILE_TOKEN_STORAGE"), None);
}

#[test]
fn opencode_preserves_krill_bot_instructions_dir_while_stripping_inherited_config() {
    let dir = "selected-opencode-profile";
    let instr_dir = "C:\\Users\\owen\\krill\\claude-settings\\42-instructions";
    let mut env = env();
    env.retain(|(k, _)| {
        !k.starts_with(b"KRILL_ACCOUNT_")
            && !k.eq_ignore_ascii_case(b"OPENCODE_CONFIG_DIR")
            && !k.eq_ignore_ascii_case(b"KRILL_OPENCODE_INSTRUCTIONS_DIR")
    });
    env.extend([
        (b"KRILL_ACCOUNT_PROVIDER".to_vec(), b"opencode".to_vec()),
        (b"KRILL_ACCOUNT_DIR".to_vec(), dir.as_bytes().to_vec()),
        (
            b"OPENCODE_CONFIG_DIR".to_vec(),
            b"C:\\Users\\owen\\inherited-user-config".to_vec(),
        ),
        (
            b"KRILL_OPENCODE_INSTRUCTIONS_DIR".to_vec(),
            instr_dir.as_bytes().to_vec(),
        ),
    ]);
    let command = command(std::env::current_exe().unwrap().as_os_str(), env).unwrap();
    assert_eq!(
        value(&command, "OPENCODE_CONFIG_DIR"),
        Some(OsString::from(instr_dir))
    );
    assert_eq!(value(&command, "KRILL_OPENCODE_INSTRUCTIONS_DIR"), None);
    assert_eq!(value(&command, "OPENCODE_DB"), Some("opencode.db".into()));
    assert_eq!(
        value(&command, "XDG_DATA_HOME"),
        Some(PathBuf::from(dir).join("data").into_os_string())
    );
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

#[test]
fn fallback_shell_strips_provider_and_opencode_account_isolation_envs() {
    let login = Launch {
        argv: vec![b"powershell.exe".to_vec(), b"-NoLogo".to_vec()],
        env: Vec::new(),
    };
    let shell = fallback_shell_command(&login).expect("fallback shell command");
    let removed: Vec<String> = shell
        .get_envs()
        .filter(|(_, val)| val.is_none())
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect();
    for expected in [
        "KRILL_ACCOUNT_PROVIDER",
        "KRILL_ACCOUNT_DIR",
        "KRILL_OPENCODE_INSTRUCTIONS_DIR",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "OPENCODE_DB",
    ] {
        assert!(
            removed.iter().any(|k| k.eq_ignore_ascii_case(expected)),
            "missing env_remove for {expected}: {removed:?}"
        );
    }
}

#[test]
fn opencode_instructions_dir_survives_windows_command_without_leaking_marker() {
    let instr_dir = "C:\\Users\\owen\\krill\\42-instructions-abc";
    for account_dir in ["", "selected 한글 O'wen"] {
        let mut vars = env();
        vars.retain(|(k, _)| {
            !k.eq_ignore_ascii_case(b"KRILL_ACCOUNT_PROVIDER")
                && !k.eq_ignore_ascii_case(b"KRILL_ACCOUNT_DIR")
                && !k.eq_ignore_ascii_case(b"KRILL_OPENCODE_INSTRUCTIONS_DIR")
                && !k.eq_ignore_ascii_case(b"OPENCODE_CONFIG_DIR")
        });
        vars.extend([
            (b"KRILL_ACCOUNT_PROVIDER".to_vec(), b"opencode".to_vec()),
            (
                b"KRILL_ACCOUNT_DIR".to_vec(),
                account_dir.as_bytes().to_vec(),
            ),
            (b"OPENCODE_CONFIG_DIR".to_vec(), b"/stale-override".to_vec()),
            (b"OPENAI_API_KEY".to_vec(), b"wrong-key".to_vec()),
            (
                b"KRILL_OPENCODE_INSTRUCTIONS_DIR".to_vec(),
                instr_dir.as_bytes().to_vec(),
            ),
        ]);
        let cmd = command(std::env::current_exe().unwrap().as_os_str(), vars).unwrap();
        assert_eq!(value(&cmd, "OPENCODE_CONFIG_DIR"), Some(instr_dir.into()));
        assert_eq!(value(&cmd, "KRILL_OPENCODE_INSTRUCTIONS_DIR"), None);
        assert_eq!(value(&cmd, "KRILL_ACCOUNT_PROVIDER"), None);
        assert_eq!(value(&cmd, "KRILL_ACCOUNT_DIR"), None);
        assert_eq!(value(&cmd, "OPENAI_API_KEY"), None);
        if !account_dir.is_empty() {
            assert_eq!(value(&cmd, "OPENCODE_DB"), Some("opencode.db".into()));
            assert_eq!(
                value(&cmd, "XDG_DATA_HOME"),
                Some(PathBuf::from(account_dir).join("data").into_os_string())
            );
        }
    }
}

#[test]
fn needs_agy_ssh_mode_matches_case_insensitively_across_args() {
    let with_gemini = vec![
        OsString::from("--add-dir"),
        OsString::from("/bot/home"),
        OsString::from("--gemini_dir=/profile/.gemini"),
    ];
    let without_gemini = vec![OsString::from("--add-dir"), OsString::from("/bot/home")];
    for program in ["agy", "AGY.EXE", "Antigravity.cmd", "/tools/AGY.EXE"] {
        assert!(
            needs_agy_ssh_mode(OsStr::new(program), &with_gemini),
            "{program}"
        );
        assert!(
            !needs_agy_ssh_mode(OsStr::new(program), &without_gemini),
            "{program}"
        );
    }
    assert!(!needs_agy_ssh_mode(OsStr::new("CLAUDE.EXE"), &with_gemini));
    #[cfg(windows)]
    assert!(needs_agy_ssh_mode(
        OsStr::new(r"C:\Tools\AGY.EXE"),
        &with_gemini
    ));
}

#[test]
fn windows_command_dedupes_mixed_case_path_comspec_and_markers_last_wins() {
    let exe = std::env::current_exe().unwrap();
    let exe_dir = exe.parent().unwrap();
    let exe_name = exe.file_name().unwrap();
    let vars = vec![
        (b"Path".to_vec(), b"/nonexistent-old-path".to_vec()),
        (b"ComSpec".to_vec(), b"old-cmd.exe".to_vec()),
        (b"COMSPEC".to_vec(), b"new-cmd.exe".to_vec()),
        (b"Krill_Account_Provider".to_vec(), b"claude".to_vec()),
        (b"KRILL_ACCOUNT_PROVIDER".to_vec(), b"codex".to_vec()),
        (b"Krill_Account_Dir".to_vec(), b"/stale-dir".to_vec()),
        (b"KRILL_ACCOUNT_DIR".to_vec(), b"/selected-codex".to_vec()),
        (b"Codex_Home".to_vec(), b"/leaked-codex".to_vec()),
        (b"OpenAI_Api_Key".to_vec(), b"sk-leaked".to_vec()),
        (
            b"PATH".to_vec(),
            exe_dir.as_os_str().as_encoded_bytes().to_vec(),
        ),
    ];
    let cmd = command(exe_name, vars).expect("must resolve executable from last-wins PATH");
    assert_eq!(value(&cmd, "CODEX_HOME"), Some("/selected-codex".into()));
    assert_eq!(value(&cmd, "OPENAI_API_KEY"), None);
    assert_eq!(value(&cmd, "COMSPEC"), Some("new-cmd.exe".into()));
    let path_entries: Vec<_> = cmd
        .get_envs()
        .filter(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case("PATH"))
        .collect();
    assert_eq!(
        path_entries.len(),
        1,
        "duplicate PATH entries must be deduped"
    );
    let comspec_entries: Vec<_> = cmd
        .get_envs()
        .filter(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case("COMSPEC"))
        .collect();
    assert_eq!(
        comspec_entries.len(),
        1,
        "duplicate ComSpec entries must be deduped"
    );
}

#[cfg(windows)]
#[test]
fn windows_pre_trust_rejects_drive_roots_and_avoids_verbatim_prefix() {
    let root = std::env::temp_dir().join(format!("krill-win-pretrust-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();

    for broad in [r"C:\", "C:/", r"\\?\C:\"] {
        assert!(
            crate::agent::is_too_broad_to_pre_trust(&home, std::path::Path::new(broad)),
            "drive root must be rejected: {broad}"
        );
    }

    assert!(crate::agent::pre_trust_cursor_in(&home, &workspace).unwrap());
    let expected = krill_proto::os::canonicalize(&workspace).unwrap();
    let expected_str = expected.to_str().unwrap();
    assert!(!expected_str.starts_with(r"\\?\"), "{expected_str}");
    let slug = crate::agent::cursor_project_slug(expected_str);
    assert!(!slug.starts_with("----"), "{slug}");
    let marker = home
        .join(".cursor/projects")
        .join(slug)
        .join(".workspace-trusted");
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
    assert_eq!(parsed["workspacePath"], expected_str);

    std::fs::remove_dir_all(&root).ok();
}
